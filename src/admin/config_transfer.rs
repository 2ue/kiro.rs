use super::*;

const REDACTED: &str = "<redacted>";
const MACHINE_FIELDS: &[&str] = &["postgres", "redis", "observabilityRedis", "host", "port"];
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigPackage {
    format_version: u32,
    exported_at: String,
    accounts: Vec<Value>,
    runtime_config: Value,
    route_scenarios: Vec<Value>,
    model_pricing: Vec<Value>,
    manual_models: Vec<Value>,
    request_api_keys: Vec<RequestApiKeyPolicy>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TransferChange {
    kind: String,
    natural_key: String,
    action: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TransferReport {
    dry_run: bool,
    unchanged: usize,
    changes: Vec<TransferChange>,
}

fn camel_case(key: &str) -> String {
    let mut parts = key.split('_');
    let mut result = parts.next().unwrap_or_default().to_string();
    for part in parts {
        let mut chars = part.chars();
        if let Some(ch) = chars.next() {
            result.extend(ch.to_uppercase());
            result.extend(chars);
        }
    }
    result
}
fn snake_case(key: &str) -> String {
    let mut result = String::new();
    let mut previous_digit = false;
    for ch in key.chars() {
        if ch.is_ascii_digit() && !previous_digit {
            result.push('_');
        }
        previous_digit = ch.is_ascii_digit();
        if ch.is_uppercase() {
            result.push('_');
            result.extend(ch.to_lowercase());
        } else {
            result.push(ch);
        }
    }
    result
}
fn rename_fields(value: &Value, rename: fn(&str) -> String) -> Value {
    Value::Object(
        value
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (rename(k), v.clone()))
            .collect(),
    )
}
fn strip_metadata(value: &mut Value) {
    if let Some(fields) = value.as_object_mut() {
        for name in [
            "revision",
            "createdAt",
            "updatedAt",
            "maskedApiKey",
            "autoDisabled",
            "autoDisabledReason",
            "autoDisabledAt",
            "autoDisabledUntil",
            "autoDisabledLastError",
            "deletedAt",
            "lastSyncedAt",
            "lastError",
        ] {
            fields.remove(name);
        }
    }
}
fn contains_redacted(value: &Value) -> bool {
    match value {
        Value::String(s) => s == REDACTED,
        Value::Array(items) => items.iter().any(contains_redacted),
        Value::Object(fields) => fields.values().any(contains_redacted),
        _ => false,
    }
}
fn package_from_rows(
    rows: Value,
    include_secrets: bool,
) -> Result<ConfigPackage, AdminServiceError> {
    let config: Config = serde_json::from_value(rows["runtimeConfig"].clone()).map_err(invalid)?;
    let mut runtime_config = serde_json::to_value(&config).map_err(internal)?;
    for key in MACHINE_FIELDS
        .iter()
        .copied()
        .chain(["apiKey", "apiKeys", "requestApiKeyPolicies"])
    {
        runtime_config.as_object_mut().unwrap().remove(key);
    }
    let mut request_api_keys: Vec<RequestApiKeyPolicy> = request_api_key_items(
        &config.request_api_keys(),
        &config.request_api_key_policies(),
    )
    .into_iter()
    .map(|key| RequestApiKeyPolicy {
        api_key: key.api_key,
        name: key.name,
        enabled: key.enabled,
        request_admission: key.request_admission,
        usage_limits: key.usage_limits,
        allowed_routes: key.allowed_routes,
    })
    .collect();
    let collection = |name: &str| -> Vec<Value> {
        rows[name]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                let mut value = rename_fields(v, camel_case);
                strip_metadata(&mut value);
                value
            })
            .collect()
    };
    let mut accounts = collection("accounts");
    if !include_secrets {
        for account in &mut accounts {
            account["apiKey"] = json!(REDACTED);
        }
        for key in &mut request_api_keys {
            key.api_key = REDACTED.into();
        }
        if !runtime_config["adminApiKey"].is_null() {
            runtime_config["adminApiKey"] = json!(REDACTED);
        }
    }
    Ok(ConfigPackage {
        format_version: 1,
        exported_at: Utc::now().to_rfc3339(),
        accounts,
        runtime_config,
        route_scenarios: collection("routeScenarios"),
        model_pricing: collection("modelPricing"),
        manual_models: collection("manualModels"),
        request_api_keys,
    })
}
fn comparable_package(package: &ConfigPackage) -> ConfigPackage {
    let mut result = package.clone();
    let names: BTreeMap<String, String> = package
        .accounts
        .iter()
        .filter_map(|item| {
            Some((
                item["id"].as_u64()?.to_string(),
                item["name"].as_str()?.to_string(),
            ))
        })
        .collect();
    fn map_routes(value: &mut Value, names: &BTreeMap<String, String>) {
        if let Some(routes) = value.as_array_mut() {
            for route in routes {
                if let Some(rules) = route["accountRule"]["rules"].as_array_mut() {
                    for rule in rules.iter_mut() {
                        if let Some(name) = rule.as_str().and_then(|id| names.get(id)) {
                            *rule = json!(name);
                        }
                    }
                    rules.sort_by_key(Value::to_string);
                }
            }
        }
    }
    map_routes(&mut result.runtime_config["routes"], &names);
    for scenario in &mut result.route_scenarios {
        map_routes(&mut scenario["content"]["routes"], &names);
        if let Some(accounts) = scenario["content"]["externalAccounts"].as_array_mut() {
            for account in accounts.iter_mut() {
                if let Some(name) = account["id"]
                    .as_u64()
                    .and_then(|id| names.get(&id.to_string()))
                {
                    account["id"] = json!(name);
                }
            }
            accounts.sort_by_key(|a| a["id"].to_string());
        }
    }
    result
}
fn compare_packages(
    current: &ConfigPackage,
    next: &ConfigPackage,
    dry_run: bool,
) -> Result<TransferReport, AdminServiceError> {
    let current = comparable_package(current);
    let next = comparable_package(next);
    let mut report = TransferReport {
        dry_run,
        unchanged: 0,
        changes: Vec::new(),
    };
    for (kind, key, before, after) in [
        (
            "account",
            "name",
            current.accounts.clone(),
            next.accounts.clone(),
        ),
        (
            "routeScenario",
            "name",
            current.route_scenarios.clone(),
            next.route_scenarios.clone(),
        ),
        (
            "modelPricing",
            "model",
            current.model_pricing.clone(),
            next.model_pricing.clone(),
        ),
        (
            "manualModel",
            "model",
            current.manual_models.clone(),
            next.manual_models.clone(),
        ),
        (
            "requestApiKey",
            "name",
            current.request_api_keys.iter().map(|k| json!(k)).collect(),
            next.request_api_keys.iter().map(|k| json!(k)).collect(),
        ),
        (
            "runtimeConfig",
            "name",
            vec![json!({"name":"default", "value":current.runtime_config})],
            vec![json!({"name":"default", "value":next.runtime_config})],
        ),
    ] {
        let index = |items: Vec<Value>| -> Result<BTreeMap<String, Value>, AdminServiceError> {
            let mut map = BTreeMap::new();
            for mut value in items {
                let name = value[key]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| invalid(format!("{kind} 缺少自然键 {key}")))?
                    .to_string();
                // id 是跨机器的引用标识，比较时不参与条目本身的变化判定。
                value.as_object_mut().unwrap().remove("id");
                if map.insert(name.clone(), value).is_some() {
                    return Err(invalid(format!("重复自然键 {kind}: {name}")));
                }
            }
            Ok(map)
        };
        let before = index(before)?;
        let after = index(after)?;
        for name in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
            let action = match (before.get(name), after.get(name)) {
                (None, Some(_)) => "add",
                (Some(_), None) => "delete",
                (Some(a), Some(b)) if a == b => {
                    report.unchanged += 1;
                    continue;
                }
                _ => "overwrite",
            };
            report.changes.push(TransferChange {
                kind: kind.into(),
                natural_key: name.clone(),
                action: action.into(),
            });
        }
    }
    Ok(report)
}
fn validate_package(package: &ConfigPackage, local: &Config) -> Result<Config, AdminServiceError> {
    if package.format_version != 1 {
        return Err(invalid("不支持的 formatVersion"));
    }
    if !package.runtime_config.is_object() {
        return Err(invalid("runtimeConfig 必须为对象"));
    }
    let mut config = package.runtime_config.clone();
    let local_value = json!(local);
    for key in MACHINE_FIELDS {
        config[*key] = local_value[*key].clone();
    }
    config["apiKey"] = Value::Null;
    config["apiKeys"] = json!(
        package
            .request_api_keys
            .iter()
            .map(|k| &k.api_key)
            .collect::<Vec<_>>()
    );
    config["requestApiKeyPolicies"] = json!(package.request_api_keys);
    let mut config: Config = serde_json::from_value(config).map_err(invalid)?;
    normalize_and_validate_runtime_config(&mut config).map_err(invalid)?;
    config.routes = normalize_routes(config.routes.clone(), &config)?;
    if package.request_api_keys.is_empty() {
        return Err(invalid("至少保留一个请求密钥"));
    }
    let mut secrets = BTreeSet::new();
    for key in &package.request_api_keys {
        if key.api_key.trim().len() < 8
            || (key.api_key != REDACTED && !secrets.insert(key.api_key.clone()))
        {
            return Err(invalid("请求密钥为空或重复"));
        }
        if key.allowed_routes.iter().any(|prefix| {
            !config
                .routes
                .routes
                .iter()
                .any(|route| route.prefix == normalize_route_prefix(prefix))
        }) {
            return Err(invalid("请求密钥引用了不存在的路由"));
        }
        validate_key_limits(&key.usage_limits).map_err(invalid)?;
        if let Some(admission) = key.request_admission {
            admission.validate().map_err(invalid)?;
        }
    }
    let mut ids = BTreeSet::new();
    for value in &package.accounts {
        let account: CreateAccountRequest =
            serde_json::from_value(value.clone()).map_err(invalid)?;
        if account.name.trim().is_empty()
            || account.api_key.trim().is_empty()
            || url::Url::parse(&account.base_url).is_err()
        {
            return Err(invalid("账号名称、密钥或 baseUrl 无效"));
        }
        let parsed_url = url::Url::parse(&account.base_url).map_err(invalid)?;
        if !matches!(parsed_url.scheme(), "http" | "https")
            || account.max_concurrent_requests == 0
            || account.route_rules.len() > 200
            || account.route_rules.iter().any(|rule| rule.len() > 256)
        {
            return Err(invalid("账号协议、并发上限或路由规则无效"));
        }
        crate::account::normalize_account_header_overrides(account.header_overrides.clone())
            .map_err(invalid)?;
        crate::account::validate_account_fields(
            account.proxy_url.as_deref(),
            account.cost_multiplier,
            &account.overrides,
        )
        .map_err(invalid)?;
        let id = value["id"]
            .as_u64()
            .ok_or_else(|| invalid("账号缺少用于路由引用的 id"))?;
        if !ids.insert(id) {
            return Err(invalid("账号 id 重复"));
        }
    }
    for route in &config.routes.routes {
        validate_route_references(route, &ids)?;
    }
    for value in &package.route_scenarios {
        let scenario: crate::admin::route_scenarios::SaveRouteScenarioRequest =
            serde_json::from_value(value.clone()).map_err(invalid)?;
        crate::admin::route_scenarios::validate_request(&scenario)?;
        scenario
            .content
            .routes
            .validate_with_config(&config)
            .map_err(invalid)?;
        for route in &scenario.content.routes.routes {
            validate_route_references(route, &ids)?;
        }
        if scenario
            .content
            .external_accounts
            .iter()
            .any(|a| !ids.contains(&a.id))
        {
            return Err(invalid("场景引用了包中不存在的账号"));
        }
    }
    for value in &package.manual_models {
        let model: ModelCapabilityItem = serde_json::from_value(value.clone()).map_err(invalid)?;
        if model.model.trim().is_empty()
            || !matches!(
                model.source.as_deref(),
                Some("manual" | "account_discovery")
            )
            || model.max_input_tokens.is_some_and(|n| n <= 0)
            || model.max_output_tokens.is_some_and(|n| n <= 0)
        {
            return Err(invalid("模型名称、来源或 token 上限无效"));
        }
    }
    for value in &package.model_pricing {
        if value["model"]
            .as_str()
            .is_none_or(|model| model.trim().is_empty())
            || value["source"].as_str().is_none()
            || value["sourceUrl"].as_str().is_none()
        {
            return Err(invalid("模型价格缺少名称或来源"));
        }
        for field in [
            "inputCostPerToken",
            "outputCostPerToken",
            "cacheCreationInputTokenCost",
            "cacheReadInputTokenCost",
            "cacheCreation1hInputTokenCost",
        ] {
            if value[field]
                .as_f64()
                .is_none_or(|price| !price.is_finite() || price < 0.0)
            {
                return Err(invalid(format!("模型价格 {field} 无效")));
            }
        }
    }
    Ok(config)
}
fn validate_route_references(
    route: &crate::model::routes::RouteConfig,
    ids: &BTreeSet<u64>,
) -> Result<(), AdminServiceError> {
    if route
        .account_rule
        .rules
        .iter()
        .any(|r| r != "*" && !r.parse::<u64>().is_ok_and(|id| ids.contains(&id)))
    {
        return Err(invalid("路由引用了包中不存在的账号"));
    }
    Ok(())
}
#[derive(Deserialize)]
pub struct ExportQuery {
    #[serde(default)]
    include_secrets: bool,
}
#[derive(Deserialize)]
pub struct ImportQuery {
    #[serde(default = "default_dry_run")]
    dry_run: bool,
}
fn default_dry_run() -> bool {
    true
}
pub async fn export_config(
    State(state): State<AdminState>,
    Query(query): Query<ExportQuery>,
) -> Response {
    let result = async {
        package_from_rows(
            state
                .service
                .postgres_store
                .export_current_configuration()
                .await
                .map_err(internal)?,
            query.include_secrets,
        )
    }
    .await;
    // 带密钥导出会把账号密钥与请求 Key 明文打进包里，成败都要留痕并记录 includeSecrets。
    let (success, error_message, detail) = match &result {
        Ok(package) => (
            true,
            None,
            json!({
                "includeSecrets": query.include_secrets,
                "accounts": package.accounts.len(),
                "routeScenarios": package.route_scenarios.len(),
                "requestApiKeys": package.request_api_keys.len(),
            }),
        ),
        Err(error) => (
            false,
            Some(error.to_string()),
            json!({ "includeSecrets": query.include_secrets }),
        ),
    };
    state.service.audit(
        "export_config",
        "config",
        None,
        success,
        error_message,
        detail,
    );
    respond(result)
}
pub async fn import_config(
    State(state): State<AdminState>,
    Query(query): Query<ImportQuery>,
    Json(package): Json<ConfigPackage>,
) -> Response {
    let result = async {
        if !query.dry_run && contains_redacted(&json!(package)) {
            return Err(invalid("包含 <redacted> 的配置包只能预览，不能应用"));
        }
        let config = validate_package(&package, &state.service.runtime.config())?;
        let current = package_from_rows(
            state
                .service
                .postgres_store
                .export_current_configuration()
                .await
                .map_err(internal)?,
            !contains_redacted(&json!(package)),
        )?;
        let report = compare_packages(&current, &package, query.dry_run)?;
        if !query.dry_run {
            let previous_models = state.service.model_capabilities.status().models;
            let previous_prices = state.service.pricing_catalog.status().models;
            let rows = |items: &[Value]| {
                items
                    .iter()
                    .map(|v| rename_fields(v, snake_case))
                    .collect::<Vec<_>>()
            };
            state
                .service
                .postgres_store
                .replace_current_configuration_transaction(
                    &config,
                    &rows(&package.accounts),
                    &rows(&package.route_scenarios),
                    &rows(&package.model_pricing),
                    &rows(&package.manual_models),
                )
                .await
                .map_err(internal)?;
            state
                .service
                .runtime
                .reload_from_postgres()
                .map_err(internal)?;
            let config = state.service.runtime.config();
            state.service.refresh_request_api_key_runtime(&config);
            let models = state
                .service
                .postgres_store
                .load_model_capabilities_status()
                .await
                .map_err(internal)?;
            let prices = state
                .service
                .postgres_store
                .load_pricing_status()
                .await
                .map_err(internal)?;
            if let Some(status) = models {
                state
                    .service
                    .model_capabilities
                    .load_persisted_status(status);
            } else {
                for mut model in previous_models {
                    if matches!(
                        model.source.as_deref(),
                        Some("manual" | "account_discovery")
                    ) {
                        model.source = Some("manual".into());
                        state
                            .service
                            .model_capabilities
                            .upsert_manual_model(model.clone());
                        state
                            .service
                            .model_capabilities
                            .delete_manual_model(&model.model);
                    }
                }
            }
            if let Some(status) = prices {
                state.service.pricing_catalog.load_persisted_status(status);
            } else {
                for price in previous_prices {
                    // 空价格包需要清除旧快照；目录现有删除入口仅接受手动来源。
                    let mut pricing = price.pricing;
                    if pricing.input_cost_per_token == 0.0 && pricing.output_cost_per_token == 0.0 {
                        pricing.input_cost_per_token = f64::MIN_POSITIVE;
                    }
                    state
                        .service
                        .pricing_catalog
                        .upsert_manual_price(&price.model, pricing);
                    state
                        .service
                        .pricing_catalog
                        .delete_manual_price(&price.model);
                }
            }
            state
                .service
                .invalidate_admin_cache_pattern("admin_cache:*");
            state.service.audit(
                "import_config",
                "runtime_config",
                None,
                true,
                None,
                json!({"changes": report.changes.len()}),
            );
            if let Err(error) = state
                .service
                .account_manager
                .publish_account_data_changed("config_import", None)
                .await
            {
                tracing::warn!(%error, "发布账号配置变更失败");
            }
            publish_runtime_change(&config).await;
        }
        Ok(report)
    }
    .await;
    respond(result)
}
pub(crate) async fn publish_runtime_change(config: &Config) {
    let result = async {
        let client = redis::Client::open(
            config
                .redis
                .url
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("Redis URL 未配置"))?,
        )?;
        let mut connection = client.get_multiplexed_async_connection().await?;
        redis::cmd("PUBLISH")
            .arg(format!(
                "{}:events:runtime_config_changed",
                config.redis.key_prefix
            ))
            .arg("{\"reason\":\"admin_config_changed\"}")
            .query_async::<i64>(&mut connection)
            .await?;
        Ok::<_, anyhow::Error>(())
    };
    if let Err(error) = tokio::time::timeout(StdDuration::from_secs(5), result)
        .await
        .unwrap_or_else(|error| Err(error.into()))
    {
        tracing::warn!(%error, "发布运行配置变更失败");
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admin_config_transfer_preserves_numeric_pricing_column_names() {
        assert_eq!(
            snake_case("cacheCreation1hInputTokenCost"),
            "cache_creation_1h_input_token_cost"
        );
        assert_eq!(
            camel_case("cache_creation_1h_input_token_cost"),
            "cacheCreation1hInputTokenCost"
        );
    }
    #[test]
    fn admin_config_transfer_compares_account_references_by_name_and_reports_deletions() {
        let mut config = Config::default();
        config.set_request_api_keys(vec!["request-key-123"]);
        config.routes.route_mut("/cc").account_rule.rules = vec!["42".into()];
        let rows = json!({"runtimeConfig":config,"accounts":[{"id":42,"name":"team","base_url":"https://example.com","api_key":"secret","tags":["premium"],"proxy_url":"http://user:pass@localhost:3128","cost_multiplier":0.6,"overrides":{"requestTimeoutSecs":120}}],"routeScenarios":[],"modelPricing":[],"manualModels":[]});
        let package = package_from_rows(rows.clone(), true).unwrap();
        validate_package(&package, &config).unwrap();
        let mut other_machine = package.clone();
        other_machine.accounts[0]["id"] = json!(7);
        other_machine.runtime_config["routes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|route| route["prefix"] == "/cc")
            .unwrap()["accountRule"]["rules"] = json!(["7"]);
        assert!(
            compare_packages(&package, &other_machine, true)
                .unwrap()
                .changes
                .is_empty()
        );
        let redacted = package_from_rows(rows, false).unwrap();
        assert_eq!(redacted.accounts[0]["apiKey"], REDACTED);
        assert_eq!(redacted.request_api_keys[0].api_key, REDACTED);
        let mut removed = package.clone();
        removed.accounts.clear();
        let report = compare_packages(&package, &removed, true).unwrap();
        assert!(report.changes.iter().any(|change| change.kind == "account"
            && change.natural_key == "team"
            && change.action == "delete"));
        assert!(validate_package(&removed, &config).is_err());
        let mut duplicate = package.clone();
        duplicate.accounts.push(duplicate.accounts[0].clone());
        assert!(compare_packages(&package, &duplicate, true).is_err());
    }
    #[test]
    fn admin_config_export_import_dry_run_round_trip_and_redaction() {
        let mut config = Config::default();
        config.set_request_api_keys(vec!["secret-request-key"]);
        config.admin_api_key = Some("secret-admin-key".into());
        let account = crate::account::tests::test_pool("https://example.com/v1", false);
        let account = serde_json::to_value(account).unwrap();
        assert_eq!(account["supportedModels"], json!([]));
        assert_eq!(account["routeRules"], json!([]));
        assert_eq!(account["headerOverrides"], json!({}));
        let rows = json!({"runtimeConfig":config,"accounts":[account],"routeScenarios":[],"modelPricing":[],"manualModels":[]});
        let package = package_from_rows(rows.clone(), true).unwrap();
        let serialized = serde_json::to_vec(&package).unwrap();
        let imported: ConfigPackage = serde_json::from_slice(&serialized).unwrap();
        validate_package(&imported, &config).unwrap();
        let report = compare_packages(&package, &imported, true).unwrap();
        assert!(report.changes.is_empty());
        assert_eq!(report.unchanged, 3);
        assert_eq!(imported.accounts[0]["supportedModels"], json!([]));
        assert_eq!(imported.accounts[0]["routeRules"], json!([]));
        assert_eq!(imported.accounts[0]["headerOverrides"], json!({}));
        let redacted = package_from_rows(rows, false).unwrap();
        assert!(contains_redacted(&json!(redacted)));
        assert!(
            !redacted
                .runtime_config
                .as_object()
                .unwrap()
                .contains_key("postgres")
        );
    }
}
