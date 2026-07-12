//! Request-local reversible mapping for tool input schema property keys.

use std::collections::{HashMap, HashSet};

use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::model::config::ToolSchemaKeyMappingMode;

const GENERATED_KEY_HASH_LEN: usize = 16;
const GENERATED_KEY_PREFIX: &str = "key";
const MAX_GENERATION_ATTEMPTS: usize = 128;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ToolSchemaKeyMap {
    by_tool: HashMap<String, HashMap<String, String>>,
}

impl ToolSchemaKeyMap {
    pub(crate) fn len(&self) -> usize {
        self.by_tool.values().map(HashMap::len).sum()
    }

    pub(crate) fn has_tool(&self, tool_name: &str) -> bool {
        self.by_tool
            .get(tool_name)
            .is_some_and(|mapping| !mapping.is_empty())
    }

    pub(crate) fn insert_tool_mapping(
        &mut self,
        tool_name: impl Into<String>,
        mapping: HashMap<String, String>,
    ) {
        if !mapping.is_empty() {
            self.by_tool.insert(tool_name.into(), mapping);
        }
    }

    pub(crate) fn reverse_tool_input(&self, tool_name: &str, input: Value) -> Value {
        let Some(mapping) = self.by_tool.get(tool_name) else {
            return input;
        };
        reverse_value_keys(input, mapping)
    }

    pub(crate) fn reverse_tool_input_json(&self, tool_name: &str, input_json: &str) -> String {
        if input_json.trim().is_empty() || !self.has_tool(tool_name) {
            return input_json.to_string();
        }
        let Ok(input) = serde_json::from_str::<Value>(input_json) else {
            return input_json.to_string();
        };
        let reversed = self.reverse_tool_input(tool_name, input);
        serde_json::to_string(&reversed).unwrap_or_else(|_| input_json.to_string())
    }
}

fn reverse_value_keys(value: Value, mapping: &HashMap<String, String>) -> Value {
    match value {
        Value::Object(obj) => {
            let mut out = serde_json::Map::new();
            for (key, value) in obj {
                let output_key = mapping.get(&key).cloned().unwrap_or(key);
                out.insert(output_key, reverse_value_keys(value, mapping));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| reverse_value_keys(item, mapping))
                .collect(),
        ),
        other => other,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SchemaKeyMappingError {
    InvalidRegex {
        pattern: String,
        error: String,
    },
    InvalidKey {
        tool_name: String,
        path: String,
        key: String,
        pattern: String,
    },
    CannotSanitize {
        tool_name: String,
        path: String,
        key: String,
        pattern: String,
    },
}

impl std::fmt::Display for SchemaKeyMappingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaKeyMappingError::InvalidRegex { pattern, error } => write!(
                f,
                "bodyConversion.toolSchemaKeyValidationRegex is invalid: pattern `{}`: {}",
                pattern, error
            ),
            SchemaKeyMappingError::InvalidKey {
                tool_name,
                path,
                key,
                pattern,
            } => write!(
                f,
                "tool `{}` input_schema property key `{}` at `{}` does not match `{}`",
                tool_name, key, path, pattern
            ),
            SchemaKeyMappingError::CannotSanitize {
                tool_name,
                path,
                key,
                pattern,
            } => write!(
                f,
                "tool `{}` input_schema property key `{}` at `{}` cannot be safely sanitized to match `{}`",
                tool_name, key, path, pattern
            ),
        }
    }
}

impl std::error::Error for SchemaKeyMappingError {}

#[derive(Debug)]
pub(crate) struct SchemaKeyMapper {
    mode: ToolSchemaKeyMappingMode,
    pattern: String,
    regex: Option<Regex>,
}

impl SchemaKeyMapper {
    pub(crate) fn new(
        mode: ToolSchemaKeyMappingMode,
        pattern: impl Into<String>,
    ) -> Result<Self, SchemaKeyMappingError> {
        let pattern = pattern.into();
        let regex = match mode {
            ToolSchemaKeyMappingMode::Disabled => None,
            ToolSchemaKeyMappingMode::Sanitize | ToolSchemaKeyMappingMode::Reject => Some(
                Regex::new(&pattern).map_err(|err| SchemaKeyMappingError::InvalidRegex {
                    pattern: pattern.clone(),
                    error: err.to_string(),
                })?,
            ),
        };
        Ok(Self {
            mode,
            pattern,
            regex,
        })
    }

