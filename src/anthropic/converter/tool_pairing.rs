//! tool_use/tool_result pairing validation and compatibility repair.

use crate::kiro::model::requests::conversation::Message;
use crate::kiro::model::requests::tool::ToolResult;

use super::ProxyWarnings;

/// Cleans historical tool results without copying rejected result content into ordinary text.
pub(super) fn sanitize_history_tool_results(history: &mut [Message], warnings: &mut ProxyWarnings) {
    use std::collections::HashSet;

    for idx in 0..history.len() {
        let valid_ids = if idx > 0 {
            match &history[idx - 1] {
                Message::Assistant(assistant) => assistant
                    .assistant_response_message
                    .tool_uses
                    .as_ref()
                    .map(|tool_uses| {
                        tool_uses
                            .iter()
                            .map(|tool_use| tool_use.tool_use_id.clone())
                            .collect::<HashSet<_>>()
                    })
                    .unwrap_or_default(),
                Message::User(_) => HashSet::new(),
            }
        } else {
            HashSet::new()
        };

        let Message::User(user) = &mut history[idx] else {
            continue;
        };
        let results = &mut user
            .user_input_message
            .user_input_message_context
            .tool_results;
        if results.is_empty() {
            continue;
        }

        let original_len = results.len();
        let mut seen = HashSet::new();
        results.retain(|result| {
            if !valid_ids.contains(&result.tool_use_id) {
                warnings.orphan_tool_results = warnings.orphan_tool_results.saturating_add(1);
                tracing::warn!(
                    "历史 tool_result 不属于紧邻的 assistant tool_use，已安全丢弃：tool_use_id={}",
                    result.tool_use_id
                );
                return false;
            }
            if !seen.insert(result.tool_use_id.clone()) {
                warnings.duplicate_tool_results = warnings.duplicate_tool_results.saturating_add(1);
                tracing::warn!(
                    "历史中重复的结构化 tool_result 已安全丢弃，仅保留第一条：tool_use_id={}",
                    result.tool_use_id
                );
                return false;
            }
            true
        });
        if results.is_empty()
            && original_len > 0
            && user.user_input_message.content.trim().is_empty()
        {
            user.user_input_message.content = super::EMPTY_USER_CONTENT_PLACEHOLDER.to_string();
        }
    }
}

