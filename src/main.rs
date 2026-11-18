mod account;
mod admin;
mod anthropic;
mod common;
mod diagnostics;
mod http_client;
mod migration;
mod model;
mod storage;
pub mod token;
mod ui;

// 发布镜像静态链接 musl，它自带的 malloc 对所有线程共用一把锁；并发处理多 MB 请求体时
// CPU 大量耗在锁竞争、futex 和反复还页上（见 docs/analysis/prod-59137-cpu-musl-malloc-root-cause-20261104.md）。
// 测试构建保留 `http_client::allocation_probe` 的计数分配器。
#[cfg(not(test))]
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::{
    future::{Future, IntoFuture},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
    },
    time::{Duration as StdDuration, Instant},
};

use account::AccountManager;
use anyhow::Context as _;
use axum::serve::ListenerExt as _;
use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use chrono::Utc;
use clap::Parser;
use common::auth::RequestApiKeyStore;
use futures::StreamExt;
use model::arg::{Args, Command, MaintenanceCommand};
use model::config::Config;
use serde_json::{Value, json};
use storage::postgres::{PostgresStore, PostgresUsageLifecycleGuard, PostgresUsageStore};
use storage::redis_cache::RedisStore;
use storage::usage_demo_seed::seed_usage_demo;

const STARTUP_DEPENDENCY_MAX_WAIT: StdDuration = StdDuration::from_secs(60);
// `docker stop` sends SIGKILL 10 seconds after SIGTERM by default. Everything below has to fit
// in that window, or the usage records still queued in memory are lost: by default at most 5 s
// for HTTP connections to drain (queued requests answer at once, see `begin_shutdown`), then at
// most 4 s for background work, with the usage writers drained first. Streams still running
// when the drain ends are cut, so deployments with a longer grace period raise the drain with
// `CLAUDE_POOL_HTTP_DRAIN_SECS` (grace period minus about 10 s).
const SERVER_GRACEFUL_SHUTDOWN_TIMEOUT: StdDuration = StdDuration::from_secs(5);
const SERVER_GRACEFUL_SHUTDOWN_MAX: StdDuration = StdDuration::from_secs(600);

fn server_graceful_shutdown_timeout() -> StdDuration {
    std::env::var("CLAUDE_POOL_HTTP_DRAIN_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(StdDuration::from_secs)
        .map(|drain| drain.min(SERVER_GRACEFUL_SHUTDOWN_MAX))
        .unwrap_or(SERVER_GRACEFUL_SHUTDOWN_TIMEOUT)
}
const BACKGROUND_SHUTDOWN_TIMEOUT: StdDuration = StdDuration::from_secs(2);
const BACKGROUND_DRAIN_TIMEOUT: StdDuration = StdDuration::from_secs(3);
const BACKGROUND_SHUTDOWN_TOTAL_TIMEOUT: StdDuration = StdDuration::from_secs(4);
const READYZ_DEPENDENCY_TIMEOUT: StdDuration = StdDuration::from_millis(500);
/// A dependency check that misses its timeout is only reported as not ready once the dependency
/// has had no successful check for this long. Under full load a ping waits behind the requests
/// sharing its connection pool, and one slow answer is a busy dependency, not a down one.
const READYZ_DEPENDENCY_GRACE: StdDuration = StdDuration::from_secs(10);
const ABORTED_TASK_JOIN_TIMEOUT: StdDuration = StdDuration::from_secs(1);
const OBSERVABILITY_REDIS_CONNECT_TIMEOUT: StdDuration = StdDuration::from_secs(5);

/// tokio 工作线程与 blocking 线程栈大小。
///
/// 消息处理链的 async 状态机在 poll 时占用大量栈：实测 debug 构建单个请求约需 2.1 MiB，
/// 超过 tokio 默认的 2 MiB 直接栈溢出；release 构建约 0.5~1 MiB，余量也不足 2 倍。
/// 栈空间按需提交物理页，调大只增加虚拟地址预留，不增加常驻内存。
const RUNTIME_THREAD_STACK_SIZE: usize = 8 * 1024 * 1024;

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(RUNTIME_THREAD_STACK_SIZE)
        .build()
        .expect("failed to build tokio runtime");
    runtime.block_on(async_main());
}

