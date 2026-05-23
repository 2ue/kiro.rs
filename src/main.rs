mod admin;
mod admin_ui;
mod anthropic;
mod common;
mod http_client;
mod kiro;
mod model;
pub mod token;

use std::collections::HashMap;
use std::sync::Arc;

use clap::Parser;
use kiro::endpoint::{IdeEndpoint, KiroEndpoint};
use kiro::model::credentials::{CredentialsConfig, KiroCredentials};
use kiro::provider::KiroProvider;
use kiro::token_manager::MultiTokenManager;
use model::arg::{Args, Command, CredentialsCommand};
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

    if let Some(command) = args.command {
        if let Err(err) =
            handle_cli_command(command, &config, credentials_config, &credentials_path)
        {
            tracing::error!("{}", err);
            std::process::exit(1);
        }
        return;
    }

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
    let usage_recorder = Arc::new(anthropic::usage::UsageRecorder::new(
        config.usage_record_limit,
        usage_record_path,
    ));
    let prompt_cache = Arc::new(anthropic::prompt_cache::PromptCacheTracker::default());
    let pricing_catalog = Arc::new(anthropic::pricing::PricingCatalog::new());
    {
        let pricing_catalog = pricing_catalog.clone();
        tokio::spawn(async move {
            let status = pricing_catalog.sync().await;
            if status.last_error.is_some() {
                tracing::warn!(
                    source = %status.source,
                    model_count = status.model_count,
                    "模型价格启动同步失败，使用当前价格目录继续运行"
                );
            } else {
                tracing::info!(
                    source = %status.source,
                    model_count = status.model_count,
                    "模型价格已初始化"
                );
            }
        });
    }

    // 创建 MultiTokenManager 和 KiroProvider
    let token_manager = MultiTokenManager::new(
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
    let token_manager = Arc::new(token_manager);
    let kiro_provider = KiroProvider::with_proxy(
        token_manager.clone(),
        proxy_config.clone(),
        endpoints,
        config.default_endpoint.clone(),
    );
    let kiro_provider = Arc::new(kiro_provider);

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
        Some(kiro_provider.clone()),
        config.extract_thinking,
        usage_recorder.clone(),
        prompt_cache.clone(),
        pricing_catalog.clone(),
        config.prompt_cache_target_read_ratio,
        config.prompt_cache_token_scale,
        config.prompt_cache_max_simulated_input_tokens,
        config.prompt_cache_cap_jitter_min_tokens,
        config.prompt_cache_cap_jitter_max_tokens,
        config.prompt_cache_scale_min_input_tokens,
        config.reported_usage.clone(),
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
                endpoint_names.clone(),
                usage_recorder.clone(),
                prompt_cache.clone(),
                pricing_catalog.clone(),
                kiro_provider.clone(),
            );
            let admin_state = admin::AdminState::new(admin_key, admin_service);
            let admin_app = admin::create_admin_router(admin_state);

            // 创建 Admin UI 路由
            let admin_ui_app = admin_ui::create_admin_ui_router();

            tracing::info!("Admin API 已启用");
            tracing::info!("Admin UI 已启用: /admin");
            anthropic_app
                .nest("/api/admin", admin_app)
                .nest("/admin", admin_ui_app)
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
    tracing::info!("  POST /v1/messages (high-cache)");
    tracing::info!("  POST /v1/messages/count_tokens");
    tracing::info!("  GET  /na/v1/models");
    tracing::info!("  POST /na/v1/messages (real-cache-usage)");
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
        tracing::info!("  GET  /api/admin/credentials-paged");
        tracing::info!("  GET  /api/admin/credentials/export");
        tracing::info!("  GET  /api/admin/usage-records");
        tracing::info!("  GET  /api/admin/usage-records-paged");
        tracing::info!("  GET  /api/admin/usage-summary");
        tracing::info!("  GET  /api/admin/model-pricing");
        tracing::info!("  POST /api/admin/model-pricing/sync");
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
    let stats = load_cli_stats(credentials_path);

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
            println!("credentialsPersist: {}", config.credentials_persist);
            println!(
                "credentialStatsPersist: {}",
                config.credential_stats_persist
            );
            for (index, credential) in credentials.iter().enumerate() {
                let id = credential.id.unwrap_or((index + 1) as u64);
                let stat = stats
                    .as_ref()
                    .and_then(|map| map.get(&id.to_string()))
                    .cloned()
                    .unwrap_or_default();
                let label = credential
                    .email
                    .as_deref()
                    .or_else(|| credential.endpoint.as_deref())
                    .unwrap_or("-");
                println!(
                    "#{id} priority={} disabled={} auth={} label={} success={} lastUsed={}",
                    credential.priority,
                    credential.disabled,
                    credential.auth_method.as_deref().unwrap_or(
                        if credential.kiro_api_key.is_some() {
                            "api_key"
                        } else {
                            "oauth"
                        }
                    ),
                    label,
                    stat.success_count,
                    stat.last_used_at.unwrap_or_else(|| "-".to_string())
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
            if !config.credentials_persist {
                println!("warning: credentials persistence is disabled");
            }
            if !config.credential_stats_persist {
                println!("warning: credential stats persistence is disabled");
            }
            if config.load_balancing_mode != "priority" && config.load_balancing_mode != "balanced"
            {
                println!(
                    "error: invalid loadBalancingMode '{}', expected priority or balanced",
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

#[derive(Default, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CliStatsEntry {
    success_count: u64,
    last_used_at: Option<String>,
}

fn load_cli_stats(
    credentials_path: &str,
) -> Option<std::collections::HashMap<String, CliStatsEntry>> {
    let stats_path = std::path::Path::new(credentials_path)
        .parent()
        .map(|dir| dir.join("kiro_stats.json"))?;
    let content = std::fs::read_to_string(stats_path).ok()?;
    serde_json::from_str(&content).ok()
}
