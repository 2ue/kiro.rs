//! 上游空轮次（只返回 usage/metadata、没有任何输出）后的续写提示。
//!
//! 现网观测：同一请求原样重发（含换号）几乎总是再次为空，而空的 `end_turn` 会让
//! Claude Code 认为本轮已结束、任务静默停下。首输出前的那一次重试改为在当前轮用户消息
//! 末尾追加一段续写提示，只存在于这次发给 Kiro 的请求里：客户端保存的对话历史不含它，
//! 下一轮请求也不会带上它。

use crate::kiro::model::requests::kiro::KiroRequest;

/// 在当前轮用户消息末尾追加续写提示（注入项 `emptyTurnNudge` 的文本）；已追加过时不重复追加。
/// 返回是否有改动。
pub(crate) fn append_empty_turn_nudge(request: &mut KiroRequest, nudge: &str) -> bool {
    let nudge = nudge.trim();
    if nudge.is_empty() {
        return false;
    }
    let content = &mut request
        .conversation_state
        .current_message
        .user_input_message
        .content;
    if content.contains(nudge) {
        return false;
    }
    if !content.trim().is_empty() {
        content.push_str("\n\n");
    }
    content.push_str(nudge);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::prompt_injection::catalog::DEFAULT_EMPTY_TURN_NUDGE as EMPTY_TURN_NUDGE;
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
        assert!(append_empty_turn_nudge(&mut request, EMPTY_TURN_NUDGE));
        assert_eq!(
            current_content(&request),
            format!("run the tests\n\n{EMPTY_TURN_NUDGE}")
        );
        assert!(!append_empty_turn_nudge(&mut request, EMPTY_TURN_NUDGE));
        assert_eq!(
            current_content(&request).matches(EMPTY_TURN_NUDGE).count(),
            1
        );
    }

    #[test]
    fn fills_empty_tool_result_turn_without_leading_separator() {
        let mut request = request_with_content("");
        assert!(append_empty_turn_nudge(&mut request, EMPTY_TURN_NUDGE));
        assert_eq!(current_content(&request), EMPTY_TURN_NUDGE);
    }
}
