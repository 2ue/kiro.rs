use std::{collections::BTreeSet, io::Write, path::Path};

use anyhow::Context;
use serde_json::{Map, Value, json};

use crate::{
    account::CreateAccountRequest,
    model::{config::Config, routes::migrate_path_overrides_to_routes},
    storage::{postgres::PostgresStore, redis_cache::RedisStore},
};

use super::{LegacyPackage, report::MigrationReport};

const ACCOUNT_FIELDS: &[&str] = &[
    "name",
    "base_url",
    "api_key",
    "auth_type",
    "header_profile",
    "append_beta_query",
    "header_overrides",
    "wire_profile",
    "tls_profile",
    "enabled",
    "priority",
    "max_concurrent_requests",
    "usage_projection_mode",
    "stream_response_mode",
    "auto_disable_policy",
    "pre_output_stream_retry_mode",
    "normalize_model_version_dots",
    "model_mapping_mode",
    "model_mapping_require_match",
    "model_mapping_rules",
    "supported_models",
    "route_mode",
    "route_rules",
    "notes",
];
const MACHINE_FIELDS: &[&str] = &["postgres", "redis", "observabilityRedis", "host", "port"];

pub(super) struct ImportPlan {
    pub config: Option<Config>,
    pub accounts: Vec<Value>,
    pub scenarios: Vec<Value>,
    pub pricing: Vec<Value>,
    pub capabilities: Vec<Value>,
    pub report: MigrationReport,
}

impl ImportPlan {
    pub fn ensure_applicable(&self) -> anyhow::Result<&Config> {
        anyhow::ensure!(
            self.report.errors.is_empty(),
            "迁移校验失败，拒绝 --apply：{}",
            self.report.errors.join("；")
        );
        self.config.as_ref().context("配置转换失败，拒绝 --apply")
    }
}

pub async fn import_legacy(file: &Path, apply: bool, deployment: &Config) -> anyhow::Result<()> {
    let package: LegacyPackage =
        serde_json::from_slice(&std::fs::read(file).context("读取迁移包失败")?)
            .context("解析迁移包失败")?;
    let plan = prepare_import(&package, deployment);
    // head 提前关闭管道属于预览正常退出，不触发 panic。
    if let Err(error) = std::io::stdout()
        .lock()
        .write_all(plan.report.render(apply).as_bytes())
    {
        if error.kind() == std::io::ErrorKind::BrokenPipe && !apply {
            return Ok(());
        }
        return Err(error.into());
    }
    let config = plan.ensure_applicable()?;
    if !apply {
        return Ok(());
    }
    let store = PostgresStore::connect(deployment).await?;
    let redis = RedisStore::connect(deployment).await?;
    let version = store
        .replace_configuration_transaction(
            config,
            &plan.accounts,
            &plan.scenarios,
            &plan.pricing,
            &plan.capabilities,
        )
        .await?;
    // 两个通知都尝试发送；数据库事务已经提交，失败时明确报告已提交状态。
    let runtime_event = async {
        let client = redis::Client::open(
            deployment
                .redis
                .url
                .as_deref()
                .context("必须配置 redis.url")?,
        )?;
        let mut connection = client.get_multiplexed_async_connection().await?;
        redis::cmd("PUBLISH")
            .arg(redis.runtime_config_changed_channel())
            .arg(json!({"version":version}).to_string())
            .query_async::<i64>(&mut connection)
            .await?;
        anyhow::Ok(())
    }
    .await;
    let account_event = redis
        .publish_account_data_changed("import_legacy", None)
        .await;
    runtime_event.context("数据库已提交，但运行配置通知失败")?;
    account_event.context("数据库已提交，但账号通知失败")?;
    println!("已在单事务内全量覆盖并发送运行配置、账号变更通知（version={version}）");
    Ok(())
}

