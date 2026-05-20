mod admin;
mod admin_ui;
mod anthropic;
mod app_config;
mod common;
mod http_client;
mod kiro;
mod model;
mod pricing;
mod storage;
pub mod token;

use std::collections::HashMap;
use std::sync::Arc;

use clap::Parser;
use kiro::endpoint::{IdeEndpoint, KiroEndpoint};
use kiro::model::credentials::{CredentialsConfig, KiroCredentials};
use kiro::provider::KiroProvider;
use kiro::token_manager::MultiTokenManager;
use model::arg::Args;
use model::config::Config;

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
    let config = Config::load(&config_path).unwrap_or_else(|e| {
        tracing::error!("加载配置失败: {}", e);
        std::process::exit(1);
    });

    // 加载凭证（支持单对象或数组格式）
    let credentials_path = args
        .credentials
        .unwrap_or_else(|| KiroCredentials::default_credentials_path().to_string());
    let credentials_config = CredentialsConfig::load(&credentials_path).unwrap_or_else(|e| {
        tracing::error!("加载凭证失败: {}", e);
        std::process::exit(1);
    });

    // 判断是否为多凭据格式（用于刷新后回写）
    let is_multiple_format = credentials_config.is_multiple();

    // 转换为按优先级排序的凭据列表
    let mut credentials_list = credentials_config.into_sorted_credentials();

    // 检查 KIRO_API_KEY 环境变量，自动创建 API Key 凭据
    if let Ok(kiro_api_key) = std::env::var("KIRO_API_KEY") {
        if kiro_api_key.is_empty() {
            tracing::warn!("KIRO_API_KEY 环境变量已设置但为空，视为未配置");
        } else {
            tracing::info!("检测到 KIRO_API_KEY 环境变量，添加 API Key 凭据（最高优先级）");
            let api_key_cred = KiroCredentials {
                kiro_api_key: Some(kiro_api_key),
                auth_method: Some("api_key".to_string()),
                priority: 0,
                ..Default::default()
            };
            credentials_list.insert(0, api_key_cred);
        }
    }

    tracing::info!("已加载 {} 个凭据配置", credentials_list.len());

    // 获取第一个凭据用于日志显示
    let first_credentials = credentials_list.first().cloned().unwrap_or_default();
    tracing::debug!("主凭证: {:?}", first_credentials);

    // 连接 PostgreSQL + Redis（硬依赖，失败即退出）
    let database_url = config.resolve_database_url().unwrap_or_else(|e| {
        tracing::error!("{}", e);
        std::process::exit(1);
    });
    let redis_url = config.resolve_redis_url().unwrap_or_else(|e| {
        tracing::error!("{}", e);
        std::process::exit(1);
    });
    let _storage = storage::init(&database_url, &redis_url)
        .await
        .unwrap_or_else(|e| {
            tracing::error!("数据持久化层初始化失败: {:#}", e);
            std::process::exit(1);
        });

    // 初始化在线运行时配置(读 app_config 表)
    let app_config_service = app_config::AppConfigService::new(_storage.db.clone())
        .await
        .unwrap_or_else(|e| {
            tracing::error!("加载 app_config 失败: {:#}", e);
            std::process::exit(1);
        });

    // 初始化模型计价(启动时若未 bootstrap 则异步同步一次)
    let pricing_registry =
        pricing::ModelPricingRegistry::new(_storage.db.clone(), app_config_service.clone());
    if let Err(err) = pricing_registry.bootstrap().await {
        tracing::warn!("ModelPricingRegistry bootstrap 失败(不影响启动): {:#}", err);
    }
    // 立即把已有的 model_prices 灌进内存缓存,保证 record 立刻能算 cost_usd
    if let Err(err) = pricing_registry.warm_cache().await {
        tracing::warn!("ModelPricingRegistry 内存缓存初始化失败: {:#}", err);
    }

    // 获取 API Key
    let api_key = config.api_key.clone().unwrap_or_else(|| {
        tracing::error!("配置文件中未设置 apiKey");
        std::process::exit(1);
    });

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
    let usage_record_path = if config.usage_record_persist {
        std::path::Path::new(&credentials_path)
            .parent()
            .map(|dir| dir.join("kiro_usage_records.jsonl"))
    } else {
        None
    };
    let mut usage_recorder =
        anthropic::usage::UsageRecorder::new(config.usage_record_limit, usage_record_path);
    usage_recorder
        .attach_storage(_storage.db.clone(), pricing_registry.clone())
        .await;
    let usage_recorder = Arc::new(usage_recorder);
    let prompt_cache = Arc::new(anthropic::prompt_cache::PromptCacheTracker::default());
    let prompt_cache_runtime_config = Arc::new(anthropic::PromptCacheRuntimeConfig::new(
        anthropic::PromptCacheRuntimeConfigSnapshot::from_config_and_app_config(
            &config,
            &app_config_service,
        ),
    ));

    // 创建 MultiTokenManager 并 attach 存储层
    let mut token_manager = MultiTokenManager::new(
        config.clone(),
        credentials_list,
        proxy_config.clone(),
        Some(credentials_path.into()),
        is_multiple_format,
    )
    .unwrap_or_else(|e| {
        tracing::error!("创建 Token 管理器失败: {}", e);
        std::process::exit(1);
    });
    if let Err(e) = token_manager.attach_storage(_storage.clone()).await {
        tracing::error!("Token 管理器接入 PG 存储失败: {:#}", e);
        std::process::exit(1);
    }
    // 应用 app_config 中的配额阈值(失败回退到默认 3 次 / 30 分)
    let strike_limit: u32 = app_config_service
        .get_as::<u32>("quota_soft_fail_limit")
        .unwrap_or(3);
    let cooldown_minutes: i64 = app_config_service
        .get_as::<i64>("quota_cooldown_minutes")
        .unwrap_or(30);
    token_manager.set_quota_settings(strike_limit, cooldown_minutes);
    let session_binding_ttl_minutes: i64 = app_config_service
        .get_as::<i64>("session_binding_ttl_minutes")
        .unwrap_or(30);
    token_manager.set_session_binding_ttl_minutes(session_binding_ttl_minutes);

    // 让 app_config 中的 load_balancing_mode 在内存生效(修双源 bug)
    if let Some(mode) = app_config_service.get_as::<String>("load_balancing_mode") {
        token_manager.override_load_balancing_mode(&mode);
    }

    let token_manager = Arc::new(token_manager);
    let kiro_provider = KiroProvider::with_proxy(
        token_manager.clone(),
        proxy_config.clone(),
        endpoints,
        config.default_endpoint.clone(),
    );
    let admin_kiro_provider = Arc::new(kiro_provider.clone());

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
        &api_key,
        Some(kiro_provider),
        config.extract_thinking,
        usage_recorder.clone(),
        prompt_cache.clone(),
        prompt_cache_runtime_config.clone(),
        config.compat_profile,
        config.expose_proxy_warnings,
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
                admin_kiro_provider.clone(),
                endpoint_names.clone(),
                usage_recorder.clone(),
                prompt_cache.clone(),
                prompt_cache_runtime_config.clone(),
                app_config_service.clone(),
                _storage.redis.clone(),
                _storage.db.clone(),
            );
            let admin_state = admin::AdminState::new(
                admin_key,
                admin_service,
                app_config_service.clone(),
                pricing_registry.clone(),
                _storage.db.clone(),
                token_manager.clone(),
                prompt_cache_runtime_config.clone(),
            );
            let admin_app = admin::create_admin_router(admin_state);

            // 创建 Admin UI 路由(旧版 + 新版并存)
            let admin_ui_app = admin_ui::create_admin_ui_router();
            let console_app = admin_ui::create_console_router();

            tracing::info!("Admin API 已启用");
            tracing::info!("Admin UI 已启用: /admin (旧版) · /console (新版)");
            anthropic_app
                .nest("/api/admin", admin_app)
                .nest("/admin", admin_ui_app)
                .nest("/console", console_app)
        }
    } else {
        anthropic_app
    };

    // 启动服务器
    let addr = format!("{}:{}", config.host, config.port);
    tracing::info!("启动 Anthropic API 端点: {}", addr);
    tracing::info!("API Key: {}***", &api_key[..(api_key.len() / 2)]);
    tracing::info!("可用 API:");
    tracing::info!("  GET  /v1/models");
    tracing::info!("  POST /v1/messages");
    tracing::info!("  POST /v1/messages/count_tokens");
    if admin_key_valid {
        tracing::info!("Admin API:");
        tracing::info!("  GET  /api/admin/credentials");
        tracing::info!("  GET  /api/admin/credentials-paged");
        tracing::info!("  GET  /api/admin/usage-records");
        tracing::info!("  GET  /api/admin/usage-records-paged");
        tracing::info!("  GET  /api/admin/usage-summary");
        tracing::info!("  POST /api/admin/credentials/:index/disabled");
        tracing::info!("  POST /api/admin/credentials/:index/priority");
        tracing::info!("  POST /api/admin/credentials/:index/reset");
        tracing::info!("  GET  /api/admin/credentials/:index/balance");
        tracing::info!("Admin UI:");
        tracing::info!("  GET  /admin");
    }

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