    pub(crate) fn apply_to_schema(
        &self,
        tool_name: &str,
        schema: &mut Value,
    ) -> Result<HashMap<String, String>, SchemaKeyMappingError> {
        if self.mode == ToolSchemaKeyMappingMode::Disabled {
            return Ok(HashMap::new());
        }
        if self.mode == ToolSchemaKeyMappingMode::Reject {
            if let Some((path, key)) = first_invalid_property_key(schema, self.regex(), "#") {
                return Err(SchemaKeyMappingError::InvalidKey {
                    tool_name: tool_name.to_string(),
                    path,
                    key,
                    pattern: self.pattern.clone(),
                });
            }
            return Ok(HashMap::new());
        }
        let mut reserved = HashSet::new();
        collect_valid_property_keys(schema, self.regex(), &mut reserved);
        let mut mapping = HashMap::new();
        sanitize_schema_value(tool_name, "#", schema, self, &mut reserved, &mut mapping)?;
        Ok(mapping)
    }

    fn regex(&self) -> &Regex {
        self.regex
            .as_ref()
            .expect("regex exists outside disabled mode")
    }

    fn is_valid_key(&self, key: &str) -> bool {
        self.regex().is_match(key)
    }

    fn sanitize_key(
        &self,
        tool_name: &str,
        path: &str,
        key: &str,
        reserved: &mut HashSet<String>,
    ) -> Result<String, SchemaKeyMappingError> {
        match self.mode {
            ToolSchemaKeyMappingMode::Disabled => Ok(key.to_string()),
            ToolSchemaKeyMappingMode::Reject => Err(SchemaKeyMappingError::InvalidKey {
                tool_name: tool_name.to_string(),
                path: path.to_string(),
                key: key.to_string(),
                pattern: self.pattern.clone(),
            }),
            ToolSchemaKeyMappingMode::Sanitize => {
                for attempt in 0..MAX_GENERATION_ATTEMPTS {
                    let candidate = generated_key(tool_name, path, key, attempt);
                    if self.is_valid_key(&candidate) && !reserved.contains(&candidate) {
                        reserved.insert(candidate.clone());
                        return Ok(candidate);
                    }
                }
                Err(SchemaKeyMappingError::CannotSanitize {
                    tool_name: tool_name.to_string(),
                    path: path.to_string(),
                    key: key.to_string(),
                    pattern: self.pattern.clone(),
                })
            }
        }
    }
}

fn generated_key(tool_name: &str, path: &str, key: &str, attempt: usize) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"kiro.rs:tool-schema-key:v1\0");
    hasher.update(tool_name.as_bytes());
    hasher.update(b"\0");
    hasher.update(path.as_bytes());
    hasher.update(b"\0");
    hasher.update(key.as_bytes());
    hasher.update(b"\0");
    hasher.update(attempt.to_string().as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    format!(
        "{}{}",
        GENERATED_KEY_PREFIX,
        &hash[..GENERATED_KEY_HASH_LEN]
    )
}

