//! Request-level prompt steering for Claude Code compatibility.
//!
//! This module injects operator-facing guidance. The same runtime `promptSteering.enabled`
//! switch is also consumed by the local converter and parsed-body pipeline as the master gate
//! for proxy-added compatibility prompts. Client-provided structured fields are not removed.

use crate::anthropic::types::{CountTokensRequest, MessagesRequest, SystemMessage};
use crate::model::config::{
    CompatProfile, IDENTITY_PROMPT_END_MARKER, IDENTITY_PROMPT_MARKER, IdentityPromptConfig,
    PROMPT_STEERING_END_MARKER, PROMPT_STEERING_MARKER, PromptSteeringConfig,
    PromptSteeringRouteMode, PromptSteeringScope, route_rule_matches,
};

/// Whether identity masking should be applied to a request on `endpoint` that is forwarded to an
/// external pool.
pub(crate) fn identity_applies_to_external_pool(
    endpoint: &str,
    config: &PromptSteeringConfig,
) -> bool {
    let identity = config.identity.for_route(endpoint);
    identity.enabled && identity.apply_to_external_pool
}

/// Prepends the identity block to a messages request. It ignores the steering master switch,
/// scope, steering route rules, and strict profile, and follows the identity route binding.
pub(crate) fn apply_identity_to_messages_request(
    endpoint: &str,
    config: &PromptSteeringConfig,
    payload: &mut MessagesRequest,
) -> bool {
    apply_identity_to_system(&mut payload.system, &config.identity.for_route(endpoint))
}

pub(crate) fn apply_identity_to_count_tokens_request(
    endpoint: &str,
    config: &PromptSteeringConfig,
    payload: &mut CountTokensRequest,
) -> bool {
    if !config.apply_to_count_tokens {
        return false;
    }
    apply_identity_to_system(&mut payload.system, &config.identity.for_route(endpoint))
}

