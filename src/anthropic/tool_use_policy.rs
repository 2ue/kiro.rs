//! Response-side enforcement of Anthropic tool-choice semantics that Kiro cannot express.
//!
//! Kiro has no `tool_choice` field and rejects history `toolUse` entries whose tool is missing
//! from the current tool list, so the converter keeps placeholder definitions for history tools
//! even when the client offered no tools for this turn. The model can still call those
//! placeholders; this gate removes such calls before they reach the client, where Claude Code
//! would otherwise execute them.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

/// Tool call the request's `tool_choice` requires in this turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RequiredToolUse {
    /// `tool_choice: {"type": "any"}`
    Any,
    /// `tool_choice: {"type": "tool", "name": ...}` (client-side name)
    Named(String),
}

/// Per-request policy derived from the Anthropic request during conversion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ResponseToolPolicy {
    /// Lower-cased tool names (Kiro-side and client-side) that must not be called this turn.
    unavailable_tool_names: HashSet<String>,
    /// Maximum number of tool calls forwarded downstream (`disable_parallel_tool_use` → 1).
    max_tool_uses: Option<usize>,
    /// Kiro cannot force a tool call; this is only diagnosed after the response completes.
    required: Option<RequiredToolUse>,
}

impl ResponseToolPolicy {
    pub(crate) fn block_tool(&mut self, name: &str) {
        let name = name.trim();
        if !name.is_empty() {
            self.unavailable_tool_names
                .insert(name.to_ascii_lowercase());
        }
    }

    pub(crate) fn require_tool_use(&mut self, required: RequiredToolUse) {
        self.required = Some(required);
    }

    #[cfg(test)]
    pub(crate) fn required(&self) -> Option<&RequiredToolUse> {
        self.required.as_ref()
    }

    pub(crate) fn limit_tool_uses(&mut self, max: usize) {
        self.max_tool_uses = Some(max);
    }

    #[cfg(test)]
    pub(crate) fn max_tool_uses(&self) -> Option<usize> {
        self.max_tool_uses
    }

    pub(crate) fn is_unavailable(&self, name: &str) -> bool {
        self.unavailable_tool_names
            .contains(&name.trim().to_ascii_lowercase())
    }

    pub(crate) fn gate(&self) -> ResponseToolGate {
        ResponseToolGate {
            policy: self.clone(),
            ..ResponseToolGate::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolUseDropReason {
    /// The tool only exists as a history placeholder; the client offered no such tool this turn.
    UnavailableTool,
    /// The client set `disable_parallel_tool_use` and an earlier call was already forwarded.
    ParallelDisabled,
}

impl ToolUseDropReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::UnavailableTool => "unavailable_tool",
            Self::ParallelDisabled => "parallel_tool_use_disabled",
        }
    }
}

/// Stateful admission decisions for one response. Decisions are keyed by tool_use id so every
/// streamed fragment of a tool call gets the same answer as its first fragment.
#[derive(Debug, Clone, Default)]
pub(crate) struct ResponseToolGate {
    policy: ResponseToolPolicy,
    decisions: HashMap<String, bool>,
    admitted: usize,
    admitted_names: Vec<String>,
    dropped: Vec<(String, ToolUseDropReason)>,
}

impl ResponseToolGate {
    /// Returns whether the tool call may be forwarded downstream.
    pub(crate) fn admit(&mut self, tool_use_id: &str, name: &str) -> bool {
        if let Some(&admitted) = self.decisions.get(tool_use_id) {
            return admitted;
        }
        let reason = if self.policy.is_unavailable(name) {
            Some(ToolUseDropReason::UnavailableTool)
        } else if self
            .policy
            .max_tool_uses
            .is_some_and(|max| self.admitted >= max)
        {
            Some(ToolUseDropReason::ParallelDisabled)
        } else {
            None
        };
        let admitted = reason.is_none();
        if let Some(reason) = reason {
            tracing::warn!(
                tool = %name,
                tool_use_id = %tool_use_id,
                reason = reason.as_str(),
                "丢弃违反本轮工具约束的 tool_use"
            );
            self.dropped.push((name.to_string(), reason));
        } else {
            self.admitted += 1;
            self.admitted_names.push(name.to_string());
        }
        if !tool_use_id.is_empty() {
            self.decisions.insert(tool_use_id.to_string(), admitted);
        }
        admitted
    }

