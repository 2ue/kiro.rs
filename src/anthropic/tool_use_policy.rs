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
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ResponseToolPolicy {
    /// Lower-cased tool names (Kiro-side and client-side) that must not be called this turn.
    unavailable_tool_names: HashSet<String>,
    /// Maximum number of tool calls forwarded downstream (`disable_parallel_tool_use` → 1).
    max_tool_uses: Option<usize>,
    /// Kiro cannot force a tool call; this is only diagnosed after the response completes.
    required: Option<RequiredToolUse>,
    /// `output_config.format` served through an internal tool; restored to JSON text downstream.
    structured_output: Option<super::structured_output::StructuredOutputTarget>,
    /// Member tools expanded from a toolset entry (client-side name → toolset name). Their
    /// downstream `tool_use` blocks carry `toolset_name`.
    toolset_members: HashMap<String, String>,
    /// Whether the request declares the single `computer` tool (`computer_20251124` and earlier).
    single_computer_tool: bool,
    /// Original-to-resized ratio of the latest screenshot, when the proxy downscaled it.
    screenshot_scale: Option<super::image_resize::ScreenshotScale>,
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

    pub(crate) fn set_structured_output(
        &mut self,
        target: super::structured_output::StructuredOutputTarget,
    ) {
        self.structured_output = Some(target);
    }

    pub(crate) fn structured_output(
        &self,
    ) -> Option<&super::structured_output::StructuredOutputTarget> {
        self.structured_output.as_ref()
    }

    pub(crate) fn set_toolset_members(&mut self, members: HashMap<String, String>) {
        self.toolset_members = members;
    }

    pub(crate) fn set_single_computer_tool(&mut self, declared: bool) {
        self.single_computer_tool = declared;
    }

    pub(crate) fn set_screenshot_scale(
        &mut self,
        scale: Option<super::image_resize::ScreenshotScale>,
    ) {
        self.screenshot_scale = scale;
    }

    /// Toolset name for a downstream tool name expanded from a toolset entry.
    pub(crate) fn toolset_name(&self, tool_name: &str) -> Option<&str> {
        self.toolset_members.get(tool_name).map(String::as_str)
    }

    /// Whether this tool's input holds screenshot coordinates that must be mapped back to the
    /// original screenshot before the client runs it.
    pub(crate) fn rescales_tool_input(&self, tool_name: &str) -> bool {
        self.screenshot_scale.is_some() && self.is_computer_tool(tool_name)
    }

    fn is_computer_tool(&self, tool_name: &str) -> bool {
        self.toolset_name(tool_name) == Some(super::anthropic_tools::COMPUTER_TOOLSET_NAME)
            || (self.single_computer_tool && tool_name == "computer")
    }

    /// Final downstream shape of a computer-use `tool_use`: screenshot coordinates mapped back to
    /// the original screenshot. Returns whether the input changed.
    pub(crate) fn rescale_tool_input(&self, tool_name: &str, input: &mut Value) -> bool {
        match self.screenshot_scale {
            Some(scale) if self.is_computer_tool(tool_name) => scale.rescale_computer_input(input),
            _ => false,
        }
    }

    /// Same as [`Self::rescale_tool_input`] for a serialized input; unparseable JSON is
    /// returned unchanged.
    pub(crate) fn rescale_tool_input_json(&self, tool_name: &str, input_json: &str) -> String {
        if !self.rescales_tool_input(tool_name) {
            return input_json.to_string();
        }
        let Ok(mut input) = serde_json::from_str::<Value>(input_json) else {
            return input_json.to_string();
        };
        if self.rescale_tool_input(tool_name, &mut input) {
            serde_json::to_string(&input).unwrap_or_else(|_| input_json.to_string())
        } else {
            input_json.to_string()
        }
    }

    /// Adds `toolset_name` and maps screenshot coordinates back in a complete non-streaming
    /// content array.
    pub(crate) fn finish_tool_use_blocks(&self, content: &mut [Value]) {
        if self.toolset_members.is_empty() && self.screenshot_scale.is_none() {
            return;
        }
        for block in content.iter_mut() {
            if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let Some(name) = block
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                continue;
            };
            if let Some(input) = block.get_mut("input") {
                self.rescale_tool_input(&name, input);
            }
            if let (Some(toolset), Some(object)) = (self.toolset_name(&name), block.as_object_mut())
            {
                object.insert(
                    "toolset_name".to_string(),
                    Value::String(toolset.to_string()),
                );
            }
        }
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

    fn computer_toolset_policy(scale: Option<(u32, u32, u32, u32)>) -> ResponseToolPolicy {
        let mut policy = ResponseToolPolicy::default();
        policy.set_toolset_members(HashMap::from([
            ("left_click".to_string(), "computer".to_string()),
            ("zoom".to_string(), "computer".to_string()),
            ("screenshot".to_string(), "computer".to_string()),
        ]));
        policy.set_screenshot_scale(scale.and_then(|(ow, oh, rw, rh)| {
            super::super::image_resize::ScreenshotScale::from_dimensions((ow, oh), (rw, rh))
        }));
        policy
    }

    #[test]
    fn toolset_member_tool_uses_get_toolset_name_and_original_coordinates() {
        let policy = computer_toolset_policy(Some((2560, 1440, 1280, 720)));
        let mut content = vec![
            json!({"type": "text", "text": "clicking"}),
            json!({"type": "tool_use", "id": "a", "name": "left_click", "input": {"coordinate": [100, 50]}}),
            json!({"type": "tool_use", "id": "b", "name": "zoom", "input": {"region": [0, 0, 640, 360]}}),
            json!({"type": "tool_use", "id": "c", "name": "Read", "input": {"coordinate": [1, 2]}}),
        ];
        policy.finish_tool_use_blocks(&mut content);

        assert!(content[0].get("toolset_name").is_none());
        assert_eq!(content[1]["toolset_name"], "computer");
        assert_eq!(content[1]["input"]["coordinate"], json!([200, 100]));
        assert_eq!(content[2]["toolset_name"], "computer");
        assert_eq!(content[2]["input"]["region"], json!([0, 0, 1280, 720]));
        // Ordinary client tools keep their input and get no toolset name.
        assert!(content[3].get("toolset_name").is_none());
        assert_eq!(content[3]["input"]["coordinate"], json!([1, 2]));
    }

    #[test]
    fn coordinates_are_untouched_without_a_resized_screenshot() {
        let policy = computer_toolset_policy(None);
        let mut content = vec![
            json!({"type": "tool_use", "id": "a", "name": "left_click", "input": {"coordinate": [100, 50]}}),
        ];
        policy.finish_tool_use_blocks(&mut content);
        assert_eq!(content[0]["toolset_name"], "computer");
        assert_eq!(content[0]["input"]["coordinate"], json!([100, 50]));
        assert!(!policy.rescales_tool_input("left_click"));
        assert_eq!(
            policy.rescale_tool_input_json("left_click", r#"{"coordinate":[100,50]}"#),
            r#"{"coordinate":[100,50]}"#
        );
    }

    #[test]
    fn single_computer_tool_coordinates_are_rescaled() {
        let mut policy = ResponseToolPolicy::default();
        policy.set_single_computer_tool(true);
        policy.set_screenshot_scale(
            super::super::image_resize::ScreenshotScale::from_dimensions(
                (3000, 2000),
                (1500, 1000),
            ),
        );
        assert!(policy.rescales_tool_input("computer"));
        assert!(!policy.rescales_tool_input("Read"));
        let rewritten = policy.rescale_tool_input_json(
            "computer",
            r#"{"action":"left_click_drag","start_coordinate":[1,2],"coordinate":[10,20]}"#,
        );
        let rewritten: Value = serde_json::from_str(&rewritten).unwrap();
        assert_eq!(rewritten["start_coordinate"], json!([2, 4]));
        assert_eq!(rewritten["coordinate"], json!([20, 40]));
        assert_eq!(rewritten["action"], "left_click_drag");
        // Invalid JSON passes through unchanged.
        assert_eq!(policy.rescale_tool_input_json("computer", "{"), "{");
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
