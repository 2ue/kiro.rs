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
    if is_single_computer_tool(Some(kind)) {
        return Some(computer());
    }
    if kind.starts_with("web_fetch_") && name == "web_fetch" {
        return Some(web_fetch());
    }
    None
}

/// Toolset name carried as `toolset_name` on the member `tool_use` / `tool_result` blocks of
/// `computer_toolset_*`.
pub(crate) const COMPUTER_TOOLSET_NAME: &str = "computer";

/// One member tool of a toolset entry, sent upstream as an ordinary tool.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolsetMember {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    pub(crate) input_schema: Value,
}

/// The toolset name and the enabled member tools of a toolset entry, in the official order, or
/// `None` for any other tool. `configs` turns single members off with `{"zoom": {"enabled":
/// false}}`; members it does not mention stay enabled.
pub(crate) fn toolset_members(
    tool_type: Option<&str>,
    configs: Option<&Value>,
) -> Option<(&'static str, Vec<ToolsetMember>)> {
    if !tool_type?.starts_with("computer_toolset_") {
        return None;
    }
    let enabled = |name: &str| {
        configs
            .and_then(|configs| configs.get(name))
            .and_then(|config| config.get("enabled"))
            .and_then(Value::as_bool)
            != Some(false)
    };
    let members = computer_toolset_members()
        .into_iter()
        .filter(|member| enabled(member.name))
        .collect();
    Some((COMPUTER_TOOLSET_NAME, members))
}

/// `computer_20251124` and earlier: one `computer` tool whose `input.action` selects the action.
pub(crate) fn is_single_computer_tool(tool_type: Option<&str>) -> bool {
    tool_type
        .is_some_and(|kind| kind.starts_with("computer_") && !kind.starts_with("computer_toolset_"))
}

fn point_schema(description: &str) -> Value {
    json!({
        "type": "array",
        "items": {"type": "integer"},
        "minItems": 2,
        "maxItems": 2,
        "description": description
    })
}

fn modifier_schema() -> Value {
    json!({
        "type": "string",
        "description": "Optional modifier keys held only for this action: shift, ctrl, alt, super, or a `+`-joined combination such as ctrl+shift."
    })
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": properties, "required": required})
}