/// 验证并过滤 tool_use/tool_result 配对
///
/// 收集所有 tool_use_id，验证 tool_result 是否匹配
/// 静默跳过孤立的 tool_use 和 tool_result，输出警告日志
///
/// # Arguments
/// * `history` - 历史消息引用
/// * `tool_results` - 当前消息中的 tool_result 列表
///
/// # Returns
/// 元组：(经过验证和过滤后的 tool_result 列表, 孤立的 tool_use_id 集合)
pub(super) fn validate_tool_pairing(
    history: &[Message],
    tool_results: &[ToolResult],
    warnings: &mut ProxyWarnings,
) -> (Vec<ToolResult>, std::collections::HashSet<String>) {
    use std::collections::HashSet;

    let mut all_tool_use_ids = HashSet::new();
    let mut history_tool_result_ids = HashSet::new();
    let mut current_tool_use_ids = HashSet::new();
    let mut last_assistant_unpaired_candidates = HashSet::new();
    let mut unpaired_tool_use_ids = HashSet::new();
    let mut current_paired_tool_use_ids = HashSet::new();

    let mut pending_assistant_tool_use_ids: Option<Vec<String>> = None;
    for message in history {
        match message {
            Message::Assistant(assistant) => {
                if let Some(ids) = pending_assistant_tool_use_ids.take() {
                    unpaired_tool_use_ids.extend(ids);
                }
                if let Some(tool_uses) = &assistant.assistant_response_message.tool_uses {
                    let ids = tool_uses
                        .iter()
                        .map(|tool_use| tool_use.tool_use_id.clone())
                        .collect::<Vec<_>>();
                    for tool_use in tool_uses {
                        all_tool_use_ids.insert(tool_use.tool_use_id.clone());
                    }
                    if !ids.is_empty() {
                        pending_assistant_tool_use_ids = Some(ids);
                    }
                }
            }
            Message::User(user) => {
                let mut paired_ids = HashSet::new();
                for result in &user
                    .user_input_message
                    .user_input_message_context
                    .tool_results
                {
                    history_tool_result_ids.insert(result.tool_use_id.clone());
                    paired_ids.insert(result.tool_use_id.clone());
                }
                if let Some(ids) = pending_assistant_tool_use_ids.take() {
                    unpaired_tool_use_ids.extend(
                        ids.into_iter()
                            .filter(|tool_use_id| !paired_ids.contains(tool_use_id)),
                    );
                }
            }
        }
    }
    if let Some(ids) = pending_assistant_tool_use_ids.take() {
        current_tool_use_ids.extend(ids.iter().cloned());
        last_assistant_unpaired_candidates.extend(ids);
        unpaired_tool_use_ids.extend(current_tool_use_ids.iter().cloned());
    }

    let mut filtered_results = Vec::new();
    let mut seen_current_results = HashSet::new();

    for result in tool_results {
        if current_tool_use_ids.contains(&result.tool_use_id)
            && seen_current_results.insert(result.tool_use_id.clone())
        {
            // 配对成功
            filtered_results.push(result.clone());
            unpaired_tool_use_ids.remove(&result.tool_use_id);
            current_paired_tool_use_ids.insert(result.tool_use_id.clone());
        } else if current_tool_use_ids.contains(&result.tool_use_id) {
            // 当前消息中同一个 tool_use_id 多次返回，仅保留第一条结构化结果。
            warnings.duplicate_tool_results = warnings.duplicate_tool_results.saturating_add(1);
            tracing::warn!(
                "跳过重复的当前结构化 tool_result，仅保留第一条：tool_use_id={}",
                result.tool_use_id
            );
        } else if history_tool_result_ids.contains(&result.tool_use_id)
            || all_tool_use_ids.contains(&result.tool_use_id)
        {
            // 不属于最后一条 assistant 的 tool_result 不能作为当前结构化结果继续发送。
            warnings.orphan_tool_results = warnings.orphan_tool_results.saturating_add(1);
            tracing::warn!(
                "tool_result 不属于最后一条 assistant tool_use，已安全丢弃：tool_use_id={}",
                result.tool_use_id
            );
        } else {
            // 孤立 tool_result - 找不到对应的 tool_use
            warnings.orphan_tool_results = warnings.orphan_tool_results.saturating_add(1);
            tracing::warn!(
                "孤立的 tool_result 找不到对应 tool_use，已安全丢弃：tool_use_id={}",
                result.tool_use_id
            );
        }
    }

    for paired_id in &current_paired_tool_use_ids {
        unpaired_tool_use_ids.remove(paired_id);
    }
    for orphaned_id in last_assistant_unpaired_candidates {
        if !current_paired_tool_use_ids.contains(&orphaned_id) {
            unpaired_tool_use_ids.insert(orphaned_id);
        }
    }

    // 检测真正孤立的 tool_use（有 tool_use 但在历史和当前消息中都没有 tool_result）
    for orphaned_id in &unpaired_tool_use_ids {
        warnings.orphan_tool_uses = warnings.orphan_tool_uses.saturating_add(1);
        tracing::warn!(
            "检测到孤立的 tool_use：找不到对应的 tool_result，将从历史中移除，tool_use_id={}",
            orphaned_id
        );
    }

    (filtered_results, unpaired_tool_use_ids)
}

/// 从历史消息中移除孤立的 tool_use
///
/// Kiro API 要求每个 tool_use 必须有对应的 tool_result，否则返回 400 Bad Request。
/// 此函数遍历历史中的 assistant 消息，移除没有对应 tool_result 的 tool_use。
///
/// # Arguments
/// * `history` - 可变的历史消息列表
/// * `orphaned_ids` - 需要移除的孤立 tool_use_id 集合
pub(super) fn remove_orphaned_tool_uses(
    history: &mut [Message],
    orphaned_ids: &std::collections::HashSet<String>,
) {
    if orphaned_ids.is_empty() {
        return;
    }

    for msg in history.iter_mut() {
        if let Message::Assistant(assistant_msg) = msg {
            if let Some(ref mut tool_uses) = assistant_msg.assistant_response_message.tool_uses {
                let original_len = tool_uses.len();
                tool_uses.retain(|tu| !orphaned_ids.contains(&tu.tool_use_id));

                // 如果移除后为空，设置为 None
                if tool_uses.is_empty() {
                    assistant_msg.assistant_response_message.tool_uses = None;
                } else if tool_uses.len() != original_len {
                    tracing::debug!(
                        "从 assistant 消息中移除了 {} 个孤立的 tool_use",
                        original_len - tool_uses.len()
                    );
                }
            }
        }
    }
}