fn collect_valid_property_keys(value: &Value, regex: &Regex, reserved: &mut HashSet<String>) {
    match value {
        Value::Object(obj) => {
            if let Some(Value::Object(properties)) = obj.get("properties") {
                for key in properties.keys() {
                    if regex.is_match(key) {
                        reserved.insert(key.clone());
                    }
                }
                for child in properties.values() {
                    collect_valid_property_keys(child, regex, reserved);
                }
            }
            for key in SCHEMA_MAP_KEYWORDS {
                if let Some(Value::Object(map)) = obj.get(*key) {
                    for child in map.values() {
                        collect_valid_property_keys(child, regex, reserved);
                    }
                }
            }
            for key in SCHEMA_VALUE_KEYWORDS {
                if let Some(child) = obj.get(*key) {
                    collect_valid_property_keys(child, regex, reserved);
                }
            }
            for key in SCHEMA_ARRAY_KEYWORDS {
                if let Some(Value::Array(items)) = obj.get(*key) {
                    for child in items {
                        collect_valid_property_keys(child, regex, reserved);
                    }
                }
            }
            if let Some(Value::Object(dependencies)) = obj.get("dependencies") {
                for child in dependencies.values() {
                    if child.is_object() || child.is_boolean() {
                        collect_valid_property_keys(child, regex, reserved);
                    }
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_valid_property_keys(item, regex, reserved);
            }
        }
        _ => {}
    }
}

fn first_invalid_property_key(
    value: &Value,
    regex: &Regex,
    path: &str,
) -> Option<(String, String)> {
    match value {
        Value::Object(obj) => {
            if let Some(Value::Object(properties)) = obj.get("properties") {
                for key in properties.keys() {
                    if !regex.is_match(key) {
                        return Some((
                            format!("{}/properties/{}", path, escape_path_segment(key)),
                            key.clone(),
                        ));
                    }
                }
                for (key, child) in properties {
                    let child_path = format!("{}/properties/{}", path, escape_path_segment(key));
                    if let Some(found) = first_invalid_property_key(child, regex, &child_path) {
                        return Some(found);
                    }
                }
            }
            for key in SCHEMA_MAP_KEYWORDS {
                if let Some(Value::Object(map)) = obj.get(*key) {
                    for (child_key, child) in map {
                        let child_path = format!("{}/{}", path, escape_path_segment(child_key));
                        if let Some(found) = first_invalid_property_key(child, regex, &child_path) {
                            return Some(found);
                        }
                    }
                }
            }
            for key in SCHEMA_VALUE_KEYWORDS {
                if let Some(child) = obj.get(*key) {
                    let child_path = format!("{}/{}", path, key);
                    if let Some(found) = first_invalid_property_key(child, regex, &child_path) {
                        return Some(found);
                    }
                }
            }
            for key in SCHEMA_ARRAY_KEYWORDS {
                if let Some(Value::Array(items)) = obj.get(*key) {
                    for (idx, child) in items.iter().enumerate() {
                        let child_path = format!("{}/{}/{}", path, key, idx);
                        if let Some(found) = first_invalid_property_key(child, regex, &child_path) {
                            return Some(found);
                        }
                    }
                }
            }
            if let Some(Value::Object(dependencies)) = obj.get("dependencies") {
                for (key, child) in dependencies {
                    if child.is_object() || child.is_boolean() {
                        let child_path =
                            format!("{}/dependencies/{}", path, escape_path_segment(key));
                        if let Some(found) = first_invalid_property_key(child, regex, &child_path) {
                            return Some(found);
                        }
                    }
                }
            }
            None
        }
        Value::Array(items) => {
            for (idx, item) in items.iter().enumerate() {
                let child_path = format!("{}/{}", path, idx);
                if let Some(found) = first_invalid_property_key(item, regex, &child_path) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

const SCHEMA_MAP_KEYWORDS: &[&str] = &["$defs", "patternProperties", "dependentSchemas"];
const SCHEMA_VALUE_KEYWORDS: &[&str] = &[
    "items",
    "contains",
    "not",
    "if",
    "then",
    "else",
    "propertyNames",
    "contentSchema",
];
const SCHEMA_ARRAY_KEYWORDS: &[&str] = &["prefixItems", "oneOf", "anyOf", "allOf"];

fn sanitize_schema_value(
    tool_name: &str,
    path: &str,
    value: &mut Value,
    mapper: &SchemaKeyMapper,
    reserved: &mut HashSet<String>,
    mapping: &mut HashMap<String, String>,
) -> Result<(), SchemaKeyMappingError> {
    match value {
        Value::Object(obj) => {
            let scope_map = sanitize_properties(tool_name, path, obj, mapper, reserved, mapping)?;
            rewrite_required(obj, &scope_map);
            rewrite_dependent_required(obj, &scope_map);
            rewrite_dependent_schema_keys(obj, &scope_map);
            rewrite_dependencies(obj, &scope_map);

            for key in SCHEMA_MAP_KEYWORDS {
                if let Some(Value::Object(map)) = obj.get_mut(*key) {
                    for (child_key, child) in map.iter_mut() {
                        let child_path = format!("{}/{}", path, escape_path_segment(child_key));
                        sanitize_schema_value(
                            tool_name,
                            &child_path,
                            child,
                            mapper,
                            reserved,
                            mapping,
                        )?;
                    }
                }
            }
            for key in SCHEMA_VALUE_KEYWORDS {
                if let Some(child) = obj.get_mut(*key) {
                    let child_path = format!("{}/{}", path, key);
                    sanitize_schema_value(
                        tool_name,
                        &child_path,
                        child,
                        mapper,
                        reserved,
                        mapping,
                    )?;
                }
            }
            for key in SCHEMA_ARRAY_KEYWORDS {
                if let Some(Value::Array(items)) = obj.get_mut(*key) {
                    for (idx, child) in items.iter_mut().enumerate() {
                        let child_path = format!("{}/{}/{}", path, key, idx);
                        sanitize_schema_value(
                            tool_name,
                            &child_path,
                            child,
                            mapper,
                            reserved,
                            mapping,
                        )?;
                    }
                }
            }
        }
        Value::Array(items) => {
            for (idx, item) in items.iter_mut().enumerate() {
                let child_path = format!("{}/{}", path, idx);
                sanitize_schema_value(tool_name, &child_path, item, mapper, reserved, mapping)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn sanitize_properties(
    tool_name: &str,
    path: &str,
    obj: &mut serde_json::Map<String, Value>,
    mapper: &SchemaKeyMapper,
    reserved: &mut HashSet<String>,
    mapping: &mut HashMap<String, String>,
) -> Result<HashMap<String, String>, SchemaKeyMappingError> {
    let Some(Value::Object(properties)) = obj.get_mut("properties") else {
        return Ok(HashMap::new());
    };

    let old = std::mem::take(properties);
    let mut new_properties = serde_json::Map::new();
    let mut scope_map = HashMap::new();
    for (key, mut schema) in old {
        let property_path = format!("{}/properties/{}", path, escape_path_segment(&key));
        let output_key = if mapper.is_valid_key(&key) {
            key.clone()
        } else {
            let sanitized = mapper.sanitize_key(tool_name, &property_path, &key, reserved)?;
            mapping.insert(sanitized.clone(), key.clone());
            scope_map.insert(key.clone(), sanitized.clone());
            sanitized
        };
        sanitize_schema_value(
            tool_name,
            &property_path,
            &mut schema,
            mapper,
            reserved,
            mapping,
        )?;
        new_properties.insert(output_key, schema);
    }

    *properties = new_properties;
    Ok(scope_map)
}

fn rewrite_required(obj: &mut serde_json::Map<String, Value>, scope_map: &HashMap<String, String>) {
    if scope_map.is_empty() {
        return;
    }
    let Some(Value::Array(items)) = obj.get_mut("required") else {
        return;
    };
    for item in items {
        let Some(name) = item.as_str() else {
            continue;
        };
        if let Some(mapped) = scope_map.get(name) {
            *item = Value::String(mapped.clone());
        }
    }
}

fn rewrite_dependent_required(
    obj: &mut serde_json::Map<String, Value>,
    scope_map: &HashMap<String, String>,
) {
    if scope_map.is_empty() {
        return;
    }
    let Some(Value::Object(map)) = obj.get_mut("dependentRequired") else {
        return;
    };
    let old = std::mem::take(map);
    let mut out = serde_json::Map::new();
    for (key, value) in old {
        let output_key = scope_map.get(&key).cloned().unwrap_or(key);
        let value = rewrite_property_name_array(value, scope_map);
        out.insert(output_key, value);
    }
    *map = out;
}

fn rewrite_dependent_schema_keys(
    obj: &mut serde_json::Map<String, Value>,
    scope_map: &HashMap<String, String>,
) {
    if scope_map.is_empty() {
        return;
    }
    let Some(Value::Object(map)) = obj.get_mut("dependentSchemas") else {
        return;
    };
    let old = std::mem::take(map);
    let mut out = serde_json::Map::new();
    for (key, value) in old {
        let output_key = scope_map.get(&key).cloned().unwrap_or(key);
        out.insert(output_key, value);
    }
    *map = out;
}

fn rewrite_dependencies(
    obj: &mut serde_json::Map<String, Value>,
    scope_map: &HashMap<String, String>,
) {
    if scope_map.is_empty() {
        return;
    }
    let Some(Value::Object(map)) = obj.get_mut("dependencies") else {
        return;
    };
    let old = std::mem::take(map);
    let mut out = serde_json::Map::new();
    for (key, value) in old {
        let output_key = scope_map.get(&key).cloned().unwrap_or(key);
        let value = rewrite_property_name_array(value, scope_map);
        out.insert(output_key, value);
    }
    *map = out;
}

fn rewrite_property_name_array(value: Value, scope_map: &HashMap<String, String>) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| {
                    item.as_str()
                        .and_then(|name| scope_map.get(name))
                        .map(|mapped| Value::String(mapped.clone()))
                        .unwrap_or(item)
                })
                .collect(),
        ),
        other => other,
    }
}

fn escape_path_segment(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::config::DEFAULT_TOOL_SCHEMA_KEY_VALIDATION_REGEX;

    fn mapper(mode: ToolSchemaKeyMappingMode) -> SchemaKeyMapper {
        SchemaKeyMapper::new(mode, DEFAULT_TOOL_SCHEMA_KEY_VALIDATION_REGEX).unwrap()
    }

    #[test]
    fn sanitizes_only_invalid_keys_and_reverses_recursively() {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "valid_key": {"type": "string"},
                "bad key": {
                    "type": "object",
                    "properties": {
                        "nested/key": {"type": "string"}
                    },
                    "required": ["nested/key"]
                }
            },
            "required": ["valid_key", "bad key"],
            "dependentRequired": {
                "bad key": ["valid_key"]
            }
        });

        let mapping = mapper(ToolSchemaKeyMappingMode::Sanitize)
            .apply_to_schema("probe", &mut schema)
            .unwrap();
        assert_eq!(mapping.len(), 2);
        assert!(
            schema["properties"]
                .as_object()
                .unwrap()
                .contains_key("valid_key")
        );
        assert!(
            !schema["properties"]
                .as_object()
                .unwrap()
                .contains_key("bad key")
        );

        let bad_key = mapping
            .iter()
            .find_map(|(sanitized, original)| (original == "bad key").then_some(sanitized))
            .unwrap();
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&Value::String(bad_key.clone()))
        );

        let mut tool_map = ToolSchemaKeyMap::default();
        tool_map.insert_tool_mapping("probe", mapping.clone());
        let nested_key = mapping
            .iter()
            .find_map(|(sanitized, original)| (original == "nested/key").then_some(sanitized))
            .unwrap();
        let input = serde_json::json!({
            "valid_key": "kept",
            bad_key: {
                nested_key: "restored"
            }
        });
        let restored = tool_map.reverse_tool_input("probe", input);
        assert_eq!(restored["valid_key"], "kept");
        assert_eq!(restored["bad key"]["nested/key"], "restored");
    }

    #[test]
    fn valid_keys_do_not_create_mapping() {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "valid_key": {"type": "string"},
                "valid-key": {"type": "string"},
                "valid.key": {"type": "string"}
            }
        });
        let mapping = mapper(ToolSchemaKeyMappingMode::Sanitize)
            .apply_to_schema("probe", &mut schema)
            .unwrap();
        assert!(mapping.is_empty());
        assert!(
            schema["properties"]
                .as_object()
                .unwrap()
                .contains_key("valid_key")
        );
        assert!(
            schema["properties"]
                .as_object()
                .unwrap()
                .contains_key("valid-key")
        );
        assert!(
            schema["properties"]
                .as_object()
                .unwrap()
                .contains_key("valid.key")
        );
    }

    #[test]
    fn reject_mode_errors_without_sanitizing() {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "bad key": {"type": "string"}
            }
        });
        let err = mapper(ToolSchemaKeyMappingMode::Reject)
            .apply_to_schema("probe", &mut schema)
            .unwrap_err();
        assert!(err.to_string().contains("bad key"));
        assert!(
            schema["properties"]
                .as_object()
                .unwrap()
                .contains_key("bad key")
        );
    }

    #[test]
    fn disabled_mode_does_not_compile_or_change_schema() {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "bad key": {"type": "string"}
            }
        });
        let mapper = SchemaKeyMapper::new(ToolSchemaKeyMappingMode::Disabled, "[").unwrap();
        let mapping = mapper.apply_to_schema("probe", &mut schema).unwrap();
        assert!(mapping.is_empty());
        assert!(
            schema["properties"]
                .as_object()
                .unwrap()
                .contains_key("bad key")
        );
    }

    #[test]
    fn generated_keys_are_hash_only_and_avoid_sibling_and_global_collisions() {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "a b": {"type": "string"},
                "a/b": {"type": "string"},
                "key0000000000000000": {"type": "string"}
            }
        });
        let mapping = mapper(ToolSchemaKeyMappingMode::Sanitize)
            .apply_to_schema("probe", &mut schema)
            .unwrap();
        assert_eq!(mapping.len(), 2);
        let sanitized = mapping.keys().collect::<HashSet<_>>();
        assert_eq!(sanitized.len(), 2);
        assert!(sanitized.iter().all(|key| {
            key.starts_with("key")
                && key.len() == "key".len() + GENERATED_KEY_HASH_LEN
                && key["key".len()..]
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
        }));
        assert!(!sanitized.contains(&"key0000000000000000".to_string()));
    }

    #[test]
    fn mappings_are_tool_scoped() {
        let mut tool_map = ToolSchemaKeyMap::default();
        tool_map.insert_tool_mapping(
            "tool_a",
            HashMap::from([("same__k_aaa".to_string(), "a key".to_string())]),
        );
        tool_map.insert_tool_mapping(
            "tool_b",
            HashMap::from([("same__k_aaa".to_string(), "b key".to_string())]),
        );

        assert_eq!(
            tool_map.reverse_tool_input("tool_a", serde_json::json!({"same__k_aaa": 1})),
            serde_json::json!({"a key": 1})
        );
        assert_eq!(
            tool_map.reverse_tool_input("tool_b", serde_json::json!({"same__k_aaa": 1})),
            serde_json::json!({"b key": 1})
        );
    }
}
