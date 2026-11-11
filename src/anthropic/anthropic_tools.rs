//! Input contracts for Anthropic-defined client tools.
//!
//! Claude Code and the SDKs declare these tools by `type` and `name` only (no `input_schema`),
//! and the model is expected to know their input shape. Kiro sends every tool as a plain JSON
//! schema tool, so an empty schema leaves the model guessing. These contracts describe the
//! official input of each tool so the upstream model emits the same `tool_use.input` the client
//! executes; the tool keeps its name and the client still runs it.

use serde_json::{Value, json};

/// The concrete schema and description for an Anthropic-defined tool, or `None` for any other
/// tool (including tools that already carry a schema).
pub(crate) fn anthropic_tool_contract(
    tool_type: Option<&str>,
    name: &str,
    has_schema: bool,
) -> Option<(Value, &'static str)> {
    if has_schema {
        return None;
    }
    let kind = tool_type?;
    if kind.starts_with("bash_") {
        return Some(bash());
    }
    if kind.starts_with("text_editor_") {
        return Some(text_editor(kind));
    }
    if kind.starts_with("memory_") {
        return Some(memory());
    }
    if kind.starts_with("computer_") && !kind.starts_with("computer_toolset_") {
        return Some(computer());
    }
    if kind.starts_with("web_fetch_") && name == "web_fetch" {
        return Some(web_fetch());
    }
    None
}

fn bash() -> (Value, &'static str) {
    (
        json!({
           "type": "object",
           "properties": {
        "command": {"type": "string", "description": "The bash command to run."},
        "restart": {"type": "boolean", "description": "Set to true to restart the bash session."}
           }
              }),
        "Run commands in a persistent bash session. State such as the working directory and environment variables carries over between calls. Pass `command` to run a command, or `restart: true` to start a fresh session.",
    )
}

fn text_editor(kind: &str) -> (Value, &'static str) {
    // `text_editor_20250124` still has `undo_edit`; later versions dropped it.
    let commands: Vec<&str> = if kind.starts_with("text_editor_20250124") {
        vec!["view", "create", "str_replace", "insert", "undo_edit"]
    } else {
        vec!["view", "create", "str_replace", "insert"]
    };
    (
        json!({
           "type": "object",
           "properties": {
        "command": {"type": "string", "enum": commands, "description": "The operation to perform."},
        "path": {"type": "string", "description": "Absolute path of the file or directory."},
        "file_text": {"type": "string", "description": "Content of the new file (create)."},
        "old_str": {"type": "string", "description": "Exact text to replace; must match exactly once (str_replace)."},
        "new_str": {"type": "string", "description": "Replacement text (str_replace), or text to insert (insert)."},
        "insert_line": {"type": "integer", "description": "Line number after which to insert; 0 inserts at the start (insert)."},
        "view_range": {
            "type": "array",
            "items": {"type": "integer"},
            "description": "Optional [start, end] line range to view; end -1 means to the end of the file (view)."
        }
           },
           "required": ["command", "path"]
              }),
        "View, create and edit files. `view` shows a file with line numbers or lists a directory, `create` writes a new file, `str_replace` replaces one exact occurrence of `old_str` with `new_str`, and `insert` adds `new_str` after `insert_line`.",
    )
}

fn memory() -> (Value, &'static str) {
    (
        json!({
           "type": "object",
           "properties": {
        "command": {
            "type": "string",
            "enum": ["view", "create", "str_replace", "insert", "delete", "rename"],
            "description": "The operation to perform in the /memories directory."
        },
        "path": {"type": "string", "description": "Path under /memories."},
        "file_text": {"type": "string", "description": "Content of the new file (create)."},
        "old_str": {"type": "string", "description": "Exact text to replace (str_replace)."},
        "new_str": {"type": "string", "description": "Replacement text (str_replace)."},
        "insert_line": {"type": "integer", "description": "Line number after which to insert (insert)."},
        "insert_text": {"type": "string", "description": "Text to insert (insert)."},
        "old_path": {"type": "string", "description": "Current path (rename)."},
        "new_path": {"type": "string", "description": "New path (rename)."},
        "view_range": {"type": "array", "items": {"type": "integer"}, "description": "Optional [start, end] line range (view)."}
           },
           "required": ["command"]
              }),
        "Store and retrieve information across conversations in the /memories directory. Check it at the start of a task, and record progress, decisions and context that will help later.",
    )
}

