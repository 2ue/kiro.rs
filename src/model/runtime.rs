//! 运行时配置快照、持久化与变更订阅。

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use tokio::runtime::{Handle, Runtime, RuntimeFlavor};

use super::config::Config;
use crate::storage::postgres::PostgresStore;

type Listener = Arc<dyn Fn(&Config, &Config, RuntimeConfigChangeReason) + Send + Sync>;
type Validator = Box<dyn Fn(&Config) -> anyhow::Result<()> + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeConfigChangeReason {
    Updated,
    Reloaded,
    LocalInstall,
}

pub struct RuntimeConfigStore {
    config: RwLock<Arc<Config>>,
    generation: AtomicU64,
    postgres: Option<Arc<PostgresStore>>,
    mutation: Mutex<()>,
    saved_version: RwLock<Option<i64>>,
    validator: RwLock<Option<Validator>>,
    listeners: RwLock<Vec<Listener>>,
}

fn next_config_generation() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl RuntimeConfigStore {
    pub fn new(mut initial: Config, postgres: Option<Arc<PostgresStore>>) -> Arc<Self> {
        initial.compiled_routes = Some(Arc::new(initial.routes.compile_with_config(&initial)));
        Arc::new(Self {
            config: RwLock::new(Arc::new(initial)),
            generation: AtomicU64::new(next_config_generation()),
            postgres,
            mutation: Mutex::new(()),
            saved_version: RwLock::new(None),
            validator: RwLock::new(None),
            listeners: RwLock::new(Vec::new()),
        })
    }

    pub fn snapshot(&self) -> Arc<Config> {
        self.config.read().clone()
    }

    pub fn snapshot_with_generation(&self) -> (Arc<Config>, u64) {
        let config = self.config.read();
        (config.clone(), self.generation.load(Ordering::Acquire))
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn config(&self) -> Config {
        Config::clone(&self.config.read())
    }

    pub fn set_validator(&self, validator: Validator) {
        *self.validator.write() = Some(validator);
    }

    pub fn subscribe(&self, listener: Listener) {
        self.listeners.write().push(listener);
    }

    /// 当前更新已保存的 PG version，供同步订阅回调发布 Redis 通知。
    pub(crate) fn saved_version(&self) -> Option<i64> {
        *self.saved_version.read()
    }

    pub fn update(&self, mutate: impl FnOnce(&mut Config)) -> anyhow::Result<Option<i64>> {
        let _mutation = self.mutation.lock();
        let previous = self.snapshot();
        let mut updated = previous.as_ref().clone();
        mutate(&mut updated);
        updated.set_config_path_for_runtime(None);
        self.validate(&updated)?;
        let version = if let Some(store) = &self.postgres {
            let store = store.clone();
            let to_save = updated.clone();
            Some(block_on_runtime_pgsql(
                "持久化运行时配置到 PgSQL",
                async move { store.save_runtime_config_returning_version(&to_save).await },
            )?)
        } else {
            None
        };
        *self.saved_version.write() = version;
        self.install(previous, updated, RuntimeConfigChangeReason::Updated);
        Ok(version)
    }

    pub fn reload_from_postgres(&self) -> anyhow::Result<bool> {
        let Some(store) = self.postgres.clone() else {
            return Ok(false);
        };
        let _mutation = self.mutation.lock();
        let Some(mut config) = block_on_runtime_pgsql("从 PgSQL 重新加载运行时配置", async move {
            store.load_runtime_config().await
        })?
        else {
            return Ok(false);
        };
        config.set_config_path_for_runtime(None);
        self.validate(&config)?;
        let previous = self.snapshot();
        self.install(previous, config, RuntimeConfigChangeReason::Reloaded);
        Ok(true)
    }

    pub fn install_local(&self, mutate: impl FnOnce(&mut Config)) {
        let _mutation = self.mutation.lock();
        let previous = self.snapshot();
        let mut updated = previous.as_ref().clone();
        mutate(&mut updated);
        self.install(previous, updated, RuntimeConfigChangeReason::LocalInstall);
    }

    fn validate(&self, config: &Config) -> anyhow::Result<()> {
        config
            .routes
            .validate_with_config(config)
            .map_err(anyhow::Error::msg)?;
        if let Some(validate) = self.validator.read().as_ref() {
            validate(config)?;
        }
        Ok(())
    }

    fn install(
        &self,
        previous: Arc<Config>,
        mut config: Config,
        reason: RuntimeConfigChangeReason,
    ) {
        config.compiled_routes = Some(Arc::new(config.routes.compile_with_config(&config)));
        let current = Arc::new(config);
        {
            let mut config = self.config.write();
            *config = current.clone();
            self.generation
                .store(next_config_generation(), Ordering::Release);
        }
        // 回调可以读快照或注册订阅；按安装顺序执行，不持有快照/订阅锁。
        let listeners = self.listeners.read().clone();
        for listener in listeners {
            listener(&previous, &current, reason);
        }
    }
}

fn block_on_runtime_pgsql<T: Send>(
    operation: &'static str,
    future: impl Future<Output = anyhow::Result<T>> + Send,
) -> anyhow::Result<T> {
    fn runtime() -> &'static Runtime {
        static RUNTIME: OnceLock<Runtime> = OnceLock::new();
        RUNTIME.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("runtime-config-storage")
                .enable_all()
                .build()
                .expect("创建运行时配置存储 runtime 失败")
        })
    }
    let started_at = Instant::now();
    let future = async move {
        let timeout = Duration::from_secs(5);
        tokio::time::timeout(timeout, future)
            .await
            .map_err(|_| anyhow::anyhow!("{}超过 {}ms", operation, timeout.as_millis()))?
    };
    let result = if let Ok(handle) = Handle::try_current() {
        match handle.runtime_flavor() {
            RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(|| runtime().block_on(future))
            }
            _ => std::thread::scope(|scope| {
                scope
                    .spawn(move || runtime().block_on(future))
                    .join()
                    .map_err(|_| anyhow::anyhow!("同步存储线程异常退出"))?
            }),
        }
    } else {
        runtime().block_on(future)
    };
    let elapsed = started_at.elapsed();
    if elapsed >= Duration::from_millis(100) {
        tracing::warn!(
            operation,
            elapsed_ms = elapsed.as_millis() as u64,
            "同步存储操作耗时较长"
        );
    }
    result.map_err(|err| anyhow::anyhow!("{}失败: {}", operation, err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_are_immutable_and_validation_failure_does_not_notify() {
        let store = RuntimeConfigStore::new(Config::default(), None);
        let (original, generation) = store.snapshot_with_generation();
        let changes = Arc::new(Mutex::new(Vec::new()));
        let observed = changes.clone();
        let weak_store = Arc::downgrade(&store);
        store.subscribe(Arc::new(move |previous, current, reason| {
            let store = weak_store.upgrade().unwrap();
            assert_eq!(store.snapshot().port, current.port);
            observed.lock().push((previous.port, current.port, reason));
        }));
        store.set_validator(Box::new(|config| {
            anyhow::ensure!(config.port != 0, "port must be nonzero");
            Ok(())
        }));
        assert!(store.update(|config| config.port = 0).is_err());
        assert_eq!(store.generation(), generation);
        assert!(Arc::ptr_eq(&original, &store.snapshot()));
        assert!(changes.lock().is_empty());
        assert_eq!(store.update(|config| config.port = 19023).unwrap(), None);
        let (updated, next_generation) = store.snapshot_with_generation();
        assert_eq!(updated.port, 19023);
        assert_ne!(next_generation, generation);
        assert_ne!(original.port, updated.port);
        assert_eq!(
            *changes.lock(),
            vec![(original.port, 19023, RuntimeConfigChangeReason::Updated)]
        );
        let mut detached = store.config();
        detached.port = 19024;
        assert_eq!(store.snapshot().port, 19023);
        assert!(!store.reload_from_postgres().unwrap());
        assert_eq!(store.generation(), next_generation);
    }

    #[test]
    fn invalid_route_updates_are_rejected_without_an_optional_validator() {
        let store = RuntimeConfigStore::new(Config::default(), None);
        let original = store.snapshot();
        assert!(
            store
                .update(|config| config.routes.routes.retain(|route| route.prefix != "/cc"))
                .is_err()
        );
        assert!(
            store
                .update(|config| config
                    .routes
                    .routes
                    .push(crate::model::routes::RouteConfig::new(" cc/ ")))
                .is_err()
        );
        assert!(Arc::ptr_eq(&original, &store.snapshot()));
    }

    #[test]
    fn concurrent_updates_and_local_installs_preserve_every_mutation() {
        let mut config = Config::default();
        config.port = 1000;
        let store = RuntimeConfigStore::new(config, None);
        let generations = Arc::new(Mutex::new(Vec::new()));
        let seen = generations.clone();
        let weak_store = Arc::downgrade(&store);
        store.subscribe(Arc::new(move |previous, current, _reason| {
            assert_eq!(current.port, previous.port + 1);
            seen.lock().push(weak_store.upgrade().unwrap().generation());
        }));
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let store = &store;
                scope.spawn(move || {
                    for _ in 0..20 {
                        store.update(|config| config.port += 1).unwrap();
                        store.install_local(|config| config.port += 1);
                    }
                });
            }
        });
        assert_eq!(store.snapshot().port, 1160);
        let generations = generations.lock();
        assert_eq!(generations.len(), 160);
        assert!(generations.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