async fn async_main() {
    // 解析命令行参数
    let args = Args::parse();

    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    if matches!(
        args.command,
        Some(Command::ExportLegacy { .. } | Command::ImportLegacy { apply: false, .. })
    ) {
        if let Err(error) = handle_cli_command(args.command.unwrap(), &Config::default()).await {
            tracing::error!("{:#}", error);
            std::process::exit(1);
        }
        return;
    }

    // 加载配置
    let config_path = args
        .config
        .unwrap_or_else(|| Config::default_config_path().to_string());
    let file_config = Config::load(&config_path).unwrap_or_else(|e| {
        tracing::error!("加载配置失败: {}", e);
        std::process::exit(1);
    });

    if let Some(command) = args.command {
        if let Err(err) = handle_cli_command(command, &file_config).await {
            tracing::error!("{}", err);
            std::process::exit(1);
        }
        return;
    }

    let postgres_store = Arc::new(
        retry_startup_dependency(
            "PgSQL",
            STARTUP_DEPENDENCY_MAX_WAIT,
            || PostgresStore::connect(&file_config),
            postgres_startup_error_is_retryable,
        )
        .await
        .unwrap_or_else(|e| {
            tracing::error!("连接或初始化 PgSQL 失败: {:#}", e);
            std::process::exit(1);
        }),
    );
    let usage_runtime_guard = retry_startup_dependency(
        "PgSQL usage runtime fence",
        STARTUP_DEPENDENCY_MAX_WAIT,
        || PostgresUsageLifecycleGuard::acquire_service(&file_config),
        |_| true,
    )
    .await
    .unwrap_or_else(|e| {
        tracing::error!("获取 PgSQL usage runtime fence 失败: {:#}", e);
        std::process::exit(1);
    });
    let redis_store = Arc::new(
        retry_startup_dependency(
            "Redis",
            STARTUP_DEPENDENCY_MAX_WAIT,
            || RedisStore::connect(&file_config),
            |_| true,
        )
        .await
        .unwrap_or_else(|e| {
            tracing::error!("连接 Redis 失败: {:#}", e);
            std::process::exit(1);
        }),
    );
    postgres_store
        .bootstrap_runtime_config_from_file(&file_config)
        .await
        .unwrap_or_else(|e| {
            tracing::error!("从配置文件 bootstrap 运行配置到 PgSQL 失败: {}", e);
            std::process::exit(1);
        });
    let mut config = postgres_store
        .load_runtime_config()
        .await
        .unwrap_or_else(|e| {
            tracing::error!("从 PgSQL 加载运行配置失败: {}", e);
            std::process::exit(1);
        })
        .unwrap_or_else(|| {
            tracing::error!("PgSQL runtime_config 为空，且从配置文件 bootstrap 失败");
            std::process::exit(1);
        });
    // Storage endpoints are deployment authority, not mutable runtime configuration. PostgreSQL
    // may contain an older serialized copy, so always overlay the file/environment values that
    // were used to connect this process before constructing dependent stores.
    config.postgres = file_config.postgres.clone();
    config.redis = file_config.redis.clone();
    config.observability_redis = file_config.observability_redis.clone();
    config.apply_storage_env_overrides();
    config
        .validate_redis_fault_domains()
        .unwrap_or_else(|error| {
            tracing::error!(error = %error, "Redis fault-domain configuration is invalid");
            std::process::exit(1);
        });
    config.set_config_path_for_runtime(None);
    apply_service_bind_env_overrides(&mut config);

    let observability_redis_store = if config
        .observability_redis
        .url
        .as_deref()
        .is_some_and(|url| !url.trim().is_empty())
    {
        match tokio::time::timeout(
            OBSERVABILITY_REDIS_CONNECT_TIMEOUT,
            RedisStore::connect_observability(&config.observability_redis),
        )
        .await
        {
            Ok(Ok(store)) => {
                let identity_check = tokio::time::timeout(
                    OBSERVABILITY_REDIS_CONNECT_TIMEOUT,
                    async {
                        let business_run_id = redis_store.server_run_id().await?;
                        let observability_run_id = store.server_run_id().await?;
                        if business_run_id == observability_run_id {
                            anyhow::bail!(
                                "observability Redis resolves to the same server process as business Redis"
                            );
                        }
                        Ok::<(), anyhow::Error>(())
                    },
                )
                .await;
                match identity_check {
                    Ok(Ok(())) => {
                        tracing::info!(
                            observability_redis_enabled = true,
                            "独立 observability Redis 已连接且 server identity 不同；usage/statistics/Admin cache 与业务 Redis 隔离"
                        );
                        Some(Arc::new(store))
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(
                            error = %error,
                            observability_redis_enabled = false,
                            "无法证明 observability Redis 与业务 Redis 是不同 server；降级为 PostgreSQL/进程内观测，不回落到业务 Redis"
                        );
                        None
                    }
                    Err(_) => {
                        tracing::warn!(
                            timeout_ms = OBSERVABILITY_REDIS_CONNECT_TIMEOUT.as_millis(),
                            observability_redis_enabled = false,
                            "观测 Redis server identity 检查超时；降级为 PostgreSQL/进程内观测，不回落到业务 Redis"
                        );
                        None
                    }
                }
            }
            Ok(Err(error)) => {
                tracing::warn!(
                    error = %error,
                    observability_redis_enabled = false,
                    "独立 observability Redis 连接失败；降级为 PostgreSQL/进程内观测，不回落到业务 Redis"
                );
                None
            }
            Err(_) => {
                tracing::warn!(
                    timeout_ms = OBSERVABILITY_REDIS_CONNECT_TIMEOUT.as_millis(),
                    observability_redis_enabled = false,
                    "独立 observability Redis 连接超时；降级为 PostgreSQL/进程内观测，不回落到业务 Redis"
                );
                None
            }
        }
    } else {
        tracing::info!(
            observability_redis_enabled = false,
            "未配置独立 observability Redis；usage/statistics/Admin cache 使用 PostgreSQL/进程内状态，业务 Redis 不承载观测流量"
        );
        None
    };

    let usage_postgres_store = Arc::new(
        retry_startup_dependency(
            "PgSQL usage pool",
            STARTUP_DEPENDENCY_MAX_WAIT,
            || PostgresStore::connect_usage(&config),
            postgres_startup_error_is_retryable,
        )
        .await
        .unwrap_or_else(|e| {
            tracing::error!("连接 usage PgSQL 独立池失败: {:#}", e);
            std::process::exit(1);
        }),
    );
    tracing::info!(
        usage_postgres_max_connections = config.postgres.usage_max_connections.max(1),
        "usage/dashboard PgSQL 已使用独立连接池，避免占用主业务 PgSQL pool"
    );

    // 获取客户端请求 API Key。历史配置使用 apiKey；新配置可额外使用 apiKeys。
    let request_api_keys = config.request_api_keys();
    if request_api_keys.is_empty() {
        tracing::error!("运行配置中未设置 apiKey/apiKeys");
        std::process::exit(1);
    }
    let request_api_key_store = Arc::new(RequestApiKeyStore::new(request_api_keys.clone()));
    let request_admission = Arc::new(
        anthropic::request_admission::RequestAdmissionController::new(config.request_admission)
            .with_usage_limit_store(Some(redis_store.clone())),
    );
    apply_request_api_key_runtime(&config, &request_api_key_store, &request_admission);
    if request_api_key_store.len() == 0 {
        tracing::error!("运行配置中没有启用的客户端请求 API Key");
        std::process::exit(1);
    }

    // 构建代理配置
    let proxy_config = config.proxy_url.as_ref().map(|url| {
        let mut proxy = http_client::ProxyConfig::new(url);
        if let (Some(username), Some(password)) = (&config.proxy_username, &config.proxy_password) {
            proxy = proxy.with_auth(username, password);
        }
        proxy
    });

    if proxy_config.is_some() {
        tracing::info!("已配置 HTTP 代理");
    }

    let usage_recorder = Arc::new(
        anthropic::usage::UsageRecorder::with_postgres_and_observability_redis(
            config.usage_record_limit,
            Arc::new(PostgresUsageStore::new(usage_postgres_store.clone())),
            observability_redis_store.clone(),
        ),
    );
    usage_recorder.set_key_limits(request_admission.usage_limits().clone());
    // Windows without history yet (new deploy, Redis flushed) start from the usage rollups.
    anthropic::request_key_limits::spawn_seed_from_history(
        request_admission.usage_limits().clone(),
        postgres_store.clone(),
        anthropic::request_key_limits::limited_periods(&config.request_api_key_policies()),
    );
    tracing::info!(
        usage_redis_cache_enabled = observability_redis_store.is_some(),
        usage_store_authority = "postgres",
        "usage 写入与查询以 PostgreSQL 为权威；可选 Redis materialization 仅使用独立 observability fault domain"
    );
    let prompt_cache = Arc::new(anthropic::prompt_cache::PromptCacheTracker::default());
    let prompt_cache_creation_controller = Arc::new(
        anthropic::prompt_cache_creation_control::PromptCacheCreationController::default(),
    );
    let pricing_catalog = Arc::new(anthropic::pricing::PricingCatalog::new());
    match postgres_store.load_pricing_status().await {
        Ok(Some(status)) => {
            pricing_catalog.load_persisted_status(status);
            tracing::info!("已从 PgSQL 加载模型价格");
        }
        Ok(None) => {}
        Err(err) => tracing::warn!("从 PgSQL 加载模型价格状态失败，使用内置价格继续: {}", err),
    }
    let model_capabilities =
        Arc::new(anthropic::model_capabilities::ModelCapabilitiesCatalog::new());
    match postgres_store.load_model_capabilities_status().await {
        Ok(Some(status)) => {
            if status.should_refresh_from_seed() {
                tracing::info!(
                    source = %status.source,
                    model_count = status.model_count,
                    "PgSQL 模型能力目录为旧内置目录，使用Anthropic seed 刷新"
                );
                let status = anthropic::model_capabilities::ModelCapabilitiesCatalog::seed_status();
                model_capabilities.load_persisted_status(status.clone());
                if let Err(err) = postgres_store.save_model_capabilities_status(&status).await {
                    tracing::warn!(
                        "刷新 上游 模型 seed 到 PgSQL 失败，继续使用内存 seed: {}",
                        err
                    );
                }
            } else {
                let mut status = status;
                let (added, removed) = status.merge_missing_seed_models();
                if !added.is_empty() || !removed.is_empty() {
                    tracing::info!(added = ?added, removed = ?removed, "按当前内置种子对齐模型目录");
                    if let Err(err) = postgres_store
                        .replace_seed_model_capabilities(&status, &removed)
                        .await
                    {
                        tracing::warn!("把对齐后的目录写回 PgSQL 失败，仅内存生效: {}", err);
                    }
                }
                model_capabilities.load_persisted_status(status);
                tracing::info!("已从 PgSQL 加载模型能力目录");
            }
        }
        Ok(None) => {
            let status = anthropic::model_capabilities::ModelCapabilitiesCatalog::seed_status();
            model_capabilities.load_persisted_status(status.clone());
            if let Err(err) = postgres_store.save_model_capabilities_status(&status).await {
                tracing::warn!(
                    "保存 上游 模型 seed 到 PgSQL 失败，继续使用内存 seed: {}",
                    err
                );
            } else {
                tracing::info!(
                    source = %status.source,
                    model_count = status.model_count,
                    "已使用Anthropic 模型 seed 初始化 PgSQL"
                );
            }
        }
        Err(err) => tracing::warn!(
            "从 PgSQL 加载模型能力状态失败，使用内置模型目录继续: {}",
            err
        ),
    }

    let runtime =
        model::runtime::RuntimeConfigStore::new(config.clone(), Some(postgres_store.clone()));

    common::runtime_lag::spawn_probe();
    let account_manager = Arc::new(AccountManager::new(
        postgres_store.clone(),
        redis_store.clone(),
    ));
    let weak_account_manager = Arc::downgrade(&account_manager);
    runtime.subscribe(Arc::new(move |previous, current, _reason| {
        if previous.accounts != current.accounts {
            if let Some(manager) = weak_account_manager.upgrade() {
                manager.invalidate_account_policy_state();
            }
        }
    }));
    runtime.subscribe(Arc::new(move |_previous, current, reason| {
        if reason == model::runtime::RuntimeConfigChangeReason::Updated {
            let config = current.clone();
            tokio::spawn(async move {
                admin::service::management::publish_runtime_change(&config).await;
            });
        }
    }));
    let admin_api_key_store =
        admin::AdminApiKeyStore::new(config.admin_api_key.clone().unwrap_or_default());
    let admin_key_updates = admin_api_key_store.clone();
    runtime.subscribe(Arc::new(move |_previous, current, _reason| {
        admin_key_updates.replace(current.admin_api_key.clone().unwrap_or_default());
    }));
    let runtime_event_health = Arc::new(RuntimeEventHealth::default());
    let runtime_event_listener = spawn_redis_runtime_event_listener(
        runtime.clone(),
        redis_store.clone(),
        account_manager.clone(),
        admin_api_key_store.clone(),
        request_api_key_store.clone(),
        request_admission.clone(),
        runtime_event_health.clone(),
    );
    let audit_retention_task = {
        let runtime = runtime.clone();
        storage::account_events::spawn_audit_retention_worker(postgres_store.clone(), move || {
            let config = runtime.config();
            config
                .audit_log_retention_enabled
                .then_some(config.audit_log_retention_days)
        })
    };
    // 用量明细保留走 usage 独立连接池，不和调度抢主业务池；默认关闭。
    let usage_retention_task = {
        let runtime = runtime.clone();
        storage::account_events::spawn_usage_retention_worker(
            usage_postgres_store.clone(),
            move || {
                let config = runtime.config();
                config
                    .usage_record_retention_enabled
                    .then_some(config.usage_record_retention_days)
            },
        )
    };
    let startup_pricing_sync_task = {
        let postgres_store = postgres_store.clone();
        let pricing_catalog = pricing_catalog.clone();
        let status = model_capabilities.status();
        tokio::spawn(async move {
            let capability_models = status.models.into_iter().map(|item| item.model);
            let pricing_status = pricing_catalog.sync_for_models(capability_models).await;
            if let Err(err) = postgres_store.save_pricing_status(&pricing_status).await {
                tracing::warn!("保存模型价格到 PgSQL 失败，不影响调度: {}", err);
            }
            if pricing_status.last_error.is_some() {
                tracing::warn!(
                    source = %pricing_status.source,
                    model_count = pricing_status.model_count,
                    "模型价格启动同步失败，使用当前价格目录继续运行"
                );
            } else {
                tracing::info!(
                    source = %pricing_status.source,
                    model_count = pricing_status.model_count,
                    "模型价格已按当前模型能力目录初始化"
                );
            }
        })
    };

    // 初始化 count_tokens 配置
    token::init_config(token::CountTokensConfig {
        api_url: None,
        api_key: None,
        auth_type: Default::default(),
        proxy: proxy_config,
        tls_backend: config.tls_backend,
    });

    // 初始化 LLM 明文采集（默认空闲，仅在 Admin API 显式启动后写盘）
    let capture_dir = std::env::var("CLAUDE_POOL_CAPTURE_DIR")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| diagnostics::capture::DEFAULT_CAPTURE_DIR.to_string());
    diagnostics::capture::init_global(capture_dir);

    // 构建 Anthropic API 路由
    let anthropic_app = anthropic::create_router(anthropic::AnthropicRouterDependencies {
        runtime: runtime.clone(),
        request_api_keys: request_api_key_store.clone(),
        request_admission: request_admission.clone(),

        usage_recorder: usage_recorder.clone(),
        prompt_cache: prompt_cache.clone(),
        prompt_cache_creation_controller: prompt_cache_creation_controller.clone(),
        pricing_catalog: pricing_catalog.clone(),
        model_capabilities: model_capabilities.clone(),
        account_manager: Some(account_manager.clone()),
    });

    // 构建 Admin API 路由（如果配置了非空的 admin_api_key）
    // 安全检查：空字符串被视为未配置，防止空 key 绕过认证

    let app = if let Some(admin_key) = &config.admin_api_key {
        if admin_key.trim().is_empty() {
            tracing::warn!("admin_api_key 配置为空，Admin API 未启用");
            anthropic_app
        } else {
            let admin_service = admin::AdminService::new(admin::AdminServiceDependencies {
                runtime: runtime.clone(),

                usage_recorder: usage_recorder.clone(),
                pricing_catalog: pricing_catalog.clone(),
                model_capabilities: model_capabilities.clone(),

                postgres_store: postgres_store.clone(),
                observability_redis_store: observability_redis_store.clone(),
                request_api_key_store: request_api_key_store.clone(),
                request_admission: request_admission.clone(),
                account_manager: account_manager.clone(),
            });
            let admin_state =
                admin::AdminState::with_key_store(admin_api_key_store.clone(), admin_service);
            // 只读号池视图免登录，默认关闭；它只借用 AdminService 的快照，不挂 Admin 中间件。
            let admin_app = admin::create_admin_router(admin_state);

            tracing::info!("Admin API 已启用");
            tracing::info!("管理后台 UI 已启用: /ui");
            anthropic_app
                .nest("/api/admin", admin_app)
                .merge(ui::create_ui_router())
        }
    } else {
        anthropic_app
    };
    let health_state = Arc::new(AppHealthState {
        postgres_store: postgres_store.clone(),
        redis_store: redis_store.clone(),
        runtime_events: runtime_event_health.clone(),
        account_manager: Some(account_manager.clone()),

        postgres_health: DependencyHealth::new(),
        redis_health: DependencyHealth::new(),
    });
    let app = app.merge(create_health_router(health_state));

    // 启动服务器
    let addr = format!("{}:{}", config.host, config.port);
    tracing::info!("启动 Anthropic API 端点: {}", addr);
    tracing::info!(
        count = request_api_key_store.len(),
        "已加载客户端请求 API Key"
    );
    tracing::info!("可用 API:");
    tracing::info!("  GET  /v1/models");
    tracing::info!("  POST /v1/messages");
    tracing::info!("  POST /v1/messages/count_tokens");
    tracing::info!("  GET  /na/v1/models");
    tracing::info!("  POST /na/v1/messages");
    tracing::info!("  POST /na/v1/messages/count_tokens");
    tracing::info!("  GET  /ha/v1/models");
    tracing::info!("  POST /ha/v1/messages");
    tracing::info!("  POST /ha/v1/messages/count_tokens");
    tracing::info!("  GET  /cc/v1/models");
    tracing::info!("  POST /cc/v1/messages");
    tracing::info!("  POST /cc/v1/messages/count_tokens");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap()
        .tap_io(enable_client_tcp_keepalive);
    let server_result = serve_until_shutdown(listener, app, || async {}).await;
    let shutdown_started_at = Instant::now();
    let shutdown_deadline = shutdown_started_at + BACKGROUND_SHUTDOWN_TOTAL_TIMEOUT;
    runtime_event_health.mark_disconnected();
    tokio::join!(
        abort_task_with_timeout("Redis runtime event listener", runtime_event_listener),
        abort_task_with_timeout("audit log retention", audit_retention_task),
        abort_task_with_timeout("usage record retention", usage_retention_task),
        abort_task_with_timeout("startup pricing sync", startup_pricing_sync_task),
    );

    // The stages below are independent; run them together so the usage drain gets the whole
    // budget instead of what is left after the others (the budget fits docker's 10 s window).
    let storage_stats = common::storage_task::best_effort_storage_task_stats();
    let drain_timeout = remaining_shutdown_budget(shutdown_deadline, BACKGROUND_DRAIN_TIMEOUT);
    let (usage_drain, storage_drain, external_release_drain) = tokio::join!(
        usage_recorder.drain(drain_timeout),
        common::storage_task::drain_best_effort_storage_tasks(drain_timeout),
        account_manager.drain_release_intents(drain_timeout),
    );
    tracing::info!(
        timed_out = usage_drain.timed_out,
        postgres_target = usage_drain.postgres.target,
        postgres_finished = usage_drain.postgres.finished,
        redis_target = usage_drain.redis.target,
        redis_finished = usage_drain.redis.finished,
        "Usage writer 排空阶段已结束"
    );
    tracing::info!(
        accepting = storage_stats.accepting,
        queued = storage_stats
            .queue_capacity
            .saturating_sub(storage_stats.queue_available),
        target = storage_drain.target,
        finished = storage_drain.finished,
        timed_out = storage_drain.timed_out,
        "后台存储任务排空阶段已结束"
    );
    tracing::info!(
        drained = external_release_drain.drained,
        pending = external_release_drain.pending,
        enqueued = external_release_drain.enqueued,
        completed = external_release_drain.completed,
        retries = external_release_drain.retries,
        worker_starts = external_release_drain.worker_starts,
        spawn_failures = external_release_drain.spawn_failures,
        "账号 Redis release intent 排空阶段已结束"
    );
    let usage_shutdown_timeout =
        remaining_shutdown_budget(shutdown_deadline, BACKGROUND_SHUTDOWN_TIMEOUT);
    let storage_shutdown_timeout =
        remaining_shutdown_budget(shutdown_deadline, BACKGROUND_SHUTDOWN_TIMEOUT);
    let (usage_report, storage_report) = tokio::join!(
        usage_recorder.shutdown(usage_shutdown_timeout),
        common::storage_task::shutdown_best_effort_storage_tasks(storage_shutdown_timeout),
    );
    tracing::info!(
        already_started = usage_report.already_started,
        drained = usage_report.drained,
        timed_out = usage_report.timed_out,
        postgres_abandoned = usage_report.postgres_abandoned,
        redis_abandoned = usage_report.redis_abandoned,
        postgres_accepted = usage_report.stats.writer_accepted,
        postgres_finished = usage_report.stats.writer_finished,
        redis_accepted = usage_report.stats.redis_writer_accepted,
        redis_finished = usage_report.stats.redis_writer_finished,
        "Usage writer 已停止"
    );
    tracing::info!(
        already_started = storage_report.already_started,
        drained = storage_report.drained,
        timed_out = storage_report.timed_out,
        abandoned = storage_report.abandoned,
        accepted = storage_report.stats.accepted,
        finished = storage_report.stats.finished,
        rejected_full = storage_report.stats.rejected_full,
        "后台存储任务执行器已停止"
    );
    tracing::info!(
        elapsed_ms = shutdown_started_at.elapsed().as_millis() as u64,
        total_budget_ms = BACKGROUND_SHUTDOWN_TOTAL_TIMEOUT.as_millis() as u64,
        deadline_exhausted = Instant::now() >= shutdown_deadline,
        "后台生命周期关闭完成"
    );

    if let Err(err) = usage_runtime_guard.release().await {
        panic!("释放 PgSQL usage runtime fence 失败: {err:#}");
    }

    if let Err(err) = server_result {
        panic!("HTTP 服务异常退出: {err}");
    }
}

