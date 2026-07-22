//! Request-level prompt steering for Claude Code compatibility.
//!
//! This module injects operator-facing guidance. The same runtime `promptSteering.enabled`
//! switch is also consumed by the local converter and parsed-body pipeline as the master gate
//! for proxy-added compatibility prompts. Client-provided structured fields are not removed.

use crate::anthropic::types::{CountTokensRequest, MessagesRequest, SystemMessage};
use crate::model::config::{
    CompatProfile, PROMPT_STEERING_END_MARKER, PROMPT_STEERING_MARKER, PromptSteeringConfig,
    PromptSteeringScope,
};

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
        PromptSteeringScope::CcOnly => is_cc_endpoint(endpoint),
        PromptSteeringScope::ClaudeCodeProfile => {
            matches!(
                compat_profile,
                CompatProfile::ClaudeCode | CompatProfile::Debug
            )
        }
        PromptSteeringScope::AllRoutes => true,
    }
}

fn is_cc_endpoint(endpoint: &str) -> bool {
    endpoint.starts_with("/cc/")
        || endpoint == "/cc/v1/messages"
        || endpoint == "/cc/v1/messages/count_tokens"
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
        Some(system) => system.insert(0, block),
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
            metadata: None,
        }
    }

    #[test]
    fn cc_messages_get_default_language_and_task_prompt() {
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
        assert!(system[0].text.contains("Do not mix languages"));
        assert!(system[0].text.contains("任务"));
    }

    #[test]
    fn v1_messages_do_not_get_default_cc_only_prompt() {
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
            PromptSteeringScope::CcOnly,
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