pub(super) fn prepare_import(package: &LegacyPackage, deployment: &Config) -> ImportPlan {
    let mut report = MigrationReport::default();
    if package.format_version != 1 || package.exported_from.kind != "kiro-rs-postgres" {
        report
            .errors
            .push("不支持的 formatVersion 或 exportedFrom.kind".into());
    }
    if package.exported_from.secrets_redacted
        || contains_redacted(&serde_json::to_value(package).expect("serialize package"))
    {
        report
            .errors
            .push("包包含 <redacted>，脱敏包拒绝 --apply".into());
    }
    let accounts = migrate_accounts(&package.accounts, &mut report);
    let config_value = migrate_config(package.runtime_config.clone(), &mut report);
    let config = match serde_json::from_value::<Config>(config_value) {
        Ok(mut config) => {
            config.postgres = deployment.postgres.clone();
            config.redis = deployment.redis.clone();
            config.observability_redis = deployment.observability_redis.clone();
            config.host = deployment.host.clone();
            config.port = deployment.port;
            config.apply_runtime_config_migrations();
            if let Err(error) = validate_config(&config) {
                report.errors.push(error.to_string());
            }
            Some(config)
        }
        Err(error) => {
            report.errors.push(format!("Config 反序列化：{error}"));
            None
        }
    };
    let scenarios = migrate_scenarios(&package.route_scenarios, &mut report);
    let pricing = migrate_models(&package.model_pricing, false, &mut report);
    let capabilities = migrate_models(&package.model_capabilities, true, &mut report);
    ImportPlan {
        config,
        accounts,
        scenarios,
        pricing,
        capabilities,
        report,
    }
}

fn contains_redacted(value: &Value) -> bool {
    match value {
        Value::String(s) => s.contains("<redacted>"),
        Value::Array(values) => values.iter().any(contains_redacted),
        Value::Object(values) => values.values().any(contains_redacted),
        _ => false,
    }
}

fn camel_case(value: &str) -> String {
    let mut parts = value.split('_');
    let mut result = parts.next().unwrap_or_default().to_string();
    for part in parts {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            result.extend(first.to_uppercase());
            result.extend(chars);
        }
    }
    result
}

fn migrate_accounts(rows: &[Value], report: &mut MigrationReport) -> Vec<Value> {
    let mut names = BTreeSet::new();
    rows.iter().filter_map(|row| {
        if row.get("deleted_at").is_some_and(|v| !v.is_null()) { return None; }
        let Some(fields) = row.as_object() else { report.errors.push("accounts[] 必须为对象".into()); return None; };
        let name = row["name"].as_str().unwrap_or("<无名称>");
        if !names.insert(name.to_string()) { report.errors.push(format!("账号自然键 name 重复：{name}")); }
        let path = format!("accounts[{name}]");
        let mut target = Map::new();
        for (key, value) in fields {
            if ACCOUNT_FIELDS.contains(&key.as_str()) {
                target.insert(key.clone(), value.clone());
                let target_key = camel_case(key);
                report.moved(&format!("{path}.{key}"), &format!("accounts[{name}].{target_key}"));
            } else { report.discarded(&format!("{path}.{key}"), value); }
        }
        if row["request_body_mode"] == "normalized" { report.normalized_accounts.push(name.into()); }
        if target.get("model_mapping_mode").and_then(Value::as_str) == Some("processed_mapping") {
            target.insert("model_mapping_mode".into(), json!("passthrough_mapping"));
            report.highlights.push(format!("{path}.model_mapping_mode: processed_mapping → passthrough_mapping；该模式已删除"));
        }
        if let Some(mode) = row.get("usage_projection_mode") {
            report.changes.push(format!("[保留旧值] {path}.usage_projection_mode = {mode}（不改为新默认）"));
        }
        let camel = Value::Object(target.iter().map(|(k,v)| (camel_case(k),v.clone())).collect());
        match serde_json::from_value::<CreateAccountRequest>(camel) {
            Ok(account) => {
                report.concurrency += u64::from(account.max_concurrent_requests);
                if let Err(error) = validate_account(&account) { report.errors.push(format!("{path}：{error}")); }
                // 对缺列的旧 schema 填当前类型默认值，SQL 写入不依赖 nullable 默认。
                let defaults = json!({"auth_type":"bearer","header_profile":"generic","append_beta_query":false,
                    "header_overrides":{},"wire_profile":"default","tls_profile":"default","enabled":true,
                    "priority":100,"max_concurrent_requests":10,"usage_projection_mode":"current_path_policy",
                    "stream_response_mode":null,"auto_disable_policy":"inherit","pre_output_stream_retry_mode":"inherit",
                    "normalize_model_version_dots":false,"model_mapping_mode":"passthrough_mapping",
                    "model_mapping_require_match":false,"model_mapping_rules":[],"supported_models":[],
                    "route_mode":"allow_all","route_rules":[],"notes":null});
                for (key, value) in defaults.as_object().unwrap() { target.entry(key.clone()).or_insert_with(|| value.clone()); }
                Some(Value::Object(target))
            }
            Err(error) => { report.errors.push(format!("{path}：{error}")); None }
        }
    }).collect()
}

