use super::*;
use crate::admin::middleware::AdminState;
use crate::model::routes::{RoutesConfig, normalize_route_prefix};
use axum::{
    Json,
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

fn masked_proxy(proxy: Option<String>) -> Option<String> {
    proxy.map(|proxy| {
        let mut masked = proxy.clone();
        if let Ok(mut url) = url::Url::parse(&proxy) {
            if !url.username().is_empty() {
                let _ = url.set_username("***");
            }
            if url.password().is_some() {
                let _ = url.set_password(Some("***"));
            }
            masked = url.to_string();
        }
        masked
    })
}

pub(super) fn mask_account(mut account: Account) -> Account {
    account.proxy_url = masked_proxy(account.proxy_url);
    account
}

fn invalid(error: impl std::fmt::Display) -> AdminServiceError {
    AdminServiceError::InvalidCredential(error.to_string())
}
fn internal(error: impl std::fmt::Display) -> AdminServiceError {
    AdminServiceError::InternalError(error.to_string())
}
fn respond<T: Serialize>(result: Result<T, AdminServiceError>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => (error.status_code(), Json(error.into_response())).into_response(),
    }
}

fn normalize_routes(
    mut routes: RoutesConfig,
    config: &Config,
) -> Result<RoutesConfig, AdminServiceError> {
    for route in &mut routes.routes {
        route.prefix = normalize_route_prefix(&route.prefix);
        route.name = route.name.trim().to_string();
        if route
            .prefix
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
        {
            return Err(invalid("路由前缀不能包含空白或控制字符"));
        }
        for rule in [&mut route.account_rule, &mut route.model_rule] {
            rule.rules = rule
                .rules
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
        }
        if route
            .model_rule
            .rules
            .iter()
            .any(|s| s.strip_suffix('*').unwrap_or(s).contains('*'))
        {
            return Err(invalid("模型通配符只允许出现在末尾"));
        }
    }
    let routes = routes.assign_route_ids();
    routes
        .validate_with_config(config)
        .map_err(AdminServiceError::InvalidRequest)?;
    Ok(routes)
}

