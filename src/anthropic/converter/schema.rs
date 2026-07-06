//! JSON Schema normalization for Kiro tool definitions.

/// 规范化 JSON Schema，修复 MCP/OpenAPI/Zod 工具定义中常见的兼容性问题。
///
/// 上游按 draft 2020-12 校验工具 `input_schema`，但 Claude Code / MCP 工具定义
/// 经常混入旧 draft、OpenAPI 或简写结构。这里保守清洗成 Kiro/Anthropic 更容易
/// 接受的 JSON Schema 子集，避免单个脏工具 schema 导致整次请求被 400 拒绝。
pub(super) fn normalize_json_schema(schema: serde_json::Value) -> serde_json::Value {
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
