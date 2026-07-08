mod admin;
mod admin_ui;
mod anthropic;
mod common;
mod external_pool;
mod http_client;
mod kiro;
mod model;
mod storage;
pub mod token;

use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
    time::{Duration as StdDuration, Instant},
};

use anthropic::prompt_cache::PromptCacheBounds;
use anyhow::Context as _;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Redirect},
    routing::get,
};
use chrono::Utc;
use clap::Parser;
use common::auth::RequestApiKeyStore;
use external_pool::ExternalPoolManager;
use futures::StreamExt;
use kiro::endpoint::{CliEndpoint, IdeEndpoint, KiroEndpoint};
use kiro::model::credentials::{CredentialsConfig, KiroCredentials};
use kiro::provider::KiroProvider;
use kiro::token_manager::MultiTokenManager;
use model::arg::{Args, Command, CredentialsCommand};
use model::config::Config;
use serde_json::{Value, json};
use storage::postgres::{PostgresStore, PostgresUsageStore};
use storage::redis_cache::RedisStore;

const STARTUP_DEPENDENCY_MAX_WAIT: StdDuration = StdDuration::from_secs(60);

#[tokio::main]
async fn main() {
    // 解析命令行参数
    let args = Args::parse();

    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // 加载配置
    let config_path = args
        .config
        .unwrap_or_else(|| Config::default_config_path().to_string());
    let file_config = Config::load(&config_path).unwrap_or_else(|e| {
        tracing::error!("加载配置失败: {}", e);
        std::process::exit(1);
    });

    if let Some(command) = args.command {
        // CLI 凭据诊断仍然面向本地文件，用于首次导入前排查 credentials.json。
        let credentials_path = args
            .credentials
            .unwrap_or_else(|| KiroCredentials::default_credentials_path().to_string());
        let credentials_config = CredentialsConfig::load(&credentials_path).unwrap_or_else(|e| {
            tracing::error!("加载凭证失败: {}", e);
            std::process::exit(1);
        });
        if let Err(err) =
            handle_cli_command(command, &file_config, credentials_config, &credentials_path)
        {
            tracing::error!("{}", err);
            std::process::exit(1);
        }
        return;
    }

    let postgres_store = Arc::new(
        retry_startup_dependency("PgSQL", STARTUP_DEPENDENCY_MAX_WAIT, || {
            PostgresStore::connect(&file_config)
        })
        .await
        .unwrap_or_else(|e| {
            tracing::error!("连接或初始化 PgSQL 失败: {}", e);
            std::process::exit(1);
        }),
    );
    let redis_store = Arc::new(
        retry_startup_dependency("Redis", STARTUP_DEPENDENCY_MAX_WAIT, || {
            RedisStore::connect(&file_config)
        })
        .await
        .unwrap_or_else(|e| {
            tracing::error!("连接 Redis 失败: {}", e);
            std::process::exit(1);
        }),
    );
    let env_kiro_api_key = match std::env::var("KIRO_API_KEY") {
        Ok(value) if value.trim().is_empty() => {
            tracing::warn!("KIRO_API_KEY 环境变量已设置但为空，视为未配置");
            None
        }
        Ok(value) => Some(value.trim().to_string()),
        Err(_) => None,
    };

    postgres_store
        .bootstrap_runtime_config_from_file(&file_config)
        .await
        .unwrap_or_else(|e| {
            tracing::error!("从配置文件 bootstrap 运行配置到 PgSQL 失败: {}", e);
            std::process::exit(1);
        });
    let credentials_exist = postgres_store
        .credentials_exist()
        .await
        .unwrap_or_else(|e| {
            tracing::error!("检查 PgSQL 凭据是否存在失败: {}", e);
            std::process::exit(1);
        });
    if !credentials_exist {
        let credentials_path = args
            .credentials
            .unwrap_or_else(|| KiroCredentials::default_credentials_path().to_string());
        match CredentialsConfig::load(&credentials_path) {
            Ok(file_credentials) => {
                let file_credentials_list = file_credentials.into_sorted_credentials();
                postgres_store
                    .bootstrap_credentials_from_file(&file_credentials_list)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::error!("从凭据文件 bootstrap 凭据到 PgSQL 失败: {}", e);
                        std::process::exit(1);
                    });
            }
            Err(err) if env_kiro_api_key.is_some() => {
                tracing::warn!(
                    "首次导入凭据文件不可用，将仅使用 KIRO_API_KEY 自动导入: {}",
                    err
                );
            }
            Err(err) => {
                tracing::error!("加载首次导入凭据文件失败: {}", err);
                std::process::exit(1);
            }
        }
    }

    if let Some(kiro_api_key) = &env_kiro_api_key {
        postgres_store
            .ensure_api_key_credential(kiro_api_key)
            .await
            .map(|credential| {
                tracing::info!(
                    credential_id = credential.id.unwrap_or_default(),
                    "KIRO_API_KEY 已作为 API Key 凭据存在或完成一次性导入"
                );
            })
            .unwrap_or_else(|e| {
                tracing::error!("导入 KIRO_API_KEY 到 PgSQL 失败: {}", e);
                std::process::exit(1);
            });
    }

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
    config.set_config_path_for_runtime(None);
    apply_service_bind_env_overrides(&mut config);

    let credentials_list = postgres_store.load_credentials().await.unwrap_or_else(|e| {
        tracing::error!("从 PgSQL 加载凭据失败: {}", e);
        std::process::exit(1);
    });

    tracing::info!("已加载 {} 个凭据配置", credentials_list.len());

    // 获取第一个凭据用于日志显示
    let first_credentials = credentials_list.first().cloned().unwrap_or_default();
    tracing::debug!("主凭证: {:?}", first_credentials);

    // 获取客户端请求 API Key。历史配置使用 apiKey；新配置可额外使用 apiKeys。
    let request_api_keys = config.request_api_keys();
    if request_api_keys.is_empty() {
        tracing::error!("运行配置中未设置 apiKey/apiKeys");
        std::process::exit(1);
    }
    let request_api_key_store = Arc::new(RequestApiKeyStore::new(request_api_keys.clone()));

    // 构建代理配置
    let proxy_config = config.proxy_url.as_ref().map(|url| {
        let mut proxy = http_client::ProxyConfig::new(url);
        if let (Some(username), Some(password)) = (&config.proxy_username, &config.proxy_password) {
            proxy = proxy.with_auth(username, password);
        }
        proxy
    });

    if proxy_config.is_some() {
        tracing::info!("已配置 HTTP 代理: {}", config.proxy_url.as_ref().unwrap());
    }

    // 构建端点注册表
    let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
    {
        let ide = IdeEndpoint::new();
        endpoints.insert(ide.name().to_string(), Arc::new(ide));
        let cli = CliEndpoint::new();
        endpoints.insert(cli.name().to_string(), Arc::new(cli));
    }

    // 校验默认端点存在
    if !endpoints.contains_key(&config.default_endpoint) {
        tracing::error!("默认端点 \"{}\" 未注册", config.default_endpoint);
        std::process::exit(1);
    }

    // 校验所有凭据声明的端点都已注册
    for cred in &credentials_list {
        let name = cred.endpoint.as_deref().unwrap_or(&config.default_endpoint);
        if !endpoints.contains_key(name) {
            tracing::error!(
                "凭据 id={:?} 指定了未知端点 \"{}\"（已注册: {:?}）",
                cred.id,
                name,
                endpoints.keys().collect::<Vec<_>>()
            );
            std::process::exit(1);
        }
    }

    let endpoint_names: Vec<String> = endpoints.keys().cloned().collect();
    let usage_recorder = Arc::new(anthropic::usage::UsageRecorder::with_postgres_and_redis(
        config.usage_record_limit,
        Arc::new(PostgresUsageStore::new(postgres_store.clone())),
        Some(redis_store.clone()),
    ));
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
                    "PgSQL 模型能力目录为旧内置目录，使用本地 Kiro seed 刷新"
                );
                let status = anthropic::model_capabilities::ModelCapabilitiesCatalog::seed_status();
                model_capabilities.load_persisted_status(status.clone());
                if let Err(err) = postgres_store.save_model_capabilities_status(&status).await {
                    tracing::warn!(
                        "刷新 Kiro 模型 seed 到 PgSQL 失败，继续使用内存 seed: {}",
                        err
                    );
                }
            } else {
                model_capabilities.load_persisted_status(status);
                tracing::info!("已从 PgSQL 加载模型能力目录");
            }
        }
        Ok(None) => {
            let status = anthropic::model_capabilities::ModelCapabilitiesCatalog::seed_status();
            model_capabilities.load_persisted_status(status.clone());
            if let Err(err) = postgres_store.save_model_capabilities_status(&status).await {
                tracing::warn!(
                    "保存 Kiro 模型 seed 到 PgSQL 失败，继续使用内存 seed: {}",
                    err
                );
            } else {
                tracing::info!(
                    source = %status.source,
                    model_count = status.model_count,
                    "已使用本地 Kiro 模型 seed 初始化 PgSQL"
                );
            }
        }
        Err(err) => tracing::warn!(
            "从 PgSQL 加载模型能力状态失败，使用内置模型目录继续: {}",
            err
        ),
    }

    // 创建 MultiTokenManager 和 KiroProvider
    let token_manager = MultiTokenManager::new_with_stores(
        config.clone(),
        credentials_list,
        proxy_config.clone(),
        None,
        true,
        Some(postgres_store.clone()),
        Some(redis_store.clone()),
    )
    .unwrap_or_else(|e| {
        tracing::error!("创建 Token 管理器失败: {}", e);
        std::process::exit(1);
    });
    let token_manager = Arc::new(token_manager);
    token_manager.spawn_stats_flush_worker();
    let runtime_event_health = Arc::new(RuntimeEventHealth::default());
    spawn_redis_runtime_event_listener(
        redis_store.clone(),
        token_manager.clone(),
        request_api_key_store.clone(),
        runtime_event_health.clone(),
    );
    let kiro_provider = KiroProvider::with_proxy(
        token_manager.clone(),
        proxy_config.clone(),
        endpoints,
        config.default_endpoint.clone(),
    );
    let kiro_provider = Arc::new(kiro_provider);
    let external_pool_manager = Arc::new(ExternalPoolManager::new(
        postgres_store.clone(),
        redis_store.clone(),
    ));
    {
        let model_capabilities = model_capabilities.clone();
        let postgres_store = postgres_store.clone();
        let kiro_provider = kiro_provider.clone();
        let pricing_catalog = pricing_catalog.clone();
        tokio::spawn(async move {
            let status = match kiro_provider.list_available_models().await {
                Ok(models) => model_capabilities.sync_from_kiro_models(models),
                Err(err) => {
                    tracing::warn!("模型能力启动同步失败，使用当前模型目录继续运行: {}", err);
                    model_capabilities.record_sync_error(err.to_string())
                }
            };
            if let Err(err) = postgres_store.save_model_capabilities_status(&status).await {
                tracing::warn!("保存模型能力到 PgSQL 失败，不影响调度: {}", err);
            }
            if status.last_error.is_some() {
                tracing::warn!(
                    source = %status.source,
                    model_count = status.model_count,
                    "模型能力启动同步失败，使用当前模型目录继续运行"
                );
            } else {
                tracing::info!(
                    source = %status.source,
                    model_count = status.model_count,
                    "模型能力已初始化"
                );
            }

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
        });
    }

    // 初始化 count_tokens 配置
    token::init_config(token::CountTokensConfig {
        api_url: config.count_tokens_api_url.clone(),
        api_key: config.count_tokens_api_key.clone(),
        auth_type: config.count_tokens_auth_type.clone(),
        proxy: proxy_config,
        tls_backend: config.tls_backend,
    });

    // 构建 Anthropic API 路由（profile_arn 由 provider 层根据实际凭据动态注入）
    let anthropic_app = anthropic::create_router_with_provider(
        request_api_key_store.clone(),
        Some(kiro_provider.clone()),
        config.extract_thinking,
        usage_recorder.clone(),
        prompt_cache.clone(),
        prompt_cache_creation_controller.clone(),
        pricing_catalog.clone(),
        model_capabilities.clone(),
        config.prompt_cache_target_read_ratio,
        config.prompt_cache_token_scale,
        config.prompt_cache_max_simulated_input_tokens,
        config.prompt_cache_cap_jitter_min_tokens,
        config.prompt_cache_cap_jitter_max_tokens,
        config.prompt_cache_scale_min_input_tokens,
        config.prompt_cache_creation_control.normalized(),
        PromptCacheBounds::from_config(
            config.prompt_cache_max_entries_per_account,
            config.prompt_cache_max_entries_global,
            config.prompt_cache_entry_ttl_secs,
            config.prompt_cache_estimated_bytes_limit,
        ),
        config.reported_usage.clone(),
        config.cache_policy.clone(),
        config.defined_cache_routes.clone(),
        config.compat_profile,
        config.thinking_trigger_mode,
        config.model_resolution_mode,
        config.model_mapping.clone().normalized(),
        config.expose_proxy_warnings,
        config.payload_guard_enabled,
        config.payload_guard_mode,
        config.payload_guard_max_bytes,
        config.payload_guard_safety_margin_bytes,
        config.payload_guard_trim_history,
        config.payload_guard_external_enabled,
        config.kiro_cache_point_enabled,
        config.kiro_cache_point_tools_only,
        config.kiro_cache_point_record_plan,
        config.kiro_upstream_stream_idle_timeout_secs,
        config.image_processing.normalized(),
        config.body_conversion,
        config.missing_max_tokens.normalized(),
        config.payload_shaping,
        config.external_pools.clone(),
        config.tool_format_debug.clone(),
        Some(external_pool_manager.clone()),
    );

    // 构建 Admin API 路由（如果配置了非空的 admin_api_key）
    // 安全检查：空字符串被视为未配置，防止空 key 绕过认证
    let admin_key_valid = config
        .admin_api_key
        .as_ref()
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false);

    let app = if let Some(admin_key) = &config.admin_api_key {
        if admin_key.trim().is_empty() {
            tracing::warn!("admin_api_key 配置为空，Admin API 未启用");
            anthropic_app
        } else {
            let admin_service = admin::AdminService::new(
                token_manager.clone(),
                endpoint_names.clone(),
                usage_recorder.clone(),
                prompt_cache.clone(),
                prompt_cache_creation_controller.clone(),
                pricing_catalog.clone(),
                model_capabilities.clone(),
                kiro_provider.clone(),
                postgres_store.clone(),
                redis_store.clone(),
                request_api_key_store.clone(),
                external_pool_manager.clone(),
            );
            let admin_state = admin::AdminState::new(admin_key, admin_service);
            let admin_app = admin::create_admin_router(admin_state);

            // 创建管理后台 UI 路由
            let new_ui_app = admin_ui::create_new_ui_router();

            tracing::info!("Admin API 已启用");
            tracing::info!("Admin UI 已启用: /ui");
            anthropic_app
                .nest("/api/admin", admin_app)
                .route("/ui/", get(new_ui_index_redirect))
                .nest("/ui", new_ui_app)
        }
    } else {
        anthropic_app
    };
    let health_state = Arc::new(AppHealthState {
        postgres_store: postgres_store.clone(),
        redis_store: redis_store.clone(),
        runtime_events: runtime_event_health,
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
    tracing::info!("  POST /v1/messages (high-cache)");
    tracing::info!("  POST /v1/messages/count_tokens");
    tracing::info!("  GET  /na/v1/models");
    tracing::info!("  POST /na/v1/messages (no-cache)");
    tracing::info!("  POST /na/v1/messages/count_tokens");
    tracing::info!("  GET  /ha/v1/models");
    tracing::info!("  POST /ha/v1/messages (high-cache input-compatible)");
    tracing::info!("  POST /ha/v1/messages/count_tokens");
    tracing::info!("  GET  /cc/v1/models");
    tracing::info!("  POST /cc/v1/messages");
    tracing::info!("  POST /cc/v1/messages/count_tokens");
    if admin_key_valid {
        tracing::info!("Admin API:");
        tracing::info!("  GET  /api/admin/credentials");
        tracing::info!("  GET  /api/admin/credentials/list");
        tracing::info!("  GET  /api/admin/credentials/summary");
        tracing::info!("  GET  /api/admin/credentials/runtime");
        tracing::info!("  GET  /api/admin/credentials/account-info");
        tracing::info!("  GET  /api/admin/credentials/usage-summary");
        tracing::info!("  GET  /api/admin/credentials-paged");
        tracing::info!("  GET  /api/admin/credentials/export");
        tracing::info!("  GET  /api/admin/usage-records");
        tracing::info!("  GET  /api/admin/usage-records-paged");
        tracing::info!("  GET  /api/admin/usage-summary");
        tracing::info!("  GET  /api/admin/usage-dashboard");
        tracing::info!("  GET  /api/admin/model-pricing");
        tracing::info!("  POST /api/admin/model-pricing/sync");
        tracing::info!("  POST /api/admin/credentials/:index/disabled");
        tracing::info!("  POST /api/admin/credentials/:index/priority");
        tracing::info!("  POST /api/admin/credentials/:index/reset");
        tracing::info!("  GET  /api/admin/credentials/:index/balance");
        tracing::info!("Admin UI:");
        tracing::info!("  GET  /ui");
    }

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn retry_startup_dependency<T, F, Fut>(
    name: &'static str,
    max_wait: StdDuration,
    mut operation: F,
) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    let started_at = Instant::now();
    let mut attempt = 1u32;
    loop {
        match operation().await {
            Ok(value) => return Ok(value),
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

fn startup_retry_delay(attempt: u32) -> StdDuration {
    let shift = attempt.saturating_sub(1).min(3);
    let millis = 500u64.saturating_mul(1u64 << shift).min(5_000);
    StdDuration::from_millis(millis)
}

fn apply_service_bind_env_overrides(config: &mut Config) {
    if let Ok(host) = std::env::var("KIRO_RS_HOST") {
        let host = host.trim();
        if !host.is_empty() {
            config.host = host.to_string();
        }
    }

    if let Ok(port) = std::env::var("KIRO_RS_PORT") {
        let port = port.trim();
        if port.is_empty() {
            return;
        }
        match port.parse::<u16>() {
            Ok(port) => config.port = port,
            Err(err) => tracing::warn!(
                value = port,
                error = %err,
                "忽略无效的 KIRO_RS_PORT 环境变量"
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
}

fn create_health_router(state: Arc<AppHealthState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(state)
}

async fn healthz() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "kiro-rs"
    }))
}

async fn readyz(State(state): State<Arc<AppHealthState>>) -> impl IntoResponse {
    let postgres_ok = state.postgres_store.ping().await.is_ok();
    let redis_ok = state.redis_store.ping().await.is_ok();
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

    (
        status,
        Json(json!({
            "status": if ready { "ready" } else { "not_ready" },
            "checks": {
                "postgres": postgres_ok,
                "redis": redis_ok,
                "redisRuntimeEvents": redis_events_connected
            },
            "lastRedisRuntimeEventAtMs": state.runtime_events.last_event_at_ms.load(Ordering::Acquire),
            "lastRedisSubscribeErrorAtMs": state.runtime_events.last_subscribe_error_at_ms.load(Ordering::Acquire)
        })),
    )
}

async fn new_ui_index_redirect() -> Redirect {
    Redirect::permanent("/ui")
}

fn spawn_redis_runtime_event_listener(
    redis_store: Arc<RedisStore>,
    token_manager: Arc<MultiTokenManager>,
    request_api_key_store: Arc<RequestApiKeyStore>,
    health: Arc<RuntimeEventHealth>,
) {
    tokio::spawn(async move {
        loop {
            let config_channel = redis_store.runtime_config_changed_channel();
            let credentials_channel = redis_store.credentials_changed_channel();
            let wakeup_channel = redis_store.dispatch_wakeup_channel();
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
                            match token_manager.reload_runtime_config_from_postgres() {
                                Ok(true) => {
                                    request_api_key_store.replace_keys(token_manager.runtime_config().request_api_keys());
                                    tracing::info!(payload, "已根据 Redis 通知热加载运行配置");
                                }
                                Ok(false) => tracing::debug!(payload, "收到运行配置通知，但未执行热加载"),
                                Err(err) => tracing::warn!(payload, "热加载运行配置失败: {}", err),
                            }
                        } else if channel == credentials_channel {
                            match token_manager.reload_credentials_from_postgres() {
                                Ok(true) => tracing::info!(payload, "已根据 Redis 通知同步凭据快照"),
                                Ok(false) => tracing::debug!(payload, "收到凭据通知，但凭据快照无变化"),
                                Err(err) => tracing::warn!(payload, "同步凭据快照失败: {}", err),
                            }
                            token_manager.notify_dispatch_state_changed();
                        } else if channel == wakeup_channel {
                            token_manager.notify_dispatch_state_changed();
                        }
                    }
                    _ = periodic_reload.tick() => {
                        match token_manager.reload_runtime_config_from_postgres() {
                            Ok(true) => request_api_key_store.replace_keys(token_manager.runtime_config().request_api_keys()),
                            Ok(false) => {}
                            Err(err) => tracing::warn!("定时热加载运行配置失败: {}", err),
                        }
                        if let Err(err) = token_manager.reload_credentials_from_postgres() {
                            tracing::warn!("定时同步凭据快照失败: {}", err);
                        }
                    }
                }
            }
            health.mark_disconnected();
            tracing::warn!("Redis 运行时事件订阅已断开，准备重新订阅");
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
}

fn handle_cli_command(
    command: Command,
    config: &Config,
    credentials_config: CredentialsConfig,
    credentials_path: &str,
) -> anyhow::Result<()> {
    match command {
        Command::Credentials { command } => {
            handle_credentials_command(command, config, credentials_config, credentials_path)
        }
    }
}

fn handle_credentials_command(
    command: CredentialsCommand,
    config: &Config,
    credentials_config: CredentialsConfig,
    credentials_path: &str,
) -> anyhow::Result<()> {
    let is_multiple = credentials_config.is_multiple();
    let credentials = credentials_config.into_sorted_credentials();

    match command {
        CredentialsCommand::Stats => {
            println!("credentials: {}", credentials.len());
            println!(
                "format: {}",
                if is_multiple { "multiple" } else { "single" }
            );
            println!("loadBalancingMode: {}", config.load_balancing_mode);
            println!(
                "credentialRpm: {}",
                config
                    .credential_rpm
                    .map(|rpm| rpm.to_string())
                    .unwrap_or_else(|| "disabled".to_string())
            );
            for (index, credential) in credentials.iter().enumerate() {
                let id = credential.id.unwrap_or((index + 1) as u64);
                let label = credential
                    .email
                    .as_deref()
                    .or_else(|| credential.endpoint.as_deref())
                    .unwrap_or("-");
                println!(
                    "#{id} priority={} disabled={} auth={} label={}",
                    credential.priority,
                    credential.disabled,
                    credential.auth_method.as_deref().unwrap_or(
                        if credential.kiro_api_key.is_some() {
                            "api_key"
                        } else {
                            "oauth"
                        }
                    ),
                    label
                );
            }
        }
        CredentialsCommand::Diagnostics => {
            println!("credentialsPath: {}", credentials_path);
            println!("credentials: {}", credentials.len());
            println!(
                "format: {}",
                if is_multiple { "multiple" } else { "single" }
            );
            if !is_multiple {
                println!("warning: single credentials format cannot be rewritten by token refresh");
            }
            if !matches!(
                config.load_balancing_mode.as_str(),
                "priority" | "balanced" | "health_balanced" | "weighted_least_inflight"
            ) {
                println!(
                    "error: invalid loadBalancingMode '{}', expected priority, balanced, health_balanced or weighted_least_inflight",
                    config.load_balancing_mode
                );
            }
            let mut ids = std::collections::HashSet::new();
            for (index, credential) in credentials.iter().enumerate() {
                let id = credential.id.unwrap_or((index + 1) as u64);
                if !ids.insert(id) {
                    println!("error: duplicate credential id #{id}");
                }
                if credential.is_api_key_credential() && credential.kiro_api_key.is_none() {
                    println!("error: credential #{id} authMethod=api_key but missing kiroApiKey");
                }
                if !credential.is_api_key_credential() && credential.refresh_token.is_none() {
                    println!("warning: credential #{id} missing refreshToken");
                }
                if credential.machine_id.is_none() {
                    println!(
                        "info: credential #{id} missing machineId, it will be generated at startup"
                    );
                }
            }
        }
    }

    Ok(())
}
