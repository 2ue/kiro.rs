//! Final Kiro payload size guard.
//!
//! Kiro upstream can return a generic `400 Improperly formed request` when the
//! serialized request body is too large. This guard runs after Anthropic->Kiro
//! conversion, measures the actual JSON payload bytes, and trims old history
//! entries while preserving Kiro history invariants.

use std::collections::HashSet;

use crate::kiro::model::requests::{
    conversation::{Message, UserInputMessage, UserMessage},
    kiro::KiroRequest,
    tool::ToolResult,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadByteBreakdown {
    pub total_bytes: usize,
    pub history_bytes: usize,
    pub current_message_bytes: usize,
    pub current_content_bytes: usize,
    pub current_tools_bytes: usize,
    pub current_tool_results_bytes: usize,
    pub current_images_bytes: usize,
    pub history_entries: usize,
    pub current_tool_count: usize,
    pub current_tool_result_count: usize,
    pub current_image_count: usize,
    pub largest_tool_bytes: usize,
    pub history_tool_use_count: usize,
    pub history_tool_result_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadGuardConfig {
    pub enabled: bool,
    pub max_bytes: usize,
    pub trim_history: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadGuardReport {
    pub enabled: bool,
    pub max_bytes: usize,
    pub original_bytes: usize,
    pub final_bytes: usize,
    pub original_history_entries: usize,
    pub final_history_entries: usize,
    pub trimmed_history_entries: usize,
    pub aligned_leading_entries: usize,
    pub removed_empty_tool_uses: usize,
    pub removed_orphan_tool_results: usize,
    pub textified_orphan_tool_results: usize,
    pub removed_orphan_tool_uses: usize,
    pub still_oversized: bool,
}

impl PayloadGuardReport {
    fn disabled(size: usize, history_entries: usize) -> Self {
        Self {
            enabled: false,
            max_bytes: 0,
            original_bytes: size,
            final_bytes: size,
            original_history_entries: history_entries,
            final_history_entries: history_entries,
            trimmed_history_entries: 0,
            aligned_leading_entries: 0,
            removed_empty_tool_uses: 0,
            removed_orphan_tool_results: 0,
            textified_orphan_tool_results: 0,
            removed_orphan_tool_uses: 0,
            still_oversized: false,
        }
    }

    pub fn was_modified(&self) -> bool {
        self.trimmed_history_entries > 0
            || self.aligned_leading_entries > 0
            || self.removed_empty_tool_uses > 0
            || self.removed_orphan_tool_results > 0
            || self.textified_orphan_tool_results > 0
            || self.removed_orphan_tool_uses > 0
    }

    pub fn warning_header_fragment(&self) -> Option<String> {
        if !self.was_modified() && !self.still_oversized {
            return None;
        }
        let mut parts = Vec::new();
        if self.trimmed_history_entries > 0 {
            parts.push(format!(
                "payload-trimmed-history={}",
                self.trimmed_history_entries
            ));
        }
        if self.aligned_leading_entries > 0 {
            parts.push(format!(
                "payload-aligned-history={}",
                self.aligned_leading_entries
            ));
        }
        if self.removed_empty_tool_uses > 0 {
            parts.push(format!(
                "payload-empty-tool-uses={}",
                self.removed_empty_tool_uses
            ));
        }
        if self.removed_orphan_tool_results > 0 {
            parts.push(format!(
                "payload-orphan-tool-results={}",
                self.removed_orphan_tool_results
            ));
        }
        if self.textified_orphan_tool_results > 0 {
            parts.push(format!(
                "payload-textified-tool-results={}",
                self.textified_orphan_tool_results
            ));
        }
        if self.removed_orphan_tool_uses > 0 {
            parts.push(format!(
                "payload-orphan-tool-uses={}",
                self.removed_orphan_tool_uses
            ));
        }
        if self.still_oversized {
            parts.push(format!("payload-oversized={}", self.final_bytes));
        }
        Some(parts.join(","))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadGuardError {
    Serialize(String),
}

impl std::fmt::Display for PayloadGuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PayloadGuardError::Serialize(err) => write!(f, "序列化请求失败: {}", err),
        }
    }
}

pub fn guard_kiro_request(
    request: &mut KiroRequest,
    config: PayloadGuardConfig,
) -> Result<(String, PayloadGuardReport), PayloadGuardError> {
    let original_body = serialize_request(request)?;
    let original_bytes = original_body.len();
    let original_history_entries = request.conversation_state.history.len();

    if !config.enabled {
        return Ok((
            original_body,
            PayloadGuardReport::disabled(original_bytes, original_history_entries),
        ));
    }
    let size_limit_enabled = config.max_bytes > 0;

    let mut report = PayloadGuardReport {
        enabled: true,
        max_bytes: config.max_bytes,
        original_bytes,
        final_bytes: original_bytes,
        original_history_entries,
        final_history_entries: original_history_entries,
        trimmed_history_entries: 0,
        aligned_leading_entries: 0,
        removed_empty_tool_uses: 0,
        removed_orphan_tool_results: 0,
        textified_orphan_tool_results: 0,
        removed_orphan_tool_uses: 0,
        still_oversized: false,
    };

    report.aligned_leading_entries +=
        align_history_to_user(&mut request.conversation_state.history);

    let initial_repair = repair_request(request);
    report.removed_empty_tool_uses += initial_repair.removed_empty_tool_uses;
    report.removed_orphan_tool_results += initial_repair.removed_orphan_tool_results;
    report.textified_orphan_tool_results += initial_repair.textified_orphan_tool_results;
    report.removed_orphan_tool_uses += initial_repair.removed_orphan_tool_uses;

    let mut body = serialize_request(request)?;
    report.final_bytes = body.len();

    if size_limit_enabled && config.trim_history {
        while report.final_bytes > config.max_bytes
            && !request.conversation_state.history.is_empty()
        {
            let before = request.conversation_state.history.len();
            trim_oldest_history_unit(&mut request.conversation_state.history);
            let after_trim = request.conversation_state.history.len();
            report.trimmed_history_entries += before.saturating_sub(after_trim);

            let aligned = align_history_to_user(&mut request.conversation_state.history);
            report.aligned_leading_entries += aligned;

            let repair = repair_request(request);
            report.removed_empty_tool_uses += repair.removed_empty_tool_uses;
            report.removed_orphan_tool_results += repair.removed_orphan_tool_results;
            report.textified_orphan_tool_results += repair.textified_orphan_tool_results;
            report.removed_orphan_tool_uses += repair.removed_orphan_tool_uses;

            body = serialize_request(request)?;
            let new_size = body.len();
            if new_size >= report.final_bytes && after_trim == before {
                break;
            }
            report.final_bytes = new_size;
        }
    }

    report.final_history_entries = request.conversation_state.history.len();
    report.final_bytes = body.len();
    report.still_oversized = size_limit_enabled && report.final_bytes > config.max_bytes;

    Ok((body, report))
}

pub fn breakdown_kiro_request(
    request: &KiroRequest,
    serialized_body: &str,
) -> PayloadByteBreakdown {
    let state = &request.conversation_state;
    let current_user = &state.current_message.user_input_message;
    let context = &current_user.user_input_message_context;

    PayloadByteBreakdown {
        total_bytes: serialized_body.len(),
        history_bytes: json_len(&state.history),
        current_message_bytes: json_len(&state.current_message),
        current_content_bytes: current_user.content.len(),
        current_tools_bytes: json_len(&context.tools),
        current_tool_results_bytes: json_len(&context.tool_results),
        current_images_bytes: json_len(&current_user.images),
        history_entries: state.history.len(),
        current_tool_count: context.tools.len(),
        current_tool_result_count: context.tool_results.len(),
        current_image_count: current_user.images.len(),
        largest_tool_bytes: context.tools.iter().map(json_len).max().unwrap_or(0),
        history_tool_use_count: count_history_tool_uses(&state.history),
        history_tool_result_count: count_history_tool_results(&state.history),
    }
}

fn serialize_request(request: &KiroRequest) -> Result<String, PayloadGuardError> {
    serde_json::to_string(request).map_err(|err| PayloadGuardError::Serialize(err.to_string()))
}

fn json_len<T: serde::Serialize + ?Sized>(value: &T) -> usize {
    serde_json::to_string(value)
        .map(|json| json.len())
        .unwrap_or(0)
}

fn count_history_tool_uses(history: &[Message]) -> usize {
    history
        .iter()
        .map(|message| match message {
            Message::Assistant(assistant) => assistant
                .assistant_response_message
                .tool_uses
                .as_ref()
                .map(Vec::len)
                .unwrap_or(0),
            Message::User(_) => 0,
        })
        .sum()
}

fn count_history_tool_results(history: &[Message]) -> usize {
    history
        .iter()
        .map(|message| match message {
            Message::User(user) => user
                .user_input_message
                .user_input_message_context
                .tool_results
                .len(),
            Message::Assistant(_) => 0,
        })
        .sum()
}

fn trim_oldest_history_unit(history: &mut Vec<Message>) {
    if history.is_empty() {
        return;
    }

    if history.len() >= 2 && starts_with_tool_pair(history) {
        history.drain(0..2);
        return;
    }

    history.remove(0);
}

fn starts_with_tool_pair(history: &[Message]) -> bool {
    let Some(Message::Assistant(assistant)) = history.first() else {
        return false;
    };
    let has_tool_uses = assistant
        .assistant_response_message
        .tool_uses
        .as_ref()
        .is_some_and(|items| !items.is_empty());
    if !has_tool_uses {
        return false;
    }
    history
        .get(1)
        .and_then(|msg| match msg {
            Message::User(user) => Some(
                !user
                    .user_input_message
                    .user_input_message_context
                    .tool_results
                    .is_empty(),
            ),
            Message::Assistant(_) => None,
        })
        .unwrap_or(false)
}

fn align_history_to_user(history: &mut Vec<Message>) -> usize {
    let mut removed = 0;
    while matches!(history.first(), Some(Message::Assistant(_))) {
        history.remove(0);
        removed += 1;
    }
    removed
}

#[derive(Default)]
struct RepairStats {
    removed_empty_tool_uses: usize,
    removed_orphan_tool_results: usize,
    textified_orphan_tool_results: usize,
    removed_orphan_tool_uses: usize,
}

fn repair_request(request: &mut KiroRequest) -> RepairStats {
    let mut stats = RepairStats::default();
    let conversation_state = &mut request.conversation_state;
    let history = &mut conversation_state.history;

    stats.removed_empty_tool_uses += strip_empty_tool_uses(history);
    let history_results = repair_orphan_tool_results(history);
    stats.removed_orphan_tool_results += history_results.removed_orphan_tool_results;
    stats.textified_orphan_tool_results += history_results.textified_orphan_tool_results;

    let current_user = &mut conversation_state.current_message.user_input_message;
    let current_results = repair_current_orphan_tool_results(history, current_user);
    stats.removed_orphan_tool_results += current_results.removed_orphan_tool_results;
    stats.textified_orphan_tool_results += current_results.textified_orphan_tool_results;

    stats.removed_orphan_tool_uses += remove_unpaired_tool_uses(
        history,
        &current_user.user_input_message_context.tool_results,
    );
    stats
}

fn strip_empty_tool_uses(history: &mut [Message]) -> usize {
    let mut removed = 0;
    for message in history {
        if let Message::Assistant(assistant) = message {
            if assistant
                .assistant_response_message
                .tool_uses
                .as_ref()
                .is_some_and(Vec::is_empty)
            {
                assistant.assistant_response_message.tool_uses = None;
                removed += 1;
            }
        }
    }
    removed
}

fn repair_orphan_tool_results(history: &mut [Message]) -> RepairStats {
    let mut stats = RepairStats::default();
    for idx in 0..history.len() {
        let valid_ids = previous_assistant_tool_use_ids(history, idx);

        let Message::User(user) = &mut history[idx] else {
            continue;
        };
        let repaired = repair_user_tool_results(&valid_ids, &mut user.user_input_message);
        stats.removed_orphan_tool_results += repaired.removed_orphan_tool_results;
        stats.textified_orphan_tool_results += repaired.textified_orphan_tool_results;
    }
    stats
}

fn repair_current_orphan_tool_results(
    history: &[Message],
    current_user: &mut UserInputMessage,
) -> RepairStats {
    let valid_ids = last_assistant_tool_use_ids(history);
    repair_user_input_tool_results(&valid_ids, current_user)
}

fn repair_user_tool_results(valid_ids: &HashSet<String>, user: &mut UserMessage) -> RepairStats {
    repair_tool_results(
        valid_ids,
        &mut user.user_input_message_context.tool_results,
        &mut user.content,
    )
}

fn repair_user_input_tool_results(
    valid_ids: &HashSet<String>,
    user: &mut UserInputMessage,
) -> RepairStats {
    repair_tool_results(
        valid_ids,
        &mut user.user_input_message_context.tool_results,
        &mut user.content,
    )
}

fn repair_tool_results(
    valid_ids: &HashSet<String>,
    results: &mut Vec<ToolResult>,
    content: &mut String,
) -> RepairStats {
    let mut stats = RepairStats::default();
    if results.is_empty() {
        return stats;
    }

    let original_len = results.len();
    let mut orphan_text = Vec::new();
    results.retain(|result| {
        let keep = valid_ids.contains(&result.tool_use_id);
        if !keep {
            if let Some(text) = tool_result_to_text(result) {
                orphan_text.push(format!(
                    "[trimmed tool result {}]\n{}",
                    result.tool_use_id, text
                ));
            }
        }
        keep
    });
    stats.removed_orphan_tool_results += original_len.saturating_sub(results.len());
    stats.textified_orphan_tool_results += orphan_text.len();
    if !orphan_text.is_empty() {
        append_text(content, &orphan_text.join("\n\n"));
    }
    stats
}

fn remove_unpaired_tool_uses(history: &mut [Message], current_results: &[ToolResult]) -> usize {
    let mut removed = 0;
    for idx in 0..history.len() {
        let paired_ids = next_tool_result_ids(history, idx, current_results);
        let Message::Assistant(assistant) = &mut history[idx] else {
            continue;
        };
        let Some(tool_uses) = &mut assistant.assistant_response_message.tool_uses else {
            continue;
        };
        let original_len = tool_uses.len();
        tool_uses.retain(|tool_use| paired_ids.contains(&tool_use.tool_use_id));
        removed += original_len.saturating_sub(tool_uses.len());
        if tool_uses.is_empty() {
            assistant.assistant_response_message.tool_uses = None;
        }
    }
    removed
}

fn previous_assistant_tool_use_ids(history: &[Message], idx: usize) -> HashSet<String> {
    if idx == 0 {
        return HashSet::new();
    }
    assistant_tool_use_ids(history.get(idx - 1))
}

fn last_assistant_tool_use_ids(history: &[Message]) -> HashSet<String> {
    assistant_tool_use_ids(history.last())
}

fn next_tool_result_ids(
    history: &[Message],
    idx: usize,
    current_results: &[ToolResult],
) -> HashSet<String> {
    if let Some(Message::User(user)) = history.get(idx + 1) {
        return user
            .user_input_message
            .user_input_message_context
            .tool_results
            .iter()
            .map(|result| result.tool_use_id.clone())
            .collect();
    }

    if idx + 1 == history.len() {
        return current_results
            .iter()
            .map(|result| result.tool_use_id.clone())
            .collect();
    }

    HashSet::new()
}

fn assistant_tool_use_ids(message: Option<&Message>) -> HashSet<String> {
    let Some(Message::Assistant(assistant)) = message else {
        return HashSet::new();
    };
    assistant
        .assistant_response_message
        .tool_uses
        .as_ref()
        .map(|tool_uses| {
            tool_uses
                .iter()
                .map(|tool_use| tool_use.tool_use_id.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn tool_result_to_text(result: &ToolResult) -> Option<String> {
    let mut parts = Vec::new();
    for item in &result.content {
        if let Some(text) = item.get("text").and_then(|value| value.as_str()) {
            if !text.is_empty() {
                parts.push(text.to_string());
            }
        } else if !item.is_empty() {
            parts.push(serde_json::Value::Object(item.clone()).to_string());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

fn append_text(content: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }
    if content.trim().is_empty() {
        *content = text.to_string();
    } else {
        content.push_str("\n\n");
        content.push_str(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::model::requests::conversation::{
        AssistantMessage, ConversationState, CurrentMessage, HistoryAssistantMessage,
        HistoryUserMessage, KiroImage, UserInputMessage, UserInputMessageContext,
    };
    use crate::kiro::model::requests::tool::{
        InputSchema, Tool, ToolResult, ToolSpecification, ToolUseEntry,
    };

    const TEST_MODEL: &str = "test-model";

    fn request_with_history(history: Vec<Message>) -> KiroRequest {
        KiroRequest {
            conversation_state: ConversationState::new("conv-test")
                .with_current_message(CurrentMessage::new(UserInputMessage::new(
                    "current", TEST_MODEL,
                )))
                .with_history(history),
            profile_arn: None,
        }
    }

    #[test]
    fn guard_trims_old_history_until_under_limit() {
        let mut history = Vec::new();
        for idx in 0..10 {
            history.push(Message::User(HistoryUserMessage::new(
                format!("user {} {}", idx, "x".repeat(500)),
                TEST_MODEL,
            )));
            history.push(Message::Assistant(HistoryAssistantMessage::new(format!(
                "assistant {} {}",
                idx,
                "y".repeat(500)
            ))));
        }
        let mut request = request_with_history(history);
        let (body, report) = guard_kiro_request(
            &mut request,
            PayloadGuardConfig {
                enabled: true,
                max_bytes: 5_000,
                trim_history: true,
            },
        )
        .expect("guard should trim");

        assert!(body.len() <= 5_000);
        assert!(report.trimmed_history_entries > 0);
        assert!(request.conversation_state.history.len() < report.original_history_entries);
        assert!(matches!(
            request.conversation_state.history.first(),
            Some(Message::User(_))
        ));
    }

    #[test]
    fn guard_repairs_orphan_tool_results_after_trim() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("tool call")
                .with_tool_uses(vec![ToolUseEntry::new("tool-1", "readFile")]),
        };
        let mut user = HistoryUserMessage::new("result message", TEST_MODEL);
        user.user_input_message.user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![
                ToolResult::success("tool-1", "valid result"),
                ToolResult::success("tool-orphan", "orphan result"),
            ]);

        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("old", TEST_MODEL)),
            Message::Assistant(assistant),
            Message::User(user),
        ]);

        let (_body, report) = guard_kiro_request(
            &mut request,
            PayloadGuardConfig {
                enabled: true,
                max_bytes: usize::MAX,
                trim_history: true,
            },
        )
        .expect("guard should repair");

        assert_eq!(report.removed_orphan_tool_results, 1);
        let Message::User(user) = &request.conversation_state.history[2] else {
            panic!("expected user");
        };
        assert_eq!(
            user.user_input_message
                .user_input_message_context
                .tool_results
                .len(),
            1
        );
        assert!(user.user_input_message.content.contains("orphan result"));
    }

    #[test]
    fn guard_marks_oversized_without_rejecting_current_message() {
        let mut request = KiroRequest {
            conversation_state: ConversationState::new("conv-test").with_current_message(
                CurrentMessage::new(UserInputMessage::new("x".repeat(10_000), TEST_MODEL)),
            ),
            profile_arn: None,
        };

        let (body, report) = guard_kiro_request(
            &mut request,
            PayloadGuardConfig {
                enabled: true,
                max_bytes: 1_000,
                trim_history: true,
            },
        )
        .expect("oversized current message should be passed through to Kiro");

        assert!(body.len() > 1_000);
        assert!(report.still_oversized);
        assert_eq!(report.final_bytes, body.len());
        assert_eq!(report.trimmed_history_entries, 0);
    }

    #[test]
    fn guard_zero_max_bytes_repairs_without_size_limit() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage {
                content: "empty tools".to_string(),
                tool_uses: Some(Vec::new()),
            },
        };
        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("user", TEST_MODEL)),
            Message::Assistant(assistant),
        ]);
        request
            .conversation_state
            .current_message
            .user_input_message
            .content = "x".repeat(10_000);

        let (body, report) = guard_kiro_request(
            &mut request,
            PayloadGuardConfig {
                enabled: true,
                max_bytes: 0,
                trim_history: true,
            },
        )
        .expect("zero max bytes should disable only size limiting");

        assert!(body.len() > 1_000);
        assert_eq!(report.max_bytes, 0);
        assert_eq!(report.final_bytes, body.len());
        assert_eq!(report.trimmed_history_entries, 0);
        assert_eq!(report.removed_empty_tool_uses, 1);
        assert!(!report.still_oversized);
        assert_eq!(request.conversation_state.history.len(), 2);
        let Message::Assistant(assistant) = &request.conversation_state.history[1] else {
            panic!("expected assistant");
        };
        assert!(assistant.assistant_response_message.tool_uses.is_none());
    }

    #[test]
    fn warning_header_fragment_reports_changes() {
        let report = PayloadGuardReport {
            enabled: true,
            max_bytes: 1000,
            original_bytes: 2000,
            final_bytes: 900,
            original_history_entries: 4,
            final_history_entries: 2,
            trimmed_history_entries: 2,
            aligned_leading_entries: 1,
            removed_empty_tool_uses: 1,
            removed_orphan_tool_results: 1,
            textified_orphan_tool_results: 1,
            removed_orphan_tool_uses: 1,
            still_oversized: false,
        };

        let header = report.warning_header_fragment().expect("header");
        assert!(header.contains("payload-trimmed-history=2"));
        assert!(header.contains("payload-empty-tool-uses=1"));
        assert!(header.contains("payload-textified-tool-results=1"));
    }

    #[test]
    fn guard_strips_empty_tool_uses() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage {
                content: "empty tools".to_string(),
                tool_uses: Some(Vec::new()),
            },
        };
        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("user", TEST_MODEL)),
            Message::Assistant(assistant),
        ]);

        let (_body, report) = guard_kiro_request(
            &mut request,
            PayloadGuardConfig {
                enabled: true,
                max_bytes: usize::MAX,
                trim_history: true,
            },
        )
        .expect("guard");

        assert_eq!(report.removed_empty_tool_uses, 1);
        let Message::Assistant(assistant) = &request.conversation_state.history[1] else {
            panic!("expected assistant");
        };
        assert!(assistant.assistant_response_message.tool_uses.is_none());
    }

    #[test]
    fn guard_aligns_leading_assistant_and_repairs_result() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("tool call")
                .with_tool_uses(vec![ToolUseEntry::new("tool-1", "readFile")]),
        };
        let mut user = HistoryUserMessage::new("result message", TEST_MODEL);
        user.user_input_message.user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![ToolResult::success("tool-1", "valid result")]);
        let mut request =
            request_with_history(vec![Message::Assistant(assistant), Message::User(user)]);

        let (_body, report) = guard_kiro_request(
            &mut request,
            PayloadGuardConfig {
                enabled: true,
                max_bytes: usize::MAX,
                trim_history: true,
            },
        )
        .expect("guard");

        assert_eq!(report.aligned_leading_entries, 1);
        assert_eq!(report.removed_orphan_tool_results, 1);
        assert!(matches!(
            request.conversation_state.history.first(),
            Some(Message::User(_))
        ));
    }

    #[test]
    fn guard_keeps_tool_use_paired_with_current_message_result() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("tool call")
                .with_tool_uses(vec![ToolUseEntry::new("tool-1", "readFile")]),
        };
        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("read", TEST_MODEL)),
            Message::Assistant(assistant),
        ]);
        request
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![ToolResult::success("tool-1", "valid result")]);

        let (_body, report) = guard_kiro_request(
            &mut request,
            PayloadGuardConfig {
                enabled: true,
                max_bytes: usize::MAX,
                trim_history: true,
            },
        )
        .expect("guard");

        assert_eq!(report.removed_orphan_tool_uses, 0);
        assert_eq!(report.removed_orphan_tool_results, 0);
        let Message::Assistant(assistant) = &request.conversation_state.history[1] else {
            panic!("expected assistant");
        };
        assert_eq!(
            assistant
                .assistant_response_message
                .tool_uses
                .as_ref()
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            request
                .conversation_state
                .current_message
                .user_input_message
                .user_input_message_context
                .tool_results
                .len(),
            1
        );
    }

    #[test]
    fn payload_breakdown_reports_current_tool_and_history_sizes() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("tool call")
                .with_tool_uses(vec![ToolUseEntry::new("tool-1", "readFile")]),
        };
        let mut user = HistoryUserMessage::new("result message", TEST_MODEL);
        user.user_input_message.user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![ToolResult::success("tool-1", "valid result")]);
        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("read", TEST_MODEL)),
            Message::Assistant(assistant),
            Message::User(user),
        ]);
        request
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context = UserInputMessageContext::new().with_tools(vec![Tool {
            tool_specification: ToolSpecification {
                name: "readFile".to_string(),
                description: "read files".to_string(),
                input_schema: InputSchema::from_json(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"}
                    }
                })),
            },
        }]);

        let body = serde_json::to_string(&request).expect("serialize");
        let breakdown = breakdown_kiro_request(&request, &body);

        assert_eq!(breakdown.total_bytes, body.len());
        assert_eq!(breakdown.history_entries, 3);
        assert_eq!(breakdown.current_tool_count, 1);
        assert_eq!(breakdown.history_tool_use_count, 1);
        assert_eq!(breakdown.history_tool_result_count, 1);
        assert!(breakdown.current_tools_bytes > 0);
        assert!(breakdown.largest_tool_bytes > 0);
    }

    #[test]
    fn image_history_bytes_are_counted() {
        let mut user = HistoryUserMessage::new("image", TEST_MODEL);
        user.user_input_message.images = vec![KiroImage::from_base64("png", "a".repeat(2048))];
        let mut request = request_with_history(vec![Message::User(user)]);

        let (_body, report) = guard_kiro_request(
            &mut request,
            PayloadGuardConfig {
                enabled: true,
                max_bytes: usize::MAX,
                trim_history: true,
            },
        )
        .expect("guard");

        assert!(report.original_bytes > 2048);
    }
}
