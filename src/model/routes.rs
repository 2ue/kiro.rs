//! 路由定义、预编译匹配与旧配置迁移。
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::config::{
    CachePolicyConfig, CacheRoutePolicy, CacheRoutePolicyPatch, Config, PromptCacheStrategyType,
    ReportedUsageConfig, ReportedUsageFieldPolicy, ReportedUsagePathPolicy,
    ResolvedCacheRoutePolicy, RouteRuleMode, resolve_cache_policy_for_route, route_policy_allows,
};
use super::prompt_injection::{
    InjectionId, ItemOverride, ResolvedInjections, RouteFeatureOverride, RouteFeatureSet,
};

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct RouteRule {
    #[serde(default)]
    pub mode: RouteRuleMode,
    #[serde(default)]
    pub rules: Vec<String>,
}
impl RouteRule {
    pub fn allows_account(&self, id: u64) -> bool {
        let matched = self
            .rules
            .iter()
            .any(|rule| rule.trim() == "*" || rule.trim() == id.to_string());
        self.allows_match(matched)
    }
    pub fn allows_match(&self, matched: bool) -> bool {
        match self.mode {
            RouteRuleMode::AllowAll => true,
            RouteRuleMode::AllowList => matched,
            RouteRuleMode::DenyList => !matched,
        }
    }
    pub fn allows_model(&self, model: &str) -> bool {
        crate::account::route_model_rule_allows(self, model)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RouteConfig {
    /// 稳定标识。控制台用它拼 URL、做引用；前缀只是入口路径，可能重命名。
    /// 旧配置没有这个字段时为 0，加载与保存时由 `assign_route_ids` 补齐。
    #[serde(default)]
    pub id: u64,
    pub prefix: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub cache_policy: CacheRoutePolicyPatch,
    #[serde(default)]
    pub reported_usage: Option<ReportedUsagePathPolicy>,
    #[serde(default)]
    pub features: RouteFeatureOverride,
    #[serde(default)]
    pub prompt_injection: BTreeMap<InjectionId, ItemOverride>,
    #[serde(default)]
    pub account_rule: RouteRule,
    #[serde(default)]
    pub model_rule: RouteRule,
}
impl RouteConfig {
    pub fn new(prefix: impl Into<String>) -> Self {
        let prefix = prefix.into();
        Self {
            id: 0,
            name: prefix.clone(),
            prefix,
            enabled: true,
            cache_policy: Default::default(),
            reported_usage: None,
            features: Default::default(),
            prompt_injection: Default::default(),
            account_rule: Default::default(),
            model_rule: Default::default(),
        }
    }
}

/// JSON 直接使用 routes 数组，Rust 保留配置容器。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(transparent)]
pub struct RoutesConfig {
    pub routes: Vec<RouteConfig>,
}
impl Default for RoutesConfig {
    fn default() -> Self {
        Self { routes: Vec::new() }.with_builtin_path_defaults()
    }
}

pub const BUILTIN_ROUTE_PREFIXES: [&str; 4] = ["", "/cc", "/ha", "/na"];
/// 自定义路由只能挂在这个专有前缀下，和内置入口、管理端 `/api`、控制台 `/ui`
/// 以及将来新增的内置路径隔离，避免任意前缀污染已有路由。
pub const CUSTOM_ROUTE_PREFIX: &str = "/dfcache/";

pub fn normalize_route_prefix(prefix: &str) -> String {
    let prefix = prefix.trim();
    if prefix.is_empty() || prefix.trim_matches('/').is_empty() {
        return String::new();
    }
    let prefix = format!("/{}", prefix.trim_start_matches('/'))
        .trim_end_matches('/')
        .to_string();
    // 旧根入口覆盖使用 /v1。
    if prefix == "/v1" {
        String::new()
    } else {
        prefix
    }
}

pub fn is_builtin_route_prefix(prefix: &str) -> bool {
    BUILTIN_ROUTE_PREFIXES.contains(&prefix)
}

/// 自定义路由前缀合法性：`/dfcache/{name}`，name 为 1–64 个小写字母、数字、`.`、`_`、`-`。
/// 返回归一化后的前缀；内置前缀原样返回。
pub fn validate_route_prefix(prefix: &str) -> Result<String, String> {
    let normalized = normalize_route_prefix(prefix);
    if is_builtin_route_prefix(&normalized) {
        return Ok(normalized);
    }
    super::config::normalize_defined_cache_route(&normalized).ok_or_else(|| {
        format!(
            "自定义路由前缀必须是 {CUSTOM_ROUTE_PREFIX}{{name}}，name 只允许小写字母、数字、点、下划线或短横线（1–64 个字符）：{prefix}"
        )
    })
}
fn prefix_matches(prefix: &str, endpoint: &str) -> bool {
    prefix.is_empty()
        || endpoint == prefix
        || endpoint
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}
impl RoutesConfig {
    /// 给没有 id 的路由分配 id：取现有最大 id 顺延，保证已有 id 不变。重复 id 视为无效。
    pub fn assign_route_ids(mut self) -> Self {
        let mut next = self.routes.iter().map(|route| route.id).max().unwrap_or(0) + 1;
        for route in &mut self.routes {
            if route.id == 0 {
                route.id = next;
                next += 1;
            }
        }
        self
    }
    pub fn route_by_id(&self, id: u64) -> Option<&RouteConfig> {
        self.routes.iter().find(|route| route.id == id)
    }
    pub fn with_builtin_path_defaults(mut self) -> Self {
        for prefix in BUILTIN_ROUTE_PREFIXES {
            if self
                .routes
                .iter()
                .any(|route| normalize_route_prefix(&route.prefix) == prefix)
            {
                continue;
            }
            let mut route = RouteConfig::new(prefix);
            route.cache_policy.cache_type = Some(if prefix == "/na" {
                PromptCacheStrategyType::NoCache
            } else {
                PromptCacheStrategyType::CurrentHighCache
            });
            route.cache_policy.route_namespace = Some(false);
            if prefix == "/cc" || prefix == "/ha" {
                let mut policy = ReportedUsagePathPolicy {
                    input: ReportedUsageFieldPolicy::sample_input_max(96),
                    ..Default::default()
                };
                if prefix == "/cc" {
                    policy.cache_creation =
                        ReportedUsageFieldPolicy::sample_target_with_multiplier(3_000, 1.2);
                }
                route.reported_usage = Some(policy);
            }
            self.routes.push(route);
        }
        self.assign_route_ids()
    }
    pub fn validate(&self) -> Result<(), String> {
        let mut seen = BTreeSet::new();
        let mut seen_ids = BTreeSet::new();
        for route in &self.routes {
            let prefix = validate_route_prefix(&route.prefix)?;
            if !seen.insert(prefix.clone()) {
                return Err(format!("重复路由前缀: {prefix}"));
            }
            if route.id != 0 && !seen_ids.insert(route.id) {
                return Err(format!("重复路由 id: {}", route.id));
            }
            if route
                .account_rule
                .rules
                .iter()
                .any(|id| id.trim() != "*" && id.trim().parse::<u64>().is_err())
            {
                return Err(format!("路由 {prefix} 的账号规则必须为账号 id 或 *"));
            }
        }
        for prefix in BUILTIN_ROUTE_PREFIXES {
            if !seen.contains(prefix) {
                return Err(format!("内置路由不可删除: {prefix}"));
            }
        }
        Ok(())
    }
    pub fn validate_with_config(&self, config: &Config) -> Result<(), String> {
        self.validate()?;
        for route in &self.routes {
            route.cache_policy.validate(
                &format!("路由 {}", route.prefix),
                config.legacy_cache_route_policy_default(),
            )?;
            if let Some(policy) = &route.reported_usage {
                policy.validate("路由上报策略")?;
            }
            super::prompt_injection::RouteFeaturesConfig {
                defaults: route.features.apply(config.route_features.defaults),
            }
            .validate()?;
        }
        Ok(())
    }
    #[cfg(test)]
    pub fn compile(&self) -> CompiledRoutes {
        self.compile_with_config(&Config::default())
    }
    pub fn compile_with_config(&self, config: &Config) -> CompiledRoutes {
        self.compile_with_policy(
            config,
            config.legacy_cache_route_policy_default(),
            &config.cache_policy,
            &config.reported_usage,
        )
    }
    pub fn compile_with_policy(
        &self,
        config: &Config,
        base: CacheRoutePolicy,
        cache: &CachePolicyConfig,
        reported: &ReportedUsageConfig,
    ) -> CompiledRoutes {
        let mut routes = self
            .clone()
            .with_builtin_path_defaults()
            .routes
            .into_iter()
            .map(|mut route| {
                route.prefix = normalize_route_prefix(&route.prefix);
                let cache_policy =
                    resolve_cache_policy_for_route(base.clone(), reported, cache, &route);
                let features = route.features.apply(config.route_features.defaults);
                let injections = ResolvedInjections::resolve_route(
                    config.compat_profile,
                    &config.prompt_injection,
                    &route.prefix,
                    &route.prompt_injection,
                );
                CompiledRoute {
                    config: route,
                    cache_policy,
                    features,
                    injections,
                }
            })
            .collect::<Vec<_>>();
        routes.sort_by_key(|route| std::cmp::Reverse(route.config.prefix.len()));
        CompiledRoutes { routes }
    }
    #[cfg(test)]
    pub fn route_mut(&mut self, prefix: &str) -> &mut RouteConfig {
        let prefix = normalize_route_prefix(prefix);
        let index = self
            .routes
            .iter()
            .position(|r| normalize_route_prefix(&r.prefix) == prefix)
            .unwrap_or_else(|| {
                self.routes.push(RouteConfig::new(&prefix));
                self.routes.len() - 1
            });
        &mut self.routes[index]
    }
    pub fn match_endpoint(&self, endpoint: &str) -> Option<&RouteConfig> {
        self.routes
            .iter()
            .filter(|route| prefix_matches(&normalize_route_prefix(&route.prefix), endpoint))
            .max_by_key(|route| normalize_route_prefix(&route.prefix).len())
    }
}
#[derive(Clone, Debug)]
pub struct CompiledRoute {
    pub config: RouteConfig,
    pub cache_policy: ResolvedCacheRoutePolicy,
    pub features: RouteFeatureSet,
    pub injections: ResolvedInjections,
}
#[derive(Clone, Debug, Default)]
pub struct CompiledRoutes {
    pub routes: Vec<CompiledRoute>,
}
impl CompiledRoutes {
    pub fn match_endpoint(&self, endpoint: &str) -> Option<&CompiledRoute> {
        self.routes
            .iter()
            .find(|route| prefix_matches(&route.config.prefix, endpoint))
    }
    pub fn endpoint_enabled(&self, endpoint: &str) -> bool {
        let Some((prefix, _)) = endpoint
            .rsplit_once("/v1/")
            .or_else(|| endpoint.strip_suffix("/v1").map(|prefix| (prefix, "")))
        else {
            return false;
        };
        self.match_endpoint(endpoint)
            .is_some_and(|route| route.config.enabled && route.config.prefix == prefix)
    }
}

pub fn migrate_path_overrides_to_routes(value: &mut Value) -> bool {
    let keys = [
        ("cachePolicy", "cachePolicy"),
        ("reportedUsage", "reportedUsage"),
        ("promptInjection", "promptInjection"),
        ("routeFeatures", "features"),
    ];
    let legacy = value.get("definedCacheRoutes").is_some()
        || keys.iter().any(|(key, _)| {
            value
                .get(*key)
                .and_then(|v| v.get("pathOverrides"))
                .is_some()
        })
        || value.pointer("/accounts/accountRouteMode").is_some()
        || value.pointer("/accounts/accountRouteRules").is_some();
    if !legacy {
        return false;
    }
    let builtin = serde_json::to_value(RoutesConfig::default()).expect("serialize builtin routes");
    let mut routes = builtin
        .as_array()
        .unwrap()
        .iter()
        .map(|route| (route["prefix"].as_str().unwrap().to_string(), route.clone()))
        .collect::<BTreeMap<String, Value>>();
    if let Some(existing) = value.get("routes").and_then(Value::as_array) {
        for route in existing {
            let prefix =
                normalize_route_prefix(route.get("prefix").and_then(Value::as_str).unwrap_or(""));
            routes.insert(prefix, route.clone());
        }
    }
    let legacy_reported = value
        .pointer("/reportedUsage/pathOverrides")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let legacy_features = value
        .pointer("/routeFeatures/pathOverrides")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let legacy_injections = value
        .pointer("/promptInjection/pathOverrides")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    // 旧缓存覆盖表的全部键（含 /dfcache、/cc/v1 这类不是入口的片段）都保留为最长前缀匹配的
    // 来源：它们在旧系统里就是"按路径前缀继承"的语义，新路由仍按同样方式继承。
    let legacy_cache_overrides = value
        .pointer("/cachePolicy/pathOverrides")
        .and_then(Value::as_object)
        .map(|overrides| {
            overrides
                .iter()
                .filter(|(_, policy)| policy.as_object().is_some_and(|fields| !fields.is_empty()))
                .map(|(prefix, policy)| (normalize_route_prefix(prefix), policy.clone()))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    for (key, target) in keys {
        if let Some(overrides) = value
            .get_mut(key)
            .and_then(Value::as_object_mut)
            .and_then(|v| v.remove("pathOverrides"))
            .and_then(|v| v.as_object().cloned())
        {
            for (prefix, policy) in overrides {
                // 旧表的键可以是任意路径片段（/cc/v1、/dfcache、/new ...）。只有内置前缀和
                // /dfcache/{name} 才是可访问入口；其余键只作为下方最长前缀匹配的来源，不生成路由。
                let Ok(prefix) = validate_route_prefix(&prefix) else {
                    continue;
                };
                let route = routes
                    .entry(prefix.clone())
                    .or_insert_with(|| json!({"prefix": prefix, "name": prefix, "enabled": true}));
                route[target] = policy;
            }
        }
    }
    if let Some(defined) = value
        .as_object_mut()
        .and_then(|v| v.remove("definedCacheRoutes"))
        .and_then(|v| v.as_array().cloned())
    {
        for name in defined.iter().filter_map(Value::as_str) {
            let candidate = if name.trim().starts_with("/dfcache/") {
                name.trim().to_string()
            } else {
                format!("/dfcache/{}", name.trim().trim_matches('/'))
            };
            if let Some(prefix) = super::config::normalize_defined_cache_route(&candidate) {
                let route = routes
                    .entry(prefix.clone())
                    .or_insert_with(|| json!({"prefix": prefix, "name": prefix, "enabled": true}));
                if route.get("cachePolicy").is_none() {
                    route["cachePolicy"] =
                        json!({"cacheType":"current_high_cache", "routeNamespace":true});
                }
            }
        }
    }
    let mut legacy_cache = legacy_cache_overrides;
    for (prefix, route) in &routes {
        if let Some(policy) = route
            .get("cachePolicy")
            .filter(|policy| policy.is_object() && !policy.as_object().unwrap().is_empty())
        {
            legacy_cache.insert(prefix.clone(), policy.clone());
        }
    }
    for (prefix, route) in &mut routes {
        let endpoint = if prefix.ends_with("/v1") {
            format!("{prefix}/messages")
        } else {
            format!("{prefix}/v1/messages")
        };
        if route.get("cachePolicy").is_none() {
            let policy = legacy_cache
                .iter()
                .filter(|(candidate, _)| {
                    // 旧根缓存覆盖只匹配 /v1，不能继承到任意新入口。
                    let candidate = if candidate.is_empty() {
                        "/v1"
                    } else {
                        candidate.as_str()
                    };
                    prefix_matches(candidate, &endpoint)
                })
                .max_by_key(|(candidate, _)| candidate.len())
                .map(|(_, policy)| policy.clone())
                .unwrap_or_else(|| json!({"cacheType":"no_cache"}));
            route["cachePolicy"] = policy;
        }
        if let Some((_, policy)) = legacy_reported
            .iter()
            .filter(|(candidate, _)| prefix_matches(&normalize_route_prefix(candidate), &endpoint))
            .max_by_key(|(candidate, _)| normalize_route_prefix(candidate).len())
        {
            route["reportedUsage"] = policy.clone();
        }
        for (source, target) in [
            (&legacy_features, "features"),
            (&legacy_injections, "promptInjection"),
        ] {
            let mut matching = source
                .iter()
                .filter(|(candidate, _)| {
                    prefix_matches(&normalize_route_prefix(candidate), &endpoint)
                })
                .collect::<Vec<_>>();
            matching.sort_by_key(|(candidate, _)| normalize_route_prefix(candidate).len());
            let mut merged = serde_json::Map::new();
            for (_, patch) in matching {
                if let Some(fields) = patch.as_object() {
                    for (key, value) in fields {
                        if target == "promptInjection" {
                            if let Some(item) = value.as_object() {
                                merged
                                    .entry(key.clone())
                                    .or_insert_with(|| json!({}))
                                    .as_object_mut()
                                    .unwrap()
                                    .extend(
                                        item.iter()
                                            .filter(|(_, value)| !value.is_null())
                                            .map(|(key, value)| (key.clone(), value.clone())),
                                    );
                            }
                        } else {
                            merged.insert(key.clone(), value.clone());
                        }
                    }
                }
            }
            if !merged.is_empty() {
                route[target] = Value::Object(merged);
            }
        }
    }
    let external = value.get_mut("accounts").and_then(Value::as_object_mut);
    if let Some(external) = external {
        let mode = external
            .remove("accountRouteMode")
            .and_then(|v| serde_json::from_value::<RouteRuleMode>(v).ok())
            .unwrap_or_default();
        let rules = external
            .remove("accountRouteRules")
            .and_then(|v| serde_json::from_value::<Vec<String>>(v).ok())
            .unwrap_or_default();
        for (prefix, route) in &mut routes {
            let endpoint = if prefix.ends_with("/v1") {
                format!("{prefix}/messages")
            } else {
                format!("{prefix}/v1/messages")
            };
            if !route_policy_allows(mode, &rules, &endpoint) {
                route["accountRule"] = json!({"mode":"deny_list", "rules":["*"]});
                tracing::warn!(route = prefix, "迁移的路由账号规则拒绝全部账号");
            }
        }
    }
    value["routes"] = Value::Array(routes.into_values().collect());
    value["runtimeConfigMigrationVersion"] = json!(2);
    true
}

#[cfg(test)]
mod tests {
    use super::super::config::{AccountsConfig, CapacityMode};
    use super::*;
    use crate::account::AccountOverrides;

    #[test]
    fn prefixes_and_longest_match() {
        for (raw, expected) in [
            ("", ""),
            (" / ", ""),
            (" cc/// ", "/cc"),
            ("/dfcache/foo/", "/dfcache/foo"),
            ("custom", "/custom"),
        ] {
            assert_eq!(normalize_route_prefix(raw), expected);
        }
        let mut routes = RoutesConfig::default();
        routes.route_mut("/dfcache/team");
        routes.route_mut("/dfcache/foo");
        let compiled = routes.compile();
        for (endpoint, prefix) in [
            ("/v1/messages", ""),
            ("/unknown", ""),
            ("/cc/v1/messages", "/cc"),
            ("/cc/team/v1/messages", "/cc"),
            ("/cc/teams", "/cc"),
            ("/dfcache/team/v1/messages", "/dfcache/team"),
            ("/dfcache/foo/v1/messages", "/dfcache/foo"),
            ("/dfcache/foobar/v1/messages", ""),
        ] {
            assert_eq!(
                compiled.match_endpoint(endpoint).unwrap().config.prefix,
                prefix
            );
        }
        assert!(!compiled.endpoint_enabled("/dfcache/missing/v1/messages"));
        assert!(!compiled.endpoint_enabled("/missing/v1/messages"));
        assert!(compiled.endpoint_enabled("/dfcache/foo/v1/messages"));
        routes.route_mut("/dfcache/foo").enabled = false;
        assert!(
            !routes
                .compile()
                .endpoint_enabled("/dfcache/foo/v1/messages")
        );
    }

    #[test]
    fn duplicate_prefixes_and_builtin_removal_are_rejected() {
        for builtin in ["", "/cc", "/ha", "/na"] {
            let mut routes = RoutesConfig::default();
            routes.routes.retain(|route| route.prefix != builtin);
            assert!(routes.validate().is_err(), "{builtin}");
            assert!(routes.with_builtin_path_defaults().validate().is_ok());
        }
        for duplicate in [" cc/ ", "/cc", "/v1", "/"] {
            let mut routes = RoutesConfig::default();
            routes.routes.push(RouteConfig::new(duplicate));
            assert!(routes.validate().is_err(), "{duplicate}");
        }
    }

    #[test]
    fn route_ids_are_assigned_once_and_kept_stable() {
        let routes = RoutesConfig::default();
        let ids = routes.routes.iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(ids, vec![1, 2, 3, 4], "内置四条按顺序拿到 1..4");
        let mut routes = routes;
        routes.routes.push(RouteConfig::new("/dfcache/team"));
        let routes = routes.assign_route_ids();
        assert_eq!(routes.route_by_id(5).unwrap().prefix, "/dfcache/team");
        // 删掉中间一条再补，不复用旧 id
        let mut routes = routes;
        routes.routes.retain(|r| r.id != 2);
        routes.routes.push(RouteConfig::new("/dfcache/other"));
        let routes = routes.assign_route_ids();
        assert_eq!(routes.route_by_id(6).unwrap().prefix, "/dfcache/other");
        assert!(routes.route_by_id(2).is_none());
        // 旧 JSON 没有 id 字段也能解析并补齐
        let parsed: RoutesConfig =
            serde_json::from_str(r#"[{"prefix":"/cc"},{"prefix":"/dfcache/x"}]"#).unwrap();
        let parsed = parsed.with_builtin_path_defaults();
        assert!(parsed.routes.iter().all(|r| r.id > 0));
        let mut dup = RoutesConfig::default();
        dup.routes[1].id = dup.routes[0].id;
        assert!(dup.validate().is_err());
    }

    /// 自定义路由只接受 /dfcache/{name}：任意根级前缀会和内置入口、/api、/ui 抢路径，必须拒绝。
    #[test]
    fn custom_prefixes_must_live_under_dfcache() {
        for (raw, expected) in [
            ("/dfcache/team-a", Some("/dfcache/team-a")),
            (" dfcache/Team.B_1/ ", Some("/dfcache/team.b_1")),
            ("/cc", Some("/cc")),
            ("v1", Some("")),
            ("/team", None),
            ("/cc/team", None),
            ("/api", None),
            ("/ui", None),
            ("/dfcache", None),
            ("/dfcache/", None),
            ("/dfcache/a/b", None),
            ("/dfcache/has space", None),
            ("/dfcachex/a", None),
        ] {
            assert_eq!(
                validate_route_prefix(raw).ok().as_deref(),
                expected,
                "{raw:?}"
            );
        }
        let mut routes = RoutesConfig::default();
        routes.routes.push(RouteConfig::new("/team"));
        assert!(routes.validate().is_err());
        let mut routes = RoutesConfig::default();
        routes.routes.push(RouteConfig::new("/dfcache/team"));
        assert!(routes.validate().is_ok());
    }

    #[test]
    fn bidirectional_rules_table() {
        let cases = [
            (RouteRuleMode::AllowAll, Vec::<String>::new(), true),
            (RouteRuleMode::AllowList, vec![], false),
            (RouteRuleMode::AllowList, vec!["*".into()], true),
            (RouteRuleMode::AllowList, vec!["7".into()], true),
            (RouteRuleMode::AllowList, vec!["8".into()], false),
            (RouteRuleMode::DenyList, vec![], true),
            (RouteRuleMode::DenyList, vec!["*".into()], false),
            (RouteRuleMode::DenyList, vec!["7".into()], false),
            (RouteRuleMode::DenyList, vec!["8".into()], true),
        ];
        for (mode, rules, expected) in &cases {
            for (account_mode, account_rules, account_expected) in &cases {
                let account_rules = account_rules
                    .iter()
                    .map(|rule| match rule.as_str() {
                        "7" => "/cc".into(),
                        "8" => "/ha".into(),
                        _ => rule.clone(),
                    })
                    .collect::<Vec<_>>();
                let route = RouteRule {
                    mode: *mode,
                    rules: rules.clone(),
                };
                let actual = route.allows_account(7)
                    && route_policy_allows(*account_mode, &account_rules, "/CC/v1/messages");
                assert_eq!(
                    actual,
                    *expected && *account_expected,
                    "{mode:?} {rules:?} × {account_mode:?} {account_rules:?}"
                );
            }
        }
    }

    #[test]
    fn model_rules_expand_variants_and_wildcards() {
        for (pattern, model, matches) in [
            (
                "claude-opus-*",
                "anthropic/claude-opus-4-6-thinking[1m]",
                true,
            ),
            ("claude-opus-*", "claude-sonnet-4-5", false),
            ("claude-sonnet-4-5", "claude-sonnet-4.5", false),
            (
                "claude-sonnet-4-5-thinking",
                "claude-sonnet-4.5-thinking",
                false,
            ),
            ("claude-sonnet-4-5", "claude-opus-4-6", false),
            ("*", "arbitrary-model", true),
        ] {
            for mode in [
                RouteRuleMode::AllowAll,
                RouteRuleMode::AllowList,
                RouteRuleMode::DenyList,
            ] {
                let rule = RouteRule {
                    mode,
                    rules: vec![pattern.into()],
                };
                assert_eq!(
                    rule.allows_model(model),
                    match mode {
                        RouteRuleMode::AllowAll => true,
                        RouteRuleMode::AllowList => matches,
                        RouteRuleMode::DenyList => !matches,
                    },
                    "{mode:?} {pattern} {model}"
                );
            }
        }
    }

    #[test]
    fn account_overrides_inherit_and_override_every_field() {
        let global = AccountsConfig::default();
        let inherited = AccountOverrides::default().merge(&global);
        let global_json = serde_json::to_value(&global).unwrap();
        assert_eq!(serde_json::to_value(inherited).unwrap(), global_json);
        for (account_field, global_field, value) in [
            ("requestTimeoutSecs", "requestTimeoutSecs", json!(123)),
            (
                "streamRequestTimeoutSecs",
                "streamRequestTimeoutSecs",
                json!(234),
            ),
            ("streamIdleTimeoutSecs", "streamIdleTimeoutSecs", json!(45)),
            (
                "streamFirstOutputTimeoutSecs",
                "streamFirstOutputTimeoutSecs",
                json!(67),
            ),
            (
                "usageProjectionUpliftPercent",
                "usageProjectionUpliftPercent",
                json!(17),
            ),
            (
                "usageProjectionCostFloorEnabled",
                "usageProjectionCostFloorEnabled",
                json!(!global.usage_projection_cost_floor_enabled),
            ),
            (
                "usageProjectionCostFloorMarginPercent",
                "usageProjectionCostFloorMarginPercent",
                json!(19),
            ),
            (
                "usageProjectionOutputUpliftMinTokens",
                "usageProjectionOutputUpliftMinTokens",
                json!(345),
            ),
            (
                "usageProjectionOutputUpliftPercent",
                "usageProjectionOutputUpliftPercent",
                json!(23),
            ),
        ] {
            let overrides: AccountOverrides =
                serde_json::from_value(json!({account_field: value})).unwrap();
            let effective = serde_json::to_value(overrides.merge(&global)).unwrap();
            let mut expected = global_json.clone();
            expected[global_field] = value;
            assert_eq!(effective, expected, "{account_field}");
        }
    }

    #[test]
    fn scheduler_defaults_table() {
        let defaults = serde_json::to_value(AccountsConfig::default()).unwrap();
        // 高并发默认值（04 章）：全局并发 1024、队列 256、首输出超时 45 秒。
        for (field, expected) in [
            ("dispatchMaxWaitSecs", json!(60)),
            ("maxQueuedRequests", json!(256)),
            ("globalMaxConcurrentRequests", json!(1024)),
            ("streamFirstOutputTimeoutSecs", json!(45)),
            ("capacityMode", json!("wait")),
            ("enabled", json!(true)),
        ] {
            assert_eq!(defaults[field], expected, "{field}");
        }
        assert_eq!(AccountsConfig::default().capacity_mode, CapacityMode::Wait);
    }

    #[test]
    fn migration_merges_union_dfcache_and_global_rules_idempotently() {
        let mut value = json!({
            "runtimeConfigMigrationVersion": 1,
            "cachePolicy": {"pathOverrides": {"/cc": {"cacheType":"current_high_cache"}, "cache": {"cacheType":"no_cache"}}},
            "reportedUsage": {"pathOverrides": {"/cc/": {"input":{"mode":"raw"}}, "/report": {}}},
            "promptInjection": {"pathOverrides": {"/cc": {"taskQuality":{"enabled":true}}, "/inject": {"custom":{"enabled":false}}}},
            "routeFeatures": {"pathOverrides": {"/cc": {"thinkingModelName":true}, "/feature": {"historySanitize":true}}},
            "definedCacheRoutes": ["foo", "/dfcache/bar/"],
            "accounts": {"accountRouteMode":"allow_list", "accountRouteRules":["/cc", "/dfcache/foo"]}
        });
        assert!(migrate_path_overrides_to_routes(&mut value));
        let config: Config = serde_json::from_value(value.clone()).unwrap();
        config.routes.validate().unwrap();
        for prefix in ["", "/cc", "/ha", "/na", "/dfcache/foo", "/dfcache/bar"] {
            assert!(
                config
                    .routes
                    .routes
                    .iter()
                    .any(|route| route.prefix == prefix),
                "{prefix}"
            );
        }
        // 旧表里的根级键不是合法入口，不能变成可访问路由。
        for prefix in ["/cache", "/report", "/inject", "/feature"] {
            assert!(
                !config
                    .routes
                    .routes
                    .iter()
                    .any(|route| route.prefix == prefix),
                "{prefix} must not become a route"
            );
        }
        let cc = config.routes.match_endpoint("/cc/v1/messages").unwrap();
        assert!(cc.reported_usage.is_some());
        assert!(cc.cache_policy.cache_type.is_some());
        assert_eq!(cc.features.thinking_model_name, Some(true));
        assert_eq!(
            cc.prompt_injection[&InjectionId::TaskQuality].enabled,
            Some(true)
        );
        assert!(cc.account_rule.allows_account(1));
        assert!(
            !config
                .routes
                .match_endpoint("/ha/v1/messages")
                .unwrap()
                .account_rule
                .allows_account(1)
        );
        assert_eq!(value["runtimeConfigMigrationVersion"], 2);
        assert!(!migrate_path_overrides_to_routes(&mut value));
        assert_eq!(
            serde_json::from_value::<Config>(value).unwrap().routes,
            config.routes
        );
    }

    #[test]
    fn migration_preserves_cache_policy_when_other_tables_add_routes() {
        let config = Config::from_json_value(json!({
            "cachePolicy":{"pathOverrides":{"/dfcache":{"cacheType":"bounded_high_cache"}}},
            "routeFeatures":{"pathOverrides":{"/new":{"historySanitize":true}, "/dfcache/team":{"thinkingModelName":true}, "/cc/team":{"historySanitize":true}}}
        })).unwrap();
        // 旧表里的 /new、/cc/team 不是合法入口（不在 /dfcache 下），迁移时丢弃，请求落回内置路由。
        for (endpoint, expected) in [
            (
                "/new/v1/messages",
                PromptCacheStrategyType::CurrentHighCache,
            ),
            (
                "/dfcache/team/v1/messages",
                PromptCacheStrategyType::BoundedHighCache,
            ),
            (
                "/cc/team/v1/messages",
                PromptCacheStrategyType::CurrentHighCache,
            ),
        ] {
            assert_eq!(
                config.cache_policy_for_path(endpoint).policy.cache_type,
                expected,
                "{endpoint}"
            );
        }
    }

    #[test]
    fn migration_preserves_longest_reported_policy_for_each_route() {
        let mut value = json!({
            "cachePolicy": {"pathOverrides": {"/dfcache/team": {"cacheType":"current_high_cache"}}},
            "reportedUsage": {"pathOverrides": {
                "/dfcache": {"input":{"mode":"sample-max", "maxTokens":42}},
                "/dfcache/team/v1": {"input":{"mode":"sample-max", "maxTokens":17}}
            }},
            "definedCacheRoutes": ["team", "other"]
        });
        assert!(migrate_path_overrides_to_routes(&mut value));
        let routes: RoutesConfig = serde_json::from_value(value["routes"].clone()).unwrap();
        for (prefix, expected) in [("/dfcache/team", 17), ("/dfcache/other", 42)] {
            let route = routes
                .routes
                .iter()
                .find(|route| route.prefix == prefix)
                .unwrap();
            assert_eq!(
                serde_json::to_value(&route.reported_usage).unwrap()["input"]["maxTokens"],
                expected
            );
        }
    }

    #[test]
    fn migration_empty_and_each_legacy_key() {
        let mut empty = json!({});
        assert!(!migrate_path_overrides_to_routes(&mut empty));
        assert!(
            serde_json::from_value::<Config>(empty)
                .unwrap()
                .routes
                .validate()
                .is_ok()
        );
        for key in [
            "cachePolicy",
            "reportedUsage",
            "promptInjection",
            "routeFeatures",
        ] {
            let mut value = json!({key: {"pathOverrides": {}}});
            assert!(migrate_path_overrides_to_routes(&mut value));
            assert!(value[key].get("pathOverrides").is_none());
            assert_eq!(value["routes"].as_array().unwrap().len(), 4);
        }
    }
}

#[cfg(test)]
mod legacy_fixture_tests {
    use super::*;

    #[test]
    fn migrate_redacted_deployed_config_shape() {
        let mut value = json!({
            "runtimeConfigMigrationVersion":1,
            "proxyUrl":"http://proxy.example.invalid:8080",
            "cachePolicy":{"currentHighCache":{"simulation":{"targetReadRatio":0.98,"tokenScale":1.6}},"pathOverrides":{
                "/v1":{"cacheType":"current_high_cache","routeNamespace":false},
                "/cc":{"cacheType":"current_high_cache","routeNamespace":false},
                "/dfcache/team-a":{"cacheType":"prefix_tool","routeNamespace":true}
            }},
            "reportedUsage":{"pathOverrides":{"/ha":{"input":{"mode":"sample-max","maxTokens":96}}}},
            "promptInjection":{"defaults":{"custom":{"enabled":false}},"texts":{"custom":"redacted deployment prompt"},"pathOverrides":{"/dfcache":{"custom":{"enabled":true}}}},
            "routeFeatures":{"pathOverrides":{"/dfcache":{"reasoningCompat":null,"historySanitize":true},"/dfcache/team-a":{"thinkingModelName":true}}},
            "definedCacheRoutes":["/dfcache/team-a","/dfcache/team-b"],
            "accounts":{"enabled":true,"accountRouteMode":"deny_list","accountRouteRules":["/na"],"dispatchMaxWaitSecs":60,"maxQueuedRequests":64,"capacityMode":"wait"}
        });
        assert!(migrate_path_overrides_to_routes(&mut value));
        let config = Config::from_json_value(value.clone()).unwrap();
        config.routes.validate_with_config(&config).unwrap();
        let compiled = config.routes.compile_with_config(&config);
        let team = compiled
            .match_endpoint("/dfcache/team-a/v1/messages")
            .unwrap();
        assert_eq!(
            team.cache_policy.policy.cache_type,
            PromptCacheStrategyType::PrefixTool
        );
        assert!(team.features.history_sanitize);
        assert!(team.features.thinking_model_name);
        assert!(team.injections.allows(InjectionId::Custom));
        assert!(
            compiled
                .match_endpoint("/dfcache/team-b/v1/messages")
                .unwrap()
                .features
                .history_sanitize
        );
        assert!(
            !compiled
                .match_endpoint("/na/v1/messages")
                .unwrap()
                .config
                .account_rule
                .allows_account(1)
        );
        assert!(!migrate_path_overrides_to_routes(&mut value));
    }

    #[test]
    fn migration_keeps_builtin_defaults_when_only_features_are_overridden() {
        let config = Config::from_json_value(
            json!({"routeFeatures":{"pathOverrides":{"/cc":{"historySanitize":true}}}}),
        )
        .unwrap();
        let route = config
            .routes
            .compile()
            .match_endpoint("/cc/v1/messages")
            .unwrap()
            .clone();
        assert_eq!(
            route.cache_policy.policy.cache_type,
            PromptCacheStrategyType::CurrentHighCache
        );
        assert_eq!(
            route.cache_policy.policy.reported_usage.input.max_tokens,
            96
        );
        assert!(route.features.history_sanitize);
    }
}