fn validate_account(account: &CreateAccountRequest) -> anyhow::Result<()> {
    anyhow::ensure!(!account.name.trim().is_empty(), "账号名称不能为空");
    let url = url::Url::parse(&account.base_url)?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        "baseUrl 必须为 http/https URL"
    );
    anyhow::ensure!(
        (1..=i32::MAX as u32).contains(&account.max_concurrent_requests),
        "账号并发超出 PostgreSQL 范围"
    );
    anyhow::ensure!(
        account.route_rules.len() <= 200 && account.route_rules.iter().all(|r| r.len() <= 256),
        "账号路由规则超限"
    );
    crate::account::normalize_account_header_overrides(account.header_overrides.clone())?;
    crate::account::validate_account_fields(None, 1.0, &account.overrides)
}

fn migrate_config(mut value: Value, report: &mut MigrationReport) -> Value {
    let Some(root) = value.as_object_mut() else {
        report.errors.push("runtimeConfig 必须为对象".into());
        return json!({});
    };
    for field in MACHINE_FIELDS {
        if root.remove(*field).is_some() {
            report.changes.push(format!(
                "[丢弃] runtimeConfig.{field}：机器本地字段，保留目标部署值"
            ));
        }
    }
    for key in ["apiKey", "apiKeys", "requestApiKeyPolicies", "adminApiKey"] {
        if root.contains_key(key) {
            report.moved(&format!("runtimeConfig.{key}"), key);
        }
    }
    report
        .highlights
        .push("adminApiKey 会被全量覆盖；下游密钥使用迁移包中的值".into());
    if let Some(old) = root.remove("externalPools") {
        let mut accounts = Map::new();
        if let Some(fields) = old.as_object() {
            for (key, entry) in fields {
                let removed = [
                    "externalDirectPolicy",
                    "directExternal",
                    "localPool",
                    "fallbackOn",
                    "externalPoolLocalRescue",
                ]
                .iter()
                .any(|prefix| key.starts_with(prefix))
                    || matches!(
                        key.as_str(),
                        "externalPoolMaxInputTokens"
                            | "externalPoolTransientFailureCooldownThreshold"
                    );
                if removed {
                    report.discarded(&format!("runtimeConfig.externalPools.{key}"), entry);
                    continue;
                }
                if key == "externalPoolsEnabled" {
                    report.moved(
                        "runtimeConfig.externalPools.externalPoolsEnabled",
                        "accounts.enabled",
                    );
                    accounts.insert("enabled".into(), entry.clone());
                    continue;
                }
                if let Some(suffix) = key.strip_prefix("externalPool") {
                    let mut chars = suffix.chars();
                    let target = chars
                        .next()
                        .map(|c| c.to_lowercase().collect::<String>() + chars.as_str())
                        .unwrap_or_default();
                    let target = match target.as_str() {
                        "routeMode" => "accountRouteMode".into(),
                        "routeRules" => "accountRouteRules".into(),
                        _ => target,
                    };
                    report.moved(
                        &format!("runtimeConfig.externalPools.{key}"),
                        &format!("accounts.{target}"),
                    );
                    accounts.insert(target, entry.clone());
                } else {
                    report.discarded(&format!("runtimeConfig.externalPools.{key}"), entry);
                }
            }
        } else {
            report.errors.push("externalPools 必须为对象".into());
        }
        root.insert("accounts".into(), Value::Object(accounts));
    }
    let defaults = serde_json::to_value(crate::model::config::AccountsConfig::default()).unwrap();
    for key in ["capacityMode", "dispatchMaxWaitSecs", "maxQueuedRequests"] {
        if value.pointer(&format!("/accounts/{key}")).is_none() {
            report
                .changed_defaults
                .push(format!("{key} → {}", defaults[key]));
        }
    }
    if value
        .as_object_mut()
        .unwrap()
        .remove("payloadGuardKiroMaxWeight")
        .is_some()
    {
        report.changes.push(
            "[丢弃 Kiro/旧顶层字段] runtimeConfig.payloadGuardKiroMaxWeight（按字段名归组）".into(),
        );
    }
    migrate_global_features(&mut value, report);
    clean_injections(&mut value, report);
    for key in ["cachePolicy", "cacheTtl"] {
        if let Some(cache) = value.get_mut(key) {
            rename_cache(cache, &format!("runtimeConfig.{key}"), report);
        }
    }
    if let Some(routes) = value.get_mut("routes").and_then(Value::as_array_mut) {
        for (index, route) in routes.iter_mut().enumerate() {
            if let Some(cache) = route.get_mut("cachePolicy") {
                rename_cache(
                    cache,
                    &format!("runtimeConfig.routes[{index}].cachePolicy"),
                    report,
                );
            }
        }
    }
    clean_legacy_features(&mut value, report);
    for (from, to) in [
        ("cachePolicy.pathOverrides", "cachePolicy"),
        ("reportedUsage.pathOverrides", "reportedUsage"),
        ("promptInjection.pathOverrides", "promptInjection"),
        ("routeFeatures.pathOverrides", "features"),
        ("definedCacheRoutes", "prefix=/dfcache/{name}"),
        ("accounts.accountRouteMode", "accountRule"),
        ("accounts.accountRouteRules", "accountRule"),
    ] {
        if value
            .pointer(&format!("/{}", from.replace('.', "/")))
            .is_some()
        {
            report.moved(&format!("runtimeConfig.{from}"), &format!("routes[].{to}"));
        }
    }
    migrate_path_overrides_to_routes(&mut value);
    // Config 忽略未知顶层键；显式分类，避免静默丢失旧库字段。
    if let Some(entry) = value
        .as_object_mut()
        .unwrap()
        .remove("promptCacheMaxEntriesPerCredential")
    {
        report.moved(
            "runtimeConfig.promptCacheMaxEntriesPerCredential",
            "promptCacheMaxEntriesPerAccount",
        );
        value["promptCacheMaxEntriesPerAccount"] = entry;
    }
    let known = serde_json::to_value(Config::default()).unwrap();
    let root = value.as_object_mut().unwrap();
    let discarded = root
        .keys()
        .filter(|key| known.get(*key).is_none())
        .cloned()
        .collect::<Vec<_>>();
    for key in discarded {
        root.remove(&key);
        report.changes.push(format!(
            "[丢弃 Kiro/旧顶层字段] runtimeConfig.{key}（按字段名归组）"
        ));
    }
    value
}