fn remaining_shutdown_budget(deadline: Instant, stage_limit: StdDuration) -> StdDuration {
    deadline
        .saturating_duration_since(Instant::now())
        .min(stage_limit)
}

async fn abort_task_with_timeout<T>(task_name: &'static str, task: tokio::task::JoinHandle<T>) {
    task.abort();
    if tokio::time::timeout(ABORTED_TASK_JOIN_TIMEOUT, task)
        .await
        .is_err()
    {
        tracing::warn!(
            task = task_name,
            timeout_ms = ABORTED_TASK_JOIN_TIMEOUT.as_millis() as u64,
            "等待已中止后台任务退出超时，丢弃任务句柄"
        );
    }
}

/// `on_shutdown_signal` runs as soon as the stop signal arrives, before in-flight connections
/// are drained: the container may be killed before draining finishes.
/// Idle time before the first keepalive probe on a client connection, then the probe interval.
const CLIENT_TCP_KEEPALIVE_TIME: StdDuration = StdDuration::from_secs(60);
const CLIENT_TCP_KEEPALIVE_INTERVAL: StdDuration = StdDuration::from_secs(10);

/// A client that vanished without closing (network cut, host gone) is otherwise only noticed
/// when a write fails. While its socket buffer is full nothing writes, the response body is not
/// polled, and the request keeps its credential slot until the kernel gives up retransmitting
/// (about 15 minutes on Linux) or forever. Keepalive probes surface the dead peer in minutes.
fn enable_client_tcp_keepalive(stream: &mut tokio::net::TcpStream) {
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(CLIENT_TCP_KEEPALIVE_TIME)
        .with_interval(CLIENT_TCP_KEEPALIVE_INTERVAL);
    if let Err(err) = socket2::SockRef::from(&*stream).set_tcp_keepalive(&keepalive) {
        tracing::debug!("设置客户端连接 TCP keepalive 失败: {}", err);
    }
}

