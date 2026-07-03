//! Anthropic → Kiro 协议转换器
//!
//! 负责将 Anthropic API 请求格式转换为 Kiro API 请求格式

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::anthropic::model_capabilities::{ModelResolution, strip_model_1m_suffix};
use crate::anthropic::prompt_cache::canonicalize_cache_value;
use crate::kiro::model::requests::conversation::{
    AssistantMessage, ConversationState, CurrentMessage, HistoryAssistantMessage,
    HistoryUserMessage, KiroImage, Message, UserInputMessage, UserInputMessageContext, UserMessage,
};
use crate::kiro::model::requests::kiro::{
    AdditionalModelRequestFields, KiroOutputConfig, KiroReasoningConfig,
};
use crate::kiro::model::requests::tool::{
    InputSchema, Tool, ToolResult, ToolSpecification, ToolUseEntry,
};
use crate::model::config::{CompatProfile, PromptCacheSimulationMode};

use super::types::{ContentBlock, MessagesRequest, normalize_thinking_effort};

const TOOL_RESULTS_PROVIDED_PLACEHOLDER: &str = "Tool results provided.";
const EMPTY_TOOL_RESULT_CONTENT_PLACEHOLDER: &str = "Tool result content was empty.";

/// 规范化 JSON Schema，修复 MCP/OpenAPI/Zod 工具定义中常见的兼容性问题。
///
/// 上游按 draft 2020-12 校验工具 `input_schema`，但 Claude Code / MCP 工具定义
/// 经常混入旧 draft、OpenAPI 或简写结构。这里保守清洗成 Kiro/Anthropic 更容易
/// 接受的 JSON Schema 子集，避免单个脏工具 schema 导致整次请求被 400 拒绝。
fn normalize_json_schema(schema: serde_json::Value) -> serde_json::Value {
    let serde_json::Value::Object(mut obj) = schema else {
        return empty_object_schema();
    };

    normalize_schema_object(&mut obj, true);
    flatten_root_schema_combinators(&mut obj);
    obj.insert(
        "type".to_string(),
        serde_json::Value::String("object".to_string()),
    );
    if !matches!(obj.get("properties"), Some(serde_json::Value::Object(_))) {
        obj.insert(
            "properties".to_string(),
            serde_json::Value::Object(serde_json::Map::new()),
        );
    }
    serde_json::Value::Object(obj)
}

fn flatten_root_schema_combinators(obj: &mut serde_json::Map<String, serde_json::Value>) {
    flatten_root_schema_combinator(obj, "allOf", RequiredMergeMode::Union);
    flatten_root_schema_combinator(obj, "oneOf", RequiredMergeMode::Intersection);
    flatten_root_schema_combinator(obj, "anyOf", RequiredMergeMode::Intersection);
}

#[derive(Clone, Copy)]
enum RequiredMergeMode {
    Union,
    Intersection,
}

fn flatten_root_schema_combinator(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    required_mode: RequiredMergeMode,
) {
    let Some(serde_json::Value::Array(items)) = obj.remove(key) else {
        return;
    };

    let mut required_sets = Vec::new();
    for item in items {
        let serde_json::Value::Object(item) = item else {
            continue;
        };
        merge_root_variant_properties(obj, &item);
        let required = schema_required_set(&item);
        if !required.is_empty() {
            required_sets.push(required);
        }
    }

    merge_root_variant_required(obj, required_sets, required_mode);
}

fn merge_root_variant_properties(
    root: &mut serde_json::Map<String, serde_json::Value>,
    variant: &serde_json::Map<String, serde_json::Value>,
) {
    let Some(serde_json::Value::Object(variant_props)) = variant.get("properties") else {
        return;
    };

    let root_props = root
        .entry("properties".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if !root_props.is_object() {
        *root_props = serde_json::Value::Object(serde_json::Map::new());
    }
    let root_props = root_props
        .as_object_mut()
        .expect("root properties should be an object after normalization");

    for (name, schema) in variant_props {
        root_props
            .entry(name.clone())
            .or_insert_with(|| schema.clone());
    }
}

fn schema_required_set(
    schema: &serde_json::Map<String, serde_json::Value>,
) -> std::collections::BTreeSet<String> {
    schema
        .get("required")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn merge_root_variant_required(
    root: &mut serde_json::Map<String, serde_json::Value>,
    required_sets: Vec<std::collections::BTreeSet<String>>,
    mode: RequiredMergeMode,
) {
    if required_sets.is_empty() {
        return;
    }

    let mut merged = schema_required_set(root);
    let variant_required = match mode {
        RequiredMergeMode::Union => required_sets
            .into_iter()
            .flatten()
            .collect::<std::collections::BTreeSet<_>>(),
        RequiredMergeMode::Intersection => {
            let mut iter = required_sets.into_iter();
            let Some(first) = iter.next() else {
                return;
            };
            iter.fold(first, |acc, required| {
                acc.intersection(&required).cloned().collect()
            })
        }
    };
    merged.extend(variant_required);

    if merged.is_empty() {
        root.remove("required");
    } else {
        root.insert(
            "required".to_string(),
            serde_json::Value::Array(
                merged
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect::<Vec<_>>(),
            ),
        );
    }
}

fn normalize_schema_object(obj: &mut serde_json::Map<String, serde_json::Value>, is_root: bool) {
    obj.remove("$schema");
    obj.remove("additionalProperties");
    obj.remove("additionalItems");
    obj.remove("unevaluatedProperties");
    obj.remove("unevaluatedItems");
    obj.remove("$vocabulary");
    obj.remove("discriminator");
    obj.remove("xml");
    obj.remove("externalDocs");
    remove_openapi_extensions(obj);

    if let Some(example) = obj.remove("example") {
        obj.entry("examples".to_string())
            .or_insert_with(|| serde_json::Value::Array(vec![example]));
    }

    if obj
        .get("$id")
        .is_some_and(|value| value.as_str().is_some_and(|text| text.trim().is_empty()))
    {
        obj.remove("$id");
    }
    if obj
        .get("$anchor")
        .is_some_and(|value| value.as_str().is_some_and(|text| text.trim().is_empty()))
    {
        obj.remove("$anchor");
    }

    if let Some(definitions) = obj.remove("definitions") {
        obj.entry("$defs".to_string()).or_insert(definitions);
    }

    if let Some(dependencies) = obj.remove("dependencies") {
        convert_legacy_dependencies(obj, dependencies);
    }

    if let Some(reference) = obj.remove("$ref") {
        if let serde_json::Value::String(mut value) = reference {
            if let Some(rest) = value.strip_prefix("#/definitions/") {
                value = format!("#/$defs/{}", rest);
            }
            obj.insert("$ref".to_string(), serde_json::Value::String(value));
        }
    }

    let nullable = matches!(obj.remove("nullable"), Some(serde_json::Value::Bool(true)));

    normalize_properties(obj, is_root);
    normalize_schema_map_keyword(obj, "patternProperties");
    normalize_schema_map_keyword(obj, "$defs");
    normalize_schema_map_keyword(obj, "dependentSchemas");
    normalize_required(obj);
    normalize_type_keyword(obj, is_root, nullable);
    normalize_items_keywords(obj);

    for key in [
        "contains",
        "propertyNames",
        "contentSchema",
        "not",
        "if",
        "then",
        "else",
    ] {
        normalize_schema_keyword(obj, key);
    }

    for key in ["oneOf", "anyOf", "allOf"] {
        normalize_schema_array_keyword(obj, key);
    }

    normalize_dependent_required(obj);
    normalize_enum_keyword(obj);
    normalize_annotation_keywords(obj);
    normalize_validation_keywords(obj);
    normalize_string_keywords(
        obj,
        &[
            "$id",
            "$anchor",
            "$dynamicRef",
            "$dynamicAnchor",
            "format",
            "pattern",
            "contentEncoding",
            "contentMediaType",
        ],
    );
    normalize_bool_keywords(obj, &["deprecated", "readOnly", "writeOnly", "uniqueItems"]);
    normalize_non_negative_integer_keywords(
        obj,
        &[
            "minLength",
            "maxLength",
            "minItems",
            "maxItems",
            "minContains",
            "maxContains",
            "minProperties",
            "maxProperties",
        ],
    );
    normalize_number_keywords(
        obj,
        &["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"],
    );

    if let Some(value) = obj.get("multipleOf") {
        if !value.as_f64().is_some_and(|number| number > 0.0) {
            obj.remove("multipleOf");
        }
    }

    if obj
        .get("format")
        .is_some_and(|value| value.as_str().is_some_and(|text| text.trim().is_empty()))
    {
        obj.remove("format");
    }
    if obj
        .get("pattern")
        .is_some_and(|value| value.as_str().is_some_and(|text| text.trim().is_empty()))
    {
        obj.remove("pattern");
    }
}

fn empty_object_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {}
    })
}

fn remove_openapi_extensions(obj: &mut serde_json::Map<String, serde_json::Value>) {
    obj.retain(|key, _| !key.starts_with("x-"));
}

fn normalize_properties(obj: &mut serde_json::Map<String, serde_json::Value>, is_root: bool) {
    match obj.get_mut("properties") {
        Some(serde_json::Value::Object(properties)) => {
            for value in properties.values_mut() {
                *value = normalize_schema_value(std::mem::take(value));
            }
        }
        Some(_) if is_root => {
            obj.insert(
                "properties".to_string(),
                serde_json::Value::Object(serde_json::Map::new()),
            );
        }
        Some(_) => {
            obj.remove("properties");
        }
        None if is_root => {
            obj.insert(
                "properties".to_string(),
                serde_json::Value::Object(serde_json::Map::new()),
            );
        }
        None => {}
    }
}

fn normalize_schema_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(mut obj) => {
            normalize_schema_object(&mut obj, false);
            serde_json::Value::Object(obj)
        }
        serde_json::Value::Bool(value) => serde_json::Value::Bool(value),
        serde_json::Value::String(value) => normalize_type_name(&value)
            .map(|schema_type| serde_json::json!({ "type": schema_type }))
            .unwrap_or_else(|| serde_json::json!({})),
        _ => serde_json::json!({}),
    }
}

fn normalize_schema_map_keyword(obj: &mut serde_json::Map<String, serde_json::Value>, key: &str) {
    match obj.remove(key) {
        Some(serde_json::Value::Object(mut map)) => {
            for value in map.values_mut() {
                *value = normalize_schema_value(std::mem::take(value));
            }
            if !map.is_empty() {
                obj.insert(key.to_string(), serde_json::Value::Object(map));
            }
        }
        Some(_) | None => {}
    }
}

fn normalize_required(obj: &mut serde_json::Map<String, serde_json::Value>) {
    let Some(value) = obj.remove("required") else {
        return;
    };
    let serde_json::Value::Array(items) = value else {
        return;
    };

    let Some(properties) = obj.get("properties").and_then(|value| value.as_object()) else {
        return;
    };

    let mut required = Vec::new();
    for item in items {
        let Some(name) = item.as_str() else {
            continue;
        };
        if properties.contains_key(name) {
            let value = serde_json::Value::String(name.to_string());
            if !required.contains(&value) {
                required.push(value);
            }
        }
    }

    if !required.is_empty() {
        obj.insert("required".to_string(), serde_json::Value::Array(required));
    }
}

fn normalize_type_keyword(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    is_root: bool,
    nullable: bool,
) {
    let raw_type = obj.remove("type");
    let mut types = Vec::<String>::new();

    if is_root {
        types.push("object".to_string());
    } else {
        collect_schema_types(raw_type, &mut types);
        if types.is_empty() {
            if obj.contains_key("properties") || obj.contains_key("patternProperties") {
                types.push("object".to_string());
            } else if obj.contains_key("items")
                || obj.contains_key("prefixItems")
                || obj.contains_key("contains")
            {
                types.push("array".to_string());
            }
        }
        if nullable && !types.is_empty() && !types.iter().any(|value| value == "null") {
            types.push("null".to_string());
        }
    }

    match types.len() {
        0 => {}
        1 => {
            obj.insert(
                "type".to_string(),
                serde_json::Value::String(types.remove(0)),
            );
        }
        _ => {
            obj.insert(
                "type".to_string(),
                serde_json::Value::Array(
                    types
                        .into_iter()
                        .map(serde_json::Value::String)
                        .collect::<Vec<_>>(),
                ),
            );
        }
    }
}

fn collect_schema_types(value: Option<serde_json::Value>, types: &mut Vec<String>) {
    match value {
        Some(serde_json::Value::String(value)) => push_schema_type(types, &value),
        Some(serde_json::Value::Array(items)) => {
            for item in items {
                if let serde_json::Value::String(value) = item {
                    push_schema_type(types, &value);
                }
            }
        }
        Some(_) | None => {}
    }
}

fn push_schema_type(types: &mut Vec<String>, value: &str) {
    if let Some(schema_type) = normalize_type_name(value) {
        if !types.iter().any(|item| item == schema_type) {
            types.push(schema_type.to_string());
        }
    }
}

fn normalize_type_name(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "null" | "nil" | "none" => Some("null"),
        "boolean" | "bool" => Some("boolean"),
        "object" | "dict" | "map" | "record" => Some("object"),
        "array" | "list" => Some("array"),
        "number" | "float" | "double" => Some("number"),
        "string" | "str" => Some("string"),
        "integer" | "int" => Some("integer"),
        _ => None,
    }
}

fn normalize_items_keywords(obj: &mut serde_json::Map<String, serde_json::Value>) {
    if let Some(items) = obj.remove("items") {
        match items {
            serde_json::Value::Array(items) => {
                if !obj.contains_key("prefixItems") {
                    let prefix_items = items
                        .into_iter()
                        .filter(|item| is_schema_like(item))
                        .map(normalize_schema_value)
                        .collect::<Vec<_>>();
                    if !prefix_items.is_empty() {
                        obj.insert(
                            "prefixItems".to_string(),
                            serde_json::Value::Array(prefix_items),
                        );
                    }
                }
            }
            value if is_schema_like(&value) => {
                obj.insert("items".to_string(), normalize_schema_value(value));
            }
            _ => {}
        }
    }

    match obj.remove("prefixItems") {
        Some(serde_json::Value::Array(items)) => {
            let prefix_items = items
                .into_iter()
                .filter(|item| is_schema_like(item))
                .map(normalize_schema_value)
                .collect::<Vec<_>>();
            if !prefix_items.is_empty() {
                obj.insert(
                    "prefixItems".to_string(),
                    serde_json::Value::Array(prefix_items),
                );
            }
        }
        Some(_) | None => {}
    }
}

fn normalize_schema_keyword(obj: &mut serde_json::Map<String, serde_json::Value>, key: &str) {
    match obj.remove(key) {
        Some(value) if is_schema_like(&value) => {
            obj.insert(key.to_string(), normalize_schema_value(value));
        }
        Some(_) | None => {}
    }
}

fn normalize_schema_array_keyword(obj: &mut serde_json::Map<String, serde_json::Value>, key: &str) {
    match obj.remove(key) {
        Some(serde_json::Value::Array(items)) => {
            let items = items
                .into_iter()
                .filter(|item| is_schema_like(item))
                .map(normalize_schema_value)
                .collect::<Vec<_>>();
            if !items.is_empty() {
                obj.insert(key.to_string(), serde_json::Value::Array(items));
            }
        }
        Some(value) if is_schema_like(&value) => {
            obj.insert(
                key.to_string(),
                serde_json::Value::Array(vec![normalize_schema_value(value)]),
            );
        }
        Some(_) | None => {}
    }
}

fn is_schema_like(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(_) | serde_json::Value::Bool(_) => true,
        serde_json::Value::String(value) => normalize_type_name(value).is_some(),
        _ => false,
    }
}

fn normalize_dependent_required(obj: &mut serde_json::Map<String, serde_json::Value>) {
    let Some(value) = obj.remove("dependentRequired") else {
        return;
    };
    let serde_json::Value::Object(map) = value else {
        return;
    };

    let mut normalized = serde_json::Map::new();
    for (key, value) in map {
        let serde_json::Value::Array(items) = value else {
            continue;
        };
        let mut required = Vec::new();
        for item in items {
            let Some(name) = item.as_str() else {
                continue;
            };
            let value = serde_json::Value::String(name.to_string());
            if !required.contains(&value) {
                required.push(value);
            }
        }
        if !required.is_empty() {
            normalized.insert(key, serde_json::Value::Array(required));
        }
    }

    if !normalized.is_empty() {
        obj.insert(
            "dependentRequired".to_string(),
            serde_json::Value::Object(normalized),
        );
    }
}

fn convert_legacy_dependencies(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    dependencies: serde_json::Value,
) {
    let serde_json::Value::Object(dependencies) = dependencies else {
        return;
    };

    let mut dependent_required = obj
        .remove("dependentRequired")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let mut dependent_schemas = obj
        .remove("dependentSchemas")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();

    for (key, value) in dependencies {
        match value {
            serde_json::Value::Array(_) => {
                dependent_required.insert(key, value);
            }
            value if is_schema_like(&value) => {
                dependent_schemas.insert(key, value);
            }
            _ => {}
        }
    }

    if !dependent_required.is_empty() {
        obj.insert(
            "dependentRequired".to_string(),
            serde_json::Value::Object(dependent_required),
        );
    }
    if !dependent_schemas.is_empty() {
        obj.insert(
            "dependentSchemas".to_string(),
            serde_json::Value::Object(dependent_schemas),
        );
    }
}

fn normalize_enum_keyword(obj: &mut serde_json::Map<String, serde_json::Value>) {
    let Some(value) = obj.remove("enum") else {
        return;
    };

    let items = match value {
        serde_json::Value::Array(items) => items,
        value => vec![value],
    };
    let mut normalized = Vec::new();
    for item in items {
        if !normalized.contains(&item) {
            normalized.push(item);
        }
    }
    if !normalized.is_empty() {
        obj.insert("enum".to_string(), serde_json::Value::Array(normalized));
    }
}

fn normalize_annotation_keywords(obj: &mut serde_json::Map<String, serde_json::Value>) {
    normalize_string_keywords(obj, &["title", "description", "$comment"]);

    match obj.remove("examples") {
        Some(serde_json::Value::Array(items)) => {
            obj.insert("examples".to_string(), serde_json::Value::Array(items));
        }
        Some(value) => {
            obj.insert(
                "examples".to_string(),
                serde_json::Value::Array(vec![value]),
            );
        }
        None => {}
    }
}