fn migrate_global_features(value: &mut Value, report: &mut MigrationReport) {
    let root = value.as_object_mut().unwrap();
    let mut moved = Map::new();
    for (old, new) in [
        ("missingMaxTokens", "missingMaxTokens"),
        ("localRequestCompat", "reasoningCompat"),
        ("payloadShaping", "payloadShaping"),
        ("imageProcessing", "imageProcessing"),
    ] {
        if let Some(mut config) = root.remove(old) {
            if old == "localRequestCompat"
                && let Some(fields) = config.as_object_mut()
                && let Some(v) = fields.remove("zeroUsageOnHoldExpiredMessageStart")
            {
                report.discarded(
                    "runtimeConfig.localRequestCompat.zeroUsageOnHoldExpiredMessageStart",
                    &v,
                );
            }
            let enabled = match old {
                "missingMaxTokens" => config["policy"] != "reject",
                "localRequestCompat" => serde_json::from_value::<
                    crate::model::config::LocalRequestCompatConfig,
                >(config.clone())
                .map(|compat| {
                    serde_json::to_value(compat)
                        .unwrap()
                        .as_object()
                        .unwrap()
                        .values()
                        .any(|v| v == true)
                })
                .unwrap_or(true),
                "payloadShaping" => config["enabled"] == true,
                "imageProcessing" => {
                    config["mode"].as_str().unwrap_or("safe") == "safe"
                        || config["resizeEnabled"] == true
                }
                _ => false,
            };
            report.moved(
                &format!("runtimeConfig.{old}"),
                &format!("routeFeatures.defaults.{new}"),
            );
            if enabled {
                report.enabled_features.push(old.into());
                moved.insert(new.into(), config);
            }
        }
    }
    let guard_keys = root
        .keys()
        .filter(|k| k.starts_with("payloadGuard") || k.as_str() == "payloadTooLongHandling")
        .cloned()
        .collect::<Vec<_>>();
    let mut guard = Map::new();
    let guard_enabled = root.get("payloadGuardEnabled") == Some(&json!(true))
        || root.get("payloadGuardExternalEnabled") == Some(&json!(true));
    for key in guard_keys {
        let entry = root.remove(&key).unwrap();
        let target = match key.as_str() {
            "payloadGuardEnabled" => Some("enabled"),
            "payloadGuardMode" => Some("mode"),
            "payloadGuardMaxBytes" => Some("maxBytes"),
            "payloadGuardSafetyMarginBytes" => Some("safetyMarginBytes"),
            "payloadGuardTrimHistory" => Some("trimHistory"),
            "payloadGuardExternalEnabled" => Some("externalEnabled"),
            "payloadTooLongHandling" => Some("tooLongHandling"),
            _ => None,
        };
        if let Some(target) = target {
            report.moved(
                &format!("runtimeConfig.{key}"),
                &format!("routeFeatures.defaults.payloadGuard.{target}"),
            );
            guard.insert(target.into(), entry);
        } else {
            report.discarded(&format!("runtimeConfig.{key}"), &entry);
        }
    }
    if guard_enabled {
        guard.insert("enabled".into(), json!(true));
        report.enabled_features.push("payloadGuardEnabled".into());
        moved.insert("payloadGuard".into(), Value::Object(guard));
    }
    if !moved.is_empty() {
        let features = root.entry("routeFeatures").or_insert_with(|| json!({}));
        if let Some(features) = features.as_object_mut() {
            let defaults = features.entry("defaults").or_insert_with(|| json!({}));
            if let Some(defaults) = defaults.as_object_mut() {
                for (key, entry) in moved {
                    defaults.entry(key).or_insert(entry);
                }
            }
        }
        report.highlights.push(format!(
            "原全局路由功能现在默认关闭，原开启项搬入 defaults：{}；请确认哪些路由要开",
            report.enabled_features.join("、")
        ));
    }
}