fn computer() -> (Value, &'static str) {
    (
        json!({
           "type": "object",
           "properties": {
        "action": {
            "type": "string",
            "enum": [
         "screenshot", "zoom", "key", "type", "mouse_move", "left_click", "right_click",
         "middle_click", "double_click", "triple_click", "left_click_drag",
         "left_mouse_down", "left_mouse_up", "scroll", "hold_key", "wait", "cursor_position"
            ],
            "description": "The action to perform."
        },
        "coordinate": {
            "type": "array",
            "items": {"type": "integer"},
            "description": "[x, y] pixel position on the screen for mouse actions."
        },
        "start_coordinate": {
            "type": "array",
            "items": {"type": "integer"},
            "description": "[x, y] start position for left_click_drag."
        },
        "region": {
            "type": "array",
            "items": {"type": "integer"},
            "description": "[x1, y1, x2, y2] screen region to inspect (zoom)."
        },
        "text": {"type": "string", "description": "Text to type, or the key combination to press (key, hold_key)."},
        "scroll_direction": {"type": "string", "enum": ["up", "down", "left", "right"]},
        "scroll_amount": {"type": "integer", "description": "Number of scroll steps."},
        "duration": {"type": "number", "description": "Seconds to wait or hold."}
           },
           "required": ["action"]
              }),
        "Control the computer screen, mouse and keyboard. Take a screenshot to see the screen, then click, type, scroll or press keys at pixel coordinates from the latest screenshot.",
    )
}

fn web_fetch() -> (Value, &'static str) {
    (
        json!({
           "type": "object",
           "properties": {
        "url": {"type": "string", "description": "The URL to fetch. It must already appear in the conversation."}
           },
           "required": ["url"]
              }),
        "Fetch the content of a web page or PDF at a URL that appears in the conversation, and return its text.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_less_anthropic_tools_get_their_contract() {
        for (kind, name, required) in [
            ("bash_20250124", "bash", None),
            (
                "text_editor_20250728",
                "str_replace_based_edit_tool",
                Some("path"),
            ),
            ("memory_20250818", "memory", Some("command")),
            ("computer_20251124", "computer", Some("action")),
            ("web_fetch_20260209", "web_fetch", Some("url")),
        ] {
            let (schema, description) = anthropic_tool_contract(Some(kind), name, false)
                .unwrap_or_else(|| panic!("{kind} needs a contract"));
            assert_eq!(schema["type"], "object", "{kind}");
            assert!(!description.is_empty());
            if let Some(field) = required {
                assert!(
                    schema["required"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|value| value == field),
                    "{kind} requires {field}"
                );
            }
        }
    }

    #[test]
    fn undo_edit_only_exists_on_the_first_text_editor_version() {
        let (old, _) =
            anthropic_tool_contract(Some("text_editor_20250124"), "str_replace_editor", false)
                .unwrap();
        assert!(
            old["properties"]["command"]["enum"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "undo_edit")
        );
        let (new, _) = anthropic_tool_contract(
            Some("text_editor_20250728"),
            "str_replace_based_edit_tool",
            false,
        )
        .unwrap();
        assert!(
            !new["properties"]["command"]["enum"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "undo_edit")
        );
    }

    #[test]
    fn tools_with_a_schema_or_unknown_types_are_untouched() {
        assert!(anthropic_tool_contract(Some("bash_20250124"), "bash", true).is_none());
        assert!(anthropic_tool_contract(None, "Read", false).is_none());
        assert!(anthropic_tool_contract(Some("custom"), "x", false).is_none());
        assert!(anthropic_tool_contract(Some("computer_toolset_20260801"), "", false).is_none());
    }
}
