//! 把旧的 `promptSteering` / `thinkingTriggerMode` / `bodyConversion` 提示词开关转换成
//! `promptInjection` + `routeFeatures`，结果对每个入口的判定与旧代码一致。
//!
//! 旧规则有两种按路由的结构：路由规则（allow/deny list）和身份的 `pathOverrides`（取最长匹配
//! 的一条，未设置的字段回落到全局值）。新结构是逐项、逐字段取最长匹配前缀。把旧规则和覆盖里
//! 出现过的所有前缀放进集合 Q：任何入口的旧判定结果，只取决于 Q 中匹配它的最长前缀，所以在
//! Q 的每个前缀上用旧逻辑求值，再写成新结构里的显式设置，就能保证逐入口等价。

use std::collections::{BTreeMap, BTreeSet};

use super::legacy::{
    IdentityPromptConfig, PromptSteeringConfig, PromptSteeringRouteMode, PromptSteeringScope,
};
use super::{
    InjectionId, ItemOverride, ItemSetting, PromptInjectionConfig, RouteFeatureOverride,
    RouteFeatureSet, RouteFeaturesConfig, TextKey,
};
use crate::model::config::{
    CompatProfile, ModelDrivenWebSearchConfig, ThinkingTriggerMode,
    normalize_reported_usage_path_prefix, normalize_route_rule, reported_usage_path_matches,
    route_rule_matches,
};

/// 旧配置里和提示词有关的字段。
#[derive(Debug, Clone, Default)]
pub(crate) struct LegacyPromptFields {
    pub prompt_steering: Option<PromptSteeringConfig>,
    pub thinking_trigger_mode: Option<ThinkingTriggerMode>,
    pub chunked_tool_policy: Option<bool>,
    pub thinking_prompt_controls: Option<bool>,
    pub model_driven_web_search: Option<ModelDrivenWebSearchConfig>,
}