fn clean_injections(value: &mut Value, report: &mut MigrationReport) {
    let Some(injections) = value.get_mut("promptInjection") else {
        return;
    };
    for section in ["defaults", "texts"] {
        if let Some(items) = injections.get_mut(section).and_then(Value::as_object_mut) {
            clean_injection_items(
                items,
                &format!("runtimeConfig.promptInjection.{section}"),
                section == "texts",
                report,
            );
        }
    }
    if let Some(paths) = injections
        .get_mut("pathOverrides")
        .and_then(Value::as_object_mut)
    {
        for (prefix, items) in paths {
            if let Some(items) = items.as_object_mut() {
                clean_injection_items(
                    items,
                    &format!("runtimeConfig.promptInjection.pathOverrides[{prefix}]"),
                    false,
                    report,
                );
            }
        }
    }
}

fn clean_injection_items(
    items: &mut Map<String, Value>,
    path: &str,
    text: bool,
    report: &mut MigrationReport,
) {
    let supported = [
        "taskQuality",
        "languageConstraint",
        "custom",
        "identityPrompt",
    ];
    let keys = items.keys().cloned().collect::<Vec<_>>();
    for key in keys {
        if !supported.contains(&key.as_str()) {
            let old = items.remove(&key).unwrap();
            report.discarded(&format!("{path}.{key}"), &old);
            continue;
        }
        if !text
            && let Some(fields) = items.get_mut(&key).and_then(Value::as_object_mut)
            && let Some(old) = fields.remove("externalPool")
        {
            if old == false && fields.get("enabled") == Some(&json!(true)) {
                report.local_injections.push(format!("{path}.{key}"));
                report.highlights.push(format!(
                    "{path}.{key}: 原 externalPool=false 且 enabled=true，导入后会对账号生效"
                ));
            }
            report.discarded(&format!("{path}.{key}.externalPool"), &old);
        }
    }
}