async fn serve_until_shutdown<L, F, Fut>(
    listener: L,
    app: Router,
    on_shutdown_signal: F,
) -> std::io::Result<()>
where
    L: axum::serve::Listener,
    L::Addr: std::fmt::Debug,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (shutdown_sender, shutdown_receiver) = tokio::sync::watch::channel(false);
    let signal_task = tokio::spawn(async move {
        shutdown_signal().await;
        let _ = shutdown_sender.send(true);
        on_shutdown_signal().await;
    });

    let mut graceful_receiver = shutdown_receiver.clone();
    let graceful_shutdown = async move {
        wait_for_shutdown_request(&mut graceful_receiver).await;
        tracing::info!("收到停止信号，停止接收新请求");
    };
    let drain_timeout = server_graceful_shutdown_timeout();
    let mut deadline_receiver = shutdown_receiver;
    let shutdown_deadline = async move {
        wait_for_shutdown_request(&mut deadline_receiver).await;
        tokio::time::sleep(drain_timeout).await;
    };

    let mut server = Box::pin(
        axum::serve(listener, app)
            .with_graceful_shutdown(graceful_shutdown)
            .into_future(),
    );
    let result = tokio::select! {
        result = &mut server => result,
        _ = shutdown_deadline => {
            tracing::warn!(
                timeout_secs = drain_timeout.as_secs(),
                "等待 HTTP 连接排空超时，停止剩余连接"
            );
            Ok(())
        }
    };
    drop(server);
    abort_task_with_timeout("shutdown signal listener", signal_task).await;
    result
}