fn normalize_validation_keywords(obj: &mut serde_json::Map<String, serde_json::Value>) {
    for key in [
        "minLength",
        "maxLength",
        "minItems",
        "maxItems",
        "minContains",
        "maxContains",
        "minProperties",
        "maxProperties",
    ] {
        if obj.get(key).is_some_and(|value| value.as_u64().is_none()) {
            obj.remove(key);
        }
    }

    for key in ["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"] {
        if obj.get(key).is_some_and(|value| !value.is_number()) {
            obj.remove(key);
        }
    }
}

fn normalize_string_keywords(obj: &mut serde_json::Map<String, serde_json::Value>, keys: &[&str]) {
    for key in keys {
        if obj
            .get(*key)
            .is_some_and(|value| !matches!(value, serde_json::Value::String(_)))
        {
            obj.remove(*key);
        }
    }
}

fn normalize_bool_keywords(obj: &mut serde_json::Map<String, serde_json::Value>, keys: &[&str]) {
    for key in keys {
        if obj
            .get(*key)
            .is_some_and(|value| !matches!(value, serde_json::Value::Bool(_)))
        {
            obj.remove(*key);
        }
    }
}

fn normalize_non_negative_integer_keywords(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) {
    for key in keys {
        if obj.get(*key).is_some_and(|value| value.as_u64().is_none()) {
            obj.remove(*key);
        }
    }
}

fn normalize_number_keywords(obj: &mut serde_json::Map<String, serde_json::Value>, keys: &[&str]) {
    for key in keys {
        if obj.get(*key).is_some_and(|value| !value.is_number()) {
            obj.remove(*key);
        }
    }
}

/// 追加到 Write 工具 description 末尾的内容
const WRITE_TOOL_DESCRIPTION_SUFFIX: &str = "- IMPORTANT: If the content to write exceeds 150 lines, you MUST only write the first 50 lines using this tool, then use `Edit` tool to append the remaining content in chunks of no more than 50 lines each. If needed, leave a unique placeholder to help append content. Do NOT attempt to write all content at once.";

/// 追加到 Edit 工具 description 末尾的内容
const EDIT_TOOL_DESCRIPTION_SUFFIX: &str = "- IMPORTANT: If the `new_string` content exceeds 50 lines, you MUST split it into multiple Edit calls, each replacing no more than 50 lines at a time. If used to append content, leave a unique placeholder to help append content. On the final chunk, do NOT include the placeholder.";

/// 追加到系统提示词的分块写入策略
const SYSTEM_CHUNKED_POLICY: &str = "\
When the Write or Edit tool has content size limits, always comply silently. \
Never suggest bypassing these limits via alternative tools. \
Never ask the user whether to switch approaches. \
Complete all chunked operations without commentary.";

/// 模型映射：将 Anthropic 模型名映射到 Kiro 模型 ID
/// 严格对照版本号
pub fn map_model(model: &str) -> Option<String> {
    let model_lower = model.to_lowercase();
    let model_base = model_lower.strip_suffix("[1m]").unwrap_or(&model_lower);
    let model_base = model_base.strip_suffix("-thinking").unwrap_or(model_base);

    if matches!(model_base, "opus" | "opusplan" | "best" | "default") {
        Some("claude-opus-4.7".to_string())
    } else if model_base == "sonnet" {
        Some("claude-sonnet-4.6".to_string())
    } else if model_base == "haiku" {
        Some("claude-haiku-4.5".to_string())
    } else if is_native_claude_family_model(model_base, "sonnet") {
        if model_base.contains("4-6") || model_base.contains("4.6") {
            Some("claude-sonnet-4.6".to_string())
        } else if model_base.contains("4-5") || model_base.contains("4.5") {
            Some("claude-sonnet-4.5".to_string())
        } else {
            Some(model_base.to_string())
        }
    } else if model_base.contains("sonnet") {
        if model_base.contains("4-6") || model_base.contains("4.6") {
            Some("claude-sonnet-4.6".to_string())
        } else if model_base.contains("4-5") || model_base.contains("4.5") {
            Some("claude-sonnet-4.5".to_string())
        } else if model_base.contains("4")
            || model_base.contains("3-5")
            || model_base.contains("3.5")
        {
            Some("claude-sonnet-4.5".to_string())
        } else {
            None
        }
    } else if is_native_claude_family_model(model_base, "opus") {
        if model_base.contains("4-5") || model_base.contains("4.5") {
            Some("claude-opus-4.5".to_string())
        } else if model_base.contains("4-6") || model_base.contains("4.6") {
            Some("claude-opus-4.6".to_string())
        } else if model_base.contains("4-7") || model_base.contains("4.7") {
            Some("claude-opus-4.7".to_string())
        } else {
            Some(model_base.to_string())
        }
    } else if model_base.contains("opus") {
        if model_base.contains("4-5") || model_base.contains("4.5") {
            Some("claude-opus-4.5".to_string())
        } else if model_base.contains("4-6") || model_base.contains("4.6") {
            Some("claude-opus-4.6".to_string())
        } else if model_base.contains("4-7") || model_base.contains("4.7") {
            Some("claude-opus-4.7".to_string())
        } else if model_base.contains("4") {
            Some("claude-opus-4.7".to_string())
        } else {
            None
        }
    } else if is_native_claude_family_model(model_base, "haiku") {
        if model_base.contains("4-5") || model_base.contains("4.5") {
            Some("claude-haiku-4.5".to_string())
        } else {
            Some(model_base.to_string())
        }
    } else if model_base.contains("haiku") {
        Some("claude-haiku-4.5".to_string())
    } else {
        None
    }
}

fn is_native_claude_family_model(model: &str, family: &str) -> bool {
    model
        .strip_prefix("claude-")
        .and_then(|rest| rest.strip_prefix(family))
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['-', '.']))
}

/// 根据模型名称返回对应的上下文窗口大小
///
/// 这是仅在 Kiro `ListAvailableModels` 能力目录缺失时使用的保守兜底。
/// 真实请求应优先使用上游目录中的 `maxInputTokens`；同名/同族模型在不同
/// 账号池中可能是 200K 或 1M，不能仅凭普通别名把 free Sonnet 误抬成 1M。
pub fn get_context_window_size(model: &str) -> i32 {
    let model_lower = model.to_lowercase();
    let explicit_one_m = model_lower.ends_with("[1m]");
    let base = strip_model_1m_suffix(&model_lower);

    if base == "auto" {
        return 1_000_000;
    }

    if explicit_one_m
        || base == "claude-opus-4.8"
        || base == "claude-opus-4.8-thinking"
        || base == "claude-opus-4.7"
        || base == "claude-opus-4.7-thinking"
        || base == "claude-opus-4.6"
        || base == "claude-opus-4.6-thinking"
        || base == "claude-sonnet-4.6"
        || base == "claude-sonnet-4.6-thinking"
    {
        return 1_000_000;
    }

    200_000
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeReasoningSchemaPath {
    OutputConfig,
    #[allow(dead_code)]
    Reasoning,
}

#[derive(Debug, Clone, Copy)]
struct NativeReasoningSchema {
    path: NativeReasoningSchemaPath,
    efforts: &'static [&'static str],
}

const EFFORTS_WITH_XHIGH: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const EFFORTS_WITHOUT_XHIGH: &[&str] = &["low", "medium", "high", "max"];

fn native_reasoning_schema(model_id: &str) -> Option<NativeReasoningSchema> {
    match model_id {
        "claude-opus-4.8" | "claude-opus-4-8" | "claude-opus-4.7" | "claude-opus-4-7" => {
            Some(NativeReasoningSchema {
                path: NativeReasoningSchemaPath::OutputConfig,
                efforts: EFFORTS_WITH_XHIGH,
            })
        }
        "claude-opus-4.6" | "claude-opus-4-6" | "claude-sonnet-4.6" | "claude-sonnet-4-6" => {
            Some(NativeReasoningSchema {
                path: NativeReasoningSchemaPath::OutputConfig,
                efforts: EFFORTS_WITHOUT_XHIGH,
            })
        }
        _ => None,
    }
}

fn requested_native_reasoning(req: &MessagesRequest) -> bool {
    req.thinking.as_ref().is_some_and(|t| t.is_enabled())
        || req
            .output_config
            .as_ref()
            .is_some_and(|oc| !oc.effort.trim().is_empty())
}

fn effort_from_budget_tokens(tokens: i32) -> &'static str {
    match tokens {
        i32::MIN..=4_000 => "low",
        4_001..=16_000 => "medium",
        16_001..=64_000 => "high",
        _ => "xhigh",
    }
}

fn select_native_reasoning_effort(req: &MessagesRequest, schema: NativeReasoningSchema) -> String {
    let requested = req
        .output_config
        .as_ref()
        .map(|oc| normalize_thinking_effort(&oc.effort))
        .or_else(|| {
            req.thinking.as_ref().map(|t| {
                if t.thinking_type == "enabled" {
                    effort_from_budget_tokens(t.budget_tokens)
                } else {
                    normalize_thinking_effort("")
                }
            })
        })
        .unwrap_or_else(|| normalize_thinking_effort(""));

    if schema.efforts.contains(&requested) {
        requested.to_string()
    } else {
        schema.efforts.last().copied().unwrap_or("high").to_string()
    }
}

fn build_additional_model_request_fields(
    req: &MessagesRequest,
    model_id: &str,
) -> Option<AdditionalModelRequestFields> {
    if req
        .thinking
        .as_ref()
        .is_some_and(|t| t.thinking_type == "disabled")
    {
        return None;
    }

    let schema = native_reasoning_schema(model_id)?;
    if !requested_native_reasoning(req) {
        return None;
    }

    let effort = select_native_reasoning_effort(req, schema);
    Some(match schema.path {
        NativeReasoningSchemaPath::OutputConfig => AdditionalModelRequestFields {
            thinking: None,
            output_config: Some(KiroOutputConfig { effort }),
            reasoning: None,
        },
        NativeReasoningSchemaPath::Reasoning => AdditionalModelRequestFields {
            thinking: None,
            output_config: None,
            reasoning: Some(KiroReasoningConfig { effort }),
        },
    })
}

fn uses_native_reasoning_fields(req: &MessagesRequest, model_id: &str) -> bool {
    build_additional_model_request_fields(req, model_id).is_some()
}

/// 转换结果
#[derive(Debug)]
pub struct ConversionResult {
    /// 转换后的 Kiro 请求
    pub conversation_state: ConversationState,
    /// 最终 Kiro tools 数组里需要在对应工具后插入 cachePoint 的工具下标。
    pub tool_cache_point_insert_after: Vec<usize>,
    /// 是否把 cachePoint 插入计划记录到 payload diagnostics。
    pub cache_point_plan_recording_enabled: bool,
    /// 工具名称映射（短名称 → 原始名称），仅当存在超长工具名时非空
    pub tool_name_map: HashMap<String, String>,
    /// 本次请求声明并实际发给上游的工具名集合，包含原始名和因长度限制生成的短名。
    ///
    /// 仅用于下游响应容错：当上游把工具调用泄漏为字面 `<invoke>` 文本时，只有工具名命中
    /// 这个集合才允许恢复成结构化 `tool_use`，避免误执行正文中展示的 XML。
    pub known_tool_names: std::collections::HashSet<String>,
    /// 代理对入参的隐式改写汇总（兜底动作的统计），用于可选的 `x-kiro-rs-warnings` 响应头。
    pub warnings: ProxyWarnings,
    /// Kiro 原生模型扩展字段，例如 reasoning effort。
    pub additional_model_request_fields: Option<AdditionalModelRequestFields>,
}

#[derive(Debug, Clone, Copy)]
pub struct ConverterOptions {
    pub compat_profile: CompatProfile,
    pub prompt_cache_simulation_mode: PromptCacheSimulationMode,
    pub kiro_cache_point_enabled: bool,
    pub kiro_cache_point_tools_only: bool,
    pub kiro_cache_point_record_plan: bool,
    pub force_visible_thinking: bool,
}

impl Default for ConverterOptions {
    fn default() -> Self {
        Self {
            compat_profile: CompatProfile::ClaudeCode,
            prompt_cache_simulation_mode: PromptCacheSimulationMode::HighCache,
            kiro_cache_point_enabled: false,
            kiro_cache_point_tools_only: true,
            kiro_cache_point_record_plan: true,
            force_visible_thinking: false,
        }
    }
}

impl ConverterOptions {
    fn is_strict(self) -> bool {
        self.compat_profile.is_strict()
    }

    fn inject_chunked_policy(self) -> bool {
        !self.is_strict()
    }

    fn inject_thinking_prefix(self) -> bool {
        self.force_visible_thinking || !self.is_strict()
    }

    fn inject_tool_choice_prefix(self) -> bool {
        !self.is_strict()
    }
}

/// 代理在请求转换过程中执行的兜底改写计数
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProxyWarnings {
    /// 末尾 assistant 消息（prefill）被丢弃的次数
    pub prefill_dropped: u32,
    /// 因找不到对应 tool_use 而被跳过的当前轮 tool_result
    pub orphan_tool_results: u32,
    /// 孤立 tool_result 被转成普通文本保留的次数
    pub orphan_tool_results_textified: u32,
    /// 因找不到对应 tool_result 而被从历史移除的 tool_use
    pub orphan_tool_uses: u32,
    /// 历史中重复出现的 tool_result（已配对过）被跳过
    pub duplicate_tool_results: u32,
    /// 重复 tool_result 被转成普通文本保留的次数
    pub duplicate_tool_results_textified: u32,
    /// user 消息只有 tool_result 且文本为空时补了 Kiro content 占位
    pub tool_result_content_placeholders: u32,
    /// user 消息没有文本也没有 tool_result 时补了 Continue 占位
    pub empty_content_placeholders: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolChoiceDirective {
    Auto,
    Any,
    None,
    Tool(String),
    Unknown,
}

impl ProxyWarnings {
    /// 编码为 `x-kiro-rs-warnings` 头值（仅包含计数 > 0 的项）。
    pub fn encode_header(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if self.prefill_dropped > 0 {
            parts.push(format!("prefill-dropped={}", self.prefill_dropped));
        }
        if self.orphan_tool_results > 0 {
            parts.push(format!("orphan-tool-result={}", self.orphan_tool_results));
        }
        if self.orphan_tool_results_textified > 0 {
            parts.push(format!(
                "orphan-tool-result-textified={}",
                self.orphan_tool_results_textified
            ));
        }
        if self.orphan_tool_uses > 0 {
            parts.push(format!("orphan-tool-use={}", self.orphan_tool_uses));
        }
        if self.duplicate_tool_results > 0 {
            parts.push(format!(
                "duplicate-tool-result={}",
                self.duplicate_tool_results
            ));
        }
        if self.duplicate_tool_results_textified > 0 {
            parts.push(format!(
                "duplicate-tool-result-textified={}",
                self.duplicate_tool_results_textified
            ));
        }
        if self.tool_result_content_placeholders > 0 {
            parts.push(format!(
                "tool-result-content-placeholder={}",
                self.tool_result_content_placeholders
            ));
        }
        if self.empty_content_placeholders > 0 {
            parts.push(format!(
                "empty-content-placeholder={}",
                self.empty_content_placeholders
            ));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(","))
        }
    }
}

/// 转换错误
#[derive(Debug)]
pub enum ConversionError {
    UnsupportedModel(String),
    EmptyMessages,
    UnsupportedContent(String),
}

impl std::fmt::Display for ConversionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConversionError::UnsupportedModel(model) => write!(f, "模型不支持: {}", model),
            ConversionError::EmptyMessages => write!(f, "消息列表为空"),
            ConversionError::UnsupportedContent(message) => write!(f, "内容块不支持: {}", message),
        }
    }
}

impl std::error::Error for ConversionError {}

/// 从 metadata.user_id 中提取 session UUID
///
/// 支持两种格式:
/// 1. 字符串格式: user_xxx_account__session_0b4445e1-f5be-49e1-87ce-62bbc28ad705
/// 2. JSON 格式: {"device_id":"...","account_uuid":"...","session_id":"UUID"}
///
/// 提取 session UUID 作为 conversationId
fn extract_session_id(user_id: &str) -> Option<String> {
    // 先尝试 JSON 解析
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(user_id) {
        if let Some(session_id) = json.get("session_id").and_then(|v| v.as_str()) {
            if is_valid_uuid(session_id) {
                return Some(session_id.to_string());
            }
        }
    }

    // 回退到字符串格式: 查找 "session_" 后面的内容
    if let Some(pos) = user_id.find("session_") {
        let session_part = &user_id[pos + 8..]; // "session_" 长度为 8
        if session_part.len() >= 36 {
            let uuid_str = &session_part[..36];
            if is_valid_uuid(uuid_str) {
                return Some(uuid_str.to_string());
            }
        }
    }
    None
}

pub(crate) fn extract_metadata_conversation_id(req: &MessagesRequest) -> Option<String> {
    req.metadata
        .as_ref()
        .and_then(|m| m.user_id.as_ref())
        .and_then(|user_id| extract_session_id(user_id))
}

pub(crate) fn extract_stable_conversation_id(req: &MessagesRequest) -> Option<String> {
    extract_metadata_conversation_id(req).or_else(|| derive_fallback_conversation_id(req))
}

fn conversation_id_for_options(req: &MessagesRequest, options: ConverterOptions) -> Option<String> {
    match options.prompt_cache_simulation_mode {
        PromptCacheSimulationMode::HighCache => extract_stable_conversation_id(req),
        PromptCacheSimulationMode::Disabled => extract_metadata_conversation_id(req),
    }
}

fn derive_fallback_conversation_id(req: &MessagesRequest) -> Option<String> {
    let seed = if let Some(first_user_message) =
        req.messages.iter().find(|message| message.role == "user")
    {
        serde_json::json!({
            "system": &req.system,
            "tools": &req.tools,
            "first_user_message": first_user_message,
        })
    } else {
        serde_json::json!({
            "system": &req.system,
            "tools": &req.tools,
            "messages": &req.messages,
        })
    };

    Some(deterministic_conversation_id(&canonicalize_cache_value(
        &seed,
    )))
}

fn deterministic_conversation_id(seed: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"kiro.rs:anthropic:conversation-id:v1:");
    hasher.update(seed.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}