fn rename_cache(value: &mut Value, path: &str, report: &mut MigrationReport) {
    match value {
        Value::Object(fields) => {
            for (old, new) in [
                ("kiroRsTool", "prefixTool"),
                ("kiroRsToolPlus", "prefixToolPlus"),
            ] {
                if let Some(entry) = fields.remove(old) {
                    fields.insert(new.into(), entry);
                    report.moved(&format!("{path}.{old}"), &format!("{path}.{new}"));
                }
            }
            if let Some(entry) = fields.remove("cachePoint") {
                report.discarded(&format!("{path}.cachePoint"), &entry);
            }
            if let Some(entry) = fields.remove("maxEntriesPerCredential") {
                report.moved(
                    &format!("{path}.maxEntriesPerCredential"),
                    &format!("{path}.maxEntriesPerAccount"),
                );
                fields.insert("maxEntriesPerAccount".into(), entry);
            }
            if path.ends_with("cacheTtl") {
                if let Some(entry) = fields.remove("externalPool") {
                    report.moved(&format!("{path}.externalPool"), &format!("{path}.account"));
                    fields.insert("account".into(), entry);
                }
                if let Some(entry) = fields.remove("local") {
                    report.discarded(&format!("{path}.local"), &entry);
                }
            }
            for (key, entry) in fields {
                let child = format!("{path}.{key}");
                if key == "cacheType" {
                    let replacement = match entry.as_str() {
                        Some("kiro_rs_tool") => Some("prefix_tool"),
                        Some("kiro_rs_tool_plus") => Some("prefix_tool_plus"),
                        _ => None,
                    };
                    if let Some(new) = replacement {
                        report.moved(&format!("{child}={entry}"), new);
                        *entry = json!(new);
                    }
                } else {
                    rename_cache(entry, &child, report);
                }
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter_mut().enumerate() {
                rename_cache(item, &format!("{path}[{i}]"), report);
            }
        }
        _ => {}
    }
}

fn clean_legacy_features(value: &mut Value, report: &mut MigrationReport) {
    let Some(features) = value
        .get_mut("routeFeatures")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let allowed =
        serde_json::to_value(crate::model::prompt_injection::RouteFeatureSet::default()).unwrap();
    fn clean(
        fields: &mut Map<String, Value>,
        path: &str,
        allowed: &Value,
        report: &mut MigrationReport,
    ) {
        let keys = fields
            .keys()
            .filter(|key| allowed.get(*key).is_none())
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            let entry = fields.remove(&key).unwrap();
            report.discarded(&format!("{path}.{key}"), &entry);
        }
    }
    if let Some(defaults) = features.get_mut("defaults").and_then(Value::as_object_mut) {
        clean(
            defaults,
            "runtimeConfig.routeFeatures.defaults",
            &allowed,
            report,
        );
    }
    if let Some(paths) = features
        .get_mut("pathOverrides")
        .and_then(Value::as_object_mut)
    {
        for (path, patch) in paths {
            if let Some(fields) = patch.as_object_mut() {
                clean(
                    fields,
                    &format!("runtimeConfig.routeFeatures.pathOverrides[{path}]"),
                    &allowed,
                    report,
                );
            }
        }
    }
}

