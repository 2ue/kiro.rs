use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration as StdDuration, Instant};

use parking_lot::{Mutex, RwLock};
use tokio::runtime::{Handle, Runtime, RuntimeFlavor};
use tokio::sync::{Mutex as AsyncMutex, Notify, mpsc};
use tokio::task::JoinHandle;

const STORAGE_TASK_QUEUE_CAPACITY: usize = 2048;
const STORAGE_TASK_WORKER_COUNT: usize = 8;
const STORAGE_CRITICAL_TASK_QUEUE_CAPACITY: usize = 256;
const STORAGE_CRITICAL_TASK_WORKER_COUNT: usize = 2;
const STORAGE_TASK_TIMEOUT: StdDuration = StdDuration::from_secs(5);
const STORAGE_ABORT_JOIN_TIMEOUT: StdDuration = StdDuration::from_millis(100);

static STORAGE_REGISTRY: StorageExecutorRegistry = StorageExecutorRegistry::new();

type StorageFuture = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'static>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StorageTaskLane {
    BestEffort,
    Critical,
}

impl StorageTaskLane {
    fn as_str(self) -> &'static str {
        match self {
            Self::BestEffort => "best_effort",
            Self::Critical => "critical",
        }
    }
}

struct StorageTask {
    operation: &'static str,
    lane: StorageTaskLane,
    future: StorageFuture,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StorageTaskStats {
    pub accepting: bool,
    pub queue_capacity: usize,
    pub queue_available: usize,
    pub critical_queue_capacity: usize,
    pub critical_queue_available: usize,
    pub accepted: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub timed_out: u64,
    pub rejected_full: u64,
    pub rejected_closed: u64,
    pub finished: u64,
    pub critical_accepted: u64,
    pub critical_succeeded: u64,
    pub critical_failed: u64,
    pub critical_timed_out: u64,
    pub critical_rejected_full: u64,
    pub critical_rejected_closed: u64,
    pub critical_finished: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StorageTaskDrainReport {
    pub target: u64,
    pub finished: u64,
    pub drained: bool,
    pub timed_out: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StorageTaskShutdownReport {
    pub already_started: bool,
    pub drained: bool,
    pub timed_out: bool,
    pub abandoned: u64,
    pub stats: StorageTaskStats,
}

#[derive(Default)]
struct StorageTaskProgress {
    accepted: AtomicU64,
    succeeded: AtomicU64,
    failed: AtomicU64,
    timed_out: AtomicU64,
    rejected_full: AtomicU64,
    rejected_closed: AtomicU64,
    finished: AtomicU64,
    critical_accepted: AtomicU64,
    critical_succeeded: AtomicU64,
    critical_failed: AtomicU64,
    critical_timed_out: AtomicU64,
    critical_rejected_full: AtomicU64,
    critical_rejected_closed: AtomicU64,
    critical_finished: AtomicU64,
    changed: Notify,
}

impl StorageTaskProgress {
    fn accept(&self, lane: StorageTaskLane) {
        self.accepted.fetch_add(1, Ordering::Release);
        if lane == StorageTaskLane::Critical {
            self.critical_accepted.fetch_add(1, Ordering::Release);
        }
    }

    fn reject_full(&self, lane: StorageTaskLane) -> u64 {
        let rejected = self.rejected_full.fetch_add(1, Ordering::Relaxed) + 1;
        if lane == StorageTaskLane::Critical {
            self.critical_rejected_full.fetch_add(1, Ordering::Relaxed);
        }
        rejected
    }

    fn reject_closed(&self, lane: StorageTaskLane) -> u64 {
        let rejected = self.rejected_closed.fetch_add(1, Ordering::Relaxed) + 1;
        if lane == StorageTaskLane::Critical {
            self.critical_rejected_closed
                .fetch_add(1, Ordering::Relaxed);
        }
        rejected
    }

    fn succeed(&self, lane: StorageTaskLane) {
        self.succeeded.fetch_add(1, Ordering::Relaxed);
        if lane == StorageTaskLane::Critical {
            self.critical_succeeded.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn fail(&self, lane: StorageTaskLane) {
        self.failed.fetch_add(1, Ordering::Relaxed);
        if lane == StorageTaskLane::Critical {
            self.critical_failed.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn time_out(&self, lane: StorageTaskLane) {
        self.timed_out.fetch_add(1, Ordering::Relaxed);
        if lane == StorageTaskLane::Critical {
            self.critical_timed_out.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn finish(&self, lane: StorageTaskLane) {
        self.finished.fetch_add(1, Ordering::Release);
        if lane == StorageTaskLane::Critical {
            self.critical_finished.fetch_add(1, Ordering::Release);
        }
        self.changed.notify_waiters();
    }
}

struct BestEffortStorageExecutorInner {
    lifecycle: RwLock<()>,
    sender: Mutex<Option<mpsc::Sender<StorageTask>>>,
    critical_sender: Mutex<Option<mpsc::Sender<StorageTask>>>,
    workers: Mutex<Option<Vec<JoinHandle<()>>>>,
    accepting: AtomicBool,
    shutdown_started: AtomicBool,
    shutdown_complete: AtomicBool,
    shutdown_timed_out: AtomicBool,
    shutdown_changed: Notify,
    progress: Arc<StorageTaskProgress>,
    queue_capacity: usize,
    critical_queue_capacity: usize,
}

#[derive(Clone)]
struct BestEffortStorageExecutor {
    inner: Arc<BestEffortStorageExecutorInner>,
}

impl BestEffortStorageExecutor {
    #[cfg(test)]
    fn new(
        handle: &Handle,
        queue_capacity: usize,
        worker_count: usize,
        task_timeout: StdDuration,
    ) -> Self {
        Self::new_with_critical_lane(
            handle,
            queue_capacity,
            worker_count,
            queue_capacity,
            1,
            task_timeout,
        )
    }

    fn new_with_critical_lane(
        handle: &Handle,
        queue_capacity: usize,
        worker_count: usize,
        critical_queue_capacity: usize,
        critical_worker_count: usize,
        task_timeout: StdDuration,
    ) -> Self {
        let queue_capacity = queue_capacity.max(1);
        let worker_count = worker_count.max(1);
        let critical_queue_capacity = critical_queue_capacity.max(1);
        let critical_worker_count = critical_worker_count.max(1);
        let task_timeout = task_timeout.max(StdDuration::from_millis(1));
        let progress = Arc::new(StorageTaskProgress::default());

        let (sender, receiver) = mpsc::channel(queue_capacity);
        let receiver = Arc::new(AsyncMutex::new(receiver));
        let (critical_sender, critical_receiver) = mpsc::channel(critical_queue_capacity);
        let critical_receiver = Arc::new(AsyncMutex::new(critical_receiver));
        let mut workers = Vec::with_capacity(worker_count + critical_worker_count);
        for worker_id in 0..worker_count {
            workers.push(handle.spawn(storage_worker_loop(
                worker_id,
                StorageTaskLane::BestEffort,
                receiver.clone(),
                progress.clone(),
                task_timeout,
            )));
        }
        for worker_id in 0..critical_worker_count {
            workers.push(handle.spawn(storage_worker_loop(
                worker_id,
                StorageTaskLane::Critical,
                critical_receiver.clone(),
                progress.clone(),
                task_timeout,
            )));
        }

        Self {
            inner: Arc::new(BestEffortStorageExecutorInner {
                lifecycle: RwLock::new(()),
                sender: Mutex::new(Some(sender)),
                critical_sender: Mutex::new(Some(critical_sender)),
                workers: Mutex::new(Some(workers)),
                accepting: AtomicBool::new(true),
                shutdown_started: AtomicBool::new(false),
                shutdown_complete: AtomicBool::new(false),
                shutdown_timed_out: AtomicBool::new(false),
                shutdown_changed: Notify::new(),
                progress,
                queue_capacity,
                critical_queue_capacity,
            }),
        }
    }

    fn try_submit(
        &self,
        operation: &'static str,
        future: impl Future<Output = anyhow::Result<()>> + Send + 'static,
    ) -> bool {
        self.try_submit_to_lane(StorageTaskLane::BestEffort, operation, future)
    }

    fn try_submit_critical(
        &self,
        operation: &'static str,
        future: impl Future<Output = anyhow::Result<()>> + Send + 'static,
    ) -> bool {
        self.try_submit_to_lane(StorageTaskLane::Critical, operation, future)
    }

    fn try_submit_to_lane(
        &self,
        lane: StorageTaskLane,
        operation: &'static str,
        future: impl Future<Output = anyhow::Result<()>> + Send + 'static,
    ) -> bool {
        let _lifecycle = self.inner.lifecycle.read();
        if !self.inner.accepting.load(Ordering::Acquire) {
            self.reject_closed(lane, operation);
            return false;
        }
        let sender = match lane {
            StorageTaskLane::BestEffort => self.inner.sender.lock().as_ref().cloned(),
            StorageTaskLane::Critical => self.inner.critical_sender.lock().as_ref().cloned(),
        };
        let Some(sender) = sender else {
            self.reject_closed(lane, operation);
            return false;
        };
        let task = StorageTask {
            operation,
            lane,
            future: Box::pin(future),
        };
        match sender.try_send(task) {
            Ok(()) => {
                self.inner.progress.accept(lane);
                true
            }
            Err(mpsc::error::TrySendError::Full(task)) => {
                let rejected = self.inner.progress.reject_full(lane);
                if should_log_counter(rejected) {
                    tracing::warn!(
                        operation = task.operation,
                        lane = lane.as_str(),
                        rejected,
                        queue_capacity = sender.max_capacity(),
                        "存储任务队列已满，拒绝新任务；调用方必须执行降级路径"
                    );
                }
                false
            }
            Err(mpsc::error::TrySendError::Closed(task)) => {
                self.reject_closed(lane, task.operation);
                false
            }
        }
    }

    fn reject_closed(&self, lane: StorageTaskLane, operation: &'static str) {
        let rejected = self.inner.progress.reject_closed(lane);
        if should_log_counter(rejected) {
            tracing::warn!(
                operation,
                lane = lane.as_str(),
                rejected,
                "存储执行器已关闭，拒绝新任务；调用方必须执行降级路径"
            );
        }
    }

    fn stats(&self) -> StorageTaskStats {
        let (queue_available, queue_capacity) = self
            .inner
            .sender
            .lock()
            .as_ref()
            .map(|sender| (sender.capacity(), sender.max_capacity()))
            .unwrap_or((0, self.inner.queue_capacity));
        let (critical_queue_available, critical_queue_capacity) = self
            .inner
            .critical_sender
            .lock()
            .as_ref()
            .map(|sender| (sender.capacity(), sender.max_capacity()))
            .unwrap_or((0, self.inner.critical_queue_capacity));
        let progress = &self.inner.progress;
        StorageTaskStats {
            accepting: self.inner.accepting.load(Ordering::Acquire),
            queue_capacity,
            queue_available,
            critical_queue_capacity,
            critical_queue_available,
            accepted: progress.accepted.load(Ordering::Acquire),
            succeeded: progress.succeeded.load(Ordering::Acquire),
            failed: progress.failed.load(Ordering::Acquire),
            timed_out: progress.timed_out.load(Ordering::Acquire),
            rejected_full: progress.rejected_full.load(Ordering::Acquire),
            rejected_closed: progress.rejected_closed.load(Ordering::Acquire),
            finished: progress.finished.load(Ordering::Acquire),
            critical_accepted: progress.critical_accepted.load(Ordering::Acquire),
            critical_succeeded: progress.critical_succeeded.load(Ordering::Acquire),
            critical_failed: progress.critical_failed.load(Ordering::Acquire),
            critical_timed_out: progress.critical_timed_out.load(Ordering::Acquire),
            critical_rejected_full: progress.critical_rejected_full.load(Ordering::Acquire),
            critical_rejected_closed: progress.critical_rejected_closed.load(Ordering::Acquire),
            critical_finished: progress.critical_finished.load(Ordering::Acquire),
        }
    }

    async fn drain(&self, timeout: StdDuration) -> StorageTaskDrainReport {
        let target = self.inner.progress.accepted.load(Ordering::Acquire);
        let wait = async {
            loop {
                let changed = self.inner.progress.changed.notified();
                let finished = self.inner.progress.finished.load(Ordering::Acquire);
                if finished >= target {
                    return finished;
                }
                changed.await;
            }
        };
        match tokio::time::timeout(timeout, wait).await {
            Ok(finished) => StorageTaskDrainReport {
                target,
                finished,
                drained: true,
                timed_out: false,
            },
            Err(_) => StorageTaskDrainReport {
                target,
                finished: self.inner.progress.finished.load(Ordering::Acquire),
                drained: false,
                timed_out: true,
            },
        }
    }

    async fn shutdown(&self, timeout: StdDuration) -> StorageTaskShutdownReport {
        let already_started = self
            .inner
            .shutdown_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err();
        if !already_started {
            {
                let _lifecycle = self.inner.lifecycle.write();
                self.inner.accepting.store(false, Ordering::Release);
                self.inner.sender.lock().take();
                self.inner.critical_sender.lock().take();
            }
            let inner = self.inner.clone();
            tokio::spawn(async move {
                complete_storage_shutdown(inner, timeout).await;
            });
        }

        let wait_timed_out = tokio::time::timeout(timeout, self.wait_for_shutdown())
            .await
            .is_err();
        self.shutdown_report(already_started, wait_timed_out)
    }

    async fn wait_for_shutdown(&self) {
        loop {
            let changed = self.inner.shutdown_changed.notified();
            if self.inner.shutdown_complete.load(Ordering::Acquire) {
                return;
            }
            changed.await;
        }
    }

    fn shutdown_report(
        &self,
        already_started: bool,
        wait_timed_out: bool,
    ) -> StorageTaskShutdownReport {
        let stats = self.stats();
        let abandoned = stats.accepted.saturating_sub(stats.finished);
        let cleanup_timed_out = self.inner.shutdown_timed_out.load(Ordering::Acquire);
        let timed_out = wait_timed_out || cleanup_timed_out;
        StorageTaskShutdownReport {
            already_started,
            drained: self.inner.shutdown_complete.load(Ordering::Acquire)
                && !timed_out
                && abandoned == 0,
            timed_out,
            abandoned,
            stats,
        }
    }
}

async fn complete_storage_shutdown(
    inner: Arc<BestEffortStorageExecutorInner>,
    timeout: StdDuration,
) {
    let workers = inner.workers.lock().take().unwrap_or_default();
    let timed_out = wait_for_storage_workers(workers, timeout).await;
    inner.shutdown_timed_out.store(timed_out, Ordering::Release);
    inner.shutdown_complete.store(true, Ordering::Release);
    inner.shutdown_changed.notify_waiters();
}

async fn wait_for_storage_workers(workers: Vec<JoinHandle<()>>, timeout: StdDuration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut workers = workers.into_iter();
    let mut remaining_workers = loop {
        let Some(mut worker) = workers.next() else {
            return false;
        };
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if !remaining.is_zero() {
            match tokio::time::timeout(remaining, &mut worker).await {
                Ok(Ok(())) => continue,
                Ok(Err(err)) => {
                    tracing::warn!("存储 worker 异常退出: {}", err);
                    continue;
                }
                Err(_) => {}
            }
        }
        let mut remaining_workers = vec![worker];
        remaining_workers.extend(workers);
        break remaining_workers;
    };

    for worker in &remaining_workers {
        worker.abort();
    }
    if tokio::time::timeout(STORAGE_ABORT_JOIN_TIMEOUT, async {
        for worker in &mut remaining_workers {
            let _ = worker.await;
        }
    })
    .await
    .is_err()
    {
        tracing::warn!(
            timeout_ms = STORAGE_ABORT_JOIN_TIMEOUT.as_millis() as u64,
            "等待已取消的存储 worker 退出再次超时"
        );
    }
    true
}

async fn storage_worker_loop(
    worker_id: usize,
    lane: StorageTaskLane,
    receiver: Arc<AsyncMutex<mpsc::Receiver<StorageTask>>>,
    progress: Arc<StorageTaskProgress>,
    task_timeout: StdDuration,
) {
    loop {
        let task = {
            let mut receiver = receiver.lock().await;
            receiver.recv().await
        };
        let Some(task) = task else {
            break;
        };
        debug_assert_eq!(task.lane, lane);
        let started_at = Instant::now();
        match tokio::time::timeout(task_timeout, task.future).await {
            Ok(Ok(())) => progress.succeed(lane),
            Ok(Err(err)) => {
                progress.fail(lane);
                tracing::warn!(
                    operation = task.operation,
                    lane = lane.as_str(),
                    worker_id,
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    "存储任务失败: {}",
                    err
                );
            }
            Err(_) => {
                progress.time_out(lane);
                tracing::warn!(
                    operation = task.operation,
                    lane = lane.as_str(),
                    worker_id,
                    timeout_ms = task_timeout.as_millis() as u64,
                    "存储任务超时"
                );
            }
        }
        progress.finish(lane);
    }
}

struct StorageExecutorRegistryState {
    accepting: bool,
    shutdown_started: bool,
}

struct StorageExecutorRegistry {
    state: Mutex<StorageExecutorRegistryState>,
    executor: OnceLock<BestEffortStorageExecutor>,
    rejected_closed: AtomicU64,
    critical_rejected_closed: AtomicU64,
}

impl StorageExecutorRegistry {
    const fn new() -> Self {
        Self {
            state: Mutex::new(StorageExecutorRegistryState {
                accepting: true,
                shutdown_started: false,
            }),
            executor: OnceLock::new(),
            rejected_closed: AtomicU64::new(0),
            critical_rejected_closed: AtomicU64::new(0),
        }
    }

    fn try_submit_with<F, I>(
        &self,
        lane: StorageTaskLane,
        operation: &'static str,
        future: F,
        init: I,
    ) -> bool
    where
        F: Future<Output = anyhow::Result<()>> + Send + 'static,
        I: FnOnce() -> BestEffortStorageExecutor,
    {
        let state = self.state.lock();
        if !state.accepting {
            let rejected = self.rejected_closed.fetch_add(1, Ordering::Relaxed) + 1;
            if lane == StorageTaskLane::Critical {
                self.critical_rejected_closed
                    .fetch_add(1, Ordering::Relaxed);
            }
            if should_log_counter(rejected) {
                tracing::warn!(
                    operation,
                    lane = lane.as_str(),
                    rejected,
                    "全局存储执行器已关闭，拒绝初始化或提交"
                );
            }
            return false;
        }
        let executor = self.executor.get_or_init(init);
        match lane {
            StorageTaskLane::BestEffort => executor.try_submit(operation, future),
            StorageTaskLane::Critical => executor.try_submit_critical(operation, future),
        }
    }

    fn begin_shutdown(&self) -> bool {
        let mut state = self.state.lock();
        let already_started = state.shutdown_started;
        state.shutdown_started = true;
        state.accepting = false;
        already_started
    }

    fn executor(&self) -> Option<&BestEffortStorageExecutor> {
        self.executor.get()
    }

    fn stats(&self) -> StorageTaskStats {
        let state = self.state.lock();
        let mut stats = self
            .executor
            .get()
            .map(BestEffortStorageExecutor::stats)
            .unwrap_or_default();
        stats.accepting = state.accepting
            && self
                .executor
                .get()
                .map(|executor| executor.inner.accepting.load(Ordering::Acquire))
                .unwrap_or(true);
        stats.rejected_closed = stats
            .rejected_closed
            .saturating_add(self.rejected_closed.load(Ordering::Acquire));
        stats.critical_rejected_closed = stats
            .critical_rejected_closed
            .saturating_add(self.critical_rejected_closed.load(Ordering::Acquire));
        stats
    }
}

fn should_log_counter(value: u64) -> bool {
    value == 1 || value.is_power_of_two()
}

fn storage_fallback_runtime() -> &'static Runtime {
    static FALLBACK_RUNTIME: OnceLock<Runtime> = OnceLock::new();
    FALLBACK_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("kiro-storage-task")
            .enable_all()
            .build()
            .expect("创建 best-effort 存储 runtime 失败")
    })
}

fn storage_executor_handle() -> Handle {
    storage_fallback_runtime().handle().clone()
}

fn default_storage_executor() -> BestEffortStorageExecutor {
    BestEffortStorageExecutor::new_with_critical_lane(
        &storage_executor_handle(),
        STORAGE_TASK_QUEUE_CAPACITY,
        STORAGE_TASK_WORKER_COUNT,
        STORAGE_CRITICAL_TASK_QUEUE_CAPACITY,
        STORAGE_CRITICAL_TASK_WORKER_COUNT,
        STORAGE_TASK_TIMEOUT,
    )
}

pub(crate) fn block_on_storage<T: Send>(
    operation: &'static str,
    future: impl Future<Output = anyhow::Result<T>> + Send,
) -> anyhow::Result<T> {
    let started_at = Instant::now();
    let result = if let Ok(handle) = Handle::try_current() {
        match handle.runtime_flavor() {
            RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(|| storage_fallback_runtime().block_on(future))
            }
            _ => std::thread::scope(|scope| {
                scope
                    .spawn(move || storage_fallback_runtime().block_on(future))
                    .join()
                    .map_err(|_| anyhow::anyhow!("同步存储线程异常退出"))?
            }),
        }
    } else {
        storage_fallback_runtime().block_on(future)
    };
    let elapsed = started_at.elapsed();
    if elapsed >= StdDuration::from_millis(100) {
        tracing::warn!(
            operation,
            elapsed_ms = elapsed.as_millis() as u64,
            "同步存储操作耗时较长"
        );
    }
    result.map_err(|err| anyhow::anyhow!("{}失败: {}", operation, err))
}

/// Returns `false` when the bounded queue did not accept the task.
pub(crate) fn spawn_best_effort_storage_task(
    operation: &'static str,
    future: impl Future<Output = anyhow::Result<()>> + Send + 'static,
) -> bool {
    STORAGE_REGISTRY.try_submit_with(
        StorageTaskLane::BestEffort,
        operation,
        future,
        default_storage_executor,
    )
}

/// Returns `false` when the reserved lane did not accept the task; callers must run their
/// operation-specific reliability fallback in that case.
pub(crate) fn spawn_critical_storage_task(
    operation: &'static str,
    future: impl Future<Output = anyhow::Result<()>> + Send + 'static,
) -> bool {
    STORAGE_REGISTRY.try_submit_with(
        StorageTaskLane::Critical,
        operation,
        future,
        default_storage_executor,
    )
}

pub async fn drain_best_effort_storage_tasks(timeout: StdDuration) -> StorageTaskDrainReport {
    let Some(executor) = STORAGE_REGISTRY.executor() else {
        return StorageTaskDrainReport {
            drained: true,
            ..StorageTaskDrainReport::default()
        };
    };
    executor.drain(timeout).await
}

pub async fn shutdown_best_effort_storage_tasks(timeout: StdDuration) -> StorageTaskShutdownReport {
    let registry_already_started = STORAGE_REGISTRY.begin_shutdown();
    let Some(executor) = STORAGE_REGISTRY.executor() else {
        return StorageTaskShutdownReport {
            already_started: registry_already_started,
            drained: true,
            stats: STORAGE_REGISTRY.stats(),
            ..StorageTaskShutdownReport::default()
        };
    };
    let mut report = executor.shutdown(timeout).await;
    report.already_started |= registry_already_started;
    report.stats = STORAGE_REGISTRY.stats();
    report
}

pub fn best_effort_storage_task_stats() -> StorageTaskStats {
    STORAGE_REGISTRY.stats()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{Barrier, Notify, Semaphore};

    fn test_executor(
        queue_capacity: usize,
        worker_count: usize,
        task_timeout: StdDuration,
    ) -> BestEffortStorageExecutor {
        BestEffortStorageExecutor::new(
            &Handle::current(),
            queue_capacity,
            worker_count,
            task_timeout,
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bounded_queue_rejects_excess_and_shutdown_drains_accepted_tasks() {
        let executor = test_executor(1, 1, StdDuration::from_secs(1));
        let gate = Arc::new(Semaphore::new(0));
        let started = Arc::new(Notify::new());
        let completed = Arc::new(AtomicUsize::new(0));

        let first_gate = gate.clone();
        let first_started = started.clone();
        let first_completed = completed.clone();
        assert!(executor.try_submit("first", async move {
            first_started.notify_one();
            let _permit = first_gate.acquire().await?;
            first_completed.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }));
        started.notified().await;

        let second_gate = gate.clone();
        let second_completed = completed.clone();
        assert!(executor.try_submit("second", async move {
            let _permit = second_gate.acquire().await?;
            second_completed.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }));
        assert!(!executor.try_submit("excess", async { Ok(()) }));

        let stats = executor.stats();
        assert_eq!(stats.accepted, 2);
        assert_eq!(stats.rejected_full, 1);
        assert_eq!(stats.queue_capacity, 1);

        gate.add_permits(2);
        let report = executor.shutdown(StdDuration::from_secs(1)).await;
        assert!(report.drained);
        assert!(!report.timed_out);
        assert_eq!(report.abandoned, 0);
        assert_eq!(completed.load(Ordering::Relaxed), 2);
        assert_eq!(report.stats.succeeded, 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn critical_lane_remains_available_when_best_effort_lane_is_full() {
        let executor = test_executor(1, 1, StdDuration::from_secs(1));
        let gate = Arc::new(Semaphore::new(0));
        let started = Arc::new(Notify::new());
        let critical_completed = Arc::new(Notify::new());

        let first_gate = gate.clone();
        let first_started = started.clone();
        assert!(executor.try_submit("busy", async move {
            first_started.notify_one();
            let _permit = first_gate.acquire().await?;
            Ok(())
        }));
        started.notified().await;
        let queued_gate = gate.clone();
        assert!(executor.try_submit("queued", async move {
            let _permit = queued_gate.acquire().await?;
            Ok(())
        }));
        assert!(!executor.try_submit("full", async { Ok(()) }));

        let completed = critical_completed.clone();
        assert!(executor.try_submit_critical("release lease", async move {
            completed.notify_one();
            Ok(())
        }));
        tokio::time::timeout(StdDuration::from_millis(200), critical_completed.notified())
            .await
            .expect("critical lane should make progress independently");
        assert_eq!(executor.stats().critical_accepted, 1);

        gate.add_permits(2);
        assert!(executor.shutdown(StdDuration::from_secs(1)).await.drained);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn critical_lane_is_bounded_and_reports_failed_admission() {
        let executor = test_executor(1, 1, StdDuration::from_secs(1));
        let gate = Arc::new(Semaphore::new(0));
        let started = Arc::new(Notify::new());
        let first_gate = gate.clone();
        let first_started = started.clone();
        assert!(executor.try_submit_critical("critical busy", async move {
            first_started.notify_one();
            let _permit = first_gate.acquire().await?;
            Ok(())
        }));
        started.notified().await;

        let queued_gate = gate.clone();
        assert!(executor.try_submit_critical("critical queued", async move {
            let _permit = queued_gate.acquire().await?;
            Ok(())
        }));
        assert!(!executor.try_submit_critical("critical full", async { Ok(()) }));
        let stats = executor.stats();
        assert_eq!(stats.critical_accepted, 2);
        assert_eq!(stats.critical_rejected_full, 1);

        gate.add_permits(2);
        assert!(executor.shutdown(StdDuration::from_secs(1)).await.drained);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn task_timeout_is_counted_and_drain_completes() {
        let executor = test_executor(2, 1, StdDuration::from_millis(20));
        assert!(executor.try_submit("never", std::future::pending()));

        let drained = executor.drain(StdDuration::from_secs(1)).await;
        assert!(drained.drained);
        assert!(!drained.timed_out);
        assert_eq!(drained.target, 1);

        let report = executor.shutdown(StdDuration::from_secs(1)).await;
        assert!(report.drained);
        assert_eq!(report.stats.timed_out, 1);
        assert_eq!(report.stats.finished, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_timeout_aborts_remaining_workers_and_still_completes() {
        let executor = test_executor(1, 1, StdDuration::from_secs(5));
        let started = Arc::new(Notify::new());
        let worker_started = started.clone();
        assert!(executor.try_submit("stuck", async move {
            worker_started.notify_one();
            std::future::pending::<anyhow::Result<()>>().await
        }));
        started.notified().await;

        let first = executor.shutdown(StdDuration::from_millis(20)).await;
        assert!(first.timed_out);
        let repeated = executor.shutdown(StdDuration::from_secs(1)).await;
        assert!(repeated.already_started);
        assert!(repeated.timed_out);
        assert!(!repeated.drained);
        assert_eq!(repeated.abandoned, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_closes_submission_and_is_idempotent() {
        let executor = test_executor(2, 1, StdDuration::from_secs(1));
        assert!(executor.try_submit("done", async { Ok(()) }));

        let first = executor.shutdown(StdDuration::from_secs(1)).await;
        assert!(first.drained);
        assert!(!executor.try_submit("late", async { Ok(()) }));
        assert_eq!(executor.stats().rejected_closed, 1);

        let second = executor.shutdown(StdDuration::from_secs(1)).await;
        assert!(second.already_started);
        assert!(second.drained);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_shutdown_owner_does_not_strand_executor() {
        let executor = test_executor(1, 1, StdDuration::from_secs(1));
        let gate = Arc::new(Semaphore::new(0));
        let started = Arc::new(Notify::new());
        let worker_gate = gate.clone();
        let worker_started = started.clone();
        assert!(executor.try_submit("slow", async move {
            worker_started.notify_one();
            let _permit = worker_gate.acquire().await?;
            Ok(())
        }));
        started.notified().await;

        let shutdown_executor = executor.clone();
        let owner =
            tokio::spawn(
                async move { shutdown_executor.shutdown(StdDuration::from_secs(1)).await },
            );
        tokio::time::timeout(StdDuration::from_millis(200), async {
            while executor.stats().accepting {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("shutdown should close admission promptly");
        owner.abort();
        let _ = owner.await;

        gate.add_permits(1);
        let report = executor.shutdown(StdDuration::from_secs(1)).await;
        assert!(report.already_started);
        assert!(report.drained);
        assert!(!report.timed_out);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn registry_linearizes_first_submit_against_shutdown() {
        for _ in 0..32 {
            let registry = Arc::new(StorageExecutorRegistry::new());
            let barrier = Arc::new(Barrier::new(2));
            let initialized = Arc::new(AtomicUsize::new(0));
            let handle = Handle::current();

            let submit_registry = registry.clone();
            let submit_barrier = barrier.clone();
            let submit_initialized = initialized.clone();
            let submit_handle = handle.clone();
            let submit = tokio::spawn(async move {
                submit_barrier.wait().await;
                submit_registry.try_submit_with(
                    StorageTaskLane::BestEffort,
                    "racing submit",
                    async { Ok(()) },
                    || {
                        submit_initialized.fetch_add(1, Ordering::Relaxed);
                        BestEffortStorageExecutor::new(
                            &submit_handle,
                            1,
                            1,
                            StdDuration::from_secs(1),
                        )
                    },
                )
            });

            let close_registry = registry.clone();
            let close_barrier = barrier.clone();
            let close = tokio::spawn(async move {
                close_barrier.wait().await;
                close_registry.begin_shutdown()
            });
            let accepted = submit.await.unwrap();
            let _ = close.await.unwrap();

            assert!(!registry.try_submit_with(
                StorageTaskLane::BestEffort,
                "late",
                async { Ok(()) },
                || panic!("closed registry must not initialize"),
            ));
            assert_eq!(initialized.load(Ordering::Relaxed), usize::from(accepted));
            if let Some(executor) = registry.executor() {
                assert!(executor.shutdown(StdDuration::from_secs(1)).await.drained);
            }
        }
    }

    #[test]
    fn uninitialized_registry_shutdown_permanently_blocks_initialization() {
        let registry = StorageExecutorRegistry::new();
        assert!(!registry.begin_shutdown());
        assert!(registry.executor().is_none());
        assert!(!registry.try_submit_with(
            StorageTaskLane::Critical,
            "late critical task",
            async { Ok(()) },
            || panic!("shutdown registry must not initialize"),
        ));
        let stats = registry.stats();
        assert!(!stats.accepting);
        assert_eq!(stats.rejected_closed, 1);
        assert_eq!(stats.critical_rejected_closed, 1);
        assert!(registry.begin_shutdown());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn block_on_storage_works_inside_current_thread_runtime() {
        let value = block_on_storage("current-thread bridge", async {
            tokio::time::sleep(StdDuration::from_millis(1)).await;
            Ok::<_, anyhow::Error>(42)
        })
        .unwrap();
        assert_eq!(value, 42);
    }
}
