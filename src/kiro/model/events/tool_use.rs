//! 工具使用事件
//!
//! 处理 toolUseEvent 类型的事件

use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::kiro::parser::error::ParseResult;
use crate::kiro::parser::frame::Frame;

use super::base::EventPayload;

/// 工具使用事件
///
/// 包含工具调用的流式数据
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolUseEvent {
    /// 工具名称
    pub name: String,
    /// 工具调用 ID
    pub tool_use_id: String,
    /// 工具输入数据 (JSON 字符串，可能是流式的部分数据)
    #[serde(default, deserialize_with = "deserialize_tool_input")]
    pub input: String,
    /// 是否是最后一个块
    #[serde(default)]
    pub stop: bool,
}

/// Kiro versions have emitted both string fragments and complete JSON values
/// for `toolUseEvent.input`. The stream adapter consumes ordered text
/// fragments, so complete values are encoded once as compact JSON here.
fn deserialize_tool_input<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Null => Ok(String::new()),
        Value::String(value) => Ok(value),
        value => serde_json::to_string(&value).map_err(serde::de::Error::custom),
    }
}

impl EventPayload for ToolUseEvent {
    fn from_frame(frame: &Frame) -> ParseResult<Self> {
        frame.payload_as_event_json("toolUseEvent")
    }
}

impl std::fmt::Display for ToolUseEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.stop {
            write!(
                f,
                "ToolUse[{}] (id={}, complete): {}",
                self.name, self.tool_use_id, self.input
            )
        } else {
            write!(
                f,
                "ToolUse[{}] (id={}, partial): {}",
                self.name, self.tool_use_id, self.input
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_input_deserializer_keeps_string_fragments_unchanged() {
        let event: ToolUseEvent = serde_json::from_str(
            r#"{"name":"Bash","toolUseId":"toolu_1","input":"{\"command\":\"pwd\"}"}"#,
        )
        .expect("string tool input");
        assert_eq!(event.input, r#"{"command":"pwd"}"#);
    }

    #[test]
    fn tool_input_deserializer_serializes_complete_json_values() {
        let event: ToolUseEvent = serde_json::from_str(
            r#"{"name":"Bash","toolUseId":"toolu_2","input":{"command":"pwd"},"stop":true}"#,
        )
        .expect("object tool input");
        assert_eq!(event.input, r#"{"command":"pwd"}"#);
        assert!(event.stop);
    }
}