pub(super) fn validate_config(config: &Config) -> anyhow::Result<()> {
    let check = || -> Result<(), String> {
        config.request_admission.validate()?;
        config.route_features.validate()?;
        config.prompt_injection.validate()?;
        config.reported_usage.validate()?;
        config.prompt_cache_creation_control.validate()?;
        config
            .cache_policy
            .validate(config.legacy_cache_route_policy_default())?;
        crate::admin::service::validate_accounts_config(&config.accounts)?;
        config.routes.validate_with_config(config)?;
        config.validate_redis_fault_domains()?;
        Ok(())
    };
    check().map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        (1..=10).contains(&config.inference_upstream_max_attempts),
        "inferenceUpstreamMaxAttempts 必须在 1 到 10 之间"
    );
    anyhow::ensure!(
        config.stream_pre_output_hold_secs <= crate::model::config::STREAM_PRE_OUTPUT_HOLD_MAX_SECS,
        "streamPreOutputHoldSecs 超限"
    );
    anyhow::ensure!(
        (crate::model::config::MIN_AUDIT_LOG_RETENTION_DAYS
            ..=crate::model::config::MAX_AUDIT_LOG_RETENTION_DAYS)
            .contains(&config.audit_log_retention_days),
        "auditLogRetentionDays 超限"
    );
    anyhow::ensure!(
        (crate::model::config::MIN_USAGE_RECORD_RETENTION_DAYS
            ..=crate::model::config::MAX_USAGE_RECORD_RETENTION_DAYS)
            .contains(&config.usage_record_retention_days),
        "usageRecordRetentionDays 超限"
    );
    anyhow::ensure!(
        config.high_cache_threshold >= 0
            && (0.0..=0.99).contains(&config.prompt_cache_target_read_ratio)
            && (1.0..=3.0).contains(&config.prompt_cache_token_scale),
        "缓存基础参数超限"
    );
    anyhow::ensure!(
        config.prompt_cache_max_simulated_input_tokens >= 0
            && config.prompt_cache_cap_jitter_min_tokens >= 0
            && config.prompt_cache_cap_jitter_max_tokens
                >= config.prompt_cache_cap_jitter_min_tokens
            && config.prompt_cache_scale_min_input_tokens >= 0,
        "缓存 token 边界无效"
    );
    anyhow::ensure!(
        config.prompt_cache_entry_ttl_secs > 0
            && (config.prompt_cache_max_entries_global == 0
                || config.prompt_cache_max_entries_per_account
                    <= config.prompt_cache_max_entries_global),
        "缓存容量或 TTL 无效"
    );
    Ok(())
}

