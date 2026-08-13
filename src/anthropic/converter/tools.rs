//! Tool definition conversion, name mapping, schema cleanup, and tool-choice steering.

use std::collections::HashMap;

use sha2::{Digest, Sha256};

use crate::anthropic::tool_schema_keys::{SchemaKeyMapper, ToolSchemaKeyMap};
use crate::anthropic::types::{MessagesRequest, Tool as AnthropicTool};
use crate::kiro::model::requests::conversation::Message;
use crate::kiro::model::requests::tool::{InputSchema, Tool, ToolSpecification};

use super::schema::normalize_json_schema;
use super::{ConversionError, ConverterOptions};

/// 追加到 Write 工具 description 末尾的内容
const WRITE_TOOL_DESCRIPTION_SUFFIX: &str = "- IMPORTANT: If the content to write exceeds 150 lines, you MUST only write the first 50 lines using this tool, then use `Edit` tool to append the remaining content in chunks of no more than 50 lines each. If needed, leave a unique placeholder to help append content. Do NOT attempt to write all content at once.";

/// 追加到 Edit 工具 description 末尾的内容
const EDIT_TOOL_DESCRIPTION_SUFFIX: &str = "- IMPORTANT: If the `new_string` content exceeds 50 lines, you MUST split it into multiple Edit calls, each replacing no more than 50 lines at a time. If used to append content, leave a unique placeholder to help append content. On the final chunk, do NOT include the placeholder.";

const EMPTY_TOOL_DESCRIPTION_PLACEHOLDER: &str = "Tool available to the assistant.";

/// 追加到系统提示词的分块写入策略
pub(super) const SYSTEM_CHUNKED_POLICY: &str = "\
When the Write or Edit tool has content size limits, always comply silently. \
Never suggest bypassing these limits via alternative tools. \
Never ask the user whether to switch approaches. \
Complete all chunked operations without commentary.";

#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolChoiceDirective {
    Auto,
    Any,
    None,
    Tool(String),
    Unknown,
}

/// 收集历史消息中使用的所有工具名称
pub(super) fn collect_history_tool_names(history: &[Message]) -> Vec<String> {
    let mut tool_names = Vec::new();
    let mut seen_lower = std::collections::HashSet::new();

    for msg in history {
        if let Message::Assistant(assistant_msg) = msg {
            if let Some(ref tool_uses) = assistant_msg.assistant_response_message.tool_uses {
                for tool_use in tool_uses {
                    if seen_lower.insert(tool_use.name.to_ascii_lowercase()) {
                        tool_names.push(tool_use.name.clone());
                    }
                }
            }
        }
    }

    tool_names
}

/// 为历史中使用但不在 tools 列表中的工具创建占位符定义
/// Kiro API 要求：历史消息中引用的工具必须在 currentMessage.tools 中有定义
pub(super) fn create_placeholder_tool(name: &str, options: ConverterOptions) -> Tool {
    let schema = serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {}
    });
    let schema = if options.conversion.tool_schema_normalization.is_enabled() {
        normalize_json_schema(schema)
    } else {
        schema
    };
    Tool {
        tool_specification: ToolSpecification {
            name: name.to_string(),
            description: "Tool used in conversation history".to_string(),
            input_schema: InputSchema::from_json(schema),
        },
    }
}

/// Kiro API 工具名称最大长度限制
pub(super) const TOOL_NAME_MAX_LEN: usize = 63;
pub(super) const TOOL_HASH_MARKER: &str = "Hash";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct ToolNameMappingSummary {
    pub total: usize,
    pub sanitized: usize,
    pub overlong: usize,
}

fn capitalize_ascii_first(value: &str) -> String {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut result = first.to_ascii_uppercase().to_string();
    result.push_str(chars.as_str());
    result
}

fn sanitize_tool_name(name: &str) -> String {
    let parts: Vec<String> = name
        .split(|c: char| c == '_' || c == '-' || !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect();

    let mut iter = parts.into_iter();
    let Some(first) = iter.next() else {
        return "tool".to_string();
    };

    let mut sanitized = first;
    let mut chars = sanitized.chars();
    if let Some(first_char) = chars.next() {
        sanitized = format!("{}{}", first_char.to_ascii_lowercase(), chars.as_str());
    }
    for part in iter {
        sanitized.push_str(&capitalize_ascii_first(&part));
    }

    if sanitized.is_empty() {
        "tool".to_string()
    } else if !sanitized
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic())
    {
        format!("tool{}", capitalize_ascii_first(&sanitized))
    } else {
        sanitized
    }
}

/// Returns the exact deterministic name used when a request tool must be mapped for Kiro.
pub(super) fn deterministic_mapped_tool_name(name: &str) -> String {
    let sanitized = sanitize_tool_name(name);
    if sanitized != name || sanitized.len() > TOOL_NAME_MAX_LEN {
        shorten_tool_name(&sanitized, name)
    } else {
        sanitized
    }
}