impl LegacyPromptFields {
    pub fn is_empty(&self) -> bool {
        self.prompt_steering.is_none()
            && self.thinking_trigger_mode.is_none()
            && self.chunked_tool_policy.is_none()
            && self.thinking_prompt_controls.is_none()
            && self.model_driven_web_search.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MigratedPromptConfig {
    pub prompt_injection: PromptInjectionConfig,
    pub route_features: RouteFeaturesConfig,
    pub claude_models_only: bool,
}

/// 不匹配任何规则、任何覆盖前缀的入口，用来求"全局默认"。
const NO_MATCH_ENDPOINT: &str = "";

pub(crate) fn migrate(legacy: LegacyPromptFields, profile: CompatProfile) -> MigratedPromptConfig {
    let steering = legacy.prompt_steering.unwrap_or_default().normalized();
    let master = steering.enabled;
    let chunked_policy = legacy.chunked_tool_policy.unwrap_or(true);
    let thinking_controls = legacy.thinking_prompt_controls.unwrap_or(true);

    let mut defaults = BTreeMap::new();
    let mut overrides: BTreeMap<String, BTreeMap<InjectionId, ItemOverride>> = BTreeMap::new();
    let mut set_default = |id: InjectionId, enabled: bool, external_pool: Option<bool>| {
        defaults.insert(
            id,
            ItemSetting {
                enabled,
                external_pool,
            },
        );
    };

    // 写进 system 的请求级提示和每轮语言提醒：共用旧的 scope / routeRules。
    let request_prefixes = request_rule_prefixes(&steering);
    let request_items: [(InjectionId, bool); 4] = [
        (InjectionId::TaskQuality, steering.task_quality.enabled),
        (
            InjectionId::LanguageConstraint,
            steering.language_constraint.enabled,
        ),
        (InjectionId::Custom, steering.custom.enabled),
        (
            InjectionId::LanguageTurnReminder,
            steering.language_constraint.enabled && steering.language_turn_reminder.enabled,
        ),
    ];
    for (id, item_enabled) in request_items {
        let value_at =
            |endpoint: &str| item_enabled && request_route_allowed(&steering, profile, endpoint);
        let external_pool = id
            .supports_external_pool()
            .then_some(steering.apply_to_external_pool);
        set_default(id, value_at(NO_MATCH_ENDPOINT), external_pool);
        for (prefix, value) in prefix_values(&request_prefixes, value_at) {
            overrides
                .entry(prefix)
                .or_default()
                .entry(id)
                .or_default()
                .enabled = Some(value);
        }
    }

    // 身份：旧的身份路由规则加上逐路由覆盖。
    let identity = &steering.identity;
    let identity_prefixes = identity_prefixes(identity);
    type IdentityCheck = fn(&IdentityPromptConfig) -> bool;
    let identity_items: [(InjectionId, IdentityCheck); 2] = [
        (InjectionId::IdentityPrompt, |resolved| {
            resolved.system_prompt_applies()
        }),
        (InjectionId::IdentityTurnReminder, |resolved| {
            resolved.turn_reminder_text().is_some()
        }),
    ];
    for (id, applies) in identity_items {
        let value_at = |endpoint: &str| applies(&identity.for_route(endpoint));
        let external_pool = id
            .supports_external_pool()
            .then(|| identity.for_route(NO_MATCH_ENDPOINT).apply_to_external_pool);
        set_default(id, value_at(NO_MATCH_ENDPOINT), external_pool);
        for (prefix, value) in prefix_values(&identity_prefixes, value_at) {
            overrides
                .entry(prefix)
                .or_default()
                .entry(id)
                .or_default()
                .enabled = Some(value);
        }
    }
    for (prefix, value) in prefix_values(&identity_prefixes, |endpoint| {
        identity.for_route(endpoint).apply_to_external_pool
    }) {
        overrides
            .entry(prefix)
            .or_default()
            .entry(InjectionId::IdentityPrompt)
            .or_default()
            .external_pool = Some(value);
    }

    // 转换层注入：旧代码不看路由，只有全局开关。
    set_default(
        InjectionId::ChunkedWritePolicy,
        master
            && steering.chunked_write.enabled
            && steering.chunked_write.system_prompt_enabled
            && chunked_policy,
        None,
    );
    set_default(
        InjectionId::ChunkedWriteToolSuffix,
        master
            && steering.chunked_write.enabled
            && steering.chunked_write.tool_description_enabled
            && chunked_policy,
        None,
    );
    // 总开关关闭时，旧代码只在客户端显式请求 reasoning 时注入 thinking 标签；而自动开启
    // thinking 的两条路径在下面都随总开关迁移为关闭，剩下需要标签的请求恰好都是显式请求，
    // 所以这里不乘总开关。
    set_default(
        InjectionId::ThinkingControls,
        thinking_controls && steering.thinking.enabled,
        None,
    );
    set_default(InjectionId::ThinkingOutputPolicy, true, None);
    let tool_choice_prompt = master && steering.tool_choice.enabled;
    set_default(InjectionId::ToolChoicePrefix, tool_choice_prompt, None);
    set_default(
        InjectionId::ToolChoiceTurnReminder,
        tool_choice_prompt,
        None,
    );
    // 旧版本没有开关，一直注入。
    set_default(InjectionId::WebSearchGuidance, true, None);
    set_default(InjectionId::EmptyTurnNudge, true, None);

    let mut texts = BTreeMap::new();
    let mut set_text = |key: TextKey, text: &str| {
        texts.insert(key, text.to_string());
    };
    set_text(TextKey::TaskQuality, &steering.task_quality.prompt);
    set_text(
        TextKey::LanguageConstraint,
        &steering.language_constraint.prompt,
    );
    set_text(
        TextKey::LanguageTurnReminderExplicit,
        &steering.language_turn_reminder.explicit_prompt,
    );
    set_text(
        TextKey::LanguageTurnReminderFollowUser,
        &steering.language_turn_reminder.follow_user_prompt,
    );
    set_text(TextKey::Custom, &steering.custom.prompt);
    set_text(TextKey::IdentityPrompt, &identity.prompt);
    set_text(
        TextKey::IdentityTurnReminder,
        &identity.turn_reminder.prompt,
    );

    let prompt_injection = PromptInjectionConfig {
        apply_to_count_tokens: steering.apply_to_count_tokens,
        defaults,
        texts,
        path_overrides: overrides,
    }
    .normalized();

    // 路由功能。
    let web_search = legacy.model_driven_web_search.unwrap_or_default();
    let no_match_identity = identity.for_route(NO_MATCH_ENDPOINT);
    let mut feature_overrides: BTreeMap<String, RouteFeatureOverride> = BTreeMap::new();
    for (prefix, value) in prefix_values(&identity_prefixes, |endpoint| {
        let resolved = identity.for_route(endpoint);
        resolved.enabled && resolved.output_guard.enabled
    }) {
        feature_overrides
            .entry(prefix)
            .or_default()
            .channel_leak_guard = Some(value);
    }
    for (prefix, value) in prefix_values(&identity_prefixes, |endpoint| {
        identity.for_route(endpoint).output_guard.stream_enabled
    }) {
        feature_overrides
            .entry(prefix)
            .or_default()
            .channel_leak_guard_stream = Some(value);
    }
    for (prefix, enabled) in &web_search.path_overrides {
        if let Some(prefix) = normalize_reported_usage_path_prefix(prefix) {
            feature_overrides
                .entry(prefix)
                .or_default()
                .model_driven_web_search = Some(*enabled);
        }
    }
    let route_features = RouteFeaturesConfig {
        defaults: RouteFeatureSet {
            thinking_model_name: master,
            thinking_trigger: if master {
                legacy.thinking_trigger_mode.unwrap_or_default()
            } else {
                ThinkingTriggerMode::Off
            },
            channel_leak_guard: no_match_identity.enabled && no_match_identity.output_guard.enabled,
            channel_leak_guard_stream: no_match_identity.output_guard.stream_enabled,
            model_driven_web_search: web_search.enabled,
        },
        path_overrides: feature_overrides,
    }
    .normalized();

    MigratedPromptConfig {
        prompt_injection,
        route_features,
        claude_models_only: identity.claude_models_only,
    }
}

/// 旧的请求级判定（`prompt_steering::should_apply`，不含 strict 和 count_tokens 两个全局条件，
/// 它们在新结构里由 strict 拦截和 `applyToCountTokens` 表达）。
fn request_route_allowed(
    steering: &PromptSteeringConfig,
    profile: CompatProfile,
    endpoint: &str,
) -> bool {
    if !steering.enabled {
        return false;
    }
    match steering.scope {
        PromptSteeringScope::RouteRules | PromptSteeringScope::CcOnly => {
            rules_allow(steering.route_mode, &steering.route_rules, endpoint)
        }
        PromptSteeringScope::ClaudeCodeProfile => {
            matches!(profile, CompatProfile::ClaudeCode | CompatProfile::Debug)
        }
        PromptSteeringScope::AllRoutes => true,
    }
}

fn rules_allow(mode: PromptSteeringRouteMode, rules: &[String], endpoint: &str) -> bool {
    let matched = || rules.iter().any(|rule| route_rule_matches(rule, endpoint));
    match mode {
        PromptSteeringRouteMode::AllowAll => true,
        PromptSteeringRouteMode::AllowList => matched(),
        PromptSteeringRouteMode::DenyList => !matched(),
    }
}

/// 规则作为前缀集合；`*` 匹配全部入口，等价于前缀 `/`。
fn rule_prefixes(mode: PromptSteeringRouteMode, rules: &[String]) -> BTreeSet<String> {
    if mode == PromptSteeringRouteMode::AllowAll {
        return BTreeSet::new();
    }
    rules
        .iter()
        .filter_map(|rule| normalize_route_rule(rule))
        .map(|rule| if rule == "*" { "/".to_string() } else { rule })
        .collect()
}

fn request_rule_prefixes(steering: &PromptSteeringConfig) -> BTreeSet<String> {
    match steering.scope {
        PromptSteeringScope::RouteRules | PromptSteeringScope::CcOnly => {
            rule_prefixes(steering.route_mode, &steering.route_rules)
        }
        PromptSteeringScope::ClaudeCodeProfile | PromptSteeringScope::AllRoutes => BTreeSet::new(),
    }
}

fn identity_prefixes(identity: &IdentityPromptConfig) -> BTreeSet<String> {
    let mut prefixes = rule_prefixes(identity.route_mode, &identity.route_rules);
    prefixes.extend(identity.path_overrides.keys().cloned());
    prefixes
}

/// 在每个前缀上求值，只保留和"继承值"（更短的已写前缀，或全局默认）不同的结果。
fn prefix_values(
    prefixes: &BTreeSet<String>,
    value_at: impl Fn(&str) -> bool,
) -> Vec<(String, bool)> {
    let default = value_at(NO_MATCH_ENDPOINT);
    let mut ordered: Vec<&String> = prefixes.iter().collect();
    ordered.sort_by_key(|prefix| prefix.len());
    let mut written: Vec<(String, bool)> = Vec::new();
    for prefix in ordered {
        let inherited = written
            .iter()
            .filter(|(shorter, _)| {
                shorter != prefix && reported_usage_path_matches(shorter, prefix)
            })
            .max_by_key(|(shorter, _)| shorter.len())
            .map_or(default, |(_, value)| *value);
        let value = value_at(prefix);
        if value != inherited {
            written.push((prefix.clone(), value));
        }
    }
    written
}

#[cfg(test)]
mod tests {
    //! 迁移等价：旧的判定逻辑原样复制在这里作为对照，随机生成旧配置，逐入口比较新旧结果。

    use super::*;
    use crate::model::prompt_injection::legacy::{IdentityRouteOverride, PromptSteeringTextBlock};
    use crate::model::prompt_injection::{BlockReason, ResolvedInjections, UpstreamKind};

    const SEED: u64 = 0x5052_4f4d_5054_2023;
    const CONFIGS: usize = 10_000;
    const ENDPOINTS: [&str; 9] = [
        "/v1/messages",
        "/cc/v1/messages",
        "/cc/v1/messages/count_tokens",
        "/ha/v1/messages",
        "/na/v1/messages",
        "/dfcache/a/v1/messages",
        "/dfcache/b/v1/messages",
        "/ccx/v1/messages",
        "/cc/strict/v1/messages",
    ];
    const RULES: [&str; 7] = [
        "/cc",
        "/v1",
        "/dfcache/a",
        "*",
        "/cc/v1",
        "/CC/",
        "/dfcache",
    ];
    const OVERRIDE_PREFIXES: [&str; 5] = ["/cc", "/cc/strict", "/dfcache", "/dfcache/a", "/na"];
    const PROFILES: [CompatProfile; 3] = [
        CompatProfile::ClaudeCode,
        CompatProfile::AnthropicStrict,
        CompatProfile::Debug,
    ];

    /// 旧版 `prompt_steering::should_apply`（messages）。
    fn old_should_apply(endpoint: &str, profile: CompatProfile, c: &PromptSteeringConfig) -> bool {
        if !c.enabled || profile.is_strict() {
            return false;
        }
        match c.scope {
            PromptSteeringScope::RouteRules | PromptSteeringScope::CcOnly => {
                rules_allow(c.route_mode, &c.route_rules, endpoint)
            }
            PromptSteeringScope::ClaudeCodeProfile => {
                matches!(profile, CompatProfile::ClaudeCode | CompatProfile::Debug)
            }
            PromptSteeringScope::AllRoutes => true,
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct Legacy {
        chunked_tool_policy: bool,
        thinking_prompt_controls: bool,
        tool_choice_steering: bool,
    }

    /// 旧代码对一个注入项的最终判定。B1 只在请求显式要求 reasoning 时才可能生成，
    /// 这里按"请求要求 reasoning"求值；`force_visible` 是旧的强制可见 thinking 结果。
    fn old_allows(
        id: InjectionId,
        endpoint: &str,
        upstream: UpstreamKind,
        profile: CompatProfile,
        c: &PromptSteeringConfig,
        legacy: Legacy,
        force_visible: bool,
    ) -> bool {
        let strict = profile.is_strict();
        let request = |enabled: bool| {
            let local = enabled && old_should_apply(endpoint, profile, c);
            match upstream {
                UpstreamKind::Local => local,
                UpstreamKind::ExternalPool => local && c.apply_to_external_pool,
            }
        };
        let identity = c.identity.for_route(endpoint);
        let local_only = |value: bool| upstream == UpstreamKind::Local && value;
        match id {
            InjectionId::TaskQuality => request(c.task_quality.enabled),
            InjectionId::LanguageConstraint => request(c.language_constraint.enabled),
            InjectionId::Custom => request(c.custom.enabled),
            InjectionId::LanguageTurnReminder => local_only(
                old_should_apply(endpoint, profile, c)
                    && c.language_constraint.enabled
                    && c.language_turn_reminder.enabled,
            ),
            InjectionId::IdentityPrompt => match upstream {
                UpstreamKind::Local => identity.system_prompt_applies(),
                UpstreamKind::ExternalPool => {
                    identity.system_prompt_applies() && identity.apply_to_external_pool
                }
            },
            InjectionId::IdentityTurnReminder => {
                local_only(identity.turn_reminder_text().is_some())
            }
            InjectionId::ChunkedWritePolicy => local_only(
                !strict
                    && c.enabled
                    && c.chunked_write.enabled
                    && c.chunked_write.system_prompt_enabled
                    && legacy.chunked_tool_policy,
            ),
            InjectionId::ChunkedWriteToolSuffix => local_only(
                !strict
                    && c.enabled
                    && c.chunked_write.enabled
                    && c.chunked_write.tool_description_enabled
                    && legacy.chunked_tool_policy,
            ),
            InjectionId::ThinkingControls => local_only(
                legacy.thinking_prompt_controls && c.thinking.enabled && (force_visible || !strict),
            ),
            // 旧代码没有单独的开关，和 B1 一起在 strict 下仅随强制可见 thinking 注入。
            InjectionId::ThinkingOutputPolicy => local_only(force_visible || !strict),
            InjectionId::ToolChoicePrefix | InjectionId::ToolChoiceTurnReminder => local_only(
                !strict && c.enabled && c.tool_choice.enabled && legacy.tool_choice_steering,
            ),
            InjectionId::WebSearchGuidance | InjectionId::EmptyTurnNudge => local_only(true),
        }
    }

    /// 新结构对同一项的判定（与注入点的判断方式一致）。
    fn new_allows(
        id: InjectionId,
        resolved: &ResolvedInjections,
        legacy: Legacy,
        force_visible: bool,
    ) -> bool {
        let item = resolved.item(id);
        match id {
            InjectionId::ThinkingControls | InjectionId::ThinkingOutputPolicy => {
                item.allowed || (force_visible && item.blocked == Some(BlockReason::Strict))
            }
            InjectionId::ToolChoicePrefix | InjectionId::ToolChoiceTurnReminder => {
                legacy.tool_choice_steering && item.allowed
            }
            _ => item.allowed,
        }
    }

    fn pick<'a>(rng: &mut fastrand::Rng, values: &'a [&'a str], max: usize) -> Vec<String> {
        (0..rng.usize(0..=max))
            .map(|_| values[rng.usize(0..values.len())].to_string())
            .collect()
    }

    fn route_mode(rng: &mut fastrand::Rng) -> PromptSteeringRouteMode {
        [
            PromptSteeringRouteMode::AllowAll,
            PromptSteeringRouteMode::AllowList,
            PromptSteeringRouteMode::DenyList,
        ][rng.usize(0..3)]
    }

    fn opt(rng: &mut fastrand::Rng) -> Option<bool> {
        [None, Some(false), Some(true)][rng.usize(0..3)]
    }

    type RandomLegacy = (
        PromptSteeringConfig,
        Legacy,
        ModelDrivenWebSearchConfig,
        ThinkingTriggerMode,
    );

    fn random_legacy(rng: &mut fastrand::Rng) -> RandomLegacy {
        let mut c = PromptSteeringConfig {
            enabled: rng.bool(),
            scope: [
                PromptSteeringScope::RouteRules,
                PromptSteeringScope::CcOnly,
                PromptSteeringScope::ClaudeCodeProfile,
                PromptSteeringScope::AllRoutes,
            ][rng.usize(0..4)],
            route_mode: route_mode(rng),
            route_rules: pick(rng, &RULES, 3),
            apply_to_external_pool: rng.bool(),
            apply_to_count_tokens: rng.bool(),
            ..PromptSteeringConfig::default()
        };
        c.language_constraint.enabled = rng.bool();
        c.language_turn_reminder.enabled = rng.bool();
        c.task_quality.enabled = rng.bool();
        c.custom = PromptSteeringTextBlock {
            enabled: rng.bool(),
            prompt: if rng.bool() {
                "custom".into()
            } else {
                String::new()
            },
        };
        c.tool_choice.enabled = rng.bool();
        c.thinking.enabled = rng.bool();
        c.chunked_write.enabled = rng.bool();
        c.chunked_write.system_prompt_enabled = rng.bool();
        c.chunked_write.tool_description_enabled = rng.bool();
        let identity = &mut c.identity;
        identity.enabled = rng.bool();
        identity.route_mode = route_mode(rng);
        identity.route_rules = pick(rng, &RULES, 3);
        identity.prompt_enabled = rng.bool();
        identity.turn_reminder.enabled = rng.bool();
        identity.output_guard.enabled = rng.bool();
        identity.output_guard.stream_enabled = rng.bool();
        identity.apply_to_external_pool = rng.bool();
        identity.claude_models_only = rng.bool();
        for prefix in pick(rng, &OVERRIDE_PREFIXES, 3) {
            identity.path_overrides.insert(
                prefix,
                IdentityRouteOverride {
                    enabled: opt(rng),
                    prompt_enabled: opt(rng),
                    turn_reminder_enabled: opt(rng),
                    output_guard_enabled: opt(rng),
                    output_guard_stream_enabled: opt(rng),
                    apply_to_external_pool: opt(rng),
                },
            );
        }
        let legacy = Legacy {
            chunked_tool_policy: rng.bool(),
            thinking_prompt_controls: rng.bool(),
            tool_choice_steering: rng.bool(),
        };
        let mut web_search = ModelDrivenWebSearchConfig {
            enabled: rng.bool(),
            ..ModelDrivenWebSearchConfig::default()
        };
        for prefix in pick(rng, &OVERRIDE_PREFIXES, 2) {
            web_search.path_overrides.insert(prefix, rng.bool());
        }
        let trigger = [
            ThinkingTriggerMode::RealRequest,
            ThinkingTriggerMode::Always,
        ][rng.usize(0..2)];
        (c, legacy, web_search, trigger)
    }

    /// 旧的 WebSearch 按路由判定（`ModelDrivenWebSearchConfig::enabled_for_route`）。
    fn old_web_search(config: &ModelDrivenWebSearchConfig, endpoint: &str) -> bool {
        config
            .path_overrides
            .iter()
            .filter_map(|(prefix, enabled)| {
                normalize_reported_usage_path_prefix(prefix).map(|prefix| (prefix, *enabled))
            })
            .filter(|(prefix, _)| reported_usage_path_matches(prefix, endpoint))
            .max_by_key(|(prefix, _)| prefix.len())
            .map_or(config.enabled, |(_, enabled)| enabled)
    }

    #[test]
    fn migration_is_equivalent_for_every_item_route_upstream_and_profile_seed_5052() {
        let mut rng = fastrand::Rng::with_seed(SEED);
        for case in 0..CONFIGS {
            let (steering, legacy, web_search, trigger) = random_legacy(&mut rng);
            let profile = PROFILES[rng.usize(0..PROFILES.len())];
            let fields = LegacyPromptFields {
                prompt_steering: Some(steering.clone()),
                thinking_trigger_mode: Some(trigger),
                chunked_tool_policy: Some(legacy.chunked_tool_policy),
                thinking_prompt_controls: Some(legacy.thinking_prompt_controls),
                model_driven_web_search: Some(web_search.clone()),
            };
            let migrated = migrate(fields, profile);
            let old = steering.clone().normalized();
            assert_eq!(
                migrated.prompt_injection.apply_to_count_tokens, old.apply_to_count_tokens,
                "case {case}"
            );
            assert_eq!(
                migrated.claude_models_only, old.identity.claude_models_only,
                "case {case}"
            );
            for endpoint in ENDPOINTS {
                for upstream in [UpstreamKind::Local, UpstreamKind::ExternalPool] {
                    let resolved = ResolvedInjections::resolve(
                        endpoint,
                        upstream,
                        profile,
                        &migrated.prompt_injection,
                    );
                    for force_visible in [false, true] {
                        for id in InjectionId::ALL {
                            let expected = old_allows(
                                id,
                                endpoint,
                                upstream,
                                profile,
                                &old,
                                legacy,
                                force_visible,
                            );
                            let actual = new_allows(id, &resolved, legacy, force_visible);
                            assert_eq!(
                                actual, expected,
                                "case {case} seed {SEED:#x}: {id:?} at {endpoint} ({upstream:?}, {profile:?}, force_visible={force_visible})\nold={steering:?}\nnew={:?}",
                                migrated.prompt_injection
                            );
                        }
                    }
                }

                let features = migrated.route_features.resolve(endpoint);
                let identity = old.identity.for_route(endpoint);
                for stream in [false, true] {
                    assert_eq!(
                        features.channel_leak_guard_applies(stream),
                        identity.enabled
                            && identity.output_guard.enabled
                            && (!stream || identity.output_guard.stream_enabled),
                        "case {case}: channel leak guard at {endpoint}, stream={stream}"
                    );
                }
                assert_eq!(
                    features.model_driven_web_search,
                    old_web_search(&web_search, endpoint),
                    "case {case}: web search at {endpoint}"
                );
                assert_eq!(features.thinking_model_name, old.enabled, "case {case}");
                let expected_trigger = if old.enabled {
                    trigger
                } else {
                    ThinkingTriggerMode::Off
                };
                assert_eq!(features.thinking_trigger, expected_trigger, "case {case}");
            }
        }
    }

    #[test]
    fn default_legacy_config_migrates_to_fresh_install_defaults() {
        let migrated = migrate(
            LegacyPromptFields {
                prompt_steering: Some(serde_json::from_value(serde_json::json!({})).unwrap()),
                ..LegacyPromptFields::default()
            },
            CompatProfile::ClaudeCode,
        );
        // 旧的语言约束默认关；任务质量默认只在 /cc 上开，与新装默认一致。
        assert_eq!(
            migrated.prompt_injection,
            PromptInjectionConfig::default().normalized()
        );
        assert_eq!(migrated.route_features, RouteFeaturesConfig::default());
        assert!(!migrated.claude_models_only);
    }

    #[test]
    fn edited_texts_survive_and_built_in_texts_follow_defaults() {
        let migrated = migrate(
            LegacyPromptFields {
                prompt_steering: Some(
                    serde_json::from_value(serde_json::json!({
                        "taskQuality": {"enabled": true, "prompt": "  edited task  "},
                        "custom": {"enabled": true, "prompt": "custom text"},
                        "identity": {"prompt": "You are Claude."}
                    }))
                    .unwrap(),
                ),
                ..LegacyPromptFields::default()
            },
            CompatProfile::ClaudeCode,
        );
        let config = migrated.prompt_injection;
        assert_eq!(config.text(TextKey::TaskQuality), "edited task");
        assert_eq!(config.text(TextKey::Custom), "custom text");
        assert_eq!(config.text(TextKey::IdentityPrompt), "You are Claude.");
        assert!(!config.texts.contains_key(&TextKey::LanguageConstraint));
        assert!(!config.texts.contains_key(&TextKey::IdentityTurnReminder));
    }
}