/// 简单验证 UUID 格式（36 字符，包含 4 个连字符）
fn is_valid_uuid(s: &str) -> bool {
    s.len() == 36 && s.chars().filter(|c| *c == '-').count() == 4
}

/// 收集历史消息中使用的所有工具名称
fn collect_history_tool_names(history: &[Message]) -> Vec<String> {
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
fn create_placeholder_tool(name: &str) -> Tool {
    Tool {
        tool_specification: ToolSpecification {
            name: name.to_string(),
            description: "Tool used in conversation history".to_string(),
            input_schema: InputSchema::from_json(normalize_json_schema(serde_json::json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {}
            }))),
        },
    }
}

/// 将 Anthropic 请求转换为 Kiro 请求
#[allow(dead_code)]
pub fn convert_request(req: &MessagesRequest) -> Result<ConversionResult, ConversionError> {
    convert_request_with_options(req, ConverterOptions::default())
}

/// 将 Anthropic 请求转换为 Kiro 请求，并按兼容 profile 控制代理侧改写。
pub fn convert_request_with_options(
    req: &MessagesRequest,
    options: ConverterOptions,
) -> Result<ConversionResult, ConversionError> {
    let model_id = map_model(&req.model)
        .ok_or_else(|| ConversionError::UnsupportedModel(req.model.clone()))?;
    convert_request_with_model_id(req, options, model_id)
}

/// 将 Anthropic 请求转换为 Kiro 请求，并使用已经按当前 Kiro 上游目录解析过的模型 ID。
pub fn convert_request_with_resolved_model(
    req: &MessagesRequest,
    options: ConverterOptions,
    resolution: &ModelResolution,
) -> Result<ConversionResult, ConversionError> {
    let model_id = resolution
        .upstream_model
        .clone()
        .ok_or_else(|| ConversionError::UnsupportedModel(req.model.clone()))?;
    convert_request_with_model_id(req, options, model_id)
}

fn convert_request_with_model_id(
    req: &MessagesRequest,
    options: ConverterOptions,
    model_id: String,
) -> Result<ConversionResult, ConversionError> {
    let mut warnings = ProxyWarnings::default();

    // 2. 检查消息列表
    if req.messages.is_empty() {
        return Err(ConversionError::EmptyMessages);
    }

    // 2.5. 预处理 prefill：如果末尾不是 user，静默丢弃尾部 prefill 并截断到最后一条 user
    // Claude 4.x 已弃用 assistant prefill，Kiro API 也不接受 assistant 作为最终消息
    let messages: &[_] = if req.messages.last().is_some_and(|m| m.role != "user") {
        warnings.prefill_dropped += 1;
        tracing::info!("检测到末尾非 user 消息（prefill），静默丢弃");
        let last_user_idx = req
            .messages
            .iter()
            .rposition(|m| m.role == "user")
            .ok_or(ConversionError::EmptyMessages)?;
        &req.messages[..=last_user_idx]
    } else {
        &req.messages
    };

    // 3. 生成会话 ID 和代理 ID
    // High-cache 模式下缺失 metadata 时从稳定请求锚点派生确定性 UUID；
    // 其他模式保持旧语义，只信任显式 metadata session。
    let conversation_id =
        conversation_id_for_options(req, options).unwrap_or_else(|| Uuid::new_v4().to_string());
    let agent_continuation_id = Uuid::new_v4().to_string();

    // 4. 确定触发类型
    let chat_trigger_type = determine_chat_trigger_type(req);

    // 5. 处理最后一条消息作为 current_message（经过 prefill 预处理，末尾必为 user）
    let last_message = messages.last().unwrap();
    let (text_content, images, tool_results) = process_message_content(&last_message.content)?;

    // 6. 转换工具定义（超长名称自动缩短并记录映射）
    let mut tool_name_map = HashMap::new();
    let converted_tools = convert_tools(&req.tools, &req.tool_choice, &mut tool_name_map, options);
    let mut tools = converted_tools.tools;
    let mut known_tool_names: std::collections::HashSet<String> = tools
        .iter()
        .map(|tool| tool.tool_specification.name.clone())
        .collect();
    for original_name in tool_name_map.values() {
        known_tool_names.insert(original_name.clone());
    }

    // 7. 构建历史消息（需要先构建，以便收集历史中使用的工具）
    let mut history = build_history(req, messages, &model_id, &mut tool_name_map, options)?;

    // 8. 验证并过滤 tool_use/tool_result 配对
    // 移除孤立的 tool_result（没有对应的 tool_use）
    // 同时返回孤立的 tool_use_id 集合，用于后续清理
    let (validated_tool_results, orphaned_tool_use_ids, orphan_tool_result_texts) =
        validate_tool_pairing(&history, &tool_results, &mut warnings);

    if options.is_strict()
        && (warnings.orphan_tool_results > 0
            || warnings.orphan_tool_uses > 0
            || warnings.duplicate_tool_results > 0
            || !orphaned_tool_use_ids.is_empty())
    {
        return Err(ConversionError::UnsupportedContent(
            "tool_use/tool_result history is not strictly paired".to_string(),
        ));
    }

    // 9. 从历史中移除孤立的 tool_use（Kiro API 要求 tool_use 必须有对应的 tool_result）
    if !options.is_strict() {
        remove_orphaned_tool_uses(&mut history, &orphaned_tool_use_ids);
    }

    // 10. 收集历史中使用的工具名称，为缺失的工具生成占位符定义
    // Kiro API 要求：历史消息中引用的工具必须在 tools 列表中有定义
    // 注意：Kiro 匹配工具名称时忽略大小写，所以这里也需要忽略大小写比较
    let history_tool_names = collect_history_tool_names(&history);
    let mut existing_tool_names: std::collections::HashSet<_> = tools
        .iter()
        .map(|t| t.tool_specification.name.to_lowercase())
        .collect();

    for tool_name in history_tool_names {
        let tool_name_lower = tool_name.to_lowercase();
        if !existing_tool_names.contains(&tool_name_lower) {
            if options.is_strict() {
                return Err(ConversionError::UnsupportedContent(format!(
                    "tool {} appears in history but is missing from tools",
                    tool_name
                )));
            }
            known_tool_names.insert(tool_name.clone());
            tools.push(create_placeholder_tool(&tool_name));
            existing_tool_names.insert(tool_name_lower);
        }
    }

    // 11. 构建 UserInputMessageContext
    let mut context = UserInputMessageContext::new();
    if !tools.is_empty() {
        context = context.with_tools(tools);
    }
    if !validated_tool_results.is_empty() {
        context = context.with_tool_results(validated_tool_results);
    }

    // 12. 构建当前消息
    // 保留文本内容，即使有工具结果也不丢弃用户文本
    let mut content = text_content;
    if !options.is_strict() {
        append_orphan_tool_result_texts(&mut content, &orphan_tool_result_texts);
    }
    if content.trim().is_empty() {
        if !context.tool_results.is_empty() {
            content = TOOL_RESULTS_PROVIDED_PLACEHOLDER.to_string();
            warnings.tool_result_content_placeholders += 1;
        } else {
            content = "Continue".to_string();
            warnings.empty_content_placeholders += 1;
        }
    }

    let mut user_input = UserInputMessage::new(content, &model_id)
        .with_context(context)
        .with_origin("AI_EDITOR");

    if !images.is_empty() {
        user_input = user_input.with_images(images);
    }
    let current_message = CurrentMessage::new(user_input);

    // 13. 构建 ConversationState
    let conversation_state = ConversationState::new(conversation_id)
        .with_agent_continuation_id(agent_continuation_id)
        .with_agent_task_type("vibe")
        .with_chat_trigger_type(chat_trigger_type)
        .with_current_message(current_message)
        .with_history(history);

    if !tool_name_map.is_empty() {
        tracing::info!("工具名称映射: {} 个超长名称已缩短", tool_name_map.len());
    }

    let additional_model_request_fields = build_additional_model_request_fields(req, &model_id);
    if additional_model_request_fields.is_none() {
        if let Some(oc) = &req.output_config {
            if !oc.effort.trim().is_empty() {
                tracing::debug!(
                    model_id = %model_id,
                    "skipping unsupported additionalModelRequestFields for model"
                );
            }
        }
    }

    Ok(ConversionResult {
        conversation_state,
        tool_cache_point_insert_after: converted_tools.tool_cache_point_insert_after,
        cache_point_plan_recording_enabled: options.kiro_cache_point_record_plan,
        tool_name_map,
        known_tool_names,
        warnings,
        additional_model_request_fields,
    })
}

/// 确定聊天触发类型
/// "AUTO" 模式可能会导致 400 Bad Request 错误
fn determine_chat_trigger_type(_req: &MessagesRequest) -> String {
    "MANUAL".to_string()
}

/// 处理消息内容，提取文本、图片和工具结果
fn process_message_content(
    content: &serde_json::Value,
) -> Result<(String, Vec<KiroImage>, Vec<ToolResult>), ConversionError> {
    let mut text_parts = Vec::new();
    let mut images = Vec::new();
    let mut tool_results = Vec::new();

    match content {
        serde_json::Value::String(s) => {
            text_parts.push(s.clone());
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let Ok(block) = serde_json::from_value::<ContentBlock>(item.clone()) {
                    match block.block_type.as_str() {
                        "text" => {
                            if let Some(text) = block.text {
                                text_parts.push(text);
                            }
                        }
                        "image" => {
                            let source = block.source.ok_or_else(|| {
                                ConversionError::UnsupportedContent(
                                    "image block missing source".to_string(),
                                )
                            })?;
                            images.push(convert_image_source(source)?);
                        }
                        "document" => {
                            let source = block.source.ok_or_else(|| {
                                ConversionError::UnsupportedContent(
                                    "document block missing source".to_string(),
                                )
                            })?;
                            let document_text = convert_document_source_to_text(source)?;
                            if !document_text.is_empty() {
                                text_parts.push(document_text);
                            }
                        }
                        "tool_result" => {
                            if let Some(tool_use_id) =
                                block.tool_use_id.as_deref().and_then(sanitize_tool_use_id)
                            {
                                let result_content = normalize_tool_result_content(
                                    extract_tool_result_content(&block.content),
                                );
                                let is_error = block.is_error.unwrap_or(false);

                                let mut result = if is_error {
                                    ToolResult::error(tool_use_id.clone(), result_content)
                                } else {
                                    ToolResult::success(tool_use_id.clone(), result_content)
                                };
                                result.status =
                                    Some(if is_error { "error" } else { "success" }.to_string());

                                tool_results.push(result);
                            }
                        }
                        "tool_use" => {
                            // tool_use 在 assistant 消息中处理，这里忽略
                        }
                        "redacted_thinking" => {
                            tracing::debug!(
                                "用户消息中的 redacted_thinking 无法传递给当前 Kiro upstream，已跳过"
                            );
                        }
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }

    Ok((text_parts.join("\n"), images, tool_results))
}

fn convert_image_source(source: super::types::ImageSource) -> Result<KiroImage, ConversionError> {
    match source.source_type.as_str() {
        "base64" => {
            let media_type = source.media_type.ok_or_else(|| {
                ConversionError::UnsupportedContent(
                    "base64 image source missing media_type".to_string(),
                )
            })?;
            let data = source.data.ok_or_else(|| {
                ConversionError::UnsupportedContent("base64 image source missing data".to_string())
            })?;
            let format =
                image_format_from_base64_or_media_type(&media_type, &data).ok_or_else(|| {
                    ConversionError::UnsupportedContent(format!(
                        "unsupported image media_type: {}",
                        media_type
                    ))
                })?;
            Ok(KiroImage::from_base64(format, data))
        }
        "url" => {
            let url = source.url.ok_or_else(|| {
                ConversionError::UnsupportedContent("image URL source missing url".to_string())
            })?;
            if let Some((media_type, data)) = parse_data_url(&url) {
                let format = image_format_from_base64_or_media_type(&media_type, &data)
                    .ok_or_else(|| {
                        ConversionError::UnsupportedContent(format!(
                            "unsupported image media_type: {}",
                            media_type
                        ))
                    })?;
                Ok(KiroImage::from_base64(format, data))
            } else {
                Err(ConversionError::UnsupportedContent(
                    "remote image URL source was not materialized before conversion".to_string(),
                ))
            }
        }
        "file" => Err(ConversionError::UnsupportedContent(
            "image file source requires an Anthropic Files API adapter, which is not implemented"
                .to_string(),
        )),
        other => Err(ConversionError::UnsupportedContent(format!(
            "unsupported image source type: {}",
            other
        ))),
    }
}

fn convert_document_source_to_text(
    source: super::types::ImageSource,
) -> Result<String, ConversionError> {
    match source.source_type.as_str() {
        "text" => {
            let media_type = source.media_type.unwrap_or_else(|| "text/plain".to_string());
            let data = source.data.ok_or_else(|| {
                ConversionError::UnsupportedContent("text document source missing data".to_string())
            })?;
            Ok(format_document_text(&media_type, data))
        }
        "base64" => {
            let media_type = source.media_type.ok_or_else(|| {
                ConversionError::UnsupportedContent(
                    "base64 document source missing media_type".to_string(),
                )
            })?;
            let data = source.data.ok_or_else(|| {
                ConversionError::UnsupportedContent(
                    "base64 document source missing data".to_string(),
                )
            })?;
            decode_document_to_text(&media_type, &data)
        }
        "url" => {
            let url = source.url.ok_or_else(|| {
                ConversionError::UnsupportedContent("document URL source missing url".to_string())
            })?;
            if let Some((media_type, data)) = parse_data_url(&url) {
                decode_document_to_text(&media_type, &data)
            } else {
                Err(ConversionError::UnsupportedContent(
                    "remote document URL source was not materialized before conversion"
                        .to_string(),
                ))
            }
        }
        "file" => {
            Err(ConversionError::UnsupportedContent(
                "document file source requires an Anthropic Files API adapter, which is not implemented"
                    .to_string(),
            ))
        }
        other => Err(ConversionError::UnsupportedContent(format!(
            "unsupported document source type: {}",
            other
        ))),
    }
}

fn decode_document_to_text(media_type: &str, data: &str) -> Result<String, ConversionError> {
    let bytes = BASE64_STANDARD.decode(data).map_err(|_| {
        ConversionError::UnsupportedContent(format!(
            "base64 document source contains invalid data for {}",
            media_type
        ))
    })?;

    let text = match media_type {
        "text/plain" | "text/markdown" | "text/html" | "text/csv" | "application/json" => {
            String::from_utf8(bytes).map_err(|_| {
                ConversionError::UnsupportedContent(format!(
                    "document media_type {} is not valid UTF-8 text",
                    media_type
                ))
            })?
        }
        "application/pdf" => extract_text_from_pdf_bytes(&bytes).ok_or_else(|| {
            ConversionError::UnsupportedContent(
                "PDF document text could not be extracted (encrypted, image-only, or malformed PDF)"
                    .to_string(),
            )
        })?,
        _ => {
            return Err(ConversionError::UnsupportedContent(format!(
                "unsupported document media_type: {}",
                media_type
            )));
        }
    };

    Ok(format_document_text(media_type, text))
}

fn format_document_text(media_type: &str, text: String) -> String {
    format!(
        "<document media_type=\"{}\">\n{}\n</document>",
        media_type, text
    )
}

fn extract_text_from_pdf_bytes(bytes: &[u8]) -> Option<String> {
    if !bytes.starts_with(b"%PDF") {
        return None;
    }

    // 优先使用 pdf-extract（支持压缩流、字体编码、布局）
    match extract_pdf_text_with_panic_guard(bytes) {
        Ok(Ok(text)) => {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
            tracing::debug!("pdf-extract 返回空文本，尝试简易解析回退");
        }
        Ok(Err(err)) => {
            tracing::debug!("pdf-extract 抽取失败，回退到简易解析: {}", err);
        }
        Err(_) => {
            tracing::warn!("pdf-extract 抽取过程发生 panic，回退到简易解析");
        }
    }

    extract_text_from_pdf_bytes_fallback(bytes)
}

fn extract_pdf_text_with_panic_guard(
    bytes: &[u8],
) -> Result<Result<String, pdf_extract::OutputError>, ()> {
    let _guard = pdf_extract_panic_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous_hook = std::panic::take_hook();
    let previous_hook_slot = Arc::new(Mutex::new(Some(previous_hook)));
    let hook_slot = Arc::clone(&previous_hook_slot);
    std::panic::set_hook(Box::new(move |info| {
        if is_pdf_extract_panic(info) {
            return;
        }
        if let Some(hook) = hook_slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
        {
            hook(info);
        }
    }));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pdf_extract::extract_text_from_mem(bytes)
    }));
    let previous_hook = previous_hook_slot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
        .unwrap_or_else(|| Box::new(|_| {}));
    std::panic::set_hook(previous_hook);
    result.map_err(|_| ())
}

fn is_pdf_extract_panic(info: &std::panic::PanicHookInfo<'_>) -> bool {
    info.location()
        .is_some_and(|location| location.file().contains("pdf-extract"))
}

