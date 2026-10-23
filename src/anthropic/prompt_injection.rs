//! 把写进 system 的注入项加到请求上：身份声明（A5）放在最前面，任务质量、语言约束、自定义
//! （A1/A2/A4）合成一个代理提示块放在它后面。是否注入由 [`ResolvedInjections`] 决定。
//!
//! 两个块各带 marker，重复处理（例如重试走同一条管线）时不会重复注入。块的格式与升级前一致，
//! 已有会话的缓存前缀不受影响。

use crate::anthropic::types::{CountTokensRequest, MessagesRequest, SystemMessage};
use crate::model::config::{InjectionId, PromptInjectionConfig, ResolvedInjections};
use crate::model::prompt_injection::TextKey;
use crate::model::prompt_injection::catalog::{
    IDENTITY_PROMPT_END_MARKER, IDENTITY_PROMPT_MARKER, PROMPT_STEERING_END_MARKER,
    PROMPT_STEERING_MARKER,
};

/// 实际加到 system 里的项。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AppliedSystemInjections {
    pub(crate) items: Vec<InjectionId>,
}

pub(crate) fn apply_to_messages_request(
    injections: &ResolvedInjections,
    payload: &mut MessagesRequest,
) -> AppliedSystemInjections {
    apply_to_system(&mut payload.system, injections)
}

pub(crate) fn apply_to_count_tokens_request(
    config: &PromptInjectionConfig,
    injections: &ResolvedInjections,
    payload: &mut CountTokensRequest,
) -> AppliedSystemInjections {
    if !config.apply_to_count_tokens {
        return AppliedSystemInjections::default();
    }
    apply_to_system(&mut payload.system, injections)
}

fn apply_to_system(
    system: &mut Option<Vec<SystemMessage>>,
    injections: &ResolvedInjections,
) -> AppliedSystemInjections {
    let mut applied = AppliedSystemInjections::default();
    if injections.allows(InjectionId::IdentityPrompt)
        && !system_contains(system, IDENTITY_PROMPT_MARKER)
    {
        let block = SystemMessage {
            text: format!(
                "{}\n{}\n{}",
                IDENTITY_PROMPT_MARKER,
                injections.text(TextKey::IdentityPrompt),
                IDENTITY_PROMPT_END_MARKER
            ),
            cache_control: None,
        };
        system.get_or_insert_with(Vec::new).insert(0, block);
        applied.items.push(InjectionId::IdentityPrompt);
    }
    if !system_contains(system, PROMPT_STEERING_MARKER)
        && let Some((text, items)) = request_prompt_block(injections)
    {
        let block = SystemMessage {
            text,
            cache_control: None,
        };
        let system = system.get_or_insert_with(Vec::new);
        // 身份块始终在第一位，无论两个块谁先加。
        let index = usize::from(
            system
                .first()
                .is_some_and(|block| block.text.contains(IDENTITY_PROMPT_MARKER)),
        );
        system.insert(index, block);
        applied.items.extend(items);
    }
    applied
}

/// 代理提示块：语言约束、任务质量、自定义，顺序与升级前一致。
fn request_prompt_block(injections: &ResolvedInjections) -> Option<(String, Vec<InjectionId>)> {
    let mut blocks = Vec::new();
    let mut items = Vec::new();
    for (id, key) in [
        (InjectionId::LanguageConstraint, TextKey::LanguageConstraint),
        (InjectionId::TaskQuality, TextKey::TaskQuality),
        (InjectionId::Custom, TextKey::Custom),
    ] {
        if !injections.allows(id) {
            continue;
        }
        let text = injections.text(key).trim();
        if text.is_empty() {
            continue;
        }
        blocks.push(text.to_string());
        items.push(id);
    }
    if blocks.is_empty() {
        return None;
    }
    Some((
        format!(
            "{}\n{}\n{}",
            PROMPT_STEERING_MARKER,
            blocks.join("\n\n"),
            PROMPT_STEERING_END_MARKER
        ),
        items,
    ))
}