/// Exact name emitted by the pre-2026-05-29 mapper for an overlong tool.
/// This is response/history compatibility only; new requests always use the
/// current Kiro-safe mapper above.
pub(super) fn legacy_overlong_mapped_tool_name(name: &str) -> Option<String> {
    if name.len() <= TOOL_NAME_MAX_LEN {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());
    let hash_hex = format!("{:x}", hasher.finalize());
    let hash_suffix = &hash_hex[..8];
    let prefix_max = TOOL_NAME_MAX_LEN - 1 - 8;
    let prefix = match name.char_indices().nth(prefix_max) {
        Some((index, _)) => &name[..index],
        None => name,
    };
    Some(format!("{prefix}_{hash_suffix}"))
}

/// 生成确定性 Kiro-safe 名称：截断前缀 + Hash + 8 位 SHA256 hex
pub(super) fn shorten_tool_name(name: &str, hash_input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(hash_input.as_bytes());
    let hash_hex = format!("{:x}", hasher.finalize());
    let hash_suffix = &hash_hex[..8];
    // 51 prefix + "Hash" + 8 hash = 63
    let prefix_max = TOOL_NAME_MAX_LEN - TOOL_HASH_MARKER.len() - 8;
    let prefix = match name.char_indices().nth(prefix_max) {
        Some((idx, _)) => &name[..idx],
        None => name,
    };
    format!("{}{}{}", prefix, TOOL_HASH_MARKER, hash_suffix)
}

/// 如果名称超长则缩短，并记录映射（short → original）
pub(super) fn map_tool_name(
    name: &str,
    tool_name_map: &mut HashMap<String, String>,
    options: ConverterOptions,
) -> String {
    if !options.conversion.tool_name_mapping.is_enabled() {
        return name.to_string();
    }
    let mapped = deterministic_mapped_tool_name(name);
    if mapped != name {
        // `convert_tools` rejects ambiguous allocations before committing this
        // reverse map. Keep this helper non-overwriting as a final invariant for
        // its test-only/direct callers.
        tool_name_map
            .entry(mapped.clone())
            .or_insert_with(|| name.to_string());
    }
    mapped
}

pub(super) fn summarize_tool_name_mapping(
    tool_name_map: &HashMap<String, String>,
) -> ToolNameMappingSummary {
    let mut summary = ToolNameMappingSummary {
        total: tool_name_map.len(),
        ..ToolNameMappingSummary::default()
    };

    for original in tool_name_map.values() {
        let sanitized = sanitize_tool_name(original);
        if sanitized != *original {
            summary.sanitized += 1;
        }
        if original.len() > TOOL_NAME_MAX_LEN || sanitized.len() > TOOL_NAME_MAX_LEN {
            summary.overlong += 1;
        }
    }

    summary
}

fn parse_tool_choice(tool_choice: &Option<serde_json::Value>) -> ToolChoiceDirective {
    let Some(value) = tool_choice else {
        return ToolChoiceDirective::Auto;
    };

    if let Some(choice) = value.as_str() {
        return match choice {
            "auto" => ToolChoiceDirective::Auto,
            "any" => ToolChoiceDirective::Any,
            "none" => ToolChoiceDirective::None,
            _ => ToolChoiceDirective::Unknown,
        };
    }

    let Some(obj) = value.as_object() else {
        return ToolChoiceDirective::Unknown;
    };

    match obj.get("type").and_then(|v| v.as_str()) {
        Some("auto") => ToolChoiceDirective::Auto,
        Some("any") => ToolChoiceDirective::Any,
        Some("none") => ToolChoiceDirective::None,
        Some("tool") => obj
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|name| !name.trim().is_empty())
            .map(|name| ToolChoiceDirective::Tool(name.to_string()))
            .unwrap_or(ToolChoiceDirective::Unknown),
        _ => ToolChoiceDirective::Unknown,
    }
}

fn selected_tool_indices(
    tools: &[AnthropicTool],
    directive: &ToolChoiceDirective,
) -> Result<Vec<usize>, ConversionError> {
    match directive {
        ToolChoiceDirective::None => Ok(Vec::new()),
        ToolChoiceDirective::Tool(requested_name) => {
            let exact = tools
                .iter()
                .enumerate()
                .filter_map(|(idx, tool)| {
                    (tool.name.as_str() == requested_name.as_str()).then_some(idx)
                })
                .collect::<Vec<_>>();
            if !exact.is_empty() {
                return Ok(exact);
            }

            let normalized_requested = sanitize_tool_name(requested_name);
            let normalized = tools
                .iter()
                .enumerate()
                .filter_map(|(idx, tool)| {
                    (sanitize_tool_name(&tool.name) == normalized_requested).then_some(idx)
                })
                .collect::<Vec<_>>();
            match normalized.len() {
                0 => {
                    tracing::warn!(
                        requested_tool = requested_name,
                        "tool_choice requested a tool that is not present in tools; preserving all tools for compatibility"
                    );
                    Ok((0..tools.len()).collect())
                }
                1 => Ok(normalized),
                _ => Err(ConversionError::UnsupportedContent(
                    "tool_choice name is ambiguous after tool-name normalization".to_string(),
                )),
            }
        }
        ToolChoiceDirective::Auto | ToolChoiceDirective::Any | ToolChoiceDirective::Unknown => {
            Ok((0..tools.len()).collect())
        }
    }
}