async fn wait_for_shutdown_request(receiver: &mut tokio::sync::watch::Receiver<bool>) {
    loop {
        if *receiver.borrow() {
            return;
        }
        if receiver.changed().await.is_err() {
            return;
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::warn!("监听 Ctrl-C 失败: {}", err);
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => {
                tracing::warn!("监听 SIGTERM 失败: {}", err);
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(unix)]
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }

    #[cfg(not(unix))]
    ctrl_c.await;
}

async fn retry_startup_dependency<T, F, Fut, R>(
    name: &'static str,
    max_wait: StdDuration,
    mut operation: F,
    mut is_retryable: R,
) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
    R: FnMut(&anyhow::Error) -> bool,
{
    let started_at = Instant::now();
    let mut attempt = 1u32;
    loop {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(err) if !is_retryable(&err) => {
                return Err(err).with_context(|| format!("{} 初始化失败且不可重试", name));
            }
            Err(err) if started_at.elapsed() >= max_wait => {
                return Err(err)
                    .with_context(|| format!("{} 在 {} 秒内未就绪", name, max_wait.as_secs()));
            }
            Err(err) => {
                let delay = startup_retry_delay(attempt);
                tracing::warn!(
                    attempt,
                    retry_in_ms = delay.as_millis() as u64,
                    "{} 暂不可用，准备重试: {}",
                    name,
                    err
                );
                tokio::time::sleep(delay).await;
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

fn postgres_startup_error_is_retryable(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<sqlx::Error>()
            .is_some_and(sqlx_postgres_startup_error_is_retryable)
    })
}

fn sqlx_postgres_startup_error_is_retryable(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Io(_)
        | sqlx::Error::Protocol(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed => true,
        sqlx::Error::Database(database_error) => database_error.code().is_some_and(|code| {
            code.starts_with("08")
                || code.starts_with("53")
                || matches!(
                    code.as_ref(),
                    "40001" | "40P01" | "55P03" | "57014" | "57P01" | "57P02" | "57P03"
                )
        }),
        _ => false,
    }
}

fn startup_retry_delay(attempt: u32) -> StdDuration {
    let shift = attempt.saturating_sub(1).min(3);
    let millis = 500u64.saturating_mul(1u64 << shift).min(5_000);
    StdDuration::from_millis(millis)
}

fn apply_service_bind_env_overrides(config: &mut Config) {
    if let Ok(host) = std::env::var("CLAUDE_POOL_HOST") {
        let host = host.trim();
        if !host.is_empty() {
            config.host = host.to_string();
        }
    }

    if let Ok(port) = std::env::var("CLAUDE_POOL_PORT") {
        let port = port.trim();
        if port.is_empty() {
            return;
        }
        match port.parse::<u16>() {
            Ok(port) => config.port = port,
            Err(err) => tracing::warn!(
                value = port,
                error = %err,
                "忽略无效的 CLAUDE_POOL_PORT 环境变量"
            ),
        }
    }
}

#[derive(Default)]
struct RuntimeEventHealth {
    redis_events_connected: AtomicBool,
    last_event_at_ms: AtomicI64,
    last_subscribe_error_at_ms: AtomicI64,
}

impl RuntimeEventHealth {
    fn mark_connected(&self) {
        self.redis_events_connected.store(true, Ordering::Release);
    }

    fn mark_disconnected(&self) {
        self.redis_events_connected.store(false, Ordering::Release);
    }

    fn mark_event(&self) {
        self.last_event_at_ms
            .store(Utc::now().timestamp_millis(), Ordering::Release);
    }

    fn mark_subscribe_error(&self) {
        self.last_subscribe_error_at_ms
            .store(Utc::now().timestamp_millis(), Ordering::Release);
        self.mark_disconnected();
    }
}

struct AppHealthState {
    postgres_store: Arc<PostgresStore>,
    redis_store: Arc<RedisStore>,
    runtime_events: Arc<RuntimeEventHealth>,
    account_manager: Option<Arc<AccountManager>>,

    postgres_health: DependencyHealth,
    redis_health: DependencyHealth,
}

/// Last successful readiness check of one dependency; see [`READYZ_DEPENDENCY_GRACE`].
struct DependencyHealth {
    origin: Instant,
    /// Milliseconds since `origin` of the last successful check.
    last_ok_ms: AtomicU64,
}

impl DependencyHealth {
    /// Starts healthy: the process only gets here after connecting to the dependency.
    fn new() -> Self {
        Self {
            origin: Instant::now(),
            last_ok_ms: AtomicU64::new(0),
        }
    }

    /// Records one check and answers whether the dependency counts as ready.
    fn observe(&self, ok: bool, now: Instant) -> bool {
        let now_ms = now.saturating_duration_since(self.origin).as_millis() as u64;
        if ok {
            self.last_ok_ms.fetch_max(now_ms, Ordering::AcqRel);
            return true;
        }
        let last_ok_ms = self.last_ok_ms.load(Ordering::Acquire);
        now_ms.saturating_sub(last_ok_ms) < READYZ_DEPENDENCY_GRACE.as_millis() as u64
    }
}

/// Queued requests with no dispatch for this long mean the scheduler is not moving.

fn create_health_router(state: Arc<AppHealthState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(state)
}

/// Unauthenticated liveness probe; it deliberately does not name the service.
async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn readyz(State(state): State<Arc<AppHealthState>>) -> impl IntoResponse {
    // Bounded: a health check that hangs on a stuck pool or a half-open Redis connection looks
    // like the whole process is down to the orchestrator.
    let (postgres_ok, redis_ok) = tokio::join!(
        async {
            tokio::time::timeout(READYZ_DEPENDENCY_TIMEOUT, state.postgres_store.ping())
                .await
                .is_ok_and(|result| result.is_ok())
        },
        async {
            tokio::time::timeout(READYZ_DEPENDENCY_TIMEOUT, state.redis_store.ping())
                .await
                .is_ok_and(|result| result.is_ok())
        },
    );
    let now = Instant::now();
    let postgres_ok = state.postgres_health.observe(postgres_ok, now);
    let redis_ok = state.redis_health.observe(redis_ok, now);
    let redis_events_connected = state
        .runtime_events
        .redis_events_connected
        .load(Ordering::Acquire);
    let ready = postgres_ok && redis_ok && redis_events_connected;
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    let dispatch = match state.account_manager.as_ref() {
        Some(manager) => Some(manager.dispatch_status().await),
        None => None,
    };

    (
        status,
        Json(json!({
            "status": if ready { "ready" } else { "not_ready" },
            "checks": {
                "postgres": postgres_ok,
                "redis": redis_ok,
                "redisRuntimeEvents": redis_events_connected,
            },
            "dispatch": readyz_dispatch_value(dispatch),
            "runtimeLagMs": common::runtime_lag::global().recent_max().as_millis() as u64,
            "lastRedisRuntimeEventAtMs": state.runtime_events.last_event_at_ms.load(Ordering::Acquire),
            "lastRedisSubscribeErrorAtMs": state.runtime_events.last_subscribe_error_at_ms.load(Ordering::Acquire)
        })),
    )
}

fn readyz_dispatch_value(dispatch: Option<account::DispatchStatus>) -> Value {
    serde_json::to_value(dispatch).expect("dispatch status contains JSON values")
}

fn refresh_admin_api_key_from_runtime_config(
    config: &Config,
    admin_api_key_store: &admin::AdminApiKeyStore,
) {
    admin_api_key_store.replace(config.admin_api_key.clone().unwrap_or_default());
}

fn apply_request_api_key_runtime(
    config: &Config,
    request_api_key_store: &RequestApiKeyStore,
    request_admission: &anthropic::request_admission::RequestAdmissionController,
) {
    let policies = config.request_api_key_policies();
    let disabled = policies
        .iter()
        .filter(|policy| !policy.enabled)
        .map(|policy| policy.api_key.as_str())
        .collect::<Vec<_>>();
    request_api_key_store.replace_keys_with_disabled(config.request_api_keys(), disabled);

    let overrides = policies
        .iter()
        .filter_map(|policy| {
            let admission = policy.request_admission?;
            let identity = request_api_key_store.authenticate(&policy.api_key)?;
            Some((identity, admission))
        })
        .collect::<Vec<_>>();
    request_admission.update_policies(config.request_admission, overrides);
    request_admission.usage_limits().update_limits(
        policies
            .iter()
            .filter(|policy| policy.enabled)
            .map(|policy| {
                (
                    common::auth::request_api_key_id(&policy.api_key),
                    policy.usage_limits.clone(),
                )
            }),
    );
}

fn spawn_redis_runtime_event_listener(
    runtime: Arc<model::runtime::RuntimeConfigStore>,
    redis_store: Arc<RedisStore>,

    account_manager: Arc<AccountManager>,
    admin_api_key_store: admin::AdminApiKeyStore,
    request_api_key_store: Arc<RequestApiKeyStore>,
    request_admission: Arc<anthropic::request_admission::RequestAdmissionController>,
    health: Arc<RuntimeEventHealth>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let config_channel = redis_store.runtime_config_changed_channel();
            let account_data_channel = redis_store.account_data_changed_channel();
            let mut pubsub = match redis_store.subscribe_runtime_events().await {
                Ok(pubsub) => pubsub,
                Err(err) => {
                    health.mark_subscribe_error();
                    tracing::warn!("订阅 Redis 运行时事件失败，5 秒后重试: {}", err);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            };
            health.mark_connected();
            tracing::info!("已订阅 Redis 运行时事件");
            let mut stream = pubsub.on_message();
            let mut periodic_reload = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                tokio::select! {
                    message = stream.next() => {
                        let Some(message) = message else {
                            break;
                        };
                        let channel = message.get_channel_name().to_string();
                        let payload = message
                            .get_payload::<String>()
                            .unwrap_or_else(|_| String::new());
                        health.mark_event();
                        if channel == config_channel {
                            match runtime.reload_from_postgres() {
                                Ok(true) => {
                                    let config = runtime.config();
                                    apply_request_api_key_runtime(
                                        &config,
                                        &request_api_key_store,
                                        &request_admission,
                                    );
                                    refresh_admin_api_key_from_runtime_config(
                                        &config,
                                        &admin_api_key_store,
                                    );
                                    tracing::info!(payload, "已根据 Redis 通知热加载运行配置");
                                }
                                Ok(false) => tracing::debug!(payload, "收到运行配置通知，但未执行热加载"),
                                Err(err) => tracing::warn!(payload, "热加载运行配置失败: {}", err),
                            }
                        } else if channel == account_data_channel {
                            if account_manager.observe_account_data_event(&payload) {
                                tracing::debug!(payload, "已失效跨实例账号数据快照");
                            } else {
                                tracing::debug!(payload, "忽略已观察或无效的账号数据通知");
                            }
                        }
                    }
                    _ = periodic_reload.tick() => {
                        match runtime.reload_from_postgres() {
                            Ok(true) => {
                                let config = runtime.config();
                                apply_request_api_key_runtime(
                                    &config,
                                    &request_api_key_store,
                                    &request_admission,
                                );
                                refresh_admin_api_key_from_runtime_config(
                                    &config,
                                    &admin_api_key_store,
                                );
                            }
                            Ok(false) => {}
                            Err(err) => tracing::warn!("定时热加载运行配置失败: {}", err),
                        }
                    }
                }
            }
            health.mark_disconnected();
            tracing::warn!("Redis 运行时事件订阅已断开，准备重新订阅");
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    })
}