fn pdf_extract_panic_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// 简易 PDF 文本抽取兜底：仅处理未压缩的 `(...) Tj` / `TJ` 形态。
///
/// 当 pdf-extract 解析失败（坏 PDF、不支持的编码等）时使用，能力非常有限。
fn extract_text_from_pdf_bytes_fallback(bytes: &[u8]) -> Option<String> {
    let raw = String::from_utf8_lossy(bytes);
    if !raw.contains("%PDF") {
        return None;
    }

    let chars: Vec<char> = raw.chars().collect();
    let mut pieces = Vec::new();
    let mut idx = 0;

    while idx < chars.len() {
        if chars[idx] != '(' {
            idx += 1;
            continue;
        }

        let start = idx + 1;
        idx = start;
        let mut escaped = false;
        let mut piece = String::new();
        while idx < chars.len() {
            let ch = chars[idx];
            if escaped {
                piece.push(match ch {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    'b' => '\u{0008}',
                    'f' => '\u{000c}',
                    '(' | ')' | '\\' => ch,
                    other => other,
                });
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == ')' {
                break;
            } else {
                piece.push(ch);
            }
            idx += 1;
        }

        if idx >= chars.len() {
            break;
        }

        let tail: String = chars.iter().skip(idx + 1).take(80).collect::<String>();
        if tail.contains("Tj") || tail.contains("TJ") || tail.contains('\'') || tail.contains('"') {
            let trimmed = piece.trim();
            if !trimmed.is_empty() {
                pieces.push(trimmed.to_string());
            }
        }

        idx += 1;
    }

    if pieces.is_empty() {
        None
    } else {
        Some(pieces.join("\n"))
    }
}

fn parse_data_url(url: &str) -> Option<(String, String)> {
    let data_part = url.strip_prefix("data:")?;
    let (metadata, data) = data_part.split_once(',')?;
    if !metadata
        .split(';')
        .skip(1)
        .any(|part| part.trim().eq_ignore_ascii_case("base64"))
    {
        return None;
    }
    let media_type = metadata
        .split(';')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    Some((media_type.to_string(), data.to_string()))
}

/// 从 media_type 获取图片格式
fn get_image_format(media_type: &str) -> Option<String> {
    match normalize_media_type(media_type).as_str() {
        "image/jpeg" => Some("jpeg".to_string()),
        "image/png" => Some("png".to_string()),
        "image/gif" => Some("gif".to_string()),
        "image/webp" => Some("webp".to_string()),
        _ => None,
    }
}

fn normalize_media_type(media_type: &str) -> String {
    media_type
        .split(';')
        .next()
        .unwrap_or(media_type)
        .trim()
        .to_ascii_lowercase()
}

fn image_format_from_base64_or_media_type(media_type: &str, data: &str) -> Option<String> {
    let declared = get_image_format(media_type);
    if let Ok(bytes) = BASE64_STANDARD.decode(data) {
        if let Some(detected) = infer_image_format_from_bytes(&bytes) {
            if declared.as_deref().is_some_and(|value| value != detected) {
                tracing::warn!(
                    declared_media_type = %media_type,
                    detected_format = detected,
                    "图片 media_type 与内容字节不一致，已按字节识别结果修正"
                );
            }
            return Some(detected.to_string());
        }
    }
    declared
}

fn infer_image_format_from_bytes(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']) {
        Some("png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

pub(crate) fn infer_image_format_from_url(url: &str) -> Option<String> {
    let path = url.split('?').next().unwrap_or(url).to_ascii_lowercase();
    if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        Some("jpeg".to_string())
    } else if path.ends_with(".png") {
        Some("png".to_string())
    } else if path.ends_with(".gif") {
        Some("gif".to_string())
    } else if path.ends_with(".webp") {
        Some("webp".to_string())
    } else {
        None
    }
}

pub(crate) fn infer_document_media_type_from_url(url: &str) -> String {
    let path = url.split('?').next().unwrap_or(url).to_ascii_lowercase();
    if path.ends_with(".pdf") {
        "application/pdf".to_string()
    } else if path.ends_with(".md") {
        "text/markdown".to_string()
    } else if path.ends_with(".html") || path.ends_with(".htm") {
        "text/html".to_string()
    } else if path.ends_with(".txt") {
        "text/plain".to_string()
    } else {
        "application/octet-stream".to_string()
    }
}

/// 提取工具结果内容
fn extract_tool_result_content(content: &Option<serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => {
            let mut parts = Vec::new();
            for item in arr {
                if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                    parts.push(text.to_string());
                } else if !item.is_null() {
                    parts.push(item.to_string());
                }
            }
            parts.join("\n")
        }
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

fn normalize_tool_result_content(content: String) -> String {
    if content.trim().is_empty() {
        EMPTY_TOOL_RESULT_CONTENT_PLACEHOLDER.to_string()
    } else {
        content
    }
}

fn normalize_tool_use_input(input: serde_json::Value) -> serde_json::Value {
    match input {
        serde_json::Value::Object(_) => input,
        serde_json::Value::Null => serde_json::json!({}),
        other => serde_json::json!({ "value": other }),
    }
}

fn sanitize_tool_use_id(id: &str) -> Option<String> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return Some(trimmed.to_string());
    }

    let sanitized = trimmed
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let sanitized = sanitized.trim_matches('_');
    let prefix = if sanitized.is_empty() {
        "toolu".to_string()
    } else {
        sanitized.to_string()
    };
    let mut hasher = Sha256::new();
    hasher.update(trimmed.as_bytes());
    let digest = hasher.finalize();
    Some(format!(
        "{}_{:02x}{:02x}{:02x}{:02x}",
        prefix, digest[0], digest[1], digest[2], digest[3]
    ))
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
/// 元组：(经过验证和过滤后的 tool_result 列表, 孤立的 tool_use_id 集合, 被保留为文本的孤立 tool_result)
fn validate_tool_pairing(
    history: &[Message],
    tool_results: &[ToolResult],
    warnings: &mut ProxyWarnings,
) -> (
    Vec<ToolResult>,
    std::collections::HashSet<String>,
    Vec<String>,
) {
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
    let mut orphan_tool_result_texts = Vec::new();
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
            warnings.duplicate_tool_results += 1;
            if let Some(text) = kiro_tool_result_to_text(result) {
                warnings.duplicate_tool_results_textified += 1;
                orphan_tool_result_texts.push(format!(
                    "[duplicate tool result {}]\n{}",
                    result.tool_use_id, text
                ));
            }
            tracing::warn!(
                "跳过重复的当前结构化 tool_result，并在兼容模式下转为普通文本：tool_use_id={}",
                result.tool_use_id
            );
        } else if history_tool_result_ids.contains(&result.tool_use_id)
            || all_tool_use_ids.contains(&result.tool_use_id)
        {
            // 不属于最后一条 assistant 的 tool_result 不能作为当前结构化结果继续发送。
            warnings.orphan_tool_results += 1;
            if let Some(text) = kiro_tool_result_to_text(result) {
                warnings.orphan_tool_results_textified += 1;
                orphan_tool_result_texts.push(format!(
                    "[orphan tool result {}]\n{}",
                    result.tool_use_id, text
                ));
            }
            tracing::warn!(
                "tool_result 不属于最后一条 assistant tool_use，已从 tool_results 移除并在兼容模式下转为普通文本，tool_use_id={}",
                result.tool_use_id
            );
        } else {
            // 孤立 tool_result - 找不到对应的 tool_use
            warnings.orphan_tool_results += 1;
            if let Some(text) = kiro_tool_result_to_text(result) {
                warnings.orphan_tool_results_textified += 1;
                orphan_tool_result_texts.push(format!(
                    "[orphan tool result {}]\n{}",
                    result.tool_use_id, text
                ));
            }
            tracing::warn!(
                "孤立的 tool_result 找不到对应 tool_use，已从 tool_results 移除并在兼容模式下转为普通文本，tool_use_id={}",
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
        warnings.orphan_tool_uses += 1;
        tracing::warn!(
            "检测到孤立的 tool_use：找不到对应的 tool_result，将从历史中移除，tool_use_id={}",
            orphaned_id
        );
    }

    (
        filtered_results,
        unpaired_tool_use_ids,
        orphan_tool_result_texts,
    )
}

fn kiro_tool_result_to_text(result: &ToolResult) -> Option<String> {
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
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn append_orphan_tool_result_texts(content: &mut String, texts: &[String]) {
    if texts.is_empty() {
        return;
    }
    let suffix = texts.join("\n\n");
    if content.trim().is_empty() {
        *content = suffix;
    } else {
        content.push_str("\n\n");
        content.push_str(&suffix);
    }
}

/// 从历史消息中移除孤立的 tool_use
///
/// Kiro API 要求每个 tool_use 必须有对应的 tool_result，否则返回 400 Bad Request。
/// 此函数遍历历史中的 assistant 消息，移除没有对应 tool_result 的 tool_use。
///
/// # Arguments
/// * `history` - 可变的历史消息列表
/// * `orphaned_ids` - 需要移除的孤立 tool_use_id 集合
fn remove_orphaned_tool_uses(
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

/// Kiro API 工具名称最大长度限制
const TOOL_NAME_MAX_LEN: usize = 63;
const TOOL_HASH_MARKER: &str = "Hash";

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

/// 生成确定性 Kiro-safe 名称：截断前缀 + Hash + 8 位 SHA256 hex
fn shorten_tool_name(name: &str, hash_input: &str) -> String {
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
fn map_tool_name(name: &str, tool_name_map: &mut HashMap<String, String>) -> String {
    let sanitized = sanitize_tool_name(name);
    let mapped = if sanitized != name || sanitized.len() > TOOL_NAME_MAX_LEN {
        shorten_tool_name(&sanitized, name)
    } else {
        sanitized
    };
    if mapped != name {
        tool_name_map.insert(mapped.clone(), name.to_string());
    }
    mapped
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

fn tool_choice_matches_name(tool_name: &str, requested_name: &str) -> bool {
    tool_name == requested_name
        || sanitize_tool_name(tool_name) == sanitize_tool_name(requested_name)
}

fn selected_tool_indices(
    tools: &[super::types::Tool],
    directive: &ToolChoiceDirective,
) -> Vec<usize> {
    match directive {
        ToolChoiceDirective::None => Vec::new(),
        ToolChoiceDirective::Tool(requested_name) => {
            let selected = tools
                .iter()
                .enumerate()
                .filter_map(|(idx, tool)| {
                    tool_choice_matches_name(&tool.name, requested_name).then_some(idx)
                })
                .collect::<Vec<_>>();
            if selected.is_empty() {
                tracing::warn!(
                    requested_tool = requested_name,
                    "tool_choice requested a tool that is not present in tools; preserving all tools for compatibility"
                );
                (0..tools.len()).collect()
            } else {
                selected
            }
        }
        ToolChoiceDirective::Auto | ToolChoiceDirective::Any | ToolChoiceDirective::Unknown => {
            (0..tools.len()).collect()
        }
    }
}

fn generate_tool_choice_prefix(req: &MessagesRequest, options: ConverterOptions) -> Option<String> {
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
struct ConvertedTools {
    tools: Vec<Tool>,
    tool_cache_point_insert_after: Vec<usize>,
}

fn convert_tools(
    tools: &Option<Vec<super::types::Tool>>,
    tool_choice: &Option<serde_json::Value>,
    tool_name_map: &mut HashMap<String, String>,
    options: ConverterOptions,
) -> ConvertedTools {
    let Some(tools) = tools else {
        return ConvertedTools::default();
    };
    let directive = parse_tool_choice(tool_choice);
    let selected_indices = selected_tool_indices(tools, &directive);
    let selected: std::collections::HashSet<_> = selected_indices.into_iter().collect();

    let mut seen_tool_names = std::collections::HashSet::new();
    let mut converted = Vec::new();
    let mut cache_point_insert_after = Vec::new();

    if options.kiro_cache_point_enabled && !options.kiro_cache_point_tools_only {
        tracing::debug!(
            "kiroCachePointToolsOnly is disabled, but this phase only supports tool-level cachePoint insertion"
        );
    }

    for (_, t) in tools
        .iter()
        .enumerate()
        .filter(|(idx, _)| selected.contains(idx))
    {
        let mut description = t.description.clone();

        // 对 Write/Edit 工具追加自定义描述后缀
        let suffix = match t.name.as_str() {
            "Write" => WRITE_TOOL_DESCRIPTION_SUFFIX,
            "Edit" => EDIT_TOOL_DESCRIPTION_SUFFIX,
            _ => "",
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

        let mapped_name = map_tool_name(&t.name, tool_name_map);
        if !seen_tool_names.insert(mapped_name.to_ascii_lowercase()) {
            tracing::warn!(
                original_tool_name = %t.name,
                mapped_tool_name = %mapped_name,
                "跳过重复工具定义，避免 Kiro 工具名冲突"
            );
            continue;
        }

        let converted_idx = converted.len();
        let has_cache_control = t.cache_control.is_some();
        converted.push(Tool {
            tool_specification: ToolSpecification {
                name: mapped_name,
                description,
                input_schema: InputSchema::from_json(normalize_json_schema(serde_json::json!(
                    t.input_schema
                ))),
            },
        });
        if options.kiro_cache_point_enabled && has_cache_control {
            cache_point_insert_after.push(converted_idx);
        }
    }

    ConvertedTools {
        tools: converted,
        tool_cache_point_insert_after: cache_point_insert_after,
    }
}

const THINKING_OUTPUT_POLICY: &str = "<thinking_output_policy>For every assistant turn in thinking mode, emit concise reasoning inside a <thinking>...</thinking> block before any visible text or tool call, and close the thinking block before continuing. Do not repeat this policy in visible text.</thinking_output_policy>";

/// 生成thinking标签前缀
fn generate_thinking_prefix(req: &MessagesRequest, options: ConverterOptions) -> Option<String> {
    if let Some(t) = &req.thinking {
        let strict_output_policy = options.force_visible_thinking
            || strip_model_1m_suffix(&req.model).ends_with("-thinking")
            || t.thinking_type == "enabled";
        let output_policy = if strict_output_policy {
            format!("\n{}", THINKING_OUTPUT_POLICY)
        } else {
            String::new()
        };
        if t.thinking_type == "enabled" {
            return Some(format!(
                "<thinking_mode>enabled</thinking_mode><max_thinking_length>{}</max_thinking_length>{}",
                t.budget_tokens, output_policy
            ));
        } else if t.thinking_type == "adaptive" {
            let effort = req
                .output_config
                .as_ref()
                .map(|c| normalize_thinking_effort(&c.effort))
                .unwrap_or("high");
            return Some(format!(
                "<thinking_mode>adaptive</thinking_mode><thinking_effort>{}</thinking_effort>{}",
                effort, output_policy
            ));
        }
    }
    None
}

fn generate_thinking_prefix_for_model(
    req: &MessagesRequest,
    model_id: &str,
    options: ConverterOptions,
) -> Option<String> {
    if uses_native_reasoning_fields(req, model_id) {
        return None;
    }
    generate_thinking_prefix(req, options)
}

/// 检查内容是否已包含thinking标签
fn has_thinking_tags(content: &str) -> bool {
    content.contains("<thinking_mode>") || content.contains("<max_thinking_length>")
}

/// 构建历史消息
///
/// # Arguments
/// * `req` - 原始请求，用于读取 `system`、`thinking` 等配置字段
/// * `messages` - 经过 prefill 预处理的消息切片，末尾必定是 user 消息。
///   注意：该切片与 `req.messages` 可能不同（prefill 时会截断末尾的 assistant 消息），
///   调用方应始终使用此参数而非 `req.messages`。
/// * `model_id` - 已映射的 Kiro 模型 ID
fn build_history(
    req: &MessagesRequest,
    messages: &[super::types::Message],
    model_id: &str,
    tool_name_map: &mut HashMap<String, String>,
    options: ConverterOptions,
) -> Result<Vec<Message>, ConversionError> {
    let mut history = Vec::new();

    // 生成thinking前缀（如果需要）
    let thinking_prefix = if options.inject_thinking_prefix() {
        generate_thinking_prefix_for_model(req, model_id, options)
    } else {
        None
    };
    let tool_choice_prefix = generate_tool_choice_prefix(req, options);

    // 1. 处理系统消息
    if let Some(ref system) = req.system {
        let system_content: String = system
            .iter()
            .map(|s| s.text.clone())
            .collect::<Vec<_>>()
            .join("\n");

        if !system_content.is_empty() {
            // 追加分块写入策略到系统消息
            let system_content = if options.inject_chunked_policy() {
                format!("{}\n{}", system_content, SYSTEM_CHUNKED_POLICY)
            } else {
                system_content
            };

            let system_content = if let Some(ref prefix) = tool_choice_prefix {
                format!("{}\n{}", prefix, system_content)
            } else {
                system_content
            };

            // 注入thinking标签到系统消息最前面（如果需要且不存在）
            let final_content = if let Some(ref prefix) = thinking_prefix {
                if !has_thinking_tags(&system_content) {
                    format!("{}\n{}", prefix, system_content)
                } else {
                    system_content
                }
            } else {
                system_content
            };

            // 系统消息作为 user + assistant 配对
            let user_msg = HistoryUserMessage::new(final_content, model_id);
            history.push(Message::User(user_msg));

            let assistant_msg = HistoryAssistantMessage::new("I will follow these instructions.");
            history.push(Message::Assistant(assistant_msg));
        }
    } else {
        let mut synthetic_prefixes = Vec::new();
        if let Some(prefix) = thinking_prefix {
            synthetic_prefixes.push(prefix);
        }
        if let Some(prefix) = tool_choice_prefix {
            synthetic_prefixes.push(prefix);
        }

        if !synthetic_prefixes.is_empty() {
            // 没有系统消息但有控制配置，插入新的系统消息
            let user_msg = HistoryUserMessage::new(synthetic_prefixes.join("\n"), model_id);
            history.push(Message::User(user_msg));

            let assistant_msg = HistoryAssistantMessage::new("I will follow these instructions.");
            history.push(Message::Assistant(assistant_msg));
        }
    }

    // 2. 处理常规消息历史
    // 最后一条消息作为 currentMessage，不加入历史
    // 经过 prefill 预处理后，messages 末尾必定是 user，故直接截掉最后一条即可
    let history_end_index = messages.len().saturating_sub(1);

    // 收集并配对消息
    let mut user_buffer: Vec<&super::types::Message> = Vec::new();
    let mut assistant_buffer: Vec<&super::types::Message> = Vec::new();

    for i in 0..history_end_index {
        let msg = &messages[i];

        if msg.role == "user" {
            // 先处理累积的 assistant 消息
            if !assistant_buffer.is_empty() {
                let merged = merge_assistant_messages(&assistant_buffer, tool_name_map)?;
                history.push(Message::Assistant(merged));
                assistant_buffer.clear();
            }
            user_buffer.push(msg);
        } else if msg.role == "assistant" {
            // 先处理累积的 user 消息
            if !user_buffer.is_empty() {
                let merged_user = merge_user_messages(&user_buffer, model_id)?;
                history.push(Message::User(merged_user));
                user_buffer.clear();
            }
            // 累积 assistant 消息（支持连续多条）
            assistant_buffer.push(msg);
        }
    }

    // 处理末尾累积的 assistant 消息
    if !assistant_buffer.is_empty() {
        let merged = merge_assistant_messages(&assistant_buffer, tool_name_map)?;
        history.push(Message::Assistant(merged));
    }

    // 处理结尾的孤立 user 消息
    if !user_buffer.is_empty() {
        let merged_user = merge_user_messages(&user_buffer, model_id)?;
        history.push(Message::User(merged_user));

        // 自动配对一个 "OK" 的 assistant 响应
        let auto_assistant = HistoryAssistantMessage::new("OK");
        history.push(Message::Assistant(auto_assistant));
    }

    Ok(history)
}

/// 合并多个 user 消息
fn merge_user_messages(
    messages: &[&super::types::Message],
    model_id: &str,
) -> Result<HistoryUserMessage, ConversionError> {
    let mut content_parts = Vec::new();
    let mut all_images = Vec::new();
    let mut all_tool_results = Vec::new();

    for msg in messages {
        let (text, images, tool_results) = process_message_content(&msg.content)?;
        if !text.is_empty() {
            content_parts.push(text);
        }
        all_images.extend(images);
        all_tool_results.extend(tool_results);
    }

    let mut content = content_parts.join("\n");
    if content.trim().is_empty() && !all_tool_results.is_empty() {
        content = TOOL_RESULTS_PROVIDED_PLACEHOLDER.to_string();
    }
    // 保留文本内容，即使有工具结果也不丢弃用户文本
    let mut user_msg = UserMessage::new(&content, model_id);

    if !all_images.is_empty() {
        user_msg = user_msg.with_images(all_images);
    }
    if !all_tool_results.is_empty() {
        let mut ctx = UserInputMessageContext::new();
        ctx = ctx.with_tool_results(all_tool_results);
        user_msg = user_msg.with_context(ctx);
    }

    Ok(HistoryUserMessage {
        user_input_message: user_msg,
    })
}

/// 转换 assistant 消息
fn convert_assistant_message(
    msg: &super::types::Message,
    tool_name_map: &mut HashMap<String, String>,
) -> Result<HistoryAssistantMessage, ConversionError> {
    let mut thinking_content = String::new();
    let mut text_content = String::new();
    let mut tool_uses = Vec::new();

    match &msg.content {
        serde_json::Value::String(s) => {
            text_content = s.clone();
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let Ok(block) = serde_json::from_value::<ContentBlock>(item.clone()) {
                    match block.block_type.as_str() {
                        "thinking" => {
                            if block.signature.as_deref().is_some_and(|s| !s.is_empty()) {
                                tracing::debug!(
                                    "当前 Kiro history 模型不支持 Anthropic thinking signature；仅透传 thinking 文本"
                                );
                            }
                            if let Some(thinking) = block.thinking {
                                thinking_content.push_str(&thinking);
                            }
                        }
                        "redacted_thinking" => {
                            if block.data.as_deref().is_some_and(|s| !s.is_empty()) {
                                tracing::debug!(
                                    "当前 Kiro history 模型不支持 redacted_thinking；已跳过该历史块"
                                );
                            }
                        }
                        "text" => {
                            if let Some(text) = block.text {
                                text_content.push_str(&text);
                            }
                        }
                        "tool_use" => {
                            if let (Some(id), Some(name)) = (
                                block.id.as_deref().and_then(sanitize_tool_use_id),
                                block
                                    .name
                                    .as_deref()
                                    .map(str::trim)
                                    .filter(|name| !name.is_empty()),
                            ) {
                                let input = normalize_tool_use_input(
                                    block.input.unwrap_or(serde_json::json!({})),
                                );
                                let mapped_name = map_tool_name(name, tool_name_map);
                                tool_uses
                                    .push(ToolUseEntry::new(id, mapped_name).with_input(input));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }

    // 组合 thinking 和 text 内容
    // 格式: <thinking>思考内容</thinking>\n\ntext内容
    // 注意: Kiro API 要求 content 字段不能为空，当只有 tool_use 时需要占位符
    let final_content = if !thinking_content.is_empty() {
        if !text_content.is_empty() {
            format!(
                "<thinking>{}</thinking>\n\n{}",
                thinking_content, text_content
            )
        } else {
            format!("<thinking>{}</thinking>", thinking_content)
        }
    } else if text_content.is_empty() && !tool_uses.is_empty() {
        " ".to_string()
    } else {
        text_content
    };

    let mut assistant = AssistantMessage::new(final_content);
    if !tool_uses.is_empty() {
        assistant = assistant.with_tool_uses(tool_uses);
    }

    Ok(HistoryAssistantMessage {
        assistant_response_message: assistant,
    })
}

/// 合并多个连续的 assistant 消息为一条
/// 用于处理网络不稳定时产生的连续 assistant 消息（Issue #79）
fn merge_assistant_messages(
    messages: &[&super::types::Message],
    tool_name_map: &mut HashMap<String, String>,
) -> Result<HistoryAssistantMessage, ConversionError> {
    assert!(!messages.is_empty());
    if messages.len() == 1 {
        return convert_assistant_message(messages[0], tool_name_map);
    }

    let mut all_tool_uses: Vec<ToolUseEntry> = Vec::new();
    let mut content_parts: Vec<String> = Vec::new();

    for msg in messages {
        let converted = convert_assistant_message(msg, tool_name_map)?;
        let am = converted.assistant_response_message;
        if !am.content.trim().is_empty() {
            content_parts.push(am.content);
        }
        if let Some(tus) = am.tool_uses {
            all_tool_uses.extend(tus);
        }
    }

    let content = if content_parts.is_empty() && !all_tool_uses.is_empty() {
        " ".to_string()
    } else {
        content_parts.join("\n\n")
    };

    let mut assistant = AssistantMessage::new(content);
    if !all_tool_uses.is_empty() {
        assistant = assistant.with_tool_uses(all_tool_uses);
    }
    Ok(HistoryAssistantMessage {
        assistant_response_message: assistant,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_model_sonnet() {
        assert!(
            map_model("claude-sonnet-4-20250514")
                .unwrap()
                .contains("sonnet")
        );
        assert!(
            map_model("claude-3-5-sonnet-20241022")
                .unwrap()
                .contains("sonnet")
        );
    }

    #[test]
    fn test_map_model_opus() {
        assert!(
            map_model("claude-opus-4-20250514")
                .unwrap()
                .contains("opus")
        );
    }

    #[test]
    fn test_map_model_haiku() {
        assert!(
            map_model("claude-haiku-4-20250514")
                .unwrap()
                .contains("haiku")
        );
    }

    #[test]
    fn test_map_model_unsupported() {
        assert!(map_model("gpt-4").is_none());
    }

    #[test]
    fn test_map_model_claude_code_aliases() {
        assert_eq!(map_model("opus"), Some("claude-opus-4.7".to_string()));
        assert_eq!(map_model("opusplan"), Some("claude-opus-4.7".to_string()));
        assert_eq!(map_model("best"), Some("claude-opus-4.7".to_string()));
        assert_eq!(map_model("default"), Some("claude-opus-4.7".to_string()));
        assert_eq!(map_model("sonnet"), Some("claude-sonnet-4.6".to_string()));
        assert_eq!(
            map_model("claude-opus-4-7[1m]"),
            Some("claude-opus-4.7".to_string())
        );
    }

    #[test]
    fn test_map_model_future_claude_models_pass_through() {
        assert_eq!(
            map_model("claude-sonnet-4-9-20270101"),
            Some("claude-sonnet-4-9-20270101".to_string())
        );
        assert_eq!(
            map_model("claude-opus-5-20270101"),
            Some("claude-opus-5-20270101".to_string())
        );
        assert_eq!(
            map_model("claude-haiku-4-7-20270101"),
            Some("claude-haiku-4-7-20270101".to_string())
        );
        assert_eq!(
            map_model("Claude-Sonnet-4-9-20270101-thinking[1m]"),
            Some("claude-sonnet-4-9-20270101".to_string())
        );
    }

    #[test]
    fn test_content_block_preserves_thinking_signature_and_redacted_data() {
        let thinking: ContentBlock = serde_json::from_value(serde_json::json!({
            "type": "thinking",
            "thinking": "reasoning",
            "signature": "sig"
        }))
        .unwrap();
        assert_eq!(thinking.thinking.as_deref(), Some("reasoning"));
        assert_eq!(thinking.signature.as_deref(), Some("sig"));

        let redacted: ContentBlock = serde_json::from_value(serde_json::json!({
            "type": "redacted_thinking",
            "data": "opaque"
        }))
        .unwrap();
        assert_eq!(redacted.data.as_deref(), Some("opaque"));
    }

    #[test]
    fn test_process_message_content_accepts_base64_image_source() {
        let content = serde_json::json!([
            {"type": "text", "text": "describe"},
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": "aW1hZ2U="
                }
            }
        ]);

        let (text, images, tool_results) = process_message_content(&content).unwrap();
        assert_eq!(text, "describe");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].format, "png");
        assert_eq!(images[0].source.bytes.as_deref(), Some("aW1hZ2U="));
        assert!(tool_results.is_empty());
    }

    #[test]
    fn test_process_message_content_accepts_image_data_url_source() {
        let content = serde_json::json!([
            {
                "type": "image",
                "source": {
                    "type": "url",
                    "url": "data:image/png;base64,aW1hZ2U="
                }
            }
        ]);

        let (_, images, _) = process_message_content(&content).unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].format, "png");
        assert_eq!(images[0].source.bytes.as_deref(), Some("aW1hZ2U="));
    }

    #[test]
    fn test_process_message_content_extracts_text_document_block() {
        let content = serde_json::json!([
            {
                "type": "document",
                "source": {
                    "type": "base64",
                    "media_type": "text/plain",
                    "data": "a2lyby1kb2MtdGVzdA=="
                }
            }
        ]);

        let (text, images, _) = process_message_content(&content).unwrap();
        assert!(images.is_empty());
        assert!(text.contains("kiro-doc-test"));
        assert!(text.contains("media_type=\"text/plain\""));
    }

    #[test]
    fn test_process_message_content_extracts_simple_pdf_text() {
        let pdf = b"%PDF-1.1\nBT /F1 12 Tf 20 100 Td (kiro-pdf-test) Tj ET\n%%EOF";
        let data = BASE64_STANDARD.encode(pdf);
        let content = serde_json::json!([
            {
                "type": "document",
                "source": {
                    "type": "base64",
                    "media_type": "application/pdf",
                    "data": data
                }
            }
        ]);

        let (text, images, _) = process_message_content(&content).unwrap();
        assert!(images.is_empty());
        assert!(text.contains("kiro-pdf-test"));
        assert!(text.contains("media_type=\"application/pdf\""));
    }

    #[test]
    fn test_map_model_thinking_suffix_sonnet() {
        // thinking 后缀不应影响 sonnet 模型映射
        let result = map_model("claude-sonnet-4-5-20250929-thinking");
        assert_eq!(result, Some("claude-sonnet-4.5".to_string()));
    }

    #[test]
    fn test_map_model_thinking_suffix_opus_4_5() {
        // thinking 后缀不应影响 opus 4.5 模型映射
        let result = map_model("claude-opus-4-5-20251101-thinking");
        assert_eq!(result, Some("claude-opus-4.5".to_string()));
    }

    #[test]
    fn test_map_model_thinking_suffix_opus_4_6() {
        // thinking 后缀不应影响 opus 4.6 模型映射
        let result = map_model("claude-opus-4-6-thinking");
        assert_eq!(result, Some("claude-opus-4.6".to_string()));
    }

    #[test]
    fn test_map_model_thinking_suffix_haiku() {
        // thinking 后缀不应影响 haiku 模型映射
        let result = map_model("claude-haiku-4-5-20251001-thinking");
        assert_eq!(result, Some("claude-haiku-4.5".to_string()));
    }

    #[test]
    fn test_context_window_size_for_kiro_auto_and_dash_variants() {
        assert_eq!(get_context_window_size("auto"), 1_000_000);
        assert_eq!(get_context_window_size("sonnet"), 200_000);
        assert_eq!(get_context_window_size("opus"), 200_000);
        assert_eq!(get_context_window_size("claude-opus-4.8"), 1_000_000);
        assert_eq!(get_context_window_size("claude-opus-4.7"), 1_000_000);
        assert_eq!(get_context_window_size("claude-sonnet-4.6"), 1_000_000);
        assert_eq!(
            get_context_window_size("claude-opus-4.7-thinking[1m]"),
            1_000_000
        );
        assert_eq!(get_context_window_size("claude-sonnet-4-6[1m]"), 1_000_000);
        assert_eq!(get_context_window_size("claude-opus-4-7"), 200_000);
        assert_eq!(get_context_window_size("claude-sonnet-4-6"), 200_000);
    }

    #[test]
    fn test_determine_chat_trigger_type() {
        // 无工具时返回 MANUAL
        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };
        assert_eq!(determine_chat_trigger_type(&req), "MANUAL");
    }

    #[test]
    fn test_collect_history_tool_names() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 创建包含工具使用的历史消息
        let mut assistant_msg = AssistantMessage::new("I'll read the file.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read")
                .with_input(serde_json::json!({"path": "/test.txt"})),
            ToolUseEntry::new("tool-2", "write")
                .with_input(serde_json::json!({"path": "/out.txt"})),
        ]);

        let history = vec![
            Message::User(HistoryUserMessage::new(
                "Read the file",
                "claude-sonnet-4.5",
            )),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
        ];

        let tool_names = collect_history_tool_names(&history);
        assert_eq!(tool_names.len(), 2);
        assert!(tool_names.contains(&"read".to_string()));
        assert!(tool_names.contains(&"write".to_string()));
    }

    #[test]
    fn test_collect_history_tool_names_dedupes_case_insensitive() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        let mut assistant_msg = AssistantMessage::new("Using tools");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "Read"),
            ToolUseEntry::new("tool-2", "read"),
        ]);
        let history = vec![Message::Assistant(HistoryAssistantMessage {
            assistant_response_message: assistant_msg,
        })];

        let tool_names = collect_history_tool_names(&history);

        assert_eq!(tool_names, vec!["Read".to_string()]);
    }

    #[test]
    fn test_create_placeholder_tool() {
        let tool = create_placeholder_tool("my_custom_tool");

        assert_eq!(tool.tool_specification.name, "my_custom_tool");
        assert!(!tool.tool_specification.description.is_empty());

        // 验证 JSON 序列化正确
        let json = serde_json::to_string(&tool).unwrap();
        assert!(json.contains("\"name\":\"my_custom_tool\""));
        assert!(!json.contains("additionalProperties"));
        assert!(!json.contains("required"));
        assert!(!json.contains("$schema"));
    }

    #[test]
    fn test_normalize_json_schema_recursively_removes_kiro_rejected_fields() {
        let schema = serde_json::json!({
            "type": "object",
            "additionalProperties": true,
            "required": [],
            "properties": {
                "path": {
                    "type": "string",
                    "required": null,
                    "additionalProperties": false
                },
                "mode": {
                    "type": "object",
                    "required": ["kind", 7, null],
                    "additionalProperties": {"type": "string"},
                    "properties": {
                        "kind": {"type": "string"},
                        "nested": {
                            "type": "object",
                            "required": [],
                            "additionalProperties": true
                        }
                    }
                }
            }
        });

        let normalized = normalize_json_schema(schema);
        assert_eq!(normalized["type"], "object");
        assert_eq!(
            normalized["properties"]["mode"]["required"],
            serde_json::json!(["kind"])
        );
        assert!(normalized.get("required").is_none());
        assert!(normalized.get("additionalProperties").is_none());
        assert!(
            normalized["properties"]["path"]
                .get("additionalProperties")
                .is_none()
        );
        assert!(
            normalized["properties"]["mode"]
                .get("additionalProperties")
                .is_none()
        );
        assert!(
            normalized["properties"]["mode"]["properties"]["nested"]
                .get("required")
                .is_none()
        );
        assert!(
            normalized["properties"]["mode"]["properties"]["nested"]
                .get("additionalProperties")
                .is_none()
        );
    }

    #[test]
    fn test_normalize_json_schema_sanitizes_openapi_and_shorthand_schema() {
        let schema = serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "$vocabulary": {"https://json-schema.org/draft/2020-12/vocab/core": true},
            "type": "object",
            "$id": "",
            "$anchor": " ",
            "nullable": true,
            "additionalProperties": false,
            "x-mcp-source": "test",
            "discriminator": {"propertyName": "kind"},
            "xml": {"name": "toolInput"},
            "externalDocs": {"url": "https://example.com"},
            "required": ["path", "missing", 7, "path"],
            "properties": {
                "path": "str",
                "enabled": {"type": "bool", "nullable": true},
                "count": {"type": ["int", null, "bad", "integer"]},
                "tags": {"type": "list", "items": "string"},
                "tuple": {"type": "array", "items": ["string", {"type": "int"}, 3]},
                "choice": {"oneOf": {"type": "str"}},
                "mode": {"type": "string", "enum": "read"},
                "bad": null,
                "constant": true,
                "title": {"text": "not a valid title"},
                "emptyPattern": {"type": "string", "pattern": ""},
                "emptyFormat": {"type": "string", "format": " "},
                "bounded": {
                    "type": "number",
                    "minimum": "zero",
                    "maximum": 10,
                    "multipleOf": 0
                }
            }
        });

        let normalized = normalize_json_schema(schema);

        assert_eq!(normalized["type"], "object");
        assert_eq!(normalized["required"], serde_json::json!(["path"]));
        assert!(normalized.get("$schema").is_none());
        assert!(normalized.get("$vocabulary").is_none());
        assert!(normalized.get("nullable").is_none());
        assert!(normalized.get("additionalProperties").is_none());
        assert!(normalized.get("x-mcp-source").is_none());
        assert!(normalized.get("discriminator").is_none());
        assert!(normalized.get("xml").is_none());
        assert!(normalized.get("externalDocs").is_none());
        assert!(normalized.get("$id").is_none());
        assert!(normalized.get("$anchor").is_none());

        let props = &normalized["properties"];
        assert_eq!(props["path"], serde_json::json!({"type": "string"}));
        assert_eq!(
            props["enabled"]["type"],
            serde_json::json!(["boolean", "null"])
        );
        assert_eq!(props["count"]["type"], serde_json::json!("integer"));
        assert_eq!(props["tags"]["type"], serde_json::json!("array"));
        assert_eq!(
            props["tags"]["items"],
            serde_json::json!({"type": "string"})
        );
        assert_eq!(
            props["tuple"]["prefixItems"],
            serde_json::json!([{"type": "string"}, {"type": "integer"}])
        );
        assert_eq!(
            props["choice"]["oneOf"],
            serde_json::json!([{"type": "string"}])
        );
        assert_eq!(props["mode"]["enum"], serde_json::json!(["read"]));
        assert_eq!(props["bad"], serde_json::json!({}));
        assert_eq!(props["constant"], serde_json::json!(true));
        assert!(props["title"].get("title").is_none());
        assert!(props["emptyPattern"].get("pattern").is_none());
        assert!(props["emptyFormat"].get("format").is_none());
        assert!(props["bounded"].get("minimum").is_none());
        assert_eq!(props["bounded"]["maximum"], serde_json::json!(10));
        assert!(props["bounded"].get("multipleOf").is_none());
    }

    #[test]
    fn test_normalize_json_schema_flattens_root_union_combinators() {
        let schema = serde_json::json!({
            "oneOf": [
                {
                    "type": "object",
                    "required": ["path", "mode"],
                    "properties": {
                        "path": {"type": "string"},
                        "mode": {"type": "string"}
                    }
                },
                {
                    "type": "object",
                    "required": ["path", "query"],
                    "properties": {
                        "path": {"type": "string"},
                        "query": {"type": "string"}
                    }
                }
            ]
        });

        let normalized = normalize_json_schema(schema);

        assert_eq!(normalized["type"], "object");
        assert!(normalized.get("oneOf").is_none());
        assert!(normalized.get("anyOf").is_none());
        assert!(normalized.get("allOf").is_none());
        assert_eq!(
            normalized["properties"]["path"],
            serde_json::json!({"type": "string"})
        );
        assert_eq!(
            normalized["properties"]["mode"],
            serde_json::json!({"type": "string"})
        );
        assert_eq!(
            normalized["properties"]["query"],
            serde_json::json!({"type": "string"})
        );
        assert_eq!(normalized["required"], serde_json::json!(["path"]));
    }

    #[test]
    fn test_normalize_json_schema_flattens_root_all_of_required_union() {
        let schema = serde_json::json!({
            "allOf": [
                {
                    "type": "object",
                    "required": ["path"],
                    "properties": {
                        "path": {"type": "string"}
                    }
                },
                {
                    "type": "object",
                    "required": ["recursive"],
                    "properties": {
                        "recursive": {"type": "boolean"}
                    }
                }
            ]
        });

        let normalized = normalize_json_schema(schema);

        assert!(normalized.get("allOf").is_none());
        assert_eq!(normalized["properties"]["path"]["type"], "string");
        assert_eq!(normalized["properties"]["recursive"]["type"], "boolean");
        assert_eq!(
            normalized["required"],
            serde_json::json!(["path", "recursive"])
        );
    }

    #[test]
    fn test_normalize_json_schema_converts_legacy_definition_keywords() {
        let schema = serde_json::json!({
            "type": "object",
            "definitions": {
                "file": {
                    "type": "object",
                    "required": ["path", "unused"],
                    "properties": {
                        "path": {"type": "str"},
                        "size": {"type": "int"}
                    }
                }
            },
            "properties": {
                "file": {"$ref": "#/definitions/file"},
                "owner": {"type": "string"}
            },
            "dependencies": {
                "file": ["owner", 1, "owner"],
                "owner": {"properties": {"team": "string"}, "required": ["team"]}
            },
            "dependentRequired": {
                "owner": ["file", null]
            }
        });

        let normalized = normalize_json_schema(schema);

        assert!(normalized.get("definitions").is_none());
        assert_eq!(normalized["properties"]["file"]["$ref"], "#/$defs/file");
        assert_eq!(
            normalized["$defs"]["file"]["properties"]["path"],
            serde_json::json!({"type": "string"})
        );
        assert_eq!(
            normalized["$defs"]["file"]["required"],
            serde_json::json!(["path"])
        );
        assert_eq!(
            normalized["dependentRequired"]["file"],
            serde_json::json!(["owner"])
        );
        assert_eq!(
            normalized["dependentRequired"]["owner"],
            serde_json::json!(["file"])
        );
        assert_eq!(
            normalized["dependentSchemas"]["owner"]["properties"]["team"],
            serde_json::json!({"type": "string"})
        );
    }

    #[test]
    fn test_shorten_tool_name_deterministic() {
        let long_name =
            "mcp__some_very_long_server_name__some_very_long_tool_name_that_exceeds_limit";
        assert!(long_name.len() > TOOL_NAME_MAX_LEN);

        let short1 = shorten_tool_name(long_name, long_name);
        let short2 = shorten_tool_name(long_name, long_name);
        assert_eq!(short1, short2, "相同输入应产生相同的短名称");
        assert!(
            short1.len() <= TOOL_NAME_MAX_LEN,
            "短名称长度应 <= 63，实际 {}",
            short1.len()
        );
    }

    #[test]
    fn test_shorten_tool_name_uniqueness() {
        let name_a = "mcp__server_alpha__tool_name_that_is_very_long_and_exceeds_the_limit_a";
        let name_b = "mcp__server_alpha__tool_name_that_is_very_long_and_exceeds_the_limit_b";
        let short_a = shorten_tool_name(name_a, name_a);
        let short_b = shorten_tool_name(name_b, name_b);
        assert_ne!(short_a, short_b, "不同输入应产生不同的短名称");
    }

    #[test]
    fn test_map_tool_name_short_passthrough() {
        let mut map = HashMap::new();
        let result = map_tool_name("shortName", &mut map);
        assert_eq!(result, "shortName");
        assert!(map.is_empty(), "Kiro-safe 短名称不应产生映射");
    }

    #[test]
    fn test_map_tool_name_sanitizes_separators_and_records_mapping() {
        let mut map = HashMap::new();
        let result = map_tool_name("mcp__server-name__read_file", &mut map);
        assert!(result.len() <= TOOL_NAME_MAX_LEN);
        assert!(result.chars().all(|ch| ch.is_ascii_alphanumeric()));
        assert!(result.contains(TOOL_HASH_MARKER));
        assert_eq!(
            map.get(&result),
            Some(&"mcp__server-name__read_file".to_string())
        );
    }

    #[test]
    fn test_map_tool_name_avoids_collisions_after_sanitizing() {
        let mut map = HashMap::new();
        let dash = map_tool_name("foo-bar", &mut map);
        let underscore = map_tool_name("foo_bar", &mut map);
        assert_ne!(dash, underscore);
        assert_eq!(map.get(&dash), Some(&"foo-bar".to_string()));
        assert_eq!(map.get(&underscore), Some(&"foo_bar".to_string()));
    }

    #[test]
    fn test_map_tool_name_long_creates_mapping() {
        let mut map = HashMap::new();
        let long_name = "mcp__plugin_very_long_server_name__extremely_long_tool_name_exceeds_63";
        let result = map_tool_name(long_name, &mut map);
        assert!(result.len() <= TOOL_NAME_MAX_LEN);
        assert!(result.chars().all(|ch| ch.is_ascii_alphanumeric()));
        assert_eq!(map.get(&result), Some(&long_name.to_string()));
    }

    #[test]
    fn test_tool_name_mapping_in_convert_request() {
        use super::super::types::{Message as AnthropicMessage, Tool as AnthropicTool};

        let long_tool_name =
            "mcp__plugin_very_long_server_name__extremely_long_tool_name_exceeds_63";
        assert!(long_tool_name.len() > TOOL_NAME_MAX_LEN);

        let mut schema = std::collections::HashMap::new();
        schema.insert("type".to_string(), serde_json::json!("object"));
        schema.insert("properties".to_string(), serde_json::json!({}));

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("test"),
            }],
            system: None,
            stream: false,
            tools: Some(vec![AnthropicTool {
                name: long_tool_name.to_string(),
                description: "A test tool".to_string(),
                input_schema: schema,
                tool_type: None,
                max_uses: None,
                cache_control: None,
            }]),
            thinking: None,
            tool_choice: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req).unwrap();

        // 应该有映射
        assert_eq!(result.tool_name_map.len(), 1);

        // 映射中的值应该是原始名称
        let (short, original) = result.tool_name_map.iter().next().unwrap();
        assert_eq!(original, long_tool_name);
        assert!(short.len() <= TOOL_NAME_MAX_LEN);

        // Kiro 请求中的工具名应该是短名称
        let tools = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tools;
        assert_eq!(tools[0].tool_specification.name, *short);
    }

    #[test]
    fn test_tool_name_mapping_in_history() {
        use super::super::types::{Message as AnthropicMessage, Tool as AnthropicTool};

        let long_tool_name =
            "mcp__plugin_very_long_server_name__extremely_long_tool_name_exceeds_63";

        let mut schema = std::collections::HashMap::new();
        schema.insert("type".to_string(), serde_json::json!("object"));
        schema.insert("properties".to_string(), serde_json::json!({}));

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("use the tool"),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!([
                        {"type": "text", "text": "calling tool"},
                        {"type": "tool_use", "id": "toolu_01", "name": long_tool_name, "input": {}}
                    ]),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_result", "tool_use_id": "toolu_01", "content": "done"}
                    ]),
                },
            ],
            system: None,
            stream: false,
            tools: Some(vec![AnthropicTool {
                name: long_tool_name.to_string(),
                description: "A test tool".to_string(),
                input_schema: schema,
                tool_type: None,
                max_uses: None,
                cache_control: None,
            }]),
            thinking: None,
            tool_choice: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req).unwrap();
        let short_name = result.tool_name_map.iter().next().unwrap().0.clone();

        // 历史中 assistant 消息的 tool_use name 也应该被映射
        let history = &result.conversation_state.history;
        let mut found = false;
        for msg in history {
            if let Message::Assistant(a) = msg {
                if let Some(ref tool_uses) = a.assistant_response_message.tool_uses {
                    for tu in tool_uses {
                        if tu.tool_use_id == "toolu_01" {
                            assert_eq!(tu.name, short_name, "历史中的 tool_use name 应该是短名称");
                            found = true;
                        }
                    }
                }
            }
        }
        assert!(found, "应该在历史中找到 tool_use");
    }

    #[test]
    fn test_history_tools_added_to_tools_list() {
        use super::super::types::Message as AnthropicMessage;

        // 创建一个请求，历史中有工具使用，但 tools 列表为空
        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("Read the file"),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!([
                        {"type": "text", "text": "I'll read the file."},
                        {"type": "tool_use", "id": "tool-1", "name": "read", "input": {"path": "/test.txt"}}
                    ]),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_result", "tool_use_id": "tool-1", "content": "file content"}
                    ]),
                },
            ],
            stream: false,
            system: None,
            tools: None, // 没有提供工具定义
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req).unwrap();

        // 验证 tools 列表中包含了历史中使用的工具的占位符定义
        let tools = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tools;

        assert!(!tools.is_empty(), "tools 列表不应为空");
        assert!(
            tools.iter().any(|t| t.tool_specification.name == "read"),
            "tools 列表应包含 'read' 工具的占位符定义"
        );
    }

    #[test]
    fn test_duplicate_declared_tools_are_deduped_before_kiro_request() {
        use super::super::types::{Message as AnthropicMessage, Tool as AnthropicTool};

        let mut schema = std::collections::HashMap::new();
        schema.insert("type".to_string(), serde_json::json!("object"));
        schema.insert("properties".to_string(), serde_json::json!({}));

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("test"),
            }],
            stream: false,
            system: None,
            tools: Some(vec![
                AnthropicTool {
                    name: "read".to_string(),
                    description: "A test tool".to_string(),
                    input_schema: schema.clone(),
                    tool_type: None,
                    max_uses: None,
                    cache_control: None,
                },
                AnthropicTool {
                    name: "read".to_string(),
                    description: "Duplicate tool".to_string(),
                    input_schema: schema,
                    tool_type: None,
                    max_uses: None,
                    cache_control: None,
                },
            ]),
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req).unwrap();
        let tools = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tools;

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool_specification.name, "read");
    }

    #[test]
    fn current_tool_result_only_message_gets_content_placeholder() {
        use super::super::types::Message as AnthropicMessage;

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("Read the file"),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_use", "id": "tool-1", "name": "read", "input": {"path": "/test.txt"}}
                    ]),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_result", "tool_use_id": "tool-1", "content": "file content"}
                    ]),
                },
            ],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req).unwrap();
        let current = &result.conversation_state.current_message.user_input_message;

        assert_eq!(current.content, TOOL_RESULTS_PROVIDED_PLACEHOLDER);
        assert_eq!(current.user_input_message_context.tool_results.len(), 1);
        assert_eq!(result.warnings.tool_result_content_placeholders, 1);
    }

    #[test]
    fn current_empty_user_message_gets_continue_placeholder() {
        use super::super::types::Message as AnthropicMessage;

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!(""),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req).unwrap();
        let current = &result.conversation_state.current_message.user_input_message;

        assert_eq!(current.content, "Continue");
        assert!(current.user_input_message_context.tool_results.is_empty());
        assert_eq!(result.warnings.empty_content_placeholders, 1);
        assert_eq!(result.warnings.tool_result_content_placeholders, 0);
    }

    #[test]
    fn history_tool_result_only_message_gets_content_placeholder() {
        use super::super::types::Message as AnthropicMessage;

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("Read the file"),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_use", "id": "tool-1", "name": "read", "input": {"path": "/test.txt"}}
                    ]),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_result", "tool_use_id": "tool-1", "content": "file content"}
                    ]),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!("The file contains content."),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("Continue"),
                },
            ],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req).unwrap();
        let tool_result_user = result
            .conversation_state
            .history
            .iter()
            .find_map(|message| match message {
                Message::User(user)
                    if !user
                        .user_input_message
                        .user_input_message_context
                        .tool_results
                        .is_empty() =>
                {
                    Some(&user.user_input_message)
                }
                _ => None,
            })
            .expect("history should contain the tool_result user message");

        assert_eq!(tool_result_user.content, TOOL_RESULTS_PROVIDED_PLACEHOLDER);
        assert_eq!(
            tool_result_user
                .user_input_message_context
                .tool_results
                .len(),
            1
        );
    }

    #[test]
    fn test_extract_session_id_valid() {
        // 测试有效的 user_id 格式
        let user_id = "user_0dede55c6dcc4a11a30bbb5e7f22e6fdf86cdeba3820019cc27612af4e1243cd_account__session_8bb5523b-ec7c-4540-a9ca-beb6d79f1552";
        let session_id = extract_session_id(user_id);
        assert_eq!(
            session_id,
            Some("8bb5523b-ec7c-4540-a9ca-beb6d79f1552".to_string())
        );
    }

    #[test]
    fn test_extract_session_id_json_format() {
        // 测试 JSON 格式的 user_id
        let user_id = r#"{"device_id":"0dede55c6dcc4a11a30bbb5e7f22e6fdf86cdeba3820019cc27612af4e1243cd","account_uuid":"","session_id":"8bb5523b-ec7c-4540-a9ca-beb6d79f1552"}"#;
        let session_id = extract_session_id(user_id);
        assert_eq!(
            session_id,
            Some("8bb5523b-ec7c-4540-a9ca-beb6d79f1552".to_string())
        );
    }

    #[test]
    fn test_extract_session_id_json_invalid_session() {
        // 测试 JSON 格式但 session_id 不是有效 UUID
        let user_id = r#"{"device_id":"abc","session_id":"not-a-uuid"}"#;
        let session_id = extract_session_id(user_id);
        assert_eq!(session_id, None);
    }

    #[test]
    fn test_extract_session_id_no_session() {
        // 测试没有 session 的 user_id
        let user_id = "user_0dede55c6dcc4a11a30bbb5e7f22e6fdf86cdeba3820019cc27612af4e1243cd";
        let session_id = extract_session_id(user_id);
        assert_eq!(session_id, None);
    }

    #[test]
    fn test_extract_session_id_invalid_uuid() {
        // 测试无效的 UUID 格式
        let user_id = "user_xxx_session_invalid-uuid";
        let session_id = extract_session_id(user_id);
        assert_eq!(session_id, None);
    }

    #[test]
    fn test_convert_request_with_session_metadata() {
        use super::super::types::{Message as AnthropicMessage, Metadata};

        // 测试带有 metadata 的请求，应该使用 session UUID 作为 conversationId
        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: Some(Metadata {
                user_id: Some(
                    "user_0dede55c6dcc4a11a30bbb5e7f22e6fdf86cdeba3820019cc27612af4e1243cd_account__session_a0662283-7fd3-4399-a7eb-52b9a717ae88".to_string(),
                ),
            }),
        };

        let result = convert_request(&req).unwrap();
        assert_eq!(
            result.conversation_state.conversation_id,
            "a0662283-7fd3-4399-a7eb-52b9a717ae88"
        );
    }

    #[test]
    fn test_convert_request_without_metadata_is_stable_across_turns() {
        use super::super::types::{Message as AnthropicMessage, SystemMessage};

        let first_req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: Some(vec![SystemMessage {
                text: "You are a helpful coding assistant.".to_string(),
                cache_control: Some(serde_json::json!({"type": "ephemeral"})),
            }]),
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let second_req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("Hello"),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!("Sure."),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("Add tests for it."),
                },
            ],
            stream: false,
            system: Some(vec![SystemMessage {
                text: "You are a helpful coding assistant.".to_string(),
                cache_control: None,
            }]),
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let first_result = convert_request(&first_req).unwrap();
        let second_result = convert_request(&second_req).unwrap();

        assert_eq!(first_result.conversation_state.conversation_id.len(), 36);
        assert_eq!(
            first_result.conversation_state.conversation_id,
            second_result.conversation_state.conversation_id
        );
    }

    #[test]
    fn test_convert_request_without_metadata_is_not_stabilized_when_high_cache_disabled() {
        use super::super::types::{Message as AnthropicMessage, SystemMessage};

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: Some(vec![SystemMessage {
                text: "You are a helpful coding assistant.".to_string(),
                cache_control: Some(serde_json::json!({"type": "ephemeral"})),
            }]),
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let first_result = convert_request_with_options(
            &req,
            ConverterOptions {
                prompt_cache_simulation_mode: PromptCacheSimulationMode::Disabled,
                ..ConverterOptions::default()
            },
        )
        .unwrap();
        let second_result = convert_request_with_options(
            &req,
            ConverterOptions {
                prompt_cache_simulation_mode: PromptCacheSimulationMode::Disabled,
                ..ConverterOptions::default()
            },
        )
        .unwrap();

        assert_ne!(
            first_result.conversation_state.conversation_id,
            second_result.conversation_state.conversation_id
        );
    }

    #[test]
    fn test_anthropic_strict_avoids_chunk_policy_and_thinking_prefix() {
        use super::super::types::{Message as AnthropicMessage, SystemMessage, Thinking};

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: Some(vec![SystemMessage {
                text: "Reply tersely.".to_string(),
                cache_control: None,
            }]),
            tools: None,
            tool_choice: None,
            thinking: Some(Thinking {
                thinking_type: "enabled".to_string(),
                budget_tokens: 20000,
            }),
            output_config: None,
            metadata: None,
        };

        let result = convert_request_with_options(
            &req,
            ConverterOptions {
                compat_profile: CompatProfile::AnthropicStrict,
                ..ConverterOptions::default()
            },
        )
        .unwrap();

        let first_user = result
            .conversation_state
            .history
            .iter()
            .find_map(|message| match message {
                Message::User(user) => Some(&user.user_input_message.content),
                _ => None,
            })
            .expect("system should be represented as first user history message");

        assert_eq!(first_user, "Reply tersely.");
        assert!(!first_user.contains(SYSTEM_CHUNKED_POLICY));
        assert!(!first_user.contains("<thinking_mode>"));
        assert!(!first_user.contains("<thinking_output_policy>"));
    }

    #[test]
    fn test_resolved_base_model_keeps_enabled_thinking_prefix() {
        use crate::anthropic::model_capabilities::ModelResolutionSource;

        use super::super::types::{Message as AnthropicMessage, OutputConfig, Thinking};

        let req = MessagesRequest {
            model: "claude-sonnet-4-6-thinking".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: Some(Thinking {
                thinking_type: "enabled".to_string(),
                budget_tokens: 20000,
            }),
            output_config: None,
            metadata: None,
        };
        let resolution = ModelResolution::resolved(
            "claude-sonnet-4-6-thinking".to_string(),
            "claude-sonnet-4.5".to_string(),
            ModelResolutionSource::FamilyNormalized,
        );

        let result =
            convert_request_with_resolved_model(&req, ConverterOptions::default(), &resolution)
                .expect("thinking request should convert through resolved base model");

        assert_eq!(
            result
                .conversation_state
                .current_message
                .user_input_message
                .model_id,
            "claude-sonnet-4.5"
        );
        let first_history_user = result
            .conversation_state
            .history
            .iter()
            .find_map(|message| match message {
                Message::User(user) => Some(&user.user_input_message),
                _ => None,
            })
            .expect("thinking controls should be injected as synthetic history");

        assert_eq!(first_history_user.model_id, "claude-sonnet-4.5");
        assert!(
            first_history_user
                .content
                .contains("<thinking_mode>enabled</thinking_mode>")
        );
        assert!(
            first_history_user
                .content
                .contains("<max_thinking_length>20000</max_thinking_length>")
        );
        assert!(
            first_history_user
                .content
                .contains("<thinking_output_policy>")
        );

        let adaptive_req = MessagesRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: Some(Thinking {
                thinking_type: "adaptive".to_string(),
                budget_tokens: 0,
            }),
            output_config: Some(OutputConfig {
                effort: "high".to_string(),
            }),
            metadata: None,
        };
        let adaptive_resolution = ModelResolution::resolved(
            "claude-sonnet-4-6".to_string(),
            "claude-sonnet-4.5".to_string(),
            ModelResolutionSource::FamilyNormalized,
        );
        let adaptive_result = convert_request_with_resolved_model(
            &adaptive_req,
            ConverterOptions::default(),
            &adaptive_resolution,
        )
        .expect("adaptive request should convert");
        let adaptive_first_history_user = adaptive_result
            .conversation_state
            .history
            .iter()
            .find_map(|message| match message {
                Message::User(user) => Some(&user.user_input_message),
                _ => None,
            })
            .expect("adaptive thinking controls should be injected");
        assert!(
            adaptive_first_history_user
                .content
                .contains("<thinking_mode>adaptive</thinking_mode>")
        );
        assert!(
            !adaptive_first_history_user
                .content
                .contains("<thinking_output_policy>")
        );
    }

    #[test]
    fn test_native_reasoning_fields_emit_for_supported_models_without_prompt_tags() {
        use super::super::types::{Message as AnthropicMessage, OutputConfig, Thinking};

        let req = MessagesRequest {
            model: "claude-opus-4-7-thinking".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: Some(Thinking {
                thinking_type: "adaptive".to_string(),
                budget_tokens: 0,
            }),
            output_config: Some(OutputConfig {
                effort: "xhigh".to_string(),
            }),
            metadata: None,
        };

        let result = convert_request_with_options(&req, ConverterOptions::default())
            .expect("supported native reasoning request should convert");

        let fields = result
            .additional_model_request_fields
            .expect("supported model should emit native reasoning fields");
        assert!(fields.thinking.is_none());
        assert_eq!(fields.output_config.unwrap().effort, "xhigh");
        assert!(
            result
                .conversation_state
                .history
                .iter()
                .all(|message| match message {
                    Message::User(user) =>
                        !user.user_input_message.content.contains("<thinking_mode>"),
                    _ => true,
                })
        );
    }

    #[test]
    fn test_sonnet_4_6_xhigh_downgrades_to_max_for_native_schema() {
        use super::super::types::{Message as AnthropicMessage, OutputConfig, Thinking};

        let req = MessagesRequest {
            model: "claude-sonnet-4-6-thinking".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: Some(Thinking {
                thinking_type: "adaptive".to_string(),
                budget_tokens: 0,
            }),
            output_config: Some(OutputConfig {
                effort: "xhigh".to_string(),
            }),
            metadata: None,
        };

        let result = convert_request_with_options(&req, ConverterOptions::default())
            .expect("sonnet native reasoning request should convert");

        let fields = result
            .additional_model_request_fields
            .expect("sonnet 4.6 should emit native reasoning fields");
        assert_eq!(fields.output_config.unwrap().effort, "max");
    }

    #[test]
    fn test_force_visible_thinking_adds_policy_for_adaptive_request() {
        use super::super::types::{Message as AnthropicMessage, OutputConfig, Thinking};

        let req = MessagesRequest {
            model: "claude-sonnet-4-5".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: Some(Thinking {
                thinking_type: "adaptive".to_string(),
                budget_tokens: 0,
            }),
            output_config: Some(OutputConfig {
                effort: "high".to_string(),
            }),
            metadata: None,
        };

        let result = convert_request_with_options(
            &req,
            ConverterOptions {
                force_visible_thinking: true,
                ..ConverterOptions::default()
            },
        )
        .expect("adaptive request should convert");
        let first_history_user = result
            .conversation_state
            .history
            .iter()
            .find_map(|message| match message {
                Message::User(user) => Some(&user.user_input_message.content),
                _ => None,
            })
            .expect("thinking controls should be injected");

        assert!(first_history_user.contains("<thinking_mode>adaptive</thinking_mode>"));
        assert!(first_history_user.contains("<thinking_effort>high</thinking_effort>"));
        assert!(first_history_user.contains("<thinking_output_policy>"));
    }

    #[test]
    fn test_force_visible_thinking_overrides_strict_prefix_suppression() {
        use super::super::types::{
            Message as AnthropicMessage, OutputConfig, SystemMessage, Thinking,
        };

        let req = MessagesRequest {
            model: "claude-sonnet-4-5".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: Some(vec![SystemMessage {
                text: "Reply tersely.".to_string(),
                cache_control: None,
            }]),
            tools: None,
            tool_choice: None,
            thinking: Some(Thinking {
                thinking_type: "adaptive".to_string(),
                budget_tokens: 0,
            }),
            output_config: Some(OutputConfig {
                effort: "high".to_string(),
            }),
            metadata: None,
        };

        let result = convert_request_with_options(
            &req,
            ConverterOptions {
                compat_profile: CompatProfile::AnthropicStrict,
                force_visible_thinking: true,
                ..ConverterOptions::default()
            },
        )
        .expect("strict adaptive request should convert with forced visible thinking");
        let first_history_user = result
            .conversation_state
            .history
            .iter()
            .find_map(|message| match message {
                Message::User(user) => Some(&user.user_input_message.content),
                _ => None,
            })
            .expect("system should be represented as first user history message");

        assert!(first_history_user.contains("<thinking_mode>adaptive</thinking_mode>"));
        assert!(first_history_user.contains("<thinking_output_policy>"));
        assert!(first_history_user.contains("Reply tersely."));
        assert!(!first_history_user.contains(SYSTEM_CHUNKED_POLICY));
    }

    #[test]
    fn test_anthropic_strict_drops_prefill_like_claude_code() {
        use super::super::types::Message as AnthropicMessage;

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("Hello"),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!("prefill"),
                },
            ],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request_with_options(
            &req,
            ConverterOptions {
                compat_profile: CompatProfile::AnthropicStrict,
                ..ConverterOptions::default()
            },
        )
        .expect("strict profile should still sanitize terminal prefill");

        assert_eq!(result.warnings.prefill_dropped, 1);
        assert_eq!(
            result
                .conversation_state
                .current_message
                .user_input_message
                .content,
            "Hello"
        );
    }

    fn test_tool(name: &str) -> super::super::types::Tool {
        super::super::types::Tool {
            tool_type: None,
            name: name.to_string(),
            description: format!("{} description", name),
            input_schema: HashMap::from([
                ("type".to_string(), serde_json::json!("object")),
                ("properties".to_string(), serde_json::json!({})),
            ]),
            max_uses: None,
            cache_control: None,
        }
    }

    fn base_tool_choice_request(tool_choice: serde_json::Value) -> MessagesRequest {
        use super::super::types::Message as AnthropicMessage;

        MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("use the appropriate tool"),
            }],
            stream: false,
            system: None,
            tools: Some(vec![test_tool("read_file"), test_tool("write_file")]),
            tool_choice: Some(tool_choice),
            thinking: None,
            output_config: None,
            metadata: None,
        }
    }

    #[test]
    fn test_tool_choice_none_omits_current_tools() {
        let req = base_tool_choice_request(serde_json::json!({"type": "none"}));

        let result = convert_request_with_options(&req, ConverterOptions::default()).unwrap();
        let context = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context;

        assert!(context.tools.is_empty());
        assert!(
            result
                .conversation_state
                .history
                .iter()
                .any(|message| matches!(
                    message,
                    Message::User(user)
                        if user.user_input_message.content.contains("<tool_choice>none</tool_choice>")
                )),
            "compat mode should steer Kiro away from tool calls when tool_choice is none"
        );
    }

    #[test]
    fn test_tool_choice_named_tool_filters_current_tools() {
        let req = base_tool_choice_request(serde_json::json!({
            "type": "tool",
            "name": "read_file"
        }));

        let result = convert_request_with_options(&req, ConverterOptions::default()).unwrap();
        let context = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context;

        assert_eq!(context.tools.len(), 1);
        let kiro_tool_name = &context.tools[0].tool_specification.name;
        assert_eq!(
            result.tool_name_map.get(kiro_tool_name),
            Some(&"read_file".to_string())
        );
        assert!(
            result
                .conversation_state
                .history
                .iter()
                .any(|message| matches!(
                    message,
                    Message::User(user)
                        if user.user_input_message.content.contains("<tool_choice_name>read_file</tool_choice_name>")
                )),
            "compat mode should add a Kiro-facing forced-tool steering prefix"
        );
    }

    #[test]
    fn test_anthropic_strict_filters_tool_choice_without_prompt_steering() {
        let req = base_tool_choice_request(serde_json::json!({
            "type": "tool",
            "name": "read_file"
        }));

        let result = convert_request_with_options(
            &req,
            ConverterOptions {
                compat_profile: CompatProfile::AnthropicStrict,
                ..ConverterOptions::default()
            },
        )
        .unwrap();
        let context = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context;

        assert_eq!(context.tools.len(), 1);
        assert!(
            result.conversation_state.history.is_empty(),
            "strict profile should avoid synthetic prompt steering"
        );
    }

    #[test]
    fn cache_point_disabled_by_default() {
        let mut req = base_tool_choice_request(serde_json::json!({"type": "auto"}));
        if let Some(tools) = req.tools.as_mut() {
            tools[0].cache_control = Some(serde_json::json!({"type": "ephemeral"}));
        }

        let result = convert_request_with_options(&req, ConverterOptions::default()).unwrap();

        assert!(result.tool_cache_point_insert_after.is_empty());
        assert!(result.cache_point_plan_recording_enabled);
    }

    #[test]
    fn cache_point_tools_only_records_selected_tool_indices() {
        let mut req = base_tool_choice_request(serde_json::json!({"type": "auto"}));
        if let Some(tools) = req.tools.as_mut() {
            tools[0].cache_control = Some(serde_json::json!({"type": "ephemeral"}));
            tools[1].cache_control = Some(serde_json::json!({"type": "ephemeral"}));
        }

        let result = convert_request_with_options(
            &req,
            ConverterOptions {
                kiro_cache_point_enabled: true,
                ..ConverterOptions::default()
            },
        )
        .unwrap();

        assert_eq!(result.tool_cache_point_insert_after, vec![0, 1]);
    }

    #[test]
    fn cache_point_respects_tool_choice_filtering() {
        let mut req = base_tool_choice_request(serde_json::json!({
            "type": "tool",
            "name": "write_file"
        }));
        if let Some(tools) = req.tools.as_mut() {
            tools[0].cache_control = Some(serde_json::json!({"type": "ephemeral"}));
            tools[1].cache_control = Some(serde_json::json!({"type": "ephemeral"}));
        }

        let result = convert_request_with_options(
            &req,
            ConverterOptions {
                kiro_cache_point_enabled: true,
                ..ConverterOptions::default()
            },
        )
        .unwrap();

        assert_eq!(result.tool_cache_point_insert_after, vec![0]);
        let context = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context;
        assert_eq!(context.tools.len(), 1);
        assert_eq!(
            result
                .tool_name_map
                .get(&context.tools[0].tool_specification.name),
            Some(&"write_file".to_string())
        );
    }

    #[test]
    fn test_validate_tool_pairing_orphaned_result() {
        // 测试孤立的 tool_result 被过滤
        // 历史中没有 tool_use，但 tool_results 中有 tool_result
        let history = vec![
            Message::User(HistoryUserMessage::new("Hello", "claude-sonnet-4.5")),
            Message::Assistant(HistoryAssistantMessage::new("Hi there!")),
        ];

        let tool_results = vec![ToolResult::success("orphan-123", "some result")];

        let (filtered, _, orphan_texts) =
            validate_tool_pairing(&history, &tool_results, &mut ProxyWarnings::default());

        // 孤立的 tool_result 应该被过滤掉
        assert!(filtered.is_empty(), "孤立的 tool_result 应该被过滤");
        assert_eq!(orphan_texts.len(), 1);
        assert!(orphan_texts[0].contains("some result"));
    }

    #[test]
    fn test_validate_tool_pairing_orphaned_use() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 测试孤立的 tool_use（有 tool_use 但没有对应的 tool_result）
        let mut assistant_msg = AssistantMessage::new("I'll read the file.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-orphan", "read")
                .with_input(serde_json::json!({"path": "/test.txt"})),
        ]);

        let history = vec![
            Message::User(HistoryUserMessage::new(
                "Read the file",
                "claude-sonnet-4.5",
            )),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
        ];

        // 没有 tool_result
        let tool_results: Vec<ToolResult> = vec![];

        let (filtered, orphaned, _) =
            validate_tool_pairing(&history, &tool_results, &mut ProxyWarnings::default());

        // 结果应该为空（因为没有 tool_result）
        // 同时应该返回孤立的 tool_use_id
        assert!(filtered.is_empty());
        assert!(orphaned.contains("tool-orphan"));
    }

    #[test]
    fn test_validate_tool_pairing_valid() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 测试正常配对的情况
        let mut assistant_msg = AssistantMessage::new("I'll read the file.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read")
                .with_input(serde_json::json!({"path": "/test.txt"})),
        ]);

        let history = vec![
            Message::User(HistoryUserMessage::new(
                "Read the file",
                "claude-sonnet-4.5",
            )),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
        ];

        let tool_results = vec![ToolResult::success("tool-1", "file content")];

        let (filtered, orphaned, _) =
            validate_tool_pairing(&history, &tool_results, &mut ProxyWarnings::default());

        // 配对成功，应该保留，无孤立
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].tool_use_id, "tool-1");
        assert!(orphaned.is_empty());
    }

    #[test]
    fn test_validate_tool_pairing_mixed() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 测试混合情况：部分配对成功，部分孤立
        let mut assistant_msg = AssistantMessage::new("I'll use two tools.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read").with_input(serde_json::json!({})),
            ToolUseEntry::new("tool-2", "write").with_input(serde_json::json!({})),
        ]);

        let history = vec![
            Message::User(HistoryUserMessage::new("Do something", "claude-sonnet-4.5")),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
        ];

        // tool_results: tool-1 配对，tool-3 孤立
        let tool_results = vec![
            ToolResult::success("tool-1", "result 1"),
            ToolResult::success("tool-3", "orphan result"), // 孤立
        ];

        let (filtered, orphaned, orphan_texts) =
            validate_tool_pairing(&history, &tool_results, &mut ProxyWarnings::default());

        // 只有 tool-1 应该保留
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].tool_use_id, "tool-1");
        // tool-2 是孤立的 tool_use（无 result），tool-3 是孤立的 tool_result
        assert!(orphaned.contains("tool-2"));
        assert_eq!(orphan_texts.len(), 1);
        assert!(orphan_texts[0].contains("orphan result"));
    }

    #[test]
    fn test_validate_tool_pairing_history_already_paired() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 测试历史中已配对的 tool_use 不应该被报告为孤立
        // 场景：多轮对话中，之前的 tool_use 已经在历史中有对应的 tool_result
        let mut assistant_msg1 = AssistantMessage::new("I'll read the file.");
        assistant_msg1 = assistant_msg1.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read")
                .with_input(serde_json::json!({"path": "/test.txt"})),
        ]);

        // 构建历史中的 user 消息，包含 tool_result
        let mut user_msg_with_result = UserMessage::new("", "claude-sonnet-4.5");
        let mut ctx = UserInputMessageContext::new();
        ctx = ctx.with_tool_results(vec![ToolResult::success("tool-1", "file content")]);
        user_msg_with_result = user_msg_with_result.with_context(ctx);

        let history = vec![
            // 第一轮：用户请求
            Message::User(HistoryUserMessage::new(
                "Read the file",
                "claude-sonnet-4.5",
            )),
            // 第一轮：assistant 使用工具
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg1,
            }),
            // 第二轮：用户返回工具结果（历史中已配对）
            Message::User(HistoryUserMessage {
                user_input_message: user_msg_with_result,
            }),
            // 第二轮：assistant 响应
            Message::Assistant(HistoryAssistantMessage::new("The file contains...")),
        ];

        // 当前消息没有 tool_results（用户只是继续对话）
        let tool_results: Vec<ToolResult> = vec![];

        let (filtered, orphaned, _) =
            validate_tool_pairing(&history, &tool_results, &mut ProxyWarnings::default());

        // 结果应该为空，且不应该有孤立 tool_use
        // 因为 tool-1 已经在历史中配对了
        assert!(filtered.is_empty());
        assert!(orphaned.is_empty());
    }

    #[test]
    fn test_validate_tool_pairing_duplicate_result() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 测试重复的 tool_result（历史中已配对，当前消息又发送了相同的 tool_result）
        let mut assistant_msg = AssistantMessage::new("I'll read the file.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read")
                .with_input(serde_json::json!({"path": "/test.txt"})),
        ]);

        // 历史中已有 tool_result
        let mut user_msg_with_result = UserMessage::new("", "claude-sonnet-4.5");
        let mut ctx = UserInputMessageContext::new();
        ctx = ctx.with_tool_results(vec![ToolResult::success("tool-1", "file content")]);
        user_msg_with_result = user_msg_with_result.with_context(ctx);

        let history = vec![
            Message::User(HistoryUserMessage::new(
                "Read the file",
                "claude-sonnet-4.5",
            )),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
            Message::User(HistoryUserMessage {
                user_input_message: user_msg_with_result,
            }),
            Message::Assistant(HistoryAssistantMessage::new("Done")),
        ];

        // 当前消息又发送了相同的 tool_result（重复）
        let tool_results = vec![ToolResult::success("tool-1", "file content again")];

        let (filtered, _, _) =
            validate_tool_pairing(&history, &tool_results, &mut ProxyWarnings::default());

        // 重复的 tool_result 应该被过滤掉
        assert!(filtered.is_empty(), "重复的 tool_result 应该被过滤");
    }

    #[test]
    fn test_validate_tool_pairing_textifies_duplicate_current_result() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        let mut assistant_msg = AssistantMessage::new("I'll read the file.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read")
                .with_input(serde_json::json!({"path": "/test.txt"})),
        ]);

        let history = vec![
            Message::User(HistoryUserMessage::new(
                "Read the file",
                "claude-sonnet-4.5",
            )),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
        ];

        let tool_results = vec![
            ToolResult::success("tool-1", "first result"),
            ToolResult::success("tool-1", "duplicate result"),
        ];
        let mut warnings = ProxyWarnings::default();

        let (filtered, orphaned, textified) =
            validate_tool_pairing(&history, &tool_results, &mut warnings);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].tool_use_id, "tool-1");
        assert!(orphaned.is_empty());
        assert_eq!(textified.len(), 1);
        assert!(textified[0].contains("duplicate result"));
        assert_eq!(warnings.duplicate_tool_results, 1);
        assert_eq!(warnings.duplicate_tool_results_textified, 1);
    }

    #[test]
    fn test_validate_tool_pairing_allows_current_result_for_reused_last_tool_use_id() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        let mut first_assistant = AssistantMessage::new("First read.");
        first_assistant = first_assistant.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read").with_input(serde_json::json!({"path": "/a"})),
        ]);

        let mut first_result_user = UserMessage::new(" ", "claude-sonnet-4.5");
        let mut first_ctx = UserInputMessageContext::new();
        first_ctx = first_ctx.with_tool_results(vec![ToolResult::success("tool-1", "first")]);
        first_result_user = first_result_user.with_context(first_ctx);

        let mut second_assistant = AssistantMessage::new("Second read.");
        second_assistant = second_assistant.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read").with_input(serde_json::json!({"path": "/b"})),
        ]);

        let history = vec![
            Message::User(HistoryUserMessage::new("Read A", "claude-sonnet-4.5")),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: first_assistant,
            }),
            Message::User(HistoryUserMessage {
                user_input_message: first_result_user,
            }),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: second_assistant,
            }),
        ];

        let tool_results = vec![ToolResult::success("tool-1", "second")];

        let (filtered, orphaned, orphan_texts) =
            validate_tool_pairing(&history, &tool_results, &mut ProxyWarnings::default());

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].tool_use_id, "tool-1");
        assert!(orphaned.is_empty());
        assert!(orphan_texts.is_empty());
    }

    #[test]
    fn test_validate_tool_pairing_textifies_result_for_non_adjacent_tool_use() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        let mut first_assistant = AssistantMessage::new("First read.");
        first_assistant = first_assistant.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read").with_input(serde_json::json!({"path": "/a"})),
        ]);

        let mut first_result_user = UserMessage::new(" ", "claude-sonnet-4.5");
        let mut first_ctx = UserInputMessageContext::new();
        first_ctx = first_ctx.with_tool_results(vec![ToolResult::success("tool-1", "first")]);
        first_result_user = first_result_user.with_context(first_ctx);

        let mut second_assistant = AssistantMessage::new("Second read.");
        second_assistant = second_assistant.with_tool_uses(vec![
            ToolUseEntry::new("tool-2", "read").with_input(serde_json::json!({"path": "/b"})),
        ]);

        let history = vec![
            Message::User(HistoryUserMessage::new("Read A", "claude-sonnet-4.5")),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: first_assistant,
            }),
            Message::User(HistoryUserMessage {
                user_input_message: first_result_user,
            }),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: second_assistant,
            }),
        ];

        let tool_results = vec![ToolResult::success("tool-1", "stale repeat")];

        let (filtered, orphaned, orphan_texts) =
            validate_tool_pairing(&history, &tool_results, &mut ProxyWarnings::default());

        assert!(filtered.is_empty());
        assert!(orphaned.contains("tool-2"));
        assert_eq!(orphan_texts.len(), 1);
        assert!(orphan_texts[0].contains("stale repeat"));
    }

    #[test]
    fn test_convert_assistant_message_tool_use_only() {
        use super::super::types::Message as AnthropicMessage;

        // 测试仅包含 tool_use 的 assistant 消息（无 text 块）
        // Kiro API 要求 content 字段不能为空
        let msg = AnthropicMessage {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "tool_use", "id": "toolu_01ABC", "name": "read_file", "input": {"path": "/test.txt"}}
            ]),
        };

        let mut tool_name_map = HashMap::new();
        let result = convert_assistant_message(&msg, &mut tool_name_map).expect("应该成功转换");

        // 验证 content 不为空（使用占位符）
        assert!(
            !result.assistant_response_message.content.is_empty(),
            "content 不应为空"
        );
        assert_eq!(
            result.assistant_response_message.content, " ",
            "仅 tool_use 时应使用 ' ' 占位符"
        );

        // 验证 tool_uses 被正确保留
        let tool_uses = result
            .assistant_response_message
            .tool_uses
            .expect("应该有 tool_uses");
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].tool_use_id, "toolu_01ABC");
        assert_ne!(tool_uses[0].name, "read_file");
        assert!(
            tool_uses[0]
                .name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric())
        );
        assert_eq!(
            tool_name_map.get(&tool_uses[0].name),
            Some(&"read_file".to_string())
        );
    }

    #[test]
    fn test_convert_assistant_message_ignores_empty_tool_use_identity() {
        let msg = super::super::types::Message {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "tool_use", "id": "   ", "name": "read_file", "input": {"path": "/test.txt"}},
                {"type": "tool_use", "id": "toolu_valid", "name": "   ", "input": {"path": "/test.txt"}},
                {"type": "tool_use", "id": "toolu_ok", "name": "read_file", "input": {"path": "/test.txt"}}
            ]),
        };

        let mut tool_name_map = HashMap::new();
        let result = convert_assistant_message(&msg, &mut tool_name_map).expect("convert");
        let tool_uses = result
            .assistant_response_message
            .tool_uses
            .expect("valid tool use should remain");

        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].tool_use_id, "toolu_ok");
    }

    #[test]
    fn test_tool_use_ids_are_sanitized_consistently() {
        let raw_id = "toolu:01/ABC";
        let sanitized = sanitize_tool_use_id(raw_id).expect("sanitized id");
        assert!(
            sanitized
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        );
        assert_ne!(sanitized, raw_id);

        let assistant = super::super::types::Message {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "tool_use", "id": raw_id, "name": "read_file", "input": {"path": "/test.txt"}}
            ]),
        };
        let user_content = serde_json::json!([
            {"type": "tool_result", "tool_use_id": raw_id, "content": "done"}
        ]);

        let mut tool_name_map = HashMap::new();
        let assistant = convert_assistant_message(&assistant, &mut tool_name_map).expect("convert");
        let (_, _, tool_results) = process_message_content(&user_content).expect("process");

        assert_eq!(
            assistant
                .assistant_response_message
                .tool_uses
                .as_ref()
                .expect("tool use")[0]
                .tool_use_id,
            sanitized
        );
        assert_eq!(tool_results[0].tool_use_id, sanitized);
    }

    #[test]
    fn test_convert_assistant_message_wraps_non_object_tool_input() {
        let msg = super::super::types::Message {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "tool_use", "id": "toolu_scalar", "name": "run", "input": "raw input"}
            ]),
        };

        let mut tool_name_map = HashMap::new();
        let result = convert_assistant_message(&msg, &mut tool_name_map).expect("convert");
        let tool_uses = result
            .assistant_response_message
            .tool_uses
            .expect("tool use should remain");

        assert_eq!(
            tool_uses[0].input,
            serde_json::json!({"value": "raw input"})
        );
    }

    #[test]
    fn test_process_message_content_ignores_empty_tool_result_id() {
        let content = serde_json::json!([
            {"type": "tool_result", "tool_use_id": " ", "content": "ignored"},
            {"type": "tool_result", "tool_use_id": "toolu_ok", "content": "kept"}
        ]);

        let (_, _, tool_results) = process_message_content(&content).expect("process");

        assert_eq!(tool_results.len(), 1);
        assert_eq!(tool_results[0].tool_use_id, "toolu_ok");
    }

    #[test]
    fn test_process_message_content_replaces_empty_tool_result_content() {
        let content = serde_json::json!([
            {"type": "tool_result", "tool_use_id": "toolu_ok", "content": []}
        ]);

        let (_, _, tool_results) = process_message_content(&content).expect("process");
        let text = tool_results[0].content[0]
            .get("text")
            .and_then(|value| value.as_str())
            .expect("tool result text");

        assert_eq!(text, EMPTY_TOOL_RESULT_CONTENT_PLACEHOLDER);
    }

    #[test]
    fn test_process_message_content_preserves_non_text_tool_result_items() {
        let content = serde_json::json!([
            {
                "type": "tool_result",
                "tool_use_id": "toolu_ok",
                "content": [
                    {"type": "text", "text": "plain"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "abc"}}
                ]
            }
        ]);

        let (_, _, tool_results) = process_message_content(&content).expect("process");
        let text = kiro_tool_result_to_text(&tool_results[0]).expect("text");

        assert!(text.contains("plain"));
        assert!(text.contains("\"image\""));
    }

    #[test]
    fn test_base64_image_uses_detected_format_over_declared_media_type() {
        let jpeg = BASE64_STANDARD.encode([0xff, 0xd8, 0xff, 0xdb, 0x00, 0x43, 0x00]);
        let content = serde_json::json!([
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": jpeg
                }
            }
        ]);

        let (_, images, _) = process_message_content(&content).expect("process");

        assert_eq!(images.len(), 1);
        assert_eq!(images[0].format, "jpeg");
    }

    #[test]
    fn test_data_url_image_uses_detected_format_over_declared_media_type() {
        let jpeg = BASE64_STANDARD.encode([0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10]);
        let content = serde_json::json!([
            {
                "type": "image",
                "source": {
                    "type": "url",
                    "url": format!("data:image/png;charset=utf-8;base64,{}", jpeg)
                }
            }
        ]);

        let (_, images, _) = process_message_content(&content).expect("process");

        assert_eq!(images.len(), 1);
        assert_eq!(images[0].format, "jpeg");
    }

    #[test]
    fn test_convert_assistant_message_with_text_and_tool_use() {
        use super::super::types::Message as AnthropicMessage;

        // 测试同时包含 text 和 tool_use 的 assistant 消息
        let msg = AnthropicMessage {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "text", "text": "Let me read that file for you."},
                {"type": "tool_use", "id": "toolu_02XYZ", "name": "read_file", "input": {"path": "/data.json"}}
            ]),
        };

        let mut tool_name_map = HashMap::new();
        let result = convert_assistant_message(&msg, &mut tool_name_map).expect("应该成功转换");

        // 验证 content 使用原始文本（不是占位符）
        assert_eq!(
            result.assistant_response_message.content,
            "Let me read that file for you."
        );

        // 验证 tool_uses 被正确保留
        let tool_uses = result
            .assistant_response_message
            .tool_uses
            .expect("应该有 tool_uses");
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].tool_use_id, "toolu_02XYZ");
        assert_eq!(
            tool_name_map.get(&tool_uses[0].name),
            Some(&"read_file".to_string())
        );
    }

    #[test]
    fn test_remove_orphaned_tool_uses() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 测试从历史中移除孤立的 tool_use
        let mut assistant_msg = AssistantMessage::new("I'll use multiple tools.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read").with_input(serde_json::json!({})),
            ToolUseEntry::new("tool-2", "write").with_input(serde_json::json!({})),
            ToolUseEntry::new("tool-3", "delete").with_input(serde_json::json!({})),
        ]);

        let mut history = vec![
            Message::User(HistoryUserMessage::new("Do something", "claude-sonnet-4.5")),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
        ];

        // 移除 tool-1 和 tool-3
        let mut orphaned = std::collections::HashSet::new();
        orphaned.insert("tool-1".to_string());
        orphaned.insert("tool-3".to_string());

        remove_orphaned_tool_uses(&mut history, &orphaned);

        // 验证只剩下 tool-2
        if let Message::Assistant(ref assistant_msg) = history[1] {
            let tool_uses = assistant_msg
                .assistant_response_message
                .tool_uses
                .as_ref()
                .expect("应该还有 tool_uses");
            assert_eq!(tool_uses.len(), 1);
            assert_eq!(tool_uses[0].tool_use_id, "tool-2");
        } else {
            panic!("应该是 Assistant 消息");
        }
    }

    #[test]
    fn test_remove_orphaned_tool_uses_all_removed() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 测试移除所有 tool_use 后，tool_uses 变为 None
        let mut assistant_msg = AssistantMessage::new("I'll use a tool.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read").with_input(serde_json::json!({})),
        ]);

        let mut history = vec![
            Message::User(HistoryUserMessage::new("Do something", "claude-sonnet-4.5")),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
        ];

        let mut orphaned = std::collections::HashSet::new();
        orphaned.insert("tool-1".to_string());

        remove_orphaned_tool_uses(&mut history, &orphaned);

        // 验证 tool_uses 变为 None
        if let Message::Assistant(ref assistant_msg) = history[1] {
            assert!(
                assistant_msg.assistant_response_message.tool_uses.is_none(),
                "移除所有 tool_use 后应为 None"
            );
        } else {
            panic!("应该是 Assistant 消息");
        }
    }

    #[test]
    fn test_merge_consecutive_assistant_messages() {
        // 测试连续 assistant 消息被正确合并（Issue #79）
        use super::super::types::Message as AnthropicMessage;

        let msg1 = AnthropicMessage {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "thinking", "thinking": "Let me think about this..."},
                {"type": "text", "text": " "}
            ]),
        };

        let msg2 = AnthropicMessage {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "thinking", "thinking": "I should read the file."},
                {"type": "text", "text": "Let me read that file."},
                {"type": "tool_use", "id": "toolu_01ABC", "name": "read_file", "input": {"path": "/test.txt"}}
            ]),
        };

        let messages: Vec<&AnthropicMessage> = vec![&msg1, &msg2];
        let result = merge_assistant_messages(&messages, &mut HashMap::new()).expect("合并应成功");

        let content = &result.assistant_response_message.content;
        assert!(content.contains("<thinking>"), "应包含 thinking 标签");
        assert!(
            content.contains("Let me read that file"),
            "应包含第二条消息的 text 内容"
        );

        let tool_uses = result
            .assistant_response_message
            .tool_uses
            .expect("应有 tool_uses");
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].tool_use_id, "toolu_01ABC");
    }

    #[test]
    fn test_consecutive_assistant_with_tool_use_result_pairing() {
        // 测试 Issue #79 的完整场景
        use super::super::types::Message as AnthropicMessage;

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("Read the config file"),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!([
                        {"type": "thinking", "thinking": "I need to read the file..."},
                        {"type": "text", "text": " "}
                    ]),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!([
                        {"type": "thinking", "thinking": "Let me read the config."},
                        {"type": "text", "text": "I'll read the config file for you."},
                        {"type": "tool_use", "id": "toolu_01XYZ", "name": "read_file", "input": {"path": "/config.json"}}
                    ]),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_result", "tool_use_id": "toolu_01XYZ", "content": "{\"key\": \"value\"}"}
                    ]),
                },
            ],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req);
        assert!(
            result.is_ok(),
            "连续 assistant 消息场景不应报错: {:?}",
            result.err()
        );

        let state = result.unwrap().conversation_state;
        let mut found_tool_use = false;
        for msg in &state.history {
            if let Message::Assistant(assistant_msg) = msg {
                if let Some(ref tool_uses) = assistant_msg.assistant_response_message.tool_uses {
                    if tool_uses.iter().any(|t| t.tool_use_id == "toolu_01XYZ") {
                        found_tool_use = true;
                        break;
                    }
                }
            }
        }
        assert!(found_tool_use, "合并后的 assistant 消息应包含 tool_use");
    }
}