fn apply_identity_to_system(
    system: &mut Option<Vec<SystemMessage>>,
    identity: &IdentityPromptConfig,
) -> bool {
    if !identity.system_prompt_applies() {
        return false;
    }
    if system.as_deref().is_some_and(|blocks| {
        blocks
            .iter()
            .any(|block| block.text.contains(IDENTITY_PROMPT_MARKER))
    }) {
        return false;
    }
    let identity = identity.clone().normalized();
    let block = SystemMessage {
        text: format!(
            "{}\n{}\n{}",
            IDENTITY_PROMPT_MARKER, identity.prompt, IDENTITY_PROMPT_END_MARKER
        ),
        cache_control: None,
    };
    match system {
        Some(system) => system.insert(0, block),
        None => *system = Some(vec![block]),
    }
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptSteeringEndpointKind {
    Messages,
    CountTokens,
}

pub(crate) fn apply_to_messages_request(
    endpoint: &str,
    compat_profile: CompatProfile,
    config: &PromptSteeringConfig,
    payload: &mut MessagesRequest,
) -> bool {
    if !should_apply(
        endpoint,
        compat_profile,
        config,
        PromptSteeringEndpointKind::Messages,
    ) {
        return false;
    }
    apply_to_system(&mut payload.system, config)
}

pub(crate) fn apply_to_count_tokens_request(
    endpoint: &str,
    compat_profile: CompatProfile,
    config: &PromptSteeringConfig,
    payload: &mut CountTokensRequest,
) -> bool {
    if !should_apply(
        endpoint,
        compat_profile,
        config,
        PromptSteeringEndpointKind::CountTokens,
    ) {
        return false;
    }
    apply_to_system(&mut payload.system, config)
}

pub(crate) fn should_apply_to_external_pool(
    endpoint: &str,
    compat_profile: CompatProfile,
    config: &PromptSteeringConfig,
) -> bool {
    config.apply_to_external_pool
        && should_apply(
            endpoint,
            compat_profile,
            config,
            PromptSteeringEndpointKind::Messages,
        )
}

pub(crate) fn language_turn_reminder_applies(
    endpoint: &str,
    compat_profile: CompatProfile,
    config: &PromptSteeringConfig,
) -> bool {
    let config = config.clone().normalized();
    should_apply(
        endpoint,
        compat_profile,
        &config,
        PromptSteeringEndpointKind::Messages,
    ) && config.language_constraint.enabled
        && config.language_turn_reminder.enabled
}

fn should_apply(
    endpoint: &str,
    compat_profile: CompatProfile,
    config: &PromptSteeringConfig,
    kind: PromptSteeringEndpointKind,
) -> bool {
    let config = config.clone().normalized();
    if !config.enabled || compat_profile.is_strict() {
        return false;
    }
    if kind == PromptSteeringEndpointKind::CountTokens && !config.apply_to_count_tokens {
        return false;
    }

    match config.scope {
        PromptSteeringScope::RouteRules | PromptSteeringScope::CcOnly => {
            prompt_steering_route_allowed(endpoint, &config)
        }
        PromptSteeringScope::ClaudeCodeProfile => {
            matches!(
                compat_profile,
                CompatProfile::ClaudeCode | CompatProfile::Debug
            )
        }
        PromptSteeringScope::AllRoutes => true,
    }
}

fn prompt_steering_route_allowed(endpoint: &str, config: &PromptSteeringConfig) -> bool {
    match config.route_mode {
        PromptSteeringRouteMode::AllowAll => true,
        PromptSteeringRouteMode::AllowList => config
            .route_rules
            .iter()
            .any(|rule| route_rule_matches(rule, endpoint)),
        PromptSteeringRouteMode::DenyList => !config
            .route_rules
            .iter()
            .any(|rule| route_rule_matches(rule, endpoint)),
    }
}

fn apply_to_system(system: &mut Option<Vec<SystemMessage>>, config: &PromptSteeringConfig) -> bool {
    if system_already_has_prompt_steering(system.as_deref()) {
        return false;
    }

    let Some(prompt) = build_request_level_prompt(config) else {
        return false;
    };

    let block = SystemMessage {
        text: prompt,
        cache_control: None,
    };
    match system {
        Some(system) => {
            // Keep the identity block first regardless of which of the two is applied first.
            let index = usize::from(
                system
                    .first()
                    .is_some_and(|block| block.text.contains(IDENTITY_PROMPT_MARKER)),
            );
            system.insert(index, block);
        }
        None => *system = Some(vec![block]),
    }
    true
}

fn build_request_level_prompt(config: &PromptSteeringConfig) -> Option<String> {
    let config = config.clone().normalized();
    let mut blocks = Vec::new();

    if config.language_constraint.enabled {
        let prompt = config.language_constraint.prompt.trim();
        if !prompt.is_empty() {
            blocks.push(prompt.to_string());
        }
    }
    if config.task_quality.enabled {
        let prompt = config.task_quality.prompt.trim();
        if !prompt.is_empty() {
            blocks.push(prompt.to_string());
        }
    }
    if config.custom.enabled {
        let prompt = config.custom.prompt.trim();
        if !prompt.is_empty() {
            blocks.push(prompt.to_string());
        }
    }

    if blocks.is_empty() {
        return None;
    }

    Some(format!(
        "{}\n{}\n{}",
        PROMPT_STEERING_MARKER,
        blocks.join("\n\n"),
        PROMPT_STEERING_END_MARKER
    ))
}

fn system_already_has_prompt_steering(system: Option<&[SystemMessage]>) -> bool {
    system.is_some_and(|blocks| {
        blocks
            .iter()
            .any(|block| block.text.contains(PROMPT_STEERING_MARKER))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::types::{Message, MessagesRequest};

    fn request() -> MessagesRequest {
        MessagesRequest {
            model: "claude-sonnet-4-5".to_string(),
            max_tokens: 16,
            messages: vec![Message {
                role: "user".to_string(),
                content: serde_json::json!("hello"),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            output_format: None,
            metadata: None,
            stop_sequences: None,
        }
    }

    fn identity_config() -> PromptSteeringConfig {
        let mut config = PromptSteeringConfig::default();
        config.identity.enabled = true;
        config
    }

    #[test]
    fn identity_is_off_by_default() {
        let mut req = request();
        assert!(!apply_identity_to_messages_request(
            "/v1/messages",
            &PromptSteeringConfig::default(),
            &mut req
        ));
        assert!(req.system.is_none());
        assert!(
            PromptSteeringConfig::default()
                .identity
                .turn_reminder_text()
                .is_none()
        );
    }

    #[test]
    fn identity_ignores_master_switch_route_rules_and_strict_profile() {
        let mut config = identity_config();
        config.enabled = false;
        config.route_mode = PromptSteeringRouteMode::AllowList;
        config.route_rules = vec!["/cc".to_string()];

        // `/v1` is outside the steering route rules and the steering master is off, yet
        // identity still applies: the served identity must not depend on the route.
        let mut req = request();
        assert!(!apply_to_messages_request(
            "/v1/messages",
            CompatProfile::AnthropicStrict,
            &config,
            &mut req,
        ));
        assert!(apply_identity_to_messages_request(
            "/v1/messages",
            &config,
            &mut req
        ));
        let system = req.system.expect("identity injected");
        assert_eq!(system.len(), 1);
        assert!(system[0].text.starts_with(IDENTITY_PROMPT_MARKER));
        assert!(system[0].text.ends_with(IDENTITY_PROMPT_END_MARKER));
        assert!(
            system[0]
                .text
                .contains(crate::model::config::DEFAULT_IDENTITY_PROMPT)
        );
    }

    #[test]
    fn identity_stays_first_and_is_not_duplicated() {
        let config = identity_config();
        for steering_first in [true, false] {
            let mut req = request();
            req.system = Some(vec![SystemMessage {
                text: "client system".to_string(),
                cache_control: None,
            }]);
            let apply_steering = |req: &mut MessagesRequest| {
                apply_to_messages_request(
                    "/cc/v1/messages",
                    CompatProfile::ClaudeCode,
                    &config,
                    req,
                )
            };
            if steering_first {
                assert!(apply_steering(&mut req));
                assert!(apply_identity_to_messages_request(
                    "/v1/messages",
                    &config,
                    &mut req
                ));
            } else {
                assert!(apply_identity_to_messages_request(
                    "/v1/messages",
                    &config,
                    &mut req
                ));
                assert!(apply_steering(&mut req));
            }
            // Repeated processing (e.g. a retry through the same pipeline) is a no-op.
            assert!(!apply_identity_to_messages_request(
                "/v1/messages",
                &config,
                &mut req
            ));

            let system = req.system.expect("system kept");
            assert_eq!(system.len(), 3, "steering_first={steering_first}");
            assert!(system[0].text.contains(IDENTITY_PROMPT_MARKER));
            assert!(system[1].text.contains(PROMPT_STEERING_MARKER));
            assert_eq!(system[2].text, "client system");
        }
    }

    #[test]
    fn identity_blank_prompts_fall_back_to_defaults() {
        let mut config = identity_config();
        config.identity.prompt = "   ".to_string();
        config.identity.turn_reminder.prompt = String::new();
        let config = config.normalized();
        assert_eq!(
            config.identity.prompt,
            crate::model::config::DEFAULT_IDENTITY_PROMPT
        );
        assert_eq!(
            config.identity.turn_reminder_text(),
            Some(crate::model::config::DEFAULT_IDENTITY_TURN_REMINDER)
        );

        let mut custom = identity_config();
        custom.identity.prompt = "  You are Claude.  ".to_string();
        custom.identity.turn_reminder.enabled = false;
        let custom = custom.normalized();
        assert_eq!(custom.identity.prompt, "You are Claude.");
        assert!(custom.identity.turn_reminder_text().is_none());
    }

    #[test]
    fn identity_count_tokens_follows_apply_to_count_tokens() {
        let payload = || CountTokensRequest {
            model: "claude-sonnet-4-5".to_string(),
            messages: request().messages,
            system: None,
            tools: None,
        };
        let mut config = identity_config();
        let mut counted = payload();
        assert!(apply_identity_to_count_tokens_request(
            "/v1/messages/count_tokens",
            &config,
            &mut counted
        ));
        assert!(counted.system.is_some());

        config.apply_to_count_tokens = false;
        let mut skipped = payload();
        assert!(!apply_identity_to_count_tokens_request(
            "/v1/messages/count_tokens",
            &config,
            &mut skipped
        ));
        assert!(skipped.system.is_none());
    }

    #[test]
    fn identity_follows_its_own_route_binding() {
        let mut config = identity_config();
        // Default: every route.
        for endpoint in ["/v1/messages", "/cc/v1/messages", "/na/v1/messages"] {
            let mut req = request();
            assert!(
                apply_identity_to_messages_request(endpoint, &config, &mut req),
                "{endpoint}"
            );
        }

        config.identity.route_mode = PromptSteeringRouteMode::AllowList;
        config.identity.route_rules = vec!["/cc".to_string()];
        let mut on = request();
        assert!(apply_identity_to_messages_request(
            "/cc/v1/messages",
            &config,
            &mut on
        ));
        let mut off = request();
        assert!(!apply_identity_to_messages_request(
            "/v1/messages",
            &config,
            &mut off
        ));
        assert!(off.system.is_none());
        assert!(
            !config
                .identity
                .for_route("/v1/messages")
                .output_guard_applies(false)
        );
        assert!(
            config
                .identity
                .for_route("/cc/v1/messages")
                .output_guard_applies(true)
        );

        config.identity.route_mode = PromptSteeringRouteMode::DenyList;
        let mut denied = request();
        assert!(!apply_identity_to_messages_request(
            "/cc/v1/messages",
            &config,
            &mut denied
        ));
        let mut allowed = request();
        assert!(apply_identity_to_messages_request(
            "/v1/messages",
            &config,
            &mut allowed
        ));
        assert!(!identity_applies_to_external_pool("/cc/v1/messages", &{
            let mut c = config.clone();
            c.identity.apply_to_external_pool = true;
            c
        }));
    }

    #[test]
    fn identity_route_overrides_win_over_defaults_by_longest_prefix() {
        use crate::model::config::IdentityRouteOverride;
        let mut config = PromptSteeringConfig::default();
        // Global default off; /cc turns it on with only the output guard, /cc/strict turns it off.
        config.identity.path_overrides.insert(
            "/cc".to_string(),
            IdentityRouteOverride {
                enabled: Some(true),
                prompt_enabled: Some(false),
                turn_reminder_enabled: Some(false),
                ..IdentityRouteOverride::default()
            },
        );
        config.identity.path_overrides.insert(
            "cc/strict/".to_string(),
            IdentityRouteOverride {
                enabled: Some(false),
                ..IdentityRouteOverride::default()
            },
        );
        config
            .identity
            .path_overrides
            .insert("/empty".to_string(), IdentityRouteOverride::default());
        let config = config.normalized();
        assert!(config.identity.path_overrides.contains_key("/cc/strict"));
        assert!(!config.identity.path_overrides.contains_key("/empty"));

        let cc = config.identity.for_route("/cc/v1/messages");
        assert!(cc.enabled);
        assert!(!cc.system_prompt_applies());
        assert!(cc.turn_reminder_text().is_none());
        assert!(cc.output_guard_applies(true));
        let mut req = request();
        assert!(!apply_identity_to_messages_request(
            "/cc/v1/messages",
            &config,
            &mut req
        ));

        assert!(!config.identity.for_route("/cc/strict/v1/messages").enabled);
        assert!(!config.identity.for_route("/v1/messages").enabled);
        // "/ccx" is a different route, not a child of "/cc".
        assert!(!config.identity.for_route("/ccx/v1/messages").enabled);

        // A route override without `enabled` inherits the global switch and route rules.
        let mut inherit = PromptSteeringConfig::default();
        inherit.identity.enabled = true;
        inherit.identity.path_overrides.insert(
            "/na".to_string(),
            IdentityRouteOverride {
                output_guard_stream_enabled: Some(false),
                apply_to_external_pool: Some(true),
                ..IdentityRouteOverride::default()
            },
        );
        let na = inherit.identity.for_route("/na/v1/messages");
        assert!(na.enabled);
        assert!(na.output_guard_applies(false));
        assert!(!na.output_guard_applies(true));
        assert!(identity_applies_to_external_pool(
            "/na/v1/messages",
            &inherit
        ));
        assert!(!identity_applies_to_external_pool("/v1/messages", &inherit));
    }

    #[test]
    fn identity_sub_switches_are_independent() {
        let mut config = identity_config();
        config.identity.prompt_enabled = false;
        let mut req = request();
        assert!(!apply_identity_to_messages_request(
            "/v1/messages",
            &config,
            &mut req
        ));
        let identity = config.identity.for_route("/v1/messages");
        assert!(identity.turn_reminder_text().is_some());
        assert!(identity.output_guard_applies(false));

        config.identity.output_guard.stream_enabled = false;
        let identity = config.identity.for_route("/v1/messages");
        assert!(identity.output_guard_applies(false));
        assert!(!identity.output_guard_applies(true));

        config.identity.enabled = false;
        let identity = config.identity.for_route("/v1/messages");
        assert!(identity.turn_reminder_text().is_none());
        assert!(!identity.output_guard_applies(false));
    }

    #[test]
    fn identity_external_pool_is_opt_in() {
        let mut config = identity_config();
        assert!(!identity_applies_to_external_pool("/v1/messages", &config));
        config.identity.apply_to_external_pool = true;
        assert!(identity_applies_to_external_pool("/v1/messages", &config));
        config.identity.enabled = false;
        assert!(!identity_applies_to_external_pool("/v1/messages", &config));
    }

    #[test]
    fn cc_messages_get_default_task_prompt_without_language_prompt() {
        let mut req = request();

        assert!(apply_to_messages_request(
            "/cc/v1/messages",
            CompatProfile::ClaudeCode,
            &PromptSteeringConfig::default(),
            &mut req,
        ));

        let system = req.system.expect("system injected");
        assert_eq!(system.len(), 1);
        assert!(system[0].text.contains(PROMPT_STEERING_MARKER));
        assert!(system[0].text.contains("任务"));
        assert!(!system[0].text.contains("<language_constraint>"));
    }

    #[test]
    fn language_prompt_is_injected_once_at_the_top_only_when_enabled() {
        let mut config = PromptSteeringConfig::default();
        config.task_quality.enabled = false;

        // Disabled with nothing else enabled: no system block at all.
        let mut off = request();
        assert!(!apply_to_messages_request(
            "/cc/v1/messages",
            CompatProfile::ClaudeCode,
            &config,
            &mut off,
        ));
        assert!(off.system.is_none());

        config.language_constraint.enabled = true;
        let mut on = request();
        on.system = Some(vec![SystemMessage {
            text: "client system".to_string(),
            cache_control: None,
        }]);
        assert!(apply_to_messages_request(
            "/cc/v1/messages",
            CompatProfile::ClaudeCode,
            &config,
            &mut on,
        ));
        let system = on.system.as_ref().expect("system kept");
        assert_eq!(system.len(), 2);
        assert!(system[0].text.contains("<language_constraint>"));
        assert!(system[0].text.contains("client settings"));
        assert_eq!(system[1].text, "client system");
        // The current user turn is never touched.
        assert_eq!(
            serde_json::to_value(&on.messages).unwrap(),
            serde_json::to_value(&request().messages).unwrap()
        );

        assert!(!apply_to_messages_request(
            "/cc/v1/messages",
            CompatProfile::ClaudeCode,
            &config,
            &mut on,
        ));
        assert_eq!(
            on.system
                .iter()
                .flatten()
                .filter(|block| block.text.contains("<language_constraint>"))
                .count(),
            1
        );
    }

    #[test]
    fn v1_messages_do_not_get_default_route_rule_prompt() {
        let mut req = request();

        assert!(!apply_to_messages_request(
            "/v1/messages",
            CompatProfile::ClaudeCode,
            &PromptSteeringConfig::default(),
            &mut req,
        ));

        assert!(req.system.is_none());
    }

    #[test]
    fn language_turn_reminder_uses_prompt_steering_route_and_language_gates() {
        let mut config = PromptSteeringConfig::default();
        config.language_constraint.enabled = true;

        assert!(language_turn_reminder_applies(
            "/cc/v1/messages",
            CompatProfile::ClaudeCode,
            &config,
        ));
        assert!(!language_turn_reminder_applies(
            "/v1/messages",
            CompatProfile::ClaudeCode,
            &config,
        ));
        assert!(!language_turn_reminder_applies(
            "/cc/v1/messages",
            CompatProfile::AnthropicStrict,
            &config,
        ));

        config.enabled = false;
        assert!(!language_turn_reminder_applies(
            "/cc/v1/messages",
            CompatProfile::ClaudeCode,
            &config,
        ));
        config.enabled = true;

        config.language_turn_reminder.enabled = false;
        assert!(!language_turn_reminder_applies(
            "/cc/v1/messages",
            CompatProfile::ClaudeCode,
            &config,
        ));
    }

    #[test]
    fn prompt_steering_route_rules_are_config_driven_for_all_paths() {
        let mut config = PromptSteeringConfig {
            route_rules: vec!["/ha".to_string()],
            ..PromptSteeringConfig::default()
        }
        .normalized();
        config.custom.enabled = true;
        config.custom.prompt = "route-rule-test".to_string();

        let mut cc = request();
        assert!(!apply_to_messages_request(
            "/cc/v1/messages",
            CompatProfile::ClaudeCode,
            &config,
            &mut cc,
        ));
        assert!(cc.system.is_none());

        let mut ha = request();
        assert!(apply_to_messages_request(
            "/ha/v1/messages",
            CompatProfile::ClaudeCode,
            &config,
            &mut ha,
        ));
        assert!(
            ha.system
                .as_ref()
                .is_some_and(|system| system[0].text.contains("route-rule-test"))
        );

        let mut count_tokens = CountTokensRequest {
            model: ha.model.clone(),
            messages: ha.messages.clone(),
            system: None,
            tools: ha.tools.clone(),
        };
        assert!(apply_to_count_tokens_request(
            "/ha/v1/messages/count_tokens",
            CompatProfile::ClaudeCode,
            &config,
            &mut count_tokens,
        ));
        assert!(
            count_tokens
                .system
                .as_ref()
                .is_some_and(|system| system[0].text.contains("route-rule-test"))
        );
    }

    #[test]
    fn external_pool_prompt_steering_is_opt_in() {
        let config = PromptSteeringConfig::default();
        assert!(!should_apply_to_external_pool(
            "/cc/v1/messages",
            CompatProfile::ClaudeCode,
            &config,
        ));

        let mut enabled = config;
        enabled.apply_to_external_pool = true;
        assert!(should_apply_to_external_pool(
            "/cc/v1/messages",
            CompatProfile::ClaudeCode,
            &enabled,
        ));
        assert!(!should_apply_to_external_pool(
            "/v1/messages",
            CompatProfile::ClaudeCode,
            &enabled,
        ));
        assert!(!should_apply_to_external_pool(
            "/cc/v1/messages",
            CompatProfile::AnthropicStrict,
            &enabled,
        ));
    }

    #[test]
    fn strict_profile_never_gets_prompt_steering() {
        let mut req = request();

        assert!(!apply_to_messages_request(
            "/cc/v1/messages",
            CompatProfile::AnthropicStrict,
            &PromptSteeringConfig::default(),
            &mut req,
        ));

        assert!(req.system.is_none());
    }

    #[test]
    fn prompt_steering_is_not_injected_twice() {
        let mut req = request();
        assert!(apply_to_messages_request(
            "/cc/v1/messages",
            CompatProfile::ClaudeCode,
            &PromptSteeringConfig::default(),
            &mut req,
        ));

        assert!(!apply_to_messages_request(
            "/cc/v1/messages",
            CompatProfile::ClaudeCode,
            &PromptSteeringConfig::default(),
            &mut req,
        ));
        assert_eq!(req.system.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn messages_and_count_tokens_share_operator_prompt_policy_for_five_rounds() {
        let scopes = [
            PromptSteeringScope::RouteRules,
            PromptSteeringScope::ClaudeCodeProfile,
            PromptSteeringScope::AllRoutes,
        ];
        let profiles = [CompatProfile::ClaudeCode, CompatProfile::AnthropicStrict];
        let endpoints = [
            ("/cc/v1/messages", "/cc/v1/messages/count_tokens"),
            ("/v1/messages", "/v1/messages/count_tokens"),
        ];

        for round in 0..5 {
            for enabled in [false, true] {
                for apply_to_count_tokens in [false, true] {
                    for scope in scopes {
                        for profile in profiles {
                            for (messages_endpoint, count_endpoint) in endpoints {
                                let mut config = PromptSteeringConfig {
                                    enabled,
                                    scope,
                                    apply_to_count_tokens,
                                    ..PromptSteeringConfig::default()
                                };
                                config.custom.enabled = true;
                                config.custom.prompt = format!("round-{round}");

                                let mut messages = request();
                                let messages_applied = apply_to_messages_request(
                                    messages_endpoint,
                                    profile,
                                    &config,
                                    &mut messages,
                                );
                                let mut count_tokens = CountTokensRequest {
                                    model: messages.model.clone(),
                                    messages: messages.messages.clone(),
                                    system: None,
                                    tools: messages.tools.clone(),
                                };
                                let count_applied = apply_to_count_tokens_request(
                                    count_endpoint,
                                    profile,
                                    &config,
                                    &mut count_tokens,
                                );

                                assert_eq!(
                                    count_applied,
                                    messages_applied && apply_to_count_tokens,
                                    "round {round}: messages={messages_endpoint}, count={count_endpoint}, enabled={enabled}, count_enabled={apply_to_count_tokens}, scope={scope:?}, profile={profile:?}"
                                );
                                if count_applied {
                                    assert_eq!(
                                        count_tokens.system.as_ref().map(|blocks| blocks
                                            .iter()
                                            .map(|block| block.text.as_str())
                                            .collect::<Vec<_>>()),
                                        messages.system.as_ref().map(|blocks| blocks
                                            .iter()
                                            .map(|block| block.text.as_str())
                                            .collect::<Vec<_>>()),
                                        "round {round}: count_tokens must inject the exact messages operator prompt"
                                    );
                                } else {
                                    assert!(count_tokens.system.is_none());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
