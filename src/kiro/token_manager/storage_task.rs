use std::future::Future;
use std::time::{Duration as StdDuration, Instant};

pub(super) fn block_on_storage<T>(
    operation: &'static str,
    future: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    let started_at = Instant::now();
    let result = if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(future))
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(future)
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

pub(super) fn spawn_best_effort_storage_task(
    operation: &'static str,
    future: impl Future<Output = anyhow::Result<()>> + Send + 'static,
) {
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(async move {
            if let Err(err) = future.await {
                tracing::warn!("{}失败: {}", operation, err);
            }
        });
        return;
    }

    if let Err(err) = std::thread::Builder::new()
        .name(format!("kiro-{}", operation))
        .spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(anyhow::Error::from)
                .and_then(|runtime| runtime.block_on(future));
            if let Err(err) = result {
                tracing::warn!("{}失败: {}", operation, err);
            }
        })
    {
        tracing::warn!("{}任务启动失败: {}", operation, err);
    }
}
