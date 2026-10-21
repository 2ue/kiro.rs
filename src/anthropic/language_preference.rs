use crate::anthropic::types::MessagesRequest;

/// Claude Code 在请求中声明的语言偏好。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClientLanguagePreference {
    /// 客户端设置了语言，值为清洗后的语言名。
    Explicit(String),
    /// 客户端未设置，由代理从用户最近一条真实文本识别出的语言。
    Detected(String),
    /// 客户端未设置且无法可靠识别，或最后一次操作是清除。
    FollowUser,
}

/// Client wrappers whose content is not something the user typed.
const NON_USER_TEXT_TAGS: &[&str] = &[
    "system-reminder",
    "command-name",
    "command-message",
    "command-args",
    "local-command-stdout",
    "local-command-stderr",
    "local-command-caveat",
];

/// Placeholders and client notices that never carry the user's language.
const NON_USER_TEXT_PREFIXES: &[&str] = &["[Request interrupted", "Tool result received."];

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
        Some(LanguageEvent::Cleared) | None => detect_latest_user_language(req)
            .map(|language| ClientLanguagePreference::Detected(language.to_string()))
            .unwrap_or(ClientLanguagePreference::FollowUser),
    }
}

/// Detects the script of the latest message the user actually typed, so the turn reminder can
/// name a language instead of asking the model to work it out on every tool round.
///
/// Only CJK scripts are detected: they are unambiguous from characters alone, and they are where
/// tool loops drift into English. Latin-script text returns `None` and keeps the follow-user
/// reminder. The latest real text decides; older messages are not consulted, so a user who
/// switches to English is not pinned to an earlier language.
fn detect_latest_user_language(req: &MessagesRequest) -> Option<&'static str> {
    let text = req
        .messages
        .iter()
        .rev()
        .filter(|message| message.role == "user")
        .map(|message| {
            user_text_blocks(&message.content)
                .into_iter()
                .map(strip_non_user_text)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .find(|text| !text.trim().is_empty())?;
    classify_script(&text)
}

fn strip_non_user_text(text: &str) -> String {
    let mut text = text.to_string();
    for tag in NON_USER_TEXT_TAGS {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        while let Some(start) = text.find(&open) {
            let end = text[start..]
                .find(&close)
                .map(|offset| start + offset + close.len())
                .unwrap_or(text.len());
            text.replace_range(start..end, " ");
        }
    }
    // Fenced code is the same in every language and would dilute the script counts.
    let mut kept = String::with_capacity(text.len());
    for (index, segment) in text.split("```").enumerate() {
        if index % 2 == 0 {
            kept.push_str(segment);
            kept.push(' ');
        }
    }
    let trimmed = kept.trim();
    if NON_USER_TEXT_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
    {
        return String::new();
    }
    trimmed.to_string()
}

fn classify_script(text: &str) -> Option<&'static str> {
    let (mut han, mut kana, mut hangul, mut latin_words) = (0usize, 0usize, 0usize, 0usize);
    let mut in_latin_word = false;
    for character in text.chars() {
        let is_latin = character.is_ascii_alphabetic();
        if is_latin && !in_latin_word {
            latin_words += 1;
        }
        in_latin_word = is_latin;
        match character as u32 {
            0x3040..=0x30FF | 0x31F0..=0x31FF => kana += 1,
            0xAC00..=0xD7AF | 0x1100..=0x11FF | 0x3130..=0x318F => hangul += 1,
            0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF => han += 1,
            _ => {}
        }
    }
    // Two CJK characters is enough for short replies such as "继续"; identifiers and paths in a
    // CJK sentence are expected, so CJK only needs to match the Latin word count.
    let cjk = han + kana + hangul;
    if cjk < 2 || cjk < latin_words {
        return None;
    }
    if hangul * 2 >= cjk {
        Some("Korean")
    } else if kana > 0 {
        Some("Japanese")
    } else {
        Some("Chinese")
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

    fn detected(req: &MessagesRequest) -> Option<String> {
        match detect_client_language_preference(req) {
            ClientLanguagePreference::Detected(language) => Some(language),
            _ => None,
        }
    }

    #[test]
    fn latest_cjk_user_text_is_detected() {
        let req = request(
            vec![user(serde_json::json!("帮我看下 src/main.rs 的 bug"))],
            None,
        );
        assert_eq!(detected(&req).as_deref(), Some("Chinese"));

        let req = request(vec![user(serde_json::json!("継続してください"))], None);
        assert_eq!(detected(&req).as_deref(), Some("Japanese"));

        let req = request(vec![user(serde_json::json!("계속 진행해 주세요"))], None);
        assert_eq!(detected(&req).as_deref(), Some("Korean"));

        let req = request(vec![user(serde_json::json!("继续"))], None);
        assert_eq!(detected(&req).as_deref(), Some("Chinese"));
    }

    #[test]
    fn latin_or_ambiguous_text_keeps_follow_user() {
        for text in [
            "fix the failing test",
            "中",
            "run cargo test and check the CI logs again 中文",
        ] {
            let req = request(vec![user(serde_json::json!(text))], None);
            assert_eq!(
                detect_client_language_preference(&req),
                ClientLanguagePreference::FollowUser,
                "{text}"
            );
        }
    }

    #[test]
    fn tool_results_reminders_and_placeholders_do_not_decide_language() {
        let req = request(
            vec![
                user(serde_json::json!("分析一下这个日志")),
                Message {
                    role: "assistant".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}
                    ]),
                },
                user(serde_json::json!([
                    {"type": "tool_result", "tool_use_id": "t1", "content": "error: build failed"},
                    {"type": "text", "text": "<system-reminder>Use English for code comments.</system-reminder>"},
                    {"type": "text", "text": "Tool result received."}
                ])),
            ],
            None,
        );
        assert_eq!(detected(&req).as_deref(), Some("Chinese"));
    }

    #[test]
    fn user_switching_language_is_followed() {
        let req = request(
            vec![
                user(serde_json::json!("先用中文回答")),
                user(serde_json::json!("now please answer in English")),
            ],
            None,
        );
        assert_eq!(
            detect_client_language_preference(&req),
            ClientLanguagePreference::FollowUser
        );

        let req = request(
            vec![
                user(serde_json::json!("please check the build")),
                user(serde_json::json!("改成中文吧")),
            ],
            None,
        );
        assert_eq!(detected(&req).as_deref(), Some("Chinese"));
    }

    #[test]
    fn fenced_code_does_not_dilute_detection() {
        let req = request(
            vec![user(serde_json::json!(
                "这段报错是什么意思\n```\nthread main panicked at src/lib.rs called unwrap on an Err value\n```"
            ))],
            None,
        );
        assert_eq!(detected(&req).as_deref(), Some("Chinese"));
    }

    #[test]
    fn client_declared_language_wins_over_detection() {
        let req = request(
            vec![user(serde_json::json!(
                "# Language\nAlways respond in English.\n帮我看下这个问题"
            ))],
            None,
        );
        assert_eq!(
            detect_client_language_preference(&req),
            ClientLanguagePreference::Explicit("English".to_string())
        );
    }
}