fn migrate_scenarios(rows: &[Value], report: &mut MigrationReport) -> Vec<Value> {
    check_natural_keys(rows, "name", "routeScenarios", report);
    rows.iter()
        .map(|row| {
            let mut row = row.clone();
            if !row["content"].is_object() {
                report
                    .errors
                    .push("routeScenarios[].content 必须为对象".into());
            }
            let name = row_name(row.get("name")).to_string();
            if let Some(content) = row.get_mut("content").and_then(Value::as_object_mut) {
                for key in ["local", "localAccounts"] {
                    if let Some(entry) = content.remove(key) {
                        report
                            .discarded(&format!("routeScenarios[{}].content.{key}", name), &entry);
                    }
                }
            }
            report.moved(
                &format!("routeScenarios[{}]", row_name(row.get("name"))),
                "route_scenarios（name 自然键）",
            );
            row
        })
        .collect()
}

fn row_name(value: Option<&Value>) -> &str {
    value.and_then(Value::as_str).unwrap_or("<缺失>")
}

fn check_natural_keys(rows: &[Value], key: &str, label: &str, report: &mut MigrationReport) {
    let mut keys = BTreeSet::new();
    for row in rows {
        match row.get(key).and_then(Value::as_str) {
            Some(name) if !name.trim().is_empty() && keys.insert(name.to_string()) => {}
            _ => report
                .errors
                .push(format!("{label} 的自然键 {key} 缺失、为空或重复")),
        }
    }
}

fn migrate_models(rows: &[Value], capabilities: bool, report: &mut MigrationReport) -> Vec<Value> {
    let label = if capabilities {
        "modelCapabilities"
    } else {
        "modelPricing"
    };
    check_natural_keys(rows, "model", label, report);
    rows.iter()
        .map(|row| {
            let mut row = row.clone();
            if capabilities {
                if row["source"] != "manual" {
                    report.errors.push(format!("{label} 仅接受 source=manual"));
                }
                if let Some(fields) = row.as_object_mut() {
                    fields.insert("listed".into(), json!(true));
                    let removed = fields
                        .keys()
                        .filter(|key| key.starts_with("reasoning_cohort_"))
                        .cloned()
                        .collect::<Vec<_>>();
                    for key in removed {
                        let entry = fields.remove(&key).unwrap();
                        report.discarded(&format!("{label}.{key}"), &entry);
                    }
                }
            }
            if let Err(error) = validate_model_row(&row, capabilities) {
                report
                    .errors
                    .push(format!("{label}[{}]：{error}", row_name(row.get("model"))));
            }
            report.moved(
                &format!("{label}[{}]", row_name(row.get("model"))),
                if capabilities {
                    "model_capabilities（model 自然键，listed=true）"
                } else {
                    "model_pricing（model 自然键）"
                },
            );
            row
        })
        .collect()
}

fn validate_model_row(row: &Value, capabilities: bool) -> anyhow::Result<()> {
    if capabilities {
        let fields = row.as_object().context("模型必须为对象")?;
        let camel = Value::Object(
            fields
                .iter()
                .map(|(key, value)| (camel_case(key), value.clone()))
                .collect(),
        );
        serde_json::from_value::<crate::anthropic::model_capabilities::ModelCapabilityItem>(camel)?;
    } else {
        for key in [
            "input_cost_per_token",
            "output_cost_per_token",
            "cache_creation_input_token_cost",
            "cache_read_input_token_cost",
        ] {
            let price = row[key]
                .as_f64()
                .with_context(|| format!("缺少或无效的 {key}"))?;
            anyhow::ensure!(
                price.is_finite() && price >= 0.0,
                "{key} 必须为非负有限数值"
            );
        }
        if let Some(price) = row.get("cache_creation_1h_input_token_cost") {
            anyhow::ensure!(
                price.as_f64().is_some_and(|p| p.is_finite() && p >= 0.0),
                "1h 缓存价格无效"
            );
        }
        for key in ["source", "source_url"] {
            anyhow::ensure!(row[key].is_string(), "{key} 必须为字符串");
        }
    }
    Ok(())
}