fn cache_value(resolved: &crate::model::config::ResolvedCacheRoutePolicy) -> Value {
    let p = &resolved.policy;
    json!({"namespace": resolved.namespace, "policy": {
        "cacheType": p.cache_type, "simulation": p.simulation,
        "creationControl": p.creation_control, "reportedUsage": p.reported_usage,
        "bounds": p.bounds, "prefixTool": p.prefix_tool, "stableSegment": p.stable_segment,
        "hitShaping": p.hit_shaping, "coldRequestGuard": p.cold_request_guard,
        "prefixToolPlus": p.prefix_tool_plus, "cacheTtl": p.cache_ttl
    }})
}
fn leaf_sources(
    value: &Value,
    explicit: &Value,
    template: &Value,
    path: &str,
    sources: &mut BTreeMap<String, String>,
) {
    if let Some(fields) = value.as_object() {
        for (key, value) in fields {
            let path = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            leaf_sources(value, &explicit[key], &template[key], &path, sources);
        }
    } else {
        sources.insert(
            path.to_string(),
            if !explicit.is_null() {
                "route"
            } else if !template.is_null() {
                "global_template"
            } else {
                "builtin_default"
            }
            .to_string(),
        );
    }
}
fn merge_source_template(target: &mut Value, fallback: &Value) {
    if let (Some(target), Some(fallback)) = (target.as_object_mut(), fallback.as_object()) {
        for (key, value) in fallback {
            if target.get(key).is_none_or(Value::is_null) {
                target.insert(key.clone(), value.clone());
            } else if let Some(current) = target.get_mut(key) {
                merge_source_template(current, value);
            }
        }
    }
}
fn effective_route(config: &Config, prefix: &str) -> Result<Value, AdminServiceError> {
    let prefix = normalize_route_prefix(prefix);
    let compiled = config.routes.compile_with_config(config);
    let route = compiled
        .routes
        .iter()
        .find(|r| r.config.prefix == prefix)
        .ok_or_else(|| invalid(format!("路由不存在: {prefix}")))?;
    let cache = cache_value(&route.cache_policy);
    let templates = serde_json::to_value(&config.cache_policy).map_err(internal)?;
    let strategy = match route.cache_policy.policy.cache_type {
        crate::model::config::PromptCacheStrategyType::NoCache => "default",
        crate::model::config::PromptCacheStrategyType::CurrentHighCache => "currentHighCache",
        crate::model::config::PromptCacheStrategyType::PrefixTool => "prefixTool",
        crate::model::config::PromptCacheStrategyType::StableSegmentCache => "stableSegmentCache",
        crate::model::config::PromptCacheStrategyType::PrefixToolPlus => "prefixToolPlus",
        crate::model::config::PromptCacheStrategyType::BoundedHighCache => "boundedHighCache",
    };
    let mut explicit = serde_json::to_value(&route.config.cache_policy).map_err(internal)?;
    if explicit["reportedUsage"].is_null() {
        explicit["reportedUsage"] = json!(route.config.reported_usage);
    }
    let mut template = templates[strategy].clone();
    if !template.is_object() {
        template = json!({});
    }
    if strategy == "currentHighCache" {
        merge_source_template(&mut template, &templates["default"]);
    }
    fn global_differences(current: &Value, builtin: &Value) -> Value {
        if let Some(fields) = current.as_object() {
            Value::Object(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), global_differences(value, &builtin[key])))
                    .collect(),
            )
        } else if current != builtin {
            current.clone()
        } else {
            Value::Null
        }
    }
    let global_base = cache_value(&crate::model::config::ResolvedCacheRoutePolicy {
        policy: config.legacy_cache_route_policy_default(),
        namespace: None,
    });
    let builtin_base = cache_value(&crate::model::config::ResolvedCacheRoutePolicy {
        policy: Config::default().legacy_cache_route_policy_default(),
        namespace: None,
    });
    merge_source_template(
        &mut template,
        &global_differences(&global_base["policy"], &builtin_base["policy"]),
    );
    let mut sources = BTreeMap::new();
    leaf_sources(
        &cache["policy"],
        &explicit,
        &template,
        "cachePolicy.policy",
        &mut sources,
    );
    sources.insert(
        "cachePolicy.namespace".into(),
        if route.config.cache_policy.route_namespace.is_some() {
            "route"
        } else {
            "builtin_default"
        }
        .into(),
    );
    let features = json!(route.features);
    let feature_overrides = json!(route.config.features);
    let global_features = json!(config.route_features.defaults);
    // null 是功能显式关闭，也属于路由层。
    for (name, value) in features.as_object().unwrap() {
        let layer = if feature_overrides.as_object().unwrap().contains_key(name)
            && (feature_overrides[name] != Value::Null
                || matches!(
                    name.as_str(),
                    "missingMaxTokens"
                        | "reasoningCompat"
                        | "payloadGuard"
                        | "payloadShaping"
                        | "imageProcessing"
                )) {
            "route"
        } else if global_features[name]
            != json!(crate::model::prompt_injection::RouteFeatureSet::default())[name]
        {
            "global_template"
        } else {
            "builtin_default"
        };
        let mut leaves = BTreeMap::new();
        leaf_sources(
            value,
            &Value::Null,
            &Value::Null,
            &format!("features.{name}"),
            &mut leaves,
        );
        for path in leaves.keys() {
            sources.insert(path.clone(), layer.into());
        }
    }
    let mut injection_items = serde_json::Map::new();
    for item in route.injections.items() {
        let name = serde_json::to_value(item.id)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        let layer = if route
            .config
            .prompt_injection
            .get(&item.id)
            .is_some_and(|o| o.enabled.is_some())
        {
            "route"
        } else if config.prompt_injection.defaults.contains_key(&item.id) {
            "global_template"
        } else {
            "builtin_default"
        };
        let value = json!({"allowed":item.allowed,"blocked":item.blocked});
        for field in ["allowed", "blocked"] {
            sources.insert(format!("injections.items.{name}.{field}"), layer.into());
        }
        injection_items.insert(name, value);
    }
    let mut injection_texts = serde_json::Map::new();
    for key in crate::model::prompt_injection::TextKey::ALL {
        let name = key.as_str();
        injection_texts.insert(name.into(), json!(route.injections.text(key)));
        let layer = if config.prompt_injection.texts.contains_key(&key) {
            "global_template"
        } else {
            "builtin_default"
        };
        sources.insert(format!("injections.texts.{name}"), layer.into());
    }
    for name in ["accountRule", "modelRule"] {
        for field in ["mode", "rules"] {
            sources.insert(format!("{name}.{field}"), "route".into());
        }
    }
    Ok(
        json!({"prefix": prefix, "cachePolicy": cache, "features": features,
        "injections": {"items":injection_items,"texts":injection_texts}, "accountRule": route.config.account_rule,
        "modelRule": route.config.model_rule, "sources": sources}),
    )
}