fn allocate_selected_tool_names(
    tools: &[AnthropicTool],
    selected_indices: &[usize],
    existing_tool_name_map: &HashMap<String, String>,
    options: &ConverterOptions,
) -> Result<(Vec<(usize, String)>, HashMap<String, String>), ConversionError> {
    let mut reserved = HashMap::<String, String>::new();
    let mut selected_definition_by_name = HashMap::<String, usize>::new();
    for (mapped, original) in existing_tool_name_map {
        let key = mapped.to_ascii_lowercase();
        if reserved.insert(key, original.clone()).is_some() {
            return Err(ConversionError::UnsupportedContent(
                "existing tool-name reverse map is ambiguous".to_string(),
            ));
        }
    }

    let mut allocated = Vec::with_capacity(selected_indices.len());
    let mut pending_reverse_map = HashMap::new();
    for &idx in selected_indices {
        let tool = &tools[idx];
        let mapped = if options.conversion.tool_name_mapping.is_enabled() {
            deterministic_mapped_tool_name(&tool.name)
        } else {
            tool.name.clone()
        };
        let collision_key = mapped.to_ascii_lowercase();
        if let Some(previous_original) = reserved.get(&collision_key) {
            if previous_original == &tool.name {
                if let Some(previous_idx) = selected_definition_by_name.get(&collision_key) {
                    if equivalent_tool_definitions(&tools[*previous_idx], tool) {
                        tracing::warn!(
                            original_tool_name = %tool.name,
                            mapped_tool_name = %mapped,
                            "跳过结构完全相同的重复工具定义"
                        );
                        continue;
                    }
                    tracing::warn!(
                        original_tool_name = %tool.name,
                        mapped_tool_name = %mapped,
                        "拒绝同名但定义冲突的工具"
                    );
                    return Err(ConversionError::UnsupportedContent(
                        "duplicate tool definitions with the same name are not structurally equivalent"
                            .to_string(),
                    ));
                }
                // A matching entry supplied by the caller reserves the reverse
                // mapping, but does not itself represent a tool definition.
                // Allow the first selected definition to claim it below.
            } else {
                tracing::warn!(
                    previous_original_tool_name = %previous_original,
                    original_tool_name = %tool.name,
                    mapped_tool_name = %mapped,
                    "拒绝歧义工具名映射"
                );
                return Err(ConversionError::UnsupportedContent(
                    "multiple tool definitions resolve to the same Kiro tool name".to_string(),
                ));
            }
        }
        reserved.insert(collision_key.clone(), tool.name.clone());
        selected_definition_by_name.insert(collision_key, idx);
        if mapped != tool.name {
            pending_reverse_map.insert(mapped.clone(), tool.name.clone());
        }
        allocated.push((idx, mapped));
    }

    Ok((allocated, pending_reverse_map))
}

fn equivalent_tool_definitions(left: &AnthropicTool, right: &AnthropicTool) -> bool {
    left.name == right.name
        && left.description == right.description
        && left.input_schema == right.input_schema
        && left.tool_type == right.tool_type
        && left.max_uses == right.max_uses
        && left.cache_control == right.cache_control
}

fn input_schema_from_json(
    schema: serde_json::Value,
    mapped_tool_name: &str,
    mapper: &SchemaKeyMapper,
    options: ConverterOptions,
) -> Result<(InputSchema, HashMap<String, String>), ConversionError> {
    let mut schema = if options.conversion.tool_schema_normalization.is_enabled() {
        normalize_json_schema(schema)
    } else {
        schema
    };
    let schema_key_mapping = mapper
        .apply_to_schema(mapped_tool_name, &mut schema)
        .map_err(|err| ConversionError::UnsupportedContent(err.to_string()))?;
    Ok((InputSchema::from_json(schema), schema_key_mapping))
}

fn normalize_tool_description(tool_name: &str, description: &str) -> String {
    if !description.trim().is_empty() {
        return description.to_string();
    }

    let tool_name = tool_name.trim();
    if tool_name.is_empty() {
        EMPTY_TOOL_DESCRIPTION_PLACEHOLDER.to_string()
    } else {
        format!("Tool `{}` available to the assistant.", tool_name)
    }
}