async fn handle_cli_command(command: Command, config: &Config) -> anyhow::Result<()> {
    match command {
        Command::ExportLegacy {
            from_postgres,
            out,
            redact_secrets,
        } => migration::export_legacy(&from_postgres, &out, redact_secrets).await,
        Command::ImportLegacy { file, apply } => {
            migration::import_legacy(&file, apply, config).await
        }
        Command::Maintenance { command } => handle_maintenance_command(command, config).await,
    }
}

async fn handle_maintenance_command(
    command: MaintenanceCommand,
    config: &Config,
) -> anyhow::Result<()> {
    let mut maintenance_config = config.clone();
    maintenance_config.postgres.compress_usage_rollups_on_start = false;
    if matches!(command, MaintenanceCommand::Migrate) {
        maintenance_config.postgres.migrate_on_start = true;
    }
    let maintenance_guard =
        PostgresUsageLifecycleGuard::acquire_offline_maintenance(&maintenance_config).await?;
    let store = PostgresStore::connect(&maintenance_config).await?;
    match command {
        MaintenanceCommand::Migrate => {
            println!("postgres startup migration completed");
        }
        MaintenanceCommand::UsageIndexes => {
            store.migrate_with_options(false).await?;
            store.create_usage_indexes_concurrently().await?;
            println!("usage indexes maintenance completed");
        }
        MaintenanceCommand::UsageLegacyCostBackfill => {
            store.backfill_usage_legacy_cost_fields().await?;
            println!("usage legacy cost field backfill completed");
        }
        MaintenanceCommand::UsageRollupCompression => {
            store.compress_usage_rollups_to_hour_buckets().await?;
            println!("usage rollup compression completed");
        }
        MaintenanceCommand::UsageOverviewTotalsBackfill => {
            store.migrate_with_options(false).await?;
            let scanned = PostgresUsageStore::new(std::sync::Arc::new(store))
                .backfill_usage_overview_totals()
                .await?;
            println!("usage overview totals backfill completed: records={scanned}");
        }
        MaintenanceCommand::UsageRouteDimensionBackfill => {
            store.migrate_with_options(false).await?;
            let scanned = PostgresUsageStore::new(std::sync::Arc::new(store))
                .rebuild_usage_route_dimensions()
                .await?;
            println!("usage route dimension backfill completed: records={scanned}");
        }
        MaintenanceCommand::UsageDemoSeed { days, seed, reset } => {
            let report = seed_usage_demo(&store, days, seed, reset).await?;
            println!(
                "usage demo seed completed: database={} days={} records={} external={} accounts={} pools={}",
                report.database,
                report.days,
                report.records,
                report.external_records,
                report.accounts,
                report.pools
            );
        }
    }
    maintenance_guard.release().await?;
    Ok(())
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn readyz_without_manager_has_null_dispatch() {
        assert_eq!(readyz_dispatch_value(None), Value::Null);
    }

    #[test]
    fn readyz_idle_manager_dispatch_is_progressing() {
        let counters = account::DispatchCounters::default();
        let value = readyz_dispatch_value(Some(counters.snapshot(Some(0), 0)));
        assert_eq!(value["queuedRequests"], 0);
        assert_eq!(value["progressing"], true);
        assert_eq!(value["dispatchedTotal"], 0);
        assert_eq!(value["globalInFlight"], 0);
        assert_eq!(value["coordinatorAdmissionSaturated"], 0);
        assert_eq!(value["dispatchRatePerSec"], 0.0);
        assert_eq!(value["secsSinceLastDispatch"], Value::Null);
        assert_eq!(
            readyz_dispatch_value(Some(counters.snapshot(None, 0)))["queuedRequests"],
            Value::Null
        );
    }

    /// A slow check right after a good one keeps the dependency ready; only failing for the
    /// whole grace period makes it not ready, and one success brings it back.
    #[test]
    fn readiness_tolerates_a_slow_dependency_check_within_the_grace_period() {
        let health = DependencyHealth::new();
        let start = health.origin;
        assert!(health.observe(true, start + StdDuration::from_secs(1)));
        assert!(health.observe(false, start + StdDuration::from_secs(2)));
        assert!(health.observe(false, start + StdDuration::from_millis(10_900)));
        assert!(!health.observe(false, start + StdDuration::from_secs(11)));
        assert!(health.observe(true, start + StdDuration::from_secs(12)));
        assert!(health.observe(false, start + StdDuration::from_secs(13)));
    }

    #[test]
    fn remaining_shutdown_budget_caps_each_stage_and_expires() {
        let future_deadline = Instant::now() + StdDuration::from_secs(1);
        assert_eq!(
            remaining_shutdown_budget(future_deadline, StdDuration::from_millis(10)),
            StdDuration::from_millis(10)
        );

        let expired_deadline = Instant::now()
            .checked_sub(StdDuration::from_millis(1))
            .expect("Instant supports a one millisecond subtraction");
        assert_eq!(
            remaining_shutdown_budget(expired_deadline, StdDuration::from_secs(1)),
            StdDuration::ZERO
        );
    }

    #[test]
    fn postgres_startup_retry_classifier_retries_io_but_not_migration_invariants() {
        let io_error = anyhow::Error::new(sqlx::Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "fixture connection refused",
        )));
        assert!(postgres_startup_error_is_retryable(&io_error));

        let checksum_error =
            anyhow::anyhow!("schema migration credential-storage-revision-v1 checksum mismatch");
        assert!(!postgres_startup_error_is_retryable(&checksum_error));
    }

    #[tokio::test]
    async fn startup_dependency_retry_stops_immediately_for_non_retryable_errors() {
        let attempts = AtomicUsize::new(0);
        let error = retry_startup_dependency(
            "fixture",
            StdDuration::from_secs(60),
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                async { Err::<(), anyhow::Error>(anyhow::anyhow!("fixture deterministic failure")) }
            },
            |_| false,
        )
        .await
        .unwrap_err();

        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(error.to_string().contains("初始化失败且不可重试"));
        assert!(format!("{error:#}").contains("fixture deterministic failure"));
    }

    #[tokio::test]
    async fn startup_dependency_retry_recovers_from_transient_errors() {
        let attempts = AtomicUsize::new(0);
        let value = retry_startup_dependency(
            "fixture",
            StdDuration::from_secs(5),
            || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    if attempt < 3 {
                        anyhow::bail!("fixture transient failure")
                    }
                    Ok(42)
                }
            },
            |_| true,
        )
        .await
        .unwrap();

        assert_eq!(value, 42);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }
}