impl AdminService {
    pub fn get_routes(&self) -> RoutesConfig {
        self.runtime.config().routes
    }
    pub fn update_routes(&self, routes: RoutesConfig) -> Result<RoutesConfig, AdminServiceError> {
        let routes = normalize_routes(routes, &self.runtime.config())?;
        self.runtime
            .update(|config| config.routes = routes.clone())
            .map_err(invalid)?;
        self.audit(
            "update_routes",
            "routes",
            None,
            true,
            None,
            json!({"prefixes": routes.routes.iter().map(|r| &r.prefix).collect::<Vec<_>>()}),
        );
        Ok(routes)
    }
    pub async fn account_effective(&self, id: u64) -> Result<Value, AdminServiceError> {
        let account = self
            .postgres_store
            .get_account(id, false)
            .await
            .map_err(internal)?
            .ok_or(AdminServiceError::NotFound { id })?;
        let config = self.runtime.config();
        let effective = account.overrides.merge(&config.accounts);
        let mut parameters = json!(effective);
        let mut sources: BTreeMap<String, String> = parameters
            .as_object()
            .unwrap()
            .keys()
            .map(|k| (k.clone(), "global".into()))
            .collect();
        for (key, value) in json!(account.overrides).as_object().unwrap() {
            if !value.is_null() {
                sources.insert(key.clone(), "account".into());
            }
        }
        if let Some(mode) = account.stream_response_mode {
            parameters["streamResponseMode"] = json!(mode);
            sources.insert("streamResponseMode".into(), "account".into());
        }
        if account.auto_disable_policy != crate::account::AutoDisablePolicy::Inherit {
            parameters["autoDisableEnabled"] =
                json!(account.auto_disable_policy == crate::account::AutoDisablePolicy::Enabled);
            sources.insert("autoDisableEnabled".into(), "account".into());
        }
        if account.pre_output_stream_retry_mode != crate::account::StreamRetryMode::Inherit {
            parameters["streamPreOutputRetryEnabled"] = json!(
                account.pre_output_stream_retry_mode != crate::account::StreamRetryMode::Disabled
            );
            sources.insert("streamPreOutputRetryEnabled".into(), "account".into());
        }
        for (key, value) in [
            ("priority", json!(account.priority)),
            (
                "maxConcurrentRequests",
                json!(account.max_concurrent_requests),
            ),
            ("costMultiplier", json!(account.cost_multiplier)),
            (
                "proxyUrl",
                json!(masked_proxy(
                    account.proxy_url.clone().or(config.proxy_url.clone())
                )),
            ),
        ] {
            parameters[key] = value;
            sources.insert(
                key.into(),
                if key == "proxyUrl" && account.proxy_url.is_none() {
                    "global"
                } else {
                    "account"
                }
                .into(),
            );
        }
        let retry_mode =
            if account.pre_output_stream_retry_mode == crate::account::StreamRetryMode::Inherit {
                if effective.stream_pre_output_retry_enabled {
                    crate::account::StreamRetryMode::Enabled
                } else {
                    crate::account::StreamRetryMode::Disabled
                }
            } else {
                account.pre_output_stream_retry_mode
            };
        parameters["preOutputStreamRetryMode"] = json!(retry_mode);
        sources.insert(
            "preOutputStreamRetryMode".into(),
            if account.pre_output_stream_retry_mode == crate::account::StreamRetryMode::Inherit {
                "global"
            } else {
                "account"
            }
            .into(),
        );
        parameters["tlsBackend"] = json!(config.tls_backend);
        sources.insert("tlsBackend".into(), "global".into());
        for (key, value) in [
            ("authType", json!(account.auth_type)),
            ("headerProfile", json!(account.header_profile)),
            ("headerOverrides", json!(account.header_overrides)),
            ("wireProfile", json!(account.wire_profile)),
            ("tlsProfile", json!(account.tls_profile)),
            ("baseUrl", json!(account.base_url)),
            ("appendBetaQuery", json!(account.append_beta_query)),
            ("usageProjectionMode", json!(account.usage_projection_mode)),
        ] {
            parameters[key] = value;
            sources.insert(key.into(), "account".into());
        }
        let mut field_sources = BTreeMap::new();
        for (key, value) in parameters.as_object().unwrap() {
            let mut leaves = BTreeMap::new();
            leaf_sources(value, &Value::Null, &Value::Null, key, &mut leaves);
            for path in leaves.keys() {
                field_sources.insert(path.clone(), sources[key].clone());
            }
        }
        Ok(json!({"id": id, "parameters": parameters, "sources": field_sources}))
    }
    pub async fn set_model_listed(
        &self,
        model: String,
        listed: bool,
    ) -> Result<ModelCapabilityItem, AdminServiceError> {
        let model = normalize_model_id(&model);
        let mut status = self.model_capabilities.status();
        let item = status
            .models
            .iter_mut()
            .find(|item| item.model == model)
            .ok_or_else(|| invalid("模型不存在"))?;
        item.listed = listed;
        let item = item.clone();
        // 复用现有模型保存事务，保留该模型的来源与价格。
        self.postgres_store
            .save_manual_model(&item, None, false)
            .await
            .map_err(internal)?;
        self.model_capabilities.load_persisted_status(status);
        self.audit(
            "set_model_listed",
            "model_capabilities",
            Some(model),
            true,
            None,
            json!({"listed": listed}),
        );
        Ok(item)
    }
}