pub(super) fn generate_tool_choice_prefix(
    req: &MessagesRequest,
    options: ConverterOptions,
) -> Option<String> {
    if !options.inject_tool_choice_prefix() {
        return None;
    }

    match parse_tool_choice(&req.tool_choice) {
        ToolChoiceDirective::Any => Some(
            "<tool_choice>any</tool_choice><tool_choice_policy>Use at least one available tool in this turn when a tool can satisfy the request.</tool_choice_policy>"
                .to_string(),
        ),
        ToolChoiceDirective::Tool(name) => Some(format!(
            "<tool_choice>tool</tool_choice><tool_choice_name>{}</tool_choice_name><tool_choice_policy>Use the named tool in this turn when responding.</tool_choice_policy>",
            name
        )),
        ToolChoiceDirective::None if req.tools.as_ref().is_some_and(|tools| !tools.is_empty()) => {
            Some(
                "<tool_choice>none</tool_choice><tool_choice_policy>Do not call tools in this turn.</tool_choice_policy>"
                    .to_string(),
            )
        }
        _ => None,
    }
}

/// 转换工具定义
#[derive(Debug, Default)]
pub(super) struct ConvertedTools {
    pub(super) tools: Vec<Tool>,
    pub(super) tool_cache_point_insert_after: Vec<usize>,
    pub(super) tool_schema_key_map: ToolSchemaKeyMap,
}

pub(super) fn convert_tools(
    tools: &Option<Vec<AnthropicTool>>,
    tool_choice: &Option<serde_json::Value>,
    tool_name_map: &mut HashMap<String, String>,
    options: ConverterOptions,
) -> Result<ConvertedTools, ConversionError> {
    let Some(tools) = tools else {
        return Ok(ConvertedTools::default());
    };
    let directive = if options.tool_choice_steering_enabled() {
        parse_tool_choice(tool_choice)
    } else {
        ToolChoiceDirective::Auto
    };
    let selected_indices = selected_tool_indices(tools, &directive)?;
    let (allocated_tools, pending_reverse_map) =
        allocate_selected_tool_names(tools, &selected_indices, tool_name_map, &options)?;

    let mut converted = Vec::new();
    let mut cache_point_insert_after = Vec::new();
    let mut tool_schema_key_map = ToolSchemaKeyMap::default();
    let schema_key_mapper = match SchemaKeyMapper::new(
        options.conversion.tool_schema_key_mapping,
        options.conversion.tool_schema_key_validation_regex.clone(),
    ) {
        Ok(mapper) => mapper,
        Err(err) => return Err(ConversionError::UnsupportedContent(err.to_string())),
    };

    if options.kiro_cache_point_enabled && !options.kiro_cache_point_tools_only {
        tracing::debug!(
            "kiroCachePointToolsOnly is disabled, but this phase only supports tool-level cachePoint insertion"
        );
    }

    for (idx, mapped_name) in allocated_tools {
        let t = &tools[idx];
        let mut description = normalize_tool_description(&t.name, &t.description);

        // 对 Write/Edit 工具追加自定义描述后缀
        let suffix = if options.inject_chunked_tool_descriptions() {
            match t.name.as_str() {
                "Write" => WRITE_TOOL_DESCRIPTION_SUFFIX,
                "Edit" => EDIT_TOOL_DESCRIPTION_SUFFIX,
                _ => "",
            }
        } else {
            ""
        };
        if !suffix.is_empty() {
            description.push('\n');
            description.push_str(suffix);
        }

        // 限制描述长度为 10000 字符（安全截断 UTF-8，单次遍历）
        let description = match description.char_indices().nth(10000) {
            Some((idx, _)) => description[..idx].to_string(),
            None => description,
        };

        let converted_idx = converted.len();
        let has_cache_control = t.cache_control.is_some();
        let (input_schema, schema_key_mapping) = input_schema_from_json(
            serde_json::json!(t.input_schema),
            &mapped_name,
            &schema_key_mapper,
            options.clone(),
        )?;
        if !schema_key_mapping.is_empty() {
            tool_schema_key_map.insert_tool_mapping(mapped_name.clone(), schema_key_mapping);
        }

        converted.push(Tool {
            tool_specification: ToolSpecification {
                name: mapped_name,
                description,
                input_schema,
            },
        });
        if options.kiro_cache_point_enabled && has_cache_control {
            cache_point_insert_after.push(converted_idx);
        }
    }

    // Commit only after every selected definition and schema converted
    // successfully, so callers never observe a partial or collision-overwritten
    // reverse map.
    tool_name_map.extend(pending_reverse_map);

    Ok(ConvertedTools {
        tools: converted,
        tool_cache_point_insert_after: cache_point_insert_after,
        tool_schema_key_map,
    })
}
