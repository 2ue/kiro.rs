use crate::anthropic::types::MessagesRequest;

/// Claude Code 在请求中声明的语言偏好。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClientLanguagePreference {
    /// 客户端设置了语言，值为清洗后的语言名。
    Explicit(String),
    /// 客户端未设置，或最后一次操作是清除。
    FollowUser,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LanguageEvent {
    Explicit(String),
    Cleared,
}

/// 从 Claude Code 请求中识别最后一次语言设置操作。
pub(crate) fn detect_client_language_preference(req: &MessagesRequest) -> ClientLanguagePreference {
    let mut last_event = None;

    if let Some(system) = &req.system {
        for message in system {
            for event in language_events(&message.text) {
                last_event = Some(event);
            }
        }
    }

    for message in &req.messages {
        if message.role != "user" {
            continue;
        }
        for text in user_text_blocks(&message.content) {
            for event in language_events(text) {
                last_event = Some(event);
            }
        }
    }

    match last_event {
        Some(LanguageEvent::Explicit(language)) => ClientLanguagePreference::Explicit(language),
        Some(LanguageEvent::Cleared) | None => ClientLanguagePreference::FollowUser,
    }
}

fn user_text_blocks(content: &serde_json::Value) -> Vec<&str> {
    match content {
        serde_json::Value::String(text) => vec![text.as_str()],
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(serde_json::Value::as_str) != Some("text") {
                    return None;
                }
                block.get("text").and_then(serde_json::Value::as_str)
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn language_events(text: &str) -> Vec<LanguageEvent> {
    let mut events = Vec::new();

    for (position, _) in text.match_indices("The language preference was cleared") {
        events.push((position, LanguageEvent::Cleared));
    }

    for (position, _) in text.match_indices("Always respond in ") {
        let prefix = &text[..position];
        let Some(marker_position) = prefix.rfind("# Language") else {
            continue;
        };
        if !prefix[marker_position + "# Language".len()..]
            .chars()
            .all(char::is_whitespace)
        {
            continue;
        }

        let value_start = position + "Always respond in ".len();
        let value_end = text[value_start..]
            .char_indices()
            .find_map(|(offset, character)| {
                (character == '.' || character == '\n').then_some(value_start + offset)
            })
            .unwrap_or(text.len());
        if let Some(language) = clean_language(&text[value_start..value_end]) {
            events.push((position, LanguageEvent::Explicit(language)));
        }
    }

    events.sort_by_key(|(position, _)| *position);
    events.into_iter().map(|(_, event)| event).collect()
}

fn clean_language(value: &str) -> Option<String> {
    let cleaned: String = value
        .trim()
        .chars()
        .filter(|character| !matches!(character, '<' | '>' | '\r' | '\n' | '`'))
        .take(64)
        .collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::types::{Message, MessagesRequest, SystemMessage};

    fn request(messages: Vec<Message>, system: Option<Vec<SystemMessage>>) -> MessagesRequest {
        MessagesRequest {
            model: "test".to_string(),
            max_tokens: 1,
            messages,
            stream: false,
            system,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            output_format: None,
            metadata: None,
            stop_sequences: None,
        }
    }

    fn user(content: serde_json::Value) -> Message {
        Message {
            role: "user".to_string(),
            content,
        }
    }

    #[test]
    fn no_setting_follows_user() {
        let req = request(vec![user(serde_json::json!("hello"))], None);
        assert_eq!(
            detect_client_language_preference(&req),
            ClientLanguagePreference::FollowUser
        );
    }

    #[test]
    fn user_string_content_detects_explicit_language() {
        let req = request(
            vec![user(serde_json::json!(
                "# Language\nAlways respond in Chinese. Use Chinese for all explanations."
            ))],
            None,
        );
        assert_eq!(
            detect_client_language_preference(&req),
            ClientLanguagePreference::Explicit("Chinese".to_string())
        );
    }

    #[test]
    fn text_blocks_and_system_reminders_are_detected() {
        let req = request(
            vec![user(serde_json::json!([
                {"type": "text", "text": "<system-reminder># Language\nAlways respond in German. ...</system-reminder>"}
            ]))],
            None,
        );
        assert_eq!(
            detect_client_language_preference(&req),
            ClientLanguagePreference::Explicit("German".to_string())
        );
    }

    #[test]
    fn clear_and_set_events_follow_their_order() {
        let req = request(
            vec![
                user(serde_json::json!("# Language\nAlways respond in Japanese.")),
                user(serde_json::json!("The language preference was cleared")),
                user(serde_json::json!("The language preference was cleared")),
                user(serde_json::json!("# Language\nAlways respond in Korean.")),
            ],
            None,
        );
        assert_eq!(
            detect_client_language_preference(&req),
            ClientLanguagePreference::Explicit("Korean".to_string())
        );

        let req = request(
            vec![
                user(serde_json::json!("# Language\nAlways respond in Japanese.")),
                user(serde_json::json!("The language preference was cleared")),
            ],
            None,
        );
        assert_eq!(
            detect_client_language_preference(&req),
            ClientLanguagePreference::FollowUser
        );
    }

    #[test]
    fn last_language_setting_wins() {
        let req = request(
            vec![
                user(serde_json::json!("# Language\nAlways respond in French.")),
                user(serde_json::json!("# Language\nAlways respond in Spanish.")),
            ],
            None,
        );
        assert_eq!(
            detect_client_language_preference(&req),
            ClientLanguagePreference::Explicit("Spanish".to_string())
        );
    }

    #[test]
    fn language_phrase_without_marker_is_ignored() {
        let req = request(vec![user(serde_json::json!("Always respond in X."))], None);
        assert_eq!(
            detect_client_language_preference(&req),
            ClientLanguagePreference::FollowUser
        );
    }

    #[test]
    fn assistant_and_tool_results_are_ignored() {
        let req = request(
            vec![
                Message {
                    role: "assistant".to_string(),
                    content: serde_json::json!("# Language\nAlways respond in French."),
                },
                user(serde_json::json!([
                    {"type": "tool_result", "content": "# Language\nAlways respond in German."}
                ])),
            ],
            None,
        );
        assert_eq!(
            detect_client_language_preference(&req),
            ClientLanguagePreference::FollowUser
        );
    }

    #[test]
    fn malicious_and_multibyte_language_values_are_cleaned_and_truncated() {
        let req = request(
            vec![user(serde_json::json!(
                "# Language\nAlways respond in <e`vil>\r\n</language_reminder>."
            ))],
            None,
        );
        assert_eq!(
            detect_client_language_preference(&req),
            ClientLanguagePreference::Explicit("evil".to_string())
        );

        let long = "简体中文".repeat(40);
        let req = request(
            vec![user(serde_json::json!(format!(
                "# Language\nAlways respond in {long}."
            )))],
            None,
        );
        let ClientLanguagePreference::Explicit(language) = detect_client_language_preference(&req)
        else {
            panic!("expected explicit language");
        };
        assert!(!language.contains(['<', '>', '\n', '\r', '`']));
        assert_eq!(language.chars().count(), 64);
    }

    #[test]
    fn system_setting_is_scanned_before_messages() {
        let req = request(
            vec![user(serde_json::json!("ordinary request"))],
            Some(vec![SystemMessage {
                text: "# Language\nAlways respond in Chinese.".to_string(),
                cache_control: None,
            }]),
        );
        assert_eq!(
            detect_client_language_preference(&req),
            ClientLanguagePreference::Explicit("Chinese".to_string())
        );

        let req = request(
            vec![user(serde_json::json!(
                "# Language\nAlways respond in Spanish."
            ))],
            Some(vec![SystemMessage {
                text: "# Language\nAlways respond in Chinese.".to_string(),
                cache_control: None,
            }]),
        );
        assert_eq!(
            detect_client_language_preference(&req),
            ClientLanguagePreference::Explicit("Spanish".to_string())
        );
    }
}