#[derive(Deserialize)]
pub struct PrefixQuery {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub prefix: Option<String>,
}
#[derive(Deserialize)]
pub struct ListedRequest {
    pub listed: bool,
}
pub async fn routes(State(state): State<AdminState>) -> Response {
    Json(state.service.get_routes()).into_response()
}
pub async fn put_routes(
    State(state): State<AdminState>,
    Json(routes): Json<RoutesConfig>,
) -> Response {
    respond(state.service.update_routes(routes))
}
pub async fn route_effective(
    State(state): State<AdminState>,
    Query(query): Query<PrefixQuery>,
) -> Response {
    let config = state.service.runtime.config();
    // 优先按 id 定位；没有 id 时沿用前缀（兼容旧调用）。
    let prefix = match query.id {
        Some(id) => match config.routes.route_by_id(id) {
            Some(route) => route.prefix.clone(),
            None => return respond::<Value>(Err(AdminServiceError::NotFound { id })),
        },
        None => query.prefix.clone().unwrap_or_default(),
    };
    respond(effective_route(&config, &prefix))
}
pub async fn account_effective(State(state): State<AdminState>, Path(id): Path<u64>) -> Response {
    respond(state.service.account_effective(id).await)
}
pub async fn model_listed(
    State(state): State<AdminState>,
    Path(model): Path<String>,
    Json(body): Json<ListedRequest>,
) -> Response {
    respond(state.service.set_model_listed(model, body.listed).await)
}

