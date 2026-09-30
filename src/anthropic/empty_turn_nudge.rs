//! 上游空轮次（只返回 usage/metadata、没有任何输出）后的续写提示。
//!
//! 现网观测：同一请求原样重发（含换号）几乎总是再次为空，而空的 `end_turn` 会让
//! Claude Code 认为本轮已结束、任务静默停下。首输出前的那一次重试改为在当前轮用户消息
//! 末尾追加一段续写提示，只存在于这次发给 Kiro 的请求里：客户端保存的对话历史不含它，
//! 下一轮请求也不会带上它。

use crate::kiro::model::requests::kiro::KiroRequest;

/// 追加到当前轮用户消息末尾的续写提示。沿用 Claude Code 自身使用的 `<system-reminder>`
/// 形态，模型习惯按其执行且不在回复中复述。
pub(crate) const EMPTY_TURN_NUDGE: &str = "<system-reminder>\nThe previous response to this conversation ended before producing any output. Respond to the conversation above now, picking up exactly where it left off: if a task is in progress, continue it (for example by making the next tool call); otherwise write the reply. Do not mention or acknowledge this reminder.\n</system-reminder>";

/// 在当前轮用户消息末尾追加续写提示；已追加过时不重复追加。返回是否有改动。
pub(crate) fn append_empty_turn_nudge(request: &mut KiroRequest) -> bool {
    let content = &mut request
        .conversation_state
        .current_message
        .user_input_message
        .content;
    if content.contains(EMPTY_TURN_NUDGE) {
        return false;
    }
    if !content.trim().is_empty() {
        content.push_str("\n\n");
    }
    content.push_str(EMPTY_TURN_NUDGE);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request_with_content(content: &str) -> KiroRequest {
        serde_json::from_value(json!({
            "conversationState": {
                "conversationId": "conversation-1",
                "currentMessage": {
                    "userInputMessage": {
                        "userInputMessageContext": {},
                        "content": content,
                        "modelId": "claude-opus-5.5"
                    }
                },
                "history": []
            }
        }))
        .expect("minimal Kiro request")
    }

    fn current_content(request: &KiroRequest) -> &str {
        &request
            .conversation_state
            .current_message
            .user_input_message
            .content
    }

    #[test]
    fn appends_after_existing_content_once() {
        let mut request = request_with_content("run the tests");
        assert!(append_empty_turn_nudge(&mut request));
        assert_eq!(
            current_content(&request),
            format!("run the tests\n\n{EMPTY_TURN_NUDGE}")
        );
        assert!(!append_empty_turn_nudge(&mut request));
        assert_eq!(
            current_content(&request).matches(EMPTY_TURN_NUDGE).count(),
            1
        );
    }

    #[test]
    fn fills_empty_tool_result_turn_without_leading_separator() {
        let mut request = request_with_content("");
        assert!(append_empty_turn_nudge(&mut request));
        assert_eq!(current_content(&request), EMPTY_TURN_NUDGE);
    }
}