fn system_contains(system: &Option<Vec<SystemMessage>>, marker: &str) -> bool {
    system
        .as_deref()
        .is_some_and(|blocks| blocks.iter().any(|block| block.text.contains(marker)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::types::Message;
    use crate::model::config::{CompatProfile, UpstreamKind};
    use crate::model::prompt_injection::catalog::{
        DEFAULT_IDENTITY_PROMPT, DEFAULT_LANGUAGE_CONSTRAINT_PROMPT, DEFAULT_TASK_QUALITY_PROMPT,
    };
    use crate::model::prompt_injection::{ItemOverride, ItemSetting};

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

    fn set(config: &mut PromptInjectionConfig, id: InjectionId, enabled: bool) {
        config.defaults.insert(
            id,
            ItemSetting {
                enabled,
                external_pool: id.supports_external_pool().then_some(false),
            },
        );
    }

    fn resolve(config: &PromptInjectionConfig, endpoint: &str) -> ResolvedInjections {
        ResolvedInjections::resolve(
            endpoint,
            UpstreamKind::Local,
            CompatProfile::ClaudeCode,
            config,
        )
    }

    #[test]
    fn fresh_install_injects_task_quality_only_on_cc() {
        let config = PromptInjectionConfig::default();
        let mut cc = request();
        let applied = apply_to_messages_request(&resolve(&config, "/cc/v1/messages"), &mut cc);
        assert_eq!(applied.items, vec![InjectionId::TaskQuality]);
        let system = cc.system.expect("cc system");
        assert_eq!(system.len(), 1);
        assert_eq!(
            system[0].text,
            format!(
                "{PROMPT_STEERING_MARKER}\n{}\n{PROMPT_STEERING_END_MARKER}",
                DEFAULT_TASK_QUALITY_PROMPT.trim()
            )
        );

        let mut v1 = request();
        let applied = apply_to_messages_request(&resolve(&config, "/v1/messages"), &mut v1);
        assert!(applied.items.is_empty());
        assert!(v1.system.is_none());
    }

    #[test]
    fn identity_stays_first_and_blocks_are_not_duplicated() {
        let mut config = PromptInjectionConfig::default();
        set(&mut config, InjectionId::IdentityPrompt, true);
        set(&mut config, InjectionId::LanguageConstraint, true);
        let injections = resolve(&config, "/cc/v1/messages");
        let mut req = request();
        req.system = Some(vec![SystemMessage {
            text: "client system".to_string(),
            cache_control: None,
        }]);
        let applied = apply_to_messages_request(&injections, &mut req);
        assert_eq!(
            applied.items,
            vec![
                InjectionId::IdentityPrompt,
                InjectionId::LanguageConstraint,
                InjectionId::TaskQuality
            ]
        );
        // 重复处理不再注入。
        assert!(
            apply_to_messages_request(&injections, &mut req)
                .items
                .is_empty()
        );
        let system = req.system.expect("system");
        assert_eq!(system.len(), 3);
        assert_eq!(
            system[0].text,
            format!(
                "{IDENTITY_PROMPT_MARKER}\n{DEFAULT_IDENTITY_PROMPT}\n{IDENTITY_PROMPT_END_MARKER}"
            )
        );
        assert!(system[1].text.starts_with(PROMPT_STEERING_MARKER));
        let language = system[1]
            .text
            .find(DEFAULT_LANGUAGE_CONSTRAINT_PROMPT.trim())
            .unwrap();
        let task = system[1].text.find("<task_quality_policy>").unwrap();
        assert!(language < task, "language block stays before task quality");
        assert_eq!(system[2].text, "client system");
    }

    #[test]
    fn each_system_item_switches_independently() {
        let all = [
            InjectionId::IdentityPrompt,
            InjectionId::LanguageConstraint,
            InjectionId::TaskQuality,
            InjectionId::Custom,
        ];
        for off in all {
            let mut config = PromptInjectionConfig::default();
            config.path_overrides.clear();
            config
                .texts
                .insert(TextKey::Custom, "custom text".to_string());
            for id in all {
                set(&mut config, id, id != off);
            }
            let mut req = request();
            let applied = apply_to_messages_request(&resolve(&config, "/v1/messages"), &mut req);
            let expected: Vec<_> = [
                InjectionId::IdentityPrompt,
                InjectionId::LanguageConstraint,
                InjectionId::TaskQuality,
                InjectionId::Custom,
            ]
            .into_iter()
            .filter(|id| *id != off)
            .collect();
            assert_eq!(applied.items, expected, "off={off:?}");
        }
    }

    #[test]
    fn route_override_wins_over_default_and_longest_prefix_wins() {
        let mut config = PromptInjectionConfig::default();
        config.path_overrides.clear();
        set(&mut config, InjectionId::TaskQuality, true);
        config.path_overrides.insert(
            "/dfcache".to_string(),
            [(
                InjectionId::TaskQuality,
                ItemOverride {
                    enabled: Some(false),
                    external_pool: None,
                },
            )]
            .into(),
        );
        config.path_overrides.insert(
            "/dfcache/a".to_string(),
            [(
                InjectionId::TaskQuality,
                ItemOverride {
                    enabled: Some(true),
                    external_pool: None,
                },
            )]
            .into(),
        );
        let config = config.normalized();
        for (endpoint, expected) in [
            ("/v1/messages", true),
            ("/dfcache/b/v1/messages", false),
            ("/dfcache/a/v1/messages", true),
        ] {
            assert_eq!(
                resolve(&config, endpoint).allows(InjectionId::TaskQuality),
                expected,
                "{endpoint}"
            );
        }
    }

    #[test]
    fn count_tokens_follows_apply_to_count_tokens() {
        let mut config = PromptInjectionConfig::default();
        let injections = resolve(&config, "/cc/v1/messages/count_tokens");
        let payload = || CountTokensRequest {
            model: "claude-sonnet-4-5".to_string(),
            messages: request().messages,
            system: None,
            tools: None,
        };
        let mut counted = payload();
        assert!(
            !apply_to_count_tokens_request(&config, &injections, &mut counted)
                .items
                .is_empty()
        );
        config.apply_to_count_tokens = false;
        let mut skipped = payload();
        assert!(
            apply_to_count_tokens_request(&config, &injections, &mut skipped)
                .items
                .is_empty()
        );
        assert!(skipped.system.is_none());
    }

    #[test]
    fn strict_blocks_request_prompts_but_not_identity() {
        let mut config = PromptInjectionConfig::default();
        set(&mut config, InjectionId::IdentityPrompt, true);
        let injections = ResolvedInjections::resolve(
            "/cc/v1/messages",
            UpstreamKind::Local,
            CompatProfile::AnthropicStrict,
            &config,
        );
        let mut req = request();
        let applied = apply_to_messages_request(&injections, &mut req);
        assert_eq!(applied.items, vec![InjectionId::IdentityPrompt]);
    }

    #[test]
    fn external_pool_needs_its_own_switch() {
        let mut config = PromptInjectionConfig::default();
        set(&mut config, InjectionId::IdentityPrompt, true);
        let external = |config: &PromptInjectionConfig| {
            ResolvedInjections::resolve(
                "/cc/v1/messages",
                UpstreamKind::ExternalPool,
                CompatProfile::ClaudeCode,
                config,
            )
        };
        let injections = external(&config);
        assert!(!injections.allows(InjectionId::IdentityPrompt));
        assert!(!injections.allows(InjectionId::TaskQuality));
        assert!(!injections.allows(InjectionId::ChunkedWritePolicy));
        config.defaults.insert(
            InjectionId::IdentityPrompt,
            ItemSetting {
                enabled: true,
                external_pool: Some(true),
            },
        );
        let injections = external(&config);
        assert!(injections.allows(InjectionId::IdentityPrompt));
        assert!(!injections.allows(InjectionId::TaskQuality));
    }

    #[test]
    fn edited_text_is_injected_and_empty_custom_is_skipped() {
        let mut config = PromptInjectionConfig::default();
        config.path_overrides.clear();
        set(&mut config, InjectionId::TaskQuality, true);
        set(&mut config, InjectionId::Custom, true);
        config
            .texts
            .insert(TextKey::TaskQuality, "edited task".to_string());
        let mut req = request();
        let applied = apply_to_messages_request(&resolve(&config, "/v1/messages"), &mut req);
        assert_eq!(applied.items, vec![InjectionId::TaskQuality]);
        assert!(req.system.unwrap()[0].text.contains("\nedited task\n"));
    }
}