#[path = "config_transfer.rs"]
mod config_transfer;
pub(crate) use config_transfer::publish_runtime_change;
pub use config_transfer::{export_config, import_config};

pub async fn usage_overview(
    State(state): State<AdminState>,
    Query(params): Query<crate::admin::handlers::UsageOverviewQueryParams>,
) -> Response {
    // 全部字段来自已物化的汇总表（走 usage 独立连接池、查询闸门与 2 秒缓存），
    // 不再每 30 秒在主业务池上对最长 90 天的 usage_records JSONB 做全表聚合。
    let result = state
        .service
        .get_usage_dashboard_overview(
            params.timezone,
            params.range,
            params.from,
            params.to,
            params.granularity,
            params.top_n,
        )
        .map(|overview| {
            json!({"generatedAt":overview.generated_at,"timezone":overview.timezone,"range":overview.range,
            "totals":overview.totals.all,"billing":overview.external,"series":overview.series.iter().map(|point| json!({"bucketStart":point.bucket_start,"label":point.label,"metrics":point.all})).collect::<Vec<_>>(),
            "topModels":overview.top_models,"topAccounts":overview.top_accounts,"topKeys":overview.top_keys,
            "topPaths":overview.top_paths,"topErrors":overview.top_errors,
            "accountCostUsd":overview.external.account_cost_usd,"rawTokens":overview.external.raw_tokens})
        });
    respond(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admin_proxy_masks_both_global_and_account_credentials() {
        assert_eq!(
            masked_proxy(Some("http://alice:secret@127.0.0.1:3128".into())).unwrap(),
            "http://***:***@127.0.0.1:3128/"
        );
        assert_eq!(masked_proxy(None), None);
        let missing: UpdateRuntimeConfigRequest = serde_json::from_value(json!({})).unwrap();
        let clear: UpdateRuntimeConfigRequest =
            serde_json::from_value(json!({"proxyUrl":null})).unwrap();
        assert_eq!(missing.proxy_url, None);
        assert_eq!(clear.proxy_url, Some(None));
    }
    #[test]
    fn admin_routes_normalize_validate_and_preserve_disabled_routes() {
        let config = Config::default();
        let mut routes = config.routes.clone();
        routes.route_mut(" /dfcache/team/// ").enabled = false;
        let normalized = normalize_routes(routes, &config).unwrap();
        assert!(
            normalized
                .routes
                .iter()
                .any(|r| r.prefix == "/dfcache/team" && !r.enabled)
        );
        // 根级自定义前缀会和内置入口、/api、/ui 抢路径，管理端保存必须拒绝。
        let mut rooted = normalized.clone();
        rooted
            .routes
            .push(crate::model::routes::RouteConfig::new("/team"));
        assert!(normalize_routes(rooted, &config).is_err());
        let mut duplicate = normalized.clone();
        duplicate
            .routes
            .push(crate::model::routes::RouteConfig::new("cc/"));
        assert!(normalize_routes(duplicate, &config).is_err());
        let mut missing = normalized;
        missing.routes.retain(|r| r.prefix != "/cc");
        assert!(normalize_routes(missing, &config).is_err());
    }
    #[test]
    fn admin_routes_effective_marks_explicit_global_and_builtin_sources() {
        let mut config = Config::default();
        config.route_features.defaults.history_sanitize = true;
        config.routes.route_mut("/cc").features.history_sanitize = Some(false);
        let value = effective_route(&config, "/cc/").unwrap();
        assert_eq!(value["sources"]["features.historySanitize"], "route");
        assert_eq!(
            value["sources"]["features.thinkingModelName"],
            "builtin_default"
        );
        let value = effective_route(&config, "/na").unwrap();
        assert_eq!(
            value["sources"]["features.historySanitize"],
            "global_template"
        );
        assert_eq!(value["features"]["historySanitize"], true);
        assert!(effective_route(&config, "/missing").is_err());
    }
}