/// The 17 members of `computer_toolset_20260801`. Coordinates are pixels in the full screenshot
/// space with the origin at the top left, also after a zoom.
fn computer_toolset_members() -> Vec<ToolsetMember> {
    let click = |name: &'static str, description: &'static str| ToolsetMember {
        name,
        description,
        input_schema: object_schema(
            json!({
                "coordinate": point_schema("Optional [x, y] screenshot pixel to click; omit to click at the current cursor position."),
                "text": modifier_schema()
            }),
            &[],
        ),
    };
    let no_input = |name: &'static str, description: &'static str| ToolsetMember {
        name,
        description,
        input_schema: object_schema(json!({}), &[]),
    };
    vec![
        no_input(
            "screenshot",
            "Take a screenshot of the full screen. Use it to see the current state before and after acting.",
        ),
        ToolsetMember {
            name: "zoom",
            description: "View a screen region at full resolution. Coordinates of later actions stay in the full screenshot space, never relative to the zoomed image.",
            input_schema: object_schema(
                json!({
                    "region": {
                        "type": "array",
                        "items": {"type": "integer"},
                        "minItems": 4,
                        "maxItems": 4,
                        "description": "[x0, y0, x1, y1]: top-left and bottom-right corners in screenshot pixels."
                    }
                }),
                &["region"],
            ),
        },
        click("left_click", "Left-click at a screenshot pixel."),
        click("right_click", "Right-click at a screenshot pixel."),
        click("middle_click", "Middle-click at a screenshot pixel."),
        click("double_click", "Double-click at a screenshot pixel."),
        click("triple_click", "Triple-click at a screenshot pixel."),
        ToolsetMember {
            name: "left_click_drag",
            description: "Press the left button at start_coordinate, drag to coordinate and release.",
            input_schema: object_schema(
                json!({
                    "start_coordinate": point_schema("[x, y] screenshot pixel where the drag starts."),
                    "coordinate": point_schema("[x, y] screenshot pixel where the drag ends."),
                    "text": modifier_schema()
                }),
                &["start_coordinate", "coordinate"],
            ),
        },
        ToolsetMember {
            name: "mouse_move",
            description: "Move the mouse cursor to a screenshot pixel.",
            input_schema: object_schema(
                json!({"coordinate": point_schema("[x, y] screenshot pixel to move the cursor to.")}),
                &["coordinate"],
            ),
        },
        no_input(
            "left_mouse_down",
            "Press the left mouse button at the current cursor position; call mouse_move first.",
        ),
        no_input(
            "left_mouse_up",
            "Release the left mouse button at the current cursor position.",
        ),
        no_input(
            "cursor_position",
            "Report the current cursor position in screenshot pixels.",
        ),
        ToolsetMember {
            name: "scroll",
            description: "Scroll the mouse wheel, optionally after moving to a screenshot pixel.",
            input_schema: object_schema(
                json!({
                    "scroll_direction": {"type": "string", "enum": ["up", "down", "left", "right"]},
                    "scroll_amount": {"type": "integer", "minimum": 1, "description": "Number of wheel clicks."},
                    "coordinate": point_schema("Optional [x, y] screenshot pixel to scroll at; omit to scroll at the current cursor position."),
                    "text": modifier_schema()
                }),
                &["scroll_direction", "scroll_amount"],
            ),
        },
        ToolsetMember {
            name: "type",
            description: "Type a string of text with the keyboard.",
            input_schema: object_schema(
                json!({"text": {"type": "string", "description": "The text to type."}}),
                &["text"],
            ),
        },
        ToolsetMember {
            name: "key",
            description: "Press a key or key combination, for example Return, ctrl+s or alt+Tab.",
            input_schema: object_schema(
                json!({
                    "text": {"type": "string", "description": "A key or `+`-joined combination."},
                    "repeat": {"type": "integer", "minimum": 1, "maximum": 100, "description": "Optional number of presses, default 1."}
                }),
                &["text"],
            ),
        },
        ToolsetMember {
            name: "hold_key",
            description: "Hold a key or key combination down for a number of seconds.",
            input_schema: object_schema(
                json!({
                    "text": {"type": "string", "description": "A key or `+`-joined combination."},
                    "duration": {"type": "number", "minimum": 0, "maximum": 300, "description": "Seconds to hold, up to 300."}
                }),
                &["text", "duration"],
            ),
        },
        ToolsetMember {
            name: "wait",
            description: "Wait for a number of seconds before the next action.",
            input_schema: object_schema(
                json!({"duration": {"type": "number", "minimum": 0, "maximum": 300, "description": "Seconds to wait, up to 300."}}),
                &["duration"],
            ),
        },
    ]
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
    fn computer_toolset_expands_to_the_seventeen_official_members() {
        let (toolset, members) =
            toolset_members(Some("computer_toolset_20260801"), None).expect("toolset members");
        assert_eq!(toolset, "computer");
        let names: Vec<&str> = members.iter().map(|member| member.name).collect();
        assert_eq!(
            names,
            [
                "screenshot",
                "zoom",
                "left_click",
                "right_click",
                "middle_click",
                "double_click",
                "triple_click",
                "left_click_drag",
                "mouse_move",
                "left_mouse_down",
                "left_mouse_up",
                "cursor_position",
                "scroll",
                "type",
                "key",
                "hold_key",
                "wait"
            ]
        );
        let member = |name: &str| members.iter().find(|member| member.name == name).unwrap();
        let required = |name: &str| -> Vec<String> {
            member(name).input_schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_string())
                .collect()
        };
        for member in &members {
            assert_eq!(member.input_schema["type"], "object", "{}", member.name);
            assert!(
                member.input_schema["properties"].is_object(),
                "{}",
                member.name
            );
            assert!(!member.description.is_empty(), "{}", member.name);
            assert!(
                member
                    .name
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch == '_'),
                "{}",
                member.name
            );
        }
        assert!(required("screenshot").is_empty());
        assert!(required("left_click").is_empty());
        assert!(member("left_click").input_schema["properties"]["coordinate"].is_object());
        assert!(member("left_click").input_schema["properties"]["text"].is_object());
        assert_eq!(
            required("left_click_drag"),
            ["start_coordinate", "coordinate"]
        );
        assert_eq!(required("mouse_move"), ["coordinate"]);
        assert_eq!(required("zoom"), ["region"]);
        assert_eq!(
            member("zoom").input_schema["properties"]["region"]["minItems"],
            4
        );
        assert_eq!(required("scroll"), ["scroll_direction", "scroll_amount"]);
        assert_eq!(
            member("scroll").input_schema["properties"]["scroll_direction"]["enum"],
            json!(["up", "down", "left", "right"])
        );
        assert_eq!(required("type"), ["text"]);
        assert_eq!(required("key"), ["text"]);
        assert_eq!(required("hold_key"), ["text", "duration"]);
        assert_eq!(required("wait"), ["duration"]);
    }

    #[test]
    fn toolset_configs_disable_single_members() {
        let configs = json!({"zoom": {"enabled": false}, "wait": {"enabled": true}, "key": {"defer_loading": true}});
        let (_, members) =
            toolset_members(Some("computer_toolset_20260801"), Some(&configs)).unwrap();
        let names: Vec<&str> = members.iter().map(|member| member.name).collect();
        assert_eq!(names.len(), 16);
        assert!(!names.contains(&"zoom"));
        assert!(names.contains(&"wait"));
        assert!(names.contains(&"key"));
        assert!(toolset_members(Some("computer_20251124"), None).is_none());
        assert!(toolset_members(None, None).is_none());
        assert!(is_single_computer_tool(Some("computer_20251124")));
        assert!(!is_single_computer_tool(Some("computer_toolset_20260801")));
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