    /// Removes disallowed `tool_use` blocks from a complete non-streaming content array.
    /// Returns the number of removed blocks.
    pub(crate) fn retain_admitted_tool_use_blocks(&mut self, content: &mut Vec<Value>) -> usize {
        let before = content.len();
        content.retain(|block| {
            if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                return true;
            }
            let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
            let name = block
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            self.admit(id, name)
        });
        before - content.len()
    }

    pub(crate) fn dropped_count(&self) -> usize {
        self.dropped.len()
    }

    /// `reason:tool` summaries of dropped calls for usage diagnostics (bounded).
    pub(crate) fn dropped_summaries(&self) -> Vec<String> {
        self.dropped
            .iter()
            .take(MAX_DIAGNOSTIC_ENTRIES)
            .map(|(name, reason)| format!("{}:{}", reason.as_str(), name))
            .collect()
    }

    /// Whether the completed response violated a required `tool_choice`, and how.
    pub(crate) fn tool_choice_violation(&self) -> Option<&'static str> {
        match self.policy.required.as_ref()? {
            RequiredToolUse::Any => (self.admitted == 0).then_some("any_without_tool_use"),
            RequiredToolUse::Named(required) => {
                if self
                    .admitted_names
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(required))
                {
                    None
                } else if self.admitted == 0 {
                    Some("named_tool_without_tool_use")
                } else {
                    Some("named_tool_mismatch")
                }
            }
        }
    }

    /// Logs dropped calls and `tool_choice` violations once per completed response.
    pub(crate) fn log_completion(&self, request_id: &str) {
        if let Some(violation) = self.tool_choice_violation() {
            tracing::warn!(
                request_id = %request_id,
                violation,
                required = ?self.policy.required,
                called_tools = ?self.admitted_names,
                "上游响应违反 tool_choice 约束（Kiro 无法强制工具调用）"
            );
        }
        if !self.dropped.is_empty() {
            tracing::warn!(
                request_id = %request_id,
                dropped_tool_uses = ?self.dropped_summaries(),
                "响应中违反工具约束的 tool_use 已丢弃"
            );
        }
    }
}

const MAX_DIAGNOSTIC_ENTRIES: usize = 8;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn blocking(names: &[&str]) -> ResponseToolPolicy {
        let mut policy = ResponseToolPolicy::default();
        for name in names {
            policy.block_tool(name);
        }
        policy
    }

    #[test]
    fn unavailable_tool_is_dropped_case_insensitively_and_consistently_per_id() {
        let mut gate = blocking(&["Bash"]).gate();

        assert!(!gate.admit("toolu_1", "bash"));
        // Later fragments of the same call keep the first decision.
        assert!(!gate.admit("toolu_1", "bash"));
        assert!(gate.admit("toolu_2", "Read"));
        assert_eq!(gate.dropped_count(), 1);
    }

    #[test]
    fn disabled_parallel_tool_use_keeps_only_the_first_call() {
        let mut policy = ResponseToolPolicy::default();
        policy.limit_tool_uses(1);
        let mut gate = policy.gate();

        assert!(gate.admit("toolu_1", "Read"));
        // Continuation fragments of the admitted call stay admitted.
        assert!(gate.admit("toolu_1", "Read"));
        assert!(!gate.admit("toolu_2", "Read"));
        assert!(!gate.admit("toolu_3", "Bash"));
        assert_eq!(gate.dropped_count(), 2);

        let mut content = vec![
            json!({"type": "text", "text": "reading"}),
            json!({"type": "tool_use", "id": "a", "name": "Read", "input": {"file_path": "a"}}),
            json!({"type": "tool_use", "id": "b", "name": "Read", "input": {"file_path": "b"}}),
        ];
        let mut gate = policy.gate();
        assert_eq!(gate.retain_admitted_tool_use_blocks(&mut content), 1);
        assert_eq!(content.len(), 2);
        assert_eq!(content[1]["id"], "a");
    }

    #[test]
    fn required_tool_choice_violations_are_classified() {
        let mut any = ResponseToolPolicy::default();
        any.require_tool_use(RequiredToolUse::Any);
        assert_eq!(
            any.gate().tool_choice_violation(),
            Some("any_without_tool_use")
        );
        let mut gate = any.gate();
        gate.admit("toolu_1", "Read");
        assert_eq!(gate.tool_choice_violation(), None);

        let mut named = ResponseToolPolicy::default();
        named.require_tool_use(RequiredToolUse::Named("Read".to_string()));
        assert_eq!(
            named.gate().tool_choice_violation(),
            Some("named_tool_without_tool_use")
        );
        let mut gate = named.gate();
        gate.admit("toolu_1", "Bash");
        assert_eq!(gate.tool_choice_violation(), Some("named_tool_mismatch"));
        gate.admit("toolu_2", "Read");
        assert_eq!(gate.tool_choice_violation(), None);

        assert_eq!(
            ResponseToolPolicy::default().gate().tool_choice_violation(),
            None
        );
    }

    #[test]
    fn dropped_calls_are_summarized_for_diagnostics() {
        let mut policy = blocking(&["Bash"]);
        policy.limit_tool_uses(1);
        let mut gate = policy.gate();
        gate.admit("a", "Bash");
        gate.admit("b", "Read");
        gate.admit("c", "Read");

        assert_eq!(
            gate.dropped_summaries(),
            ["unavailable_tool:Bash", "parallel_tool_use_disabled:Read"]
        );
    }

    #[test]
    fn empty_policy_admits_everything() {
        let mut gate = ResponseToolPolicy::default().gate();

        assert!(gate.admit("toolu_1", "Bash"));
        assert!(gate.admit("toolu_2", "Bash"));
        assert_eq!(gate.dropped_count(), 0);
    }

    #[test]
    fn non_stream_filter_keeps_text_and_allowed_tools_in_order() {
        let mut gate = blocking(&["Bash"]).gate();
        let mut content = vec![
            json!({"type": "text", "text": "hi"}),
            json!({"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {}}),
            json!({"type": "tool_use", "id": "toolu_2", "name": "Read", "input": {}}),
        ];

        assert_eq!(gate.retain_admitted_tool_use_blocks(&mut content), 1);
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["name"], "Read");
    }
}
