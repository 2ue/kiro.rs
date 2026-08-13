//! Lightweight facts extracted from Anthropic Messages request bodies.
//!
//! This module is intentionally narrower than full request parsing. Raw external
//! passthrough paths use it to inspect or patch top-level fields without
//! deserializing `messages`, images, tool results, or other heavy body content.

use bytes::Bytes;
use serde::{
    Deserialize,
    de::{DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use std::{borrow::Cow, collections::HashSet, fmt};

use super::request_body::MAX_MESSAGES_BODY_SIZE;

pub(crate) const MAX_MESSAGES_JSON_NESTING_DEPTH: usize = 192;
pub(crate) const MAX_MESSAGES_MODEL_ID_BYTES: usize = 160;
const MAX_RAW_BODY_PROBE_WORK_UNITS: usize = MAX_MESSAGES_BODY_SIZE * 2;
const MAX_REASONING_PROTOCOL_FIELD_BYTES: usize = 16 * 1024;
const MAX_REASONING_PROTOCOL_SCAN_WORK_UNITS: usize = MAX_REASONING_PROTOCOL_FIELD_BYTES * 2;
const MAX_TOP_LEVEL_PROBED_KEY_BYTES: usize = "output_config".len();
const MAX_JSON_ASCII_ESCAPE_BYTES: usize = 6;
const MAX_TOP_LEVEL_PROBED_KEY_ENCODED_BYTES: usize =
    MAX_TOP_LEVEL_PROBED_KEY_BYTES * MAX_JSON_ASCII_ESCAPE_BYTES + 2;
const MAX_MODEL_ID_ENCODED_BYTES: usize =
    MAX_MESSAGES_MODEL_ID_BYTES * MAX_JSON_ASCII_ESCAPE_BYTES + 2;
const MAX_JSON_OBJECT_KEYS: usize = 16_384;
const MAX_JSON_DOCUMENT_KEYS: usize = 131_072;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RawMessagesBodyProbeError {
    BodyTooLarge,
    ModelTooLong,
    WorkLimitExceeded,
    NestingTooDeep,
    DuplicateObjectKey,
    MalformedTopLevelJson,
}

impl fmt::Display for RawMessagesBodyProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BodyTooLarge => write!(
                formatter,
                "JSON body exceeds the {} byte probe limit",
                MAX_MESSAGES_BODY_SIZE
            ),
            Self::ModelTooLong => write!(
                formatter,
                "model exceeds the {} byte limit",
                MAX_MESSAGES_MODEL_ID_BYTES
            ),
            Self::WorkLimitExceeded => write!(
                formatter,
                "JSON body exceeds the {} work unit probe limit",
                MAX_RAW_BODY_PROBE_WORK_UNITS
            ),
            Self::NestingTooDeep => write!(
                formatter,
                "JSON body nesting exceeds the supported limit of {}",
                MAX_MESSAGES_JSON_NESTING_DEPTH
            ),
            Self::DuplicateObjectKey => {
                formatter.write_str("JSON body contains a repeated object field")
            }
            Self::MalformedTopLevelJson => {
                formatter.write_str("JSON body is not a complete top-level object")
            }
        }
    }
}

#[derive(Clone, Default)]
struct RawRequestBodySnapshot(Bytes);

impl RawRequestBodySnapshot {
    fn new(body: &Bytes) -> Self {
        Self(body.clone())
    }

    fn matches(&self, body: &Bytes) -> bool {
        self.0.as_ptr() == body.as_ptr() && self.0.len() == body.len()
    }
}

impl fmt::Debug for RawRequestBodySnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RawRequestBodySnapshot")
            .field("len", &self.0.len())
            .field("address", &(self.0.as_ptr() as usize))
            .finish()
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct RawMessagesBodyProbe {
    pub(crate) model: Option<String>,
    pub(crate) stream: Option<bool>,
    pub(crate) max_tokens_present: bool,
    pub(crate) max_tokens: Option<i64>,
    pub(crate) complete_top_level_object: bool,
    object_start_index: Option<usize>,
    model_value_span: Option<std::ops::Range<usize>>,
    thinking_value_span: Option<std::ops::Range<usize>>,
    output_config_value_span: Option<std::ops::Range<usize>>,
    duplicate_thinking_field: bool,
    duplicate_output_config_field: bool,
    model_field_seen: bool,
    stream_field_seen: bool,
    max_tokens_field_seen: bool,
    thinking_field_seen: bool,
    output_config_field_seen: bool,
    duplicate_model_field: bool,
    duplicate_stream_field: bool,
    duplicate_max_tokens_field: bool,
    object_end_index: Option<usize>,
    scan_error: Option<RawMessagesBodyProbeError>,
    body_snapshot: RawRequestBodySnapshot,
    scan_work_units: usize,
    max_nesting_depth: usize,
}

impl RawMessagesBodyProbe {
    pub(crate) fn matches_body(&self, raw_body: &Bytes) -> bool {
        self.body_snapshot.matches(raw_body)
    }

    pub(crate) fn scan_error(&self) -> Option<RawMessagesBodyProbeError> {
        self.scan_error
    }

    #[cfg(test)]
    pub(crate) fn scan_work_units(&self) -> usize {
        self.scan_work_units
    }

    #[cfg(test)]
    pub(crate) fn max_nesting_depth(&self) -> usize {
        self.max_nesting_depth
    }
}

#[cfg(test)]
thread_local! {
    static RAW_BODY_PROBE_INVOCATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn raw_body_probe_invocations_for_current_thread() -> usize {
    RAW_BODY_PROBE_INVOCATIONS.get()
}

pub(crate) fn probe_raw_messages_body(raw_body: &Bytes) -> RawMessagesBodyProbe {
    #[cfg(test)]
    RAW_BODY_PROBE_INVOCATIONS.set(RAW_BODY_PROBE_INVOCATIONS.get().saturating_add(1));
    scan_raw_top_level_messages_body(raw_body)
}

#[cfg(test)]
pub(crate) fn raw_messages_body_hints(raw_body: &Bytes) -> (Option<String>, Option<bool>) {
    let probe = probe_raw_messages_body(raw_body);
    (probe.model, probe.stream)
}

pub(crate) fn deserialize_messages_request_with_probe(
    raw_body: &Bytes,
    probe: &RawMessagesBodyProbe,
) -> Result<super::types::MessagesRequest, String> {
    if !probe.matches_body(raw_body) {
        return Err("raw request probe does not match the request body snapshot".to_string());
    }
    if let Some(error) = probe.scan_error() {
        return Err(error.to_string());
    }

    let mut deserializer = serde_json::Deserializer::from_slice(raw_body);
    deserializer.disable_recursion_limit();
    let payload = super::types::MessagesRequest::deserialize(&mut deserializer)
        .map_err(|error| format!("Invalid JSON body: {error}"))?;
    deserializer
        .end()
        .map_err(|error| format!("Invalid JSON body: {error}"))?;
    Ok(payload)
}

/// Validate the two small top-level reasoning controls before raw external
/// passthrough. The request bytes are never rewritten or fully materialized as
/// a JSON DOM, so a valid raw route remains byte-identical.
#[cfg(test)]
pub(crate) fn validate_raw_reasoning_protocol(raw_body: &Bytes) -> Result<(), String> {
    let probe = probe_raw_messages_body(raw_body);
    validate_raw_reasoning_protocol_with_probe(raw_body, &probe)
}

pub(crate) fn validate_raw_reasoning_protocol_with_probe(
    raw_body: &Bytes,
    probe: &RawMessagesBodyProbe,
) -> Result<(), String> {
    if !probe.matches_body(raw_body) {
        return Err("raw request probe does not match the request body snapshot".to_string());
    }
    if let Some(error) = probe.scan_error() {
        return Err(error.to_string());
    }
    if probe.duplicate_model_field
        || probe.duplicate_stream_field
        || probe.duplicate_max_tokens_field
        || probe.duplicate_thinking_field
        || probe.duplicate_output_config_field
    {
        return Err(
            "model, stream, max_tokens, thinking, and output_config must not be repeated"
                .to_string(),
        );
    }

    let thinking = parse_bounded_protocol_value(raw_body, probe.thinking_value_span.as_ref())?;
    let output_config =
        parse_bounded_protocol_value(raw_body, probe.output_config_value_span.as_ref())?;
    for (name, span) in [
        ("thinking", probe.thinking_value_span.as_ref()),
        ("output_config", probe.output_config_value_span.as_ref()),
    ] {
        if let Some(span) = span {
            if json_object_has_duplicate_keys(&raw_body[span.clone()])? {
                return Err(format!("{name} must not contain repeated fields"));
            }
        }
    }

    match output_config.as_ref() {
        None | Some(serde_json::Value::Null) => {}
        Some(serde_json::Value::Object(object)) => {
            let effort = object
                .get("effort")
                .map(|value| {
                    value
                        .as_str()
                        .ok_or_else(|| "output_config.effort must be a string".to_string())
                })
                .transpose()?;
            if effort.is_some_and(|effort| {
                crate::anthropic::types::parse_thinking_effort(effort).is_none()
            }) {
                return Err(format!(
                    "output_config.effort must be one of: {}",
                    crate::anthropic::types::THINKING_EFFORT_VALUES.join(", ")
                ));
            }
        }
        Some(_) => return Err("output_config must be an object".to_string()),
    }

    match thinking.as_ref() {
        None | Some(serde_json::Value::Null) => {}
        Some(serde_json::Value::Object(object)) => {
            let thinking_type = object
                .get("type")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "thinking.type is required and must be a string".to_string())?;
            if !matches!(thinking_type, "enabled" | "adaptive" | "disabled") {
                return Err("thinking.type must be one of: enabled, adaptive, disabled".to_string());
            }
            match thinking_type {
                "enabled" => {
                    let budget = object
                        .get("budget_tokens")
                        .map(|value| {
                            value.as_i64().ok_or_else(|| {
                                "thinking.budget_tokens must be an integer".to_string()
                            })
                        })
                        .transpose()?
                        .ok_or_else(|| {
                            "thinking.budget_tokens is required when thinking.type is enabled"
                                .to_string()
                        })?;
                    if !(1_024..=i32::MAX as i64).contains(&budget) {
                        return Err("thinking.budget_tokens must be between 1024 and 2147483647"
                            .to_string());
                    }
                    if probe
                        .max_tokens
                        .is_some_and(|max_tokens| budget >= max_tokens)
                    {
                        return Err(
                            "thinking.budget_tokens must be less than max_tokens".to_string()
                        );
                    }
                }
                "adaptive" | "disabled" if object.contains_key("budget_tokens") => {
                    return Err(format!(
                        "thinking.budget_tokens is not valid when thinking.type is {thinking_type}"
                    ));
                }
                _ => {}
            }
        }
        Some(_) => return Err("thinking must be an object".to_string()),
    };

    Ok(())
}

fn parse_bounded_protocol_value(
    raw_body: &Bytes,
    span: Option<&std::ops::Range<usize>>,
) -> Result<Option<serde_json::Value>, String> {
    let Some(span) = span else {
        return Ok(None);
    };
    let len = span.end.saturating_sub(span.start);
    if len > MAX_REASONING_PROTOCOL_FIELD_BYTES {
        return Err(format!(
            "thinking/output_config exceeds the {} byte protocol field limit",
            MAX_REASONING_PROTOCOL_FIELD_BYTES
        ));
    }
    serde_json::from_slice(&raw_body[span.clone()])
        .map(Some)
        .map_err(|error| format!("invalid thinking/output_config JSON: {error}"))
}

fn json_object_has_duplicate_keys(bytes: &[u8]) -> Result<bool, String> {
    let mut budget = JsonScanBudget::new(MAX_REASONING_PROTOCOL_SCAN_WORK_UNITS);
    let mut i = skip_json_ws(bytes, 0, &mut budget)
        .map_err(|_| "reasoning control object scan exceeded its work limit".to_string())?;
    if bytes.get(i) != Some(&b'{') {
        return Ok(false);
    }
    budget
        .consume(1)
        .map_err(|_| "reasoning control object scan exceeded its work limit".to_string())?;
    budget
        .observe_depth(1)
        .map_err(|_| "reasoning control object nesting is too deep".to_string())?;
    i += 1;
    let mut seen = HashSet::new();
    loop {
        i = skip_json_ws(bytes, i, &mut budget)
            .map_err(|_| "reasoning control object scan exceeded its work limit".to_string())?;
        if bytes.get(i) == Some(&b'}') {
            return Ok(false);
        }
        let (key, key_end) = parse_json_string_at(bytes, i, &mut budget)
            .map_err(|_| "invalid reasoning control object key".to_string())?;
        if !seen.insert(key) {
            return Ok(true);
        }
        i = skip_json_ws(bytes, key_end, &mut budget)
            .map_err(|_| "reasoning control object scan exceeded its work limit".to_string())?;
        if bytes.get(i) != Some(&b':') {
            return Err("invalid reasoning control object".to_string());
        }
        budget
            .consume(1)
            .map_err(|_| "reasoning control object scan exceeded its work limit".to_string())?;
        i = skip_json_ws(bytes, i + 1, &mut budget)
            .map_err(|_| "reasoning control object scan exceeded its work limit".to_string())?;
        let value_end = skip_json_value(bytes, i, 1, &mut budget)
            .map_err(|_| "invalid reasoning control object value".to_string())?;
        i = skip_json_ws(bytes, value_end, &mut budget)
            .map_err(|_| "reasoning control object scan exceeded its work limit".to_string())?;
        match bytes.get(i) {
            Some(b',') => {
                budget.consume(1).map_err(|_| {
                    "reasoning control object scan exceeded its work limit".to_string()
                })?;
                i += 1;
            }
            Some(b'}') => return Ok(false),
            _ => return Err("invalid reasoning control object".to_string()),
        }
    }
}

pub(crate) fn rewrite_raw_missing_top_level_max_tokens_with_probe(
    raw_body: &Bytes,
    probe: &RawMessagesBodyProbe,
    default_value: i32,
) -> Result<Option<Bytes>, String> {
    if !probe.matches_body(raw_body) {
        return Err("raw request probe does not match the request body snapshot".to_string());
    }
    if let Some(error) = probe.scan_error() {
        return Err(error.to_string());
    }
    if probe.max_tokens_present {
        return Ok(None);
    }
    if !probe.complete_top_level_object {
        return Ok(None);
    }
    let Some(object_start) = probe.object_start_index else {
        return Ok(None);
    };
    let Some(object_end) = probe.object_end_index else {
        return Ok(None);
    };
    let has_existing_fields = raw_body[object_start + 1..object_end]
        .iter()
        .any(|byte| !byte.is_ascii_whitespace());
    let field = if has_existing_fields {
        format!(r#","max_tokens":{}"#, default_value)
    } else {
        format!(r#""max_tokens":{}"#, default_value)
    };
    let mut out = Vec::with_capacity(raw_body.len().saturating_add(field.len()));
    out.extend_from_slice(&raw_body[..object_end]);
    out.extend_from_slice(field.as_bytes());
    out.extend_from_slice(&raw_body[object_end..]);
    Ok(Some(Bytes::from(out)))
}

#[cfg(test)]
pub(crate) fn rewrite_raw_top_level_model(raw_body: &Bytes, model: &str) -> Result<Bytes, String> {
    let probe = probe_raw_messages_body(raw_body);
    rewrite_raw_top_level_model_with_probe(raw_body, &probe, model)
}

pub(crate) fn rewrite_raw_top_level_model_with_probe(
    raw_body: &Bytes,
    probe: &RawMessagesBodyProbe,
    model: &str,
) -> Result<Bytes, String> {
    if !probe.matches_body(raw_body) {
        return Err("raw request probe does not match the request body snapshot".to_string());
    }
    if let Some(error) = probe.scan_error() {
        return Err(error.to_string());
    }
    if probe.duplicate_model_field {
        return Err("top-level model field must not be repeated".to_string());
    }
    let Some(span) = probe.model_value_span.as_ref() else {
        return Err("top-level model field was not found".to_string());
    };
    if probe.model.as_deref() == Some(model) {
        return Ok(raw_body.clone());
    }
    let encoded_model = serde_json::to_string(model).map_err(|err| err.to_string())?;
    let mut out = Vec::with_capacity(
        raw_body
            .len()
            .saturating_sub(span.end.saturating_sub(span.start))
            .saturating_add(encoded_model.len()),
    );
    out.extend_from_slice(&raw_body[..span.start]);
    out.extend_from_slice(encoded_model.as_bytes());
    out.extend_from_slice(&raw_body[span.end..]);
    Ok(Bytes::from(out))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsonScanError {
    ModelTooLong,
    WorkLimitExceeded,
    NestingTooDeep,
    DuplicateObjectKey,
    Malformed,
}

impl From<JsonScanError> for RawMessagesBodyProbeError {
    fn from(error: JsonScanError) -> Self {
        match error {
            JsonScanError::ModelTooLong => Self::ModelTooLong,
            JsonScanError::WorkLimitExceeded => Self::WorkLimitExceeded,
            JsonScanError::NestingTooDeep => Self::NestingTooDeep,
            JsonScanError::DuplicateObjectKey => Self::DuplicateObjectKey,
            JsonScanError::Malformed => Self::MalformedTopLevelJson,
        }
    }
}

struct JsonScanBudget {
    remaining: usize,
    consumed: usize,
    max_depth: usize,
}

impl JsonScanBudget {
    fn new(work_limit: usize) -> Self {
        Self {
            remaining: work_limit,
            consumed: 0,
            max_depth: 0,
        }
    }

    fn consume(&mut self, amount: usize) -> Result<(), JsonScanError> {
        self.remaining = self
            .remaining
            .checked_sub(amount)
            .ok_or(JsonScanError::WorkLimitExceeded)?;
        self.consumed = self.consumed.saturating_add(amount);
        Ok(())
    }

    fn observe_depth(&mut self, depth: usize) -> Result<(), JsonScanError> {
        if depth > MAX_MESSAGES_JSON_NESTING_DEPTH {
            return Err(JsonScanError::NestingTooDeep);
        }
        self.max_depth = self.max_depth.max(depth);
        Ok(())
    }
}

fn scan_raw_top_level_messages_body(raw_body: &Bytes) -> RawMessagesBodyProbe {
    let bytes = raw_body.as_ref();
    let mut probe = RawMessagesBodyProbe {
        body_snapshot: RawRequestBodySnapshot::new(raw_body),
        ..RawMessagesBodyProbe::default()
    };
    if bytes.len() > MAX_MESSAGES_BODY_SIZE {
        probe.scan_error = Some(RawMessagesBodyProbeError::BodyTooLarge);
        return probe;
    }

    let mut budget = JsonScanBudget::new(MAX_RAW_BODY_PROBE_WORK_UNITS);
    let result = scan_top_level_object(bytes, &mut probe, &mut budget)
        .and_then(|()| validate_complete_json_document(bytes));
    probe.scan_work_units = budget.consumed;
    probe.max_nesting_depth = budget.max_depth;
    if let Err(error) = result {
        probe.scan_error = Some(error.into());
    } else if !probe.complete_top_level_object {
        probe.scan_error = Some(RawMessagesBodyProbeError::MalformedTopLevelJson);
    }
    probe
}

fn validate_complete_json_document(bytes: &[u8]) -> Result<(), JsonScanError> {
    let mut validation = JsonDocumentValidation::default();
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    deserializer.disable_recursion_limit();
    let result = JsonDocumentSeed {
        validation: &mut validation,
    }
    .deserialize(&mut deserializer)
    .and_then(|()| deserializer.end());
    match validation.failure {
        Some(JsonDocumentValidationFailure::DuplicateObjectKey) => {
            Err(JsonScanError::DuplicateObjectKey)
        }
        Some(JsonDocumentValidationFailure::KeyLimitExceeded) => {
            Err(JsonScanError::WorkLimitExceeded)
        }
        None => result.map_err(|_| JsonScanError::Malformed),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsonDocumentValidationFailure {
    DuplicateObjectKey,
    KeyLimitExceeded,
}

#[derive(Default)]
struct JsonDocumentValidation {
    total_keys: usize,
    failure: Option<JsonDocumentValidationFailure>,
}

struct JsonDocumentSeed<'a> {
    validation: &'a mut JsonDocumentValidation,
}

impl<'de> DeserializeSeed<'de> for JsonDocumentSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(JsonDocumentVisitor {
            validation: self.validation,
        })
    }
}

struct JsonDocumentVisitor<'a> {
    validation: &'a mut JsonDocumentValidation,
}

struct JsonObjectKeySeed;

impl<'de> DeserializeSeed<'de> for JsonObjectKeySeed {
    type Value = Cow<'de, str>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_str(JsonObjectKeyVisitor)
    }
}

struct JsonObjectKeyVisitor;

impl<'de> Visitor<'de> for JsonObjectKeyVisitor {
    type Value = Cow<'de, str>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON object field name")
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E> {
        Ok(Cow::Borrowed(value))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Cow::Owned(value.to_string()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(Cow::Owned(value))
    }
}

impl<'de> Visitor<'de> for JsonDocumentVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded JSON value without repeated object fields")
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence
            .next_element_seed(JsonDocumentSeed {
                validation: &mut *self.validation,
            })?
            .is_some()
        {}
        Ok(())
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut seen = HashSet::new();
        while let Some(key) = map.next_key_seed(JsonObjectKeySeed)? {
            self.validation.total_keys = self.validation.total_keys.saturating_add(1);
            if self.validation.total_keys > MAX_JSON_DOCUMENT_KEYS
                || seen.len() >= MAX_JSON_OBJECT_KEYS
            {
                self.validation.failure = Some(JsonDocumentValidationFailure::KeyLimitExceeded);
                return Err(serde::de::Error::custom(
                    "JSON object key inspection exceeded its resource limit",
                ));
            }
            if !seen.insert(key) {
                self.validation.failure = Some(JsonDocumentValidationFailure::DuplicateObjectKey);
                return Err(serde::de::Error::custom("repeated JSON object field"));
            }
            map.next_value_seed(JsonDocumentSeed {
                validation: &mut *self.validation,
            })?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbedTopLevelKey {
    Model,
    Stream,
    MaxTokens,
    Thinking,
    OutputConfig,
    Other,
}

fn scan_top_level_object(
    bytes: &[u8],
    probe: &mut RawMessagesBodyProbe,
    budget: &mut JsonScanBudget,
) -> Result<(), JsonScanError> {
    let mut i = skip_json_ws(bytes, 0, budget)?;
    if bytes.get(i) != Some(&b'{') {
        return Err(JsonScanError::Malformed);
    }
    budget.consume(1)?;
    budget.observe_depth(1)?;
    probe.object_start_index = Some(i);
    i += 1;

    loop {
        i = skip_json_ws(bytes, i, budget)?;
        if bytes.get(i) == Some(&b'}') {
            budget.consume(1)?;
            let object_end = i;
            i = skip_json_ws(bytes, i + 1, budget)?;
            if i != bytes.len() {
                return Err(JsonScanError::Malformed);
            }
            probe.complete_top_level_object = true;
            probe.object_end_index = Some(object_end);
            return Ok(());
        }
        if bytes.get(i) != Some(&b'"') {
            return Err(JsonScanError::Malformed);
        }

        let (key, key_end) = parse_probed_top_level_key_at(bytes, i, budget)?;
        i = skip_json_ws(bytes, key_end, budget)?;
        if bytes.get(i) != Some(&b':') {
            return Err(JsonScanError::Malformed);
        }
        budget.consume(1)?;
        i = skip_json_ws(bytes, i + 1, budget)?;
        let value_start = i;

        if key == ProbedTopLevelKey::Model {
            probe.duplicate_model_field |= probe.model_field_seen;
            probe.model_field_seen = true;
            if bytes.get(value_start) == Some(&b'"') {
                let (model, value_end) = parse_bounded_model_string_at(bytes, value_start, budget)?;
                probe.model = Some(model);
                probe.model_value_span = Some(value_start..value_end);
                i = value_end;
            } else {
                i = skip_json_value(bytes, value_start, 1, budget)?;
            }
        } else if key == ProbedTopLevelKey::Stream {
            probe.duplicate_stream_field |= probe.stream_field_seen;
            probe.stream_field_seen = true;
            if bytes.get(value_start..value_start.saturating_add(4)) == Some(b"true") {
                budget.consume(4)?;
                probe.stream = Some(true);
                i = value_start + 4;
            } else if bytes.get(value_start..value_start.saturating_add(5)) == Some(b"false") {
                budget.consume(5)?;
                probe.stream = Some(false);
                i = value_start + 5;
            } else {
                i = skip_json_value(bytes, value_start, 1, budget)?;
            }
        } else if key == ProbedTopLevelKey::MaxTokens {
            probe.duplicate_max_tokens_field |= probe.max_tokens_field_seen;
            probe.max_tokens_field_seen = true;
            probe.max_tokens_present = true;
            let value_end = skip_json_value(bytes, value_start, 1, budget)?;
            probe.max_tokens = serde_json::from_slice::<i64>(&bytes[value_start..value_end]).ok();
            i = value_end;
        } else if matches!(
            key,
            ProbedTopLevelKey::Thinking | ProbedTopLevelKey::OutputConfig
        ) {
            let value_end = skip_json_value(bytes, value_start, 1, budget)?;
            let span = value_start..value_end;
            if key == ProbedTopLevelKey::Thinking {
                probe.duplicate_thinking_field |= probe.thinking_field_seen;
                probe.thinking_field_seen = true;
                probe.thinking_value_span = Some(span);
            } else {
                probe.duplicate_output_config_field |= probe.output_config_field_seen;
                probe.output_config_field_seen = true;
                probe.output_config_value_span = Some(span);
            }
            i = value_end;
        } else {
            i = skip_json_value(bytes, value_start, 1, budget)?;
        }

        i = skip_json_ws(bytes, i, budget)?;
        match bytes.get(i) {
            Some(b',') => {
                budget.consume(1)?;
                i += 1;
            }
            Some(b'}') => {
                budget.consume(1)?;
                let object_end = i;
                i = skip_json_ws(bytes, i + 1, budget)?;
                if i != bytes.len() {
                    return Err(JsonScanError::Malformed);
                }
                probe.complete_top_level_object = true;
                probe.object_end_index = Some(object_end);
                return Ok(());
            }
            _ => return Err(JsonScanError::Malformed),
        }
    }
}

fn parse_probed_top_level_key_at(
    bytes: &[u8],
    start: usize,
    budget: &mut JsonScanBudget,
) -> Result<(ProbedTopLevelKey, usize), JsonScanError> {
    let end = skip_json_string(bytes, start, budget)?;
    let encoded_len = end.saturating_sub(start);
    if encoded_len > MAX_TOP_LEVEL_PROBED_KEY_ENCODED_BYTES {
        return Ok((ProbedTopLevelKey::Other, end));
    }

    budget.consume(encoded_len)?;
    let key = serde_json::from_slice::<String>(&bytes[start..end])
        .map_err(|_| JsonScanError::Malformed)?;
    let key = match key.as_str() {
        "model" => ProbedTopLevelKey::Model,
        "stream" => ProbedTopLevelKey::Stream,
        "max_tokens" => ProbedTopLevelKey::MaxTokens,
        "thinking" => ProbedTopLevelKey::Thinking,
        "output_config" => ProbedTopLevelKey::OutputConfig,
        _ => ProbedTopLevelKey::Other,
    };
    Ok((key, end))
}

fn parse_bounded_model_string_at(
    bytes: &[u8],
    start: usize,
    budget: &mut JsonScanBudget,
) -> Result<(String, usize), JsonScanError> {
    let end = skip_json_string(bytes, start, budget)?;
    let encoded_len = end.saturating_sub(start);
    if encoded_len > MAX_MODEL_ID_ENCODED_BYTES {
        return Err(JsonScanError::ModelTooLong);
    }

    budget.consume(encoded_len)?;
    let model = serde_json::from_slice::<String>(&bytes[start..end])
        .map_err(|_| JsonScanError::Malformed)?;
    if model.len() > MAX_MESSAGES_MODEL_ID_BYTES {
        return Err(JsonScanError::ModelTooLong);
    }
    Ok((model, end))
}

fn skip_json_ws(
    bytes: &[u8],
    mut i: usize,
    budget: &mut JsonScanBudget,
) -> Result<usize, JsonScanError> {
    while bytes.get(i).is_some_and(|byte| byte.is_ascii_whitespace()) {
        budget.consume(1)?;
        i += 1;
    }
    Ok(i)
}

fn parse_json_string_at(
    bytes: &[u8],
    start: usize,
    budget: &mut JsonScanBudget,
) -> Result<(String, usize), JsonScanError> {
    let end = skip_json_string(bytes, start, budget)?;
    // JSON decoding is a second bounded pass over this small key/model span.
    budget.consume(end.saturating_sub(start))?;
    let value = serde_json::from_slice::<String>(&bytes[start..end])
        .map_err(|_| JsonScanError::Malformed)?;
    Ok((value, end))
}

fn skip_json_string(
    bytes: &[u8],
    start: usize,
    budget: &mut JsonScanBudget,
) -> Result<usize, JsonScanError> {
    if bytes.get(start) != Some(&b'"') {
        return Err(JsonScanError::Malformed);
    }
    budget.consume(1)?;
    let mut i = start + 1;
    let mut escaped = false;
    while let Some(byte) = bytes.get(i).copied() {
        budget.consume(1)?;
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            return Ok(i + 1);
        }
        i += 1;
    }
    Err(JsonScanError::Malformed)
}

fn skip_json_value(
    bytes: &[u8],
    start: usize,
    parent_depth: usize,
    budget: &mut JsonScanBudget,
) -> Result<usize, JsonScanError> {
    let mut i = skip_json_ws(bytes, start, budget)?;
    match bytes.get(i).copied().ok_or(JsonScanError::Malformed)? {
        b'"' => skip_json_string(bytes, i, budget),
        b'{' | b'[' => skip_json_container(bytes, i, parent_depth, budget),
        b't' if bytes.get(i..i + 4) == Some(b"true") => {
            budget.consume(4)?;
            Ok(i + 4)
        }
        b'f' if bytes.get(i..i + 5) == Some(b"false") => {
            budget.consume(5)?;
            Ok(i + 5)
        }
        b'n' if bytes.get(i..i + 4) == Some(b"null") => {
            budget.consume(4)?;
            Ok(i + 4)
        }
        b'-' | b'0'..=b'9' => {
            while bytes.get(i).is_some_and(|byte| {
                !matches!(byte, b',' | b'}' | b']') && !byte.is_ascii_whitespace()
            }) {
                budget.consume(1)?;
                i += 1;
            }
            Ok(i)
        }
        _ => Err(JsonScanError::Malformed),
    }
}

fn skip_json_container(
    bytes: &[u8],
    start: usize,
    parent_depth: usize,
    budget: &mut JsonScanBudget,
) -> Result<usize, JsonScanError> {
    let first = bytes.get(start).copied().ok_or(JsonScanError::Malformed)?;
    if !matches!(first, b'{' | b'[') {
        return Err(JsonScanError::Malformed);
    }

    let mut stack = [0u8; MAX_MESSAGES_JSON_NESTING_DEPTH];
    let mut stack_len = 1usize;
    stack[0] = first;
    budget.observe_depth(parent_depth.saturating_add(stack_len))?;
    budget.consume(1)?;
    let mut i = start + 1;
    while let Some(byte) = bytes.get(i).copied() {
        match byte {
            b'"' => {
                i = skip_json_string(bytes, i, budget)?;
                continue;
            }
            b'{' | b'[' => {
                if parent_depth.saturating_add(stack_len).saturating_add(1)
                    > MAX_MESSAGES_JSON_NESTING_DEPTH
                {
                    return Err(JsonScanError::NestingTooDeep);
                }
                stack[stack_len] = byte;
                stack_len += 1;
                budget.observe_depth(parent_depth.saturating_add(stack_len))?;
            }
            b'}' => {
                if stack_len == 0 || stack[stack_len - 1] != b'{' {
                    return Err(JsonScanError::Malformed);
                }
                stack_len -= 1;
            }
            b']' => {
                if stack_len == 0 || stack[stack_len - 1] != b'[' {
                    return Err(JsonScanError::Malformed);
                }
                stack_len -= 1;
            }
            _ => {}
        }
        budget.consume(1)?;
        i += 1;
        if stack_len == 0 {
            return Ok(i);
        }
    }
    Err(JsonScanError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn raw_hints_ignore_nested_model_without_top_level_model() {
        let raw = Bytes::from(
            json!({
                "messages": [
                    {
                        "role": "user",
                        "content": [{"type": "text", "model": "nested"}]
                    }
                ],
                "stream": true
            })
            .to_string(),
        );

        let (model, stream) = raw_messages_body_hints(&raw);

        assert_eq!(model, None);
        assert_eq!(stream, Some(true));
    }

    #[test]
    fn raw_top_level_model_rewrite_preserves_nested_content() {
        let raw = Bytes::from_static(
            br#"{"messages":[{"role":"user","content":[{"type":"text","text":"model old"}]}],"model":"old","stream":false}"#,
        );

        let rewritten = rewrite_raw_top_level_model(&raw, "new").expect("rewrite");
        let value: serde_json::Value = serde_json::from_slice(&rewritten).expect("json");

        assert_eq!(value["model"], "new");
        assert_eq!(value["messages"][0]["content"][0]["text"], "model old");
        assert_eq!(value["stream"], false);
    }

    #[test]
    fn raw_probe_detects_missing_max_tokens_and_rewrite_inserts_field() {
        let raw = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5","messages":[{"role":"user","content":"hi"}],"stream":true}"#,
        );

        let probe = probe_raw_messages_body(&raw);
        assert_eq!(probe.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(probe.stream, Some(true));
        assert!(!probe.max_tokens_present);
        assert!(probe.complete_top_level_object);

        let rewritten = rewrite_raw_missing_top_level_max_tokens_with_probe(&raw, &probe, 4096)
            .expect("rewrite")
            .expect("missing max tokens");
        let value: serde_json::Value = serde_json::from_slice(&rewritten).expect("json");
        assert_eq!(value["max_tokens"], 4096);
        assert_eq!(value["messages"][0]["content"], "hi");
    }

    #[test]
    fn raw_probe_does_not_rewrite_when_max_tokens_exists_and_rejects_incomplete_json() {
        let raw = Bytes::from_static(br#"{"model":"m","max_tokens":16,"messages":[]}"#);
        let probe = probe_raw_messages_body(&raw);
        assert!(
            rewrite_raw_missing_top_level_max_tokens_with_probe(&raw, &probe, 4096)
                .expect("probe")
                .is_none()
        );

        let incomplete = Bytes::from_static(br#"{"model":"m","messages":[]"#);
        let probe = probe_raw_messages_body(&incomplete);
        assert!(!probe.complete_top_level_object);
        assert_eq!(
            probe.scan_error(),
            Some(RawMessagesBodyProbeError::MalformedTopLevelJson)
        );
        assert!(
            rewrite_raw_missing_top_level_max_tokens_with_probe(&incomplete, &probe, 4096).is_err()
        );
    }

    #[test]
    fn raw_missing_max_tokens_rewrite_handles_whitespace_around_object() {
        let raw = Bytes::from_static(br#"  { "model":"m","messages":[] }  "#);
        let probe = probe_raw_messages_body(&raw);

        let rewritten = rewrite_raw_missing_top_level_max_tokens_with_probe(&raw, &probe, 4096)
            .expect("rewrite")
            .expect("missing max tokens");
        let value: serde_json::Value = serde_json::from_slice(&rewritten).expect("json");

        assert_eq!(value["max_tokens"], 4096);
        assert_eq!(value["model"], "m");
    }

    #[test]
    fn raw_reasoning_protocol_accepts_supported_forms_without_changing_bytes_for_five_rounds() {
        let fixtures = [
            br#"{"model":"m","thinking":{"type":"adaptive"},"output_config":{"effort":"max"},"messages":[]}"#.as_slice(),
            br#"{"model":"m","max_tokens":128000,"thinking":{"type":"enabled","budget_tokens":65536},"messages":[]}"#.as_slice(),
            br#"{"model":"m","thinking":{"type":"disabled"},"messages":[]}"#.as_slice(),
            br#"{"model":"m","thinking":{"type":"disabled"},"output_config":{},"messages":[]}"#.as_slice(),
            br#"{"model":"m","thinking":{"type":"disabled"},"output_config":{"effort":"high"},"messages":[]}"#.as_slice(),
            br#"{"model":"m","thinking":{"type":"disabled"},"output_config":{"effort":"max"},"messages":[]}"#.as_slice(),
            br#"{"model":"m","max_tokens":8192,"thinking":{"type":"enabled","budget_tokens":4096},"output_config":{"effort":"high"},"messages":[]}"#.as_slice(),
            br#"{"model":"m","max_tokens":8192,"thinking":{"type":"enabled","budget_tokens":4096},"output_config":{},"messages":[]}"#.as_slice(),
            br#"{"model":"m","output_config":{"effort":"xhigh"},"messages":[]}"#.as_slice(),
            br#" { "model":"m", "output_config":{}, "messages":[] } "#.as_slice(),
        ];

        for round in 0..5 {
            for fixture in fixtures {
                let body = Bytes::copy_from_slice(fixture);
                let before = body.clone();
                validate_raw_reasoning_protocol(&body)
                    .unwrap_or_else(|error| panic!("round {round}: {error}"));
                assert_eq!(body, before, "round {round}: validation must be read-only");
            }
        }
    }

    #[test]
    fn raw_reasoning_protocol_rejects_ambiguous_or_invalid_forms_for_five_rounds() {
        let fixtures = [
            br#"{"thinking":{"type":"mystery"}}"#.as_slice(),
            br#"{"thinking":{"type":"adaptive","budget_tokens":4096}}"#.as_slice(),
            br#"{"thinking":{"type":"enabled","budget_tokens":1023}}"#.as_slice(),
            br#"{"thinking":{"type":"enabled"}}"#.as_slice(),
            br#"{"max_tokens":4096,"thinking":{"type":"enabled","budget_tokens":4096}}"#.as_slice(),
            br#"{"output_config":{"effort":"MAX"}}"#.as_slice(),
            br#"{"output_config":{"effort":" max "}}"#.as_slice(),
            br#"{"output_config":{"effort":"unknown"}}"#.as_slice(),
            br#"{"output_config":{"effort":1}}"#.as_slice(),
            br#"{"thinking":{"type":"adaptive"},"thinking":{"type":"adaptive"}}"#.as_slice(),
            br#"{"output_config":{},"output_config":{}}"#.as_slice(),
            br#"{"model":"first","model":"second"}"#.as_slice(),
            br#"{"model":"first","mo\u0064el":"second"}"#.as_slice(),
            br#"{"stream":false,"stream":true}"#.as_slice(),
            br#"{"max_tokens":4096,"max_tokens":8192}"#.as_slice(),
            br#"{"thinking":{"type":"adaptive","type":"disabled"}}"#.as_slice(),
            br#"{"thinking":{"type":"enabled","budget_tokens":2048,"budget_tokens":4096}}"#
                .as_slice(),
            br#"{"output_config":{"effort":"high","effort":"max"}}"#.as_slice(),
        ];

        for round in 0..5 {
            for fixture in fixtures {
                let error = validate_raw_reasoning_protocol(&Bytes::copy_from_slice(fixture))
                    .expect_err("invalid reasoning protocol must fail closed");
                assert!(!error.is_empty(), "round {round}");
            }
        }
    }

    #[test]
    fn raw_reasoning_protocol_rejects_oversized_extension_without_materializing_it() {
        let padding = "x".repeat(MAX_REASONING_PROTOCOL_FIELD_BYTES + 1);
        let body = Bytes::from(format!(
            r#"{{"output_config":{{"effort":"high","padding":"{padding}"}}}}"#
        ));

        let error = validate_raw_reasoning_protocol(&body).expect_err("oversized field");
        assert!(error.contains("protocol field limit"));
    }

    #[test]
    fn raw_probe_enforces_fixed_depth_before_typed_parse_for_five_rounds() {
        fn nested_body(container_depth: usize) -> Bytes {
            let prefix = br#"{"model":"m","max_tokens":8,"messages":"#;
            let suffix = br#"}"#;
            let mut body =
                Vec::with_capacity(prefix.len() + container_depth * 2 + suffix.len() + 1);
            body.extend_from_slice(prefix);
            body.extend(std::iter::repeat_n(b'[', container_depth));
            body.push(b'0');
            body.extend(std::iter::repeat_n(b']', container_depth));
            body.extend_from_slice(suffix);
            Bytes::from(body)
        }

        let accepted = nested_body(MAX_MESSAGES_JSON_NESTING_DEPTH - 1);
        let rejected = nested_body(MAX_MESSAGES_JSON_NESTING_DEPTH);
        for round in 0..5 {
            let accepted_probe = probe_raw_messages_body(&accepted);
            assert_eq!(accepted_probe.scan_error(), None, "round {round}");
            assert_eq!(
                accepted_probe.max_nesting_depth(),
                MAX_MESSAGES_JSON_NESTING_DEPTH,
                "round {round}"
            );

            let rejected_probe = probe_raw_messages_body(&rejected);
            assert_eq!(
                rejected_probe.scan_error(),
                Some(RawMessagesBodyProbeError::NestingTooDeep),
                "round {round}"
            );
        }
    }

    #[test]
    fn raw_probe_size_and_linear_work_matrix_preserves_bytes_for_five_rounds() {
        let prefix = br#"{"model":"m","max_tokens":8,"messages":[],"padding":""#;
        let suffix = br#""}"#;
        for target_size in [
            1_024usize,
            1024 * 1024,
            5 * 1024 * 1024,
            MAX_MESSAGES_BODY_SIZE - 1,
        ] {
            let mut body = Vec::with_capacity(target_size);
            body.extend_from_slice(prefix);
            body.resize(target_size - suffix.len(), b'x');
            body.extend_from_slice(suffix);
            let body = Bytes::from(body);
            let before = body.clone();

            for round in 0..5 {
                let probe = probe_raw_messages_body(&body);
                assert_eq!(
                    probe.scan_error(),
                    None,
                    "size {target_size}, round {round}"
                );
                assert!(
                    probe.scan_work_units() <= body.len().saturating_mul(2),
                    "size {target_size}, round {round}: {} work units",
                    probe.scan_work_units()
                );
                assert_eq!(body, before, "size {target_size}, round {round}");
            }
        }
    }

    #[test]
    fn raw_probe_binding_and_noop_model_rewrite_are_zero_copy_for_five_rounds() {
        let body =
            Bytes::from_static(br#"{"model":"same","max_tokens":8,"messages":[],"stream":false}"#);
        let probe = probe_raw_messages_body(&body);
        let copied_body = Bytes::copy_from_slice(&body);

        for round in 0..5 {
            let rewritten = rewrite_raw_top_level_model_with_probe(&body, &probe, "same")
                .expect("no-op rewrite");
            assert_eq!(rewritten.as_ptr(), body.as_ptr(), "round {round}");
            assert!(rewrite_raw_top_level_model_with_probe(&copied_body, &probe, "same").is_err());
        }
    }

    #[test]
    fn raw_probe_rejects_all_escaped_duplicate_critical_keys_for_five_rounds() {
        let fixtures = [
            br#"{"model":"a","mo\u0064el":"b"}"#.as_slice(),
            br#"{"stream":false,"str\u0065am":true}"#.as_slice(),
            br#"{"max_tokens":8,"max_tok\u0065ns":9}"#.as_slice(),
            br#"{"thinking":{"type":"adaptive"},"think\u0069ng":{"type":"adaptive"}}"#.as_slice(),
            br#"{"output_config":{},"output_confi\u0067":{}}"#.as_slice(),
        ];
        for round in 0..5 {
            for fixture in fixtures {
                let error = validate_raw_reasoning_protocol(&Bytes::copy_from_slice(fixture))
                    .expect_err("escaped duplicate must reject");
                assert!(
                    error.contains("repeated object field"),
                    "round {round}: {error}"
                );
            }
        }
    }

    fn body_at_absolute_json_depth(depth: usize) -> Bytes {
        assert!(depth >= 1);
        let nested_containers = depth - 1;
        let prefix = br#"{"model":"m","max_tokens":8,"messages":[],"future":"#;
        let mut body = Vec::with_capacity(prefix.len() + nested_containers * 2 + 2);
        body.extend_from_slice(prefix);
        body.extend(std::iter::repeat_n(b'[', nested_containers));
        body.push(b'0');
        body.extend(std::iter::repeat_n(b']', nested_containers));
        body.push(b'}');
        Bytes::from(body)
    }

    #[test]
    fn raw_probe_and_unbounded_serde_share_the_192_level_contract_for_five_rounds() {
        for round in 0..5 {
            for depth in [127usize, 128, 129, 191, 192] {
                let body = body_at_absolute_json_depth(depth);
                let probe = probe_raw_messages_body(&body);
                assert_eq!(probe.scan_error(), None, "round {round}, depth {depth}");
                assert_eq!(
                    probe.max_nesting_depth(),
                    depth,
                    "round {round}, depth {depth}"
                );
                let payload = deserialize_messages_request_with_probe(&body, &probe)
                    .unwrap_or_else(|error| panic!("round {round}, depth {depth}: {error}"));
                assert_eq!(payload.model, "m");
            }

            let body = body_at_absolute_json_depth(193);
            let probe = probe_raw_messages_body(&body);
            assert_eq!(
                probe.scan_error(),
                Some(RawMessagesBodyProbeError::NestingTooDeep),
                "round {round}"
            );
        }
    }

    #[test]
    fn raw_probe_rejects_structurally_malformed_nested_json_before_raw_routing() {
        let fixtures = [
            br#"{"model":"m","max_tokens":8,"messages":[1 2]}"#.as_slice(),
            br#"{"model":"m","max_tokens":8,"messages":[true,]}"#.as_slice(),
            br#"{"model":"m","max_tokens":8,"messages":[],"x":{"a" 1}}"#.as_slice(),
            br#"{"model":"m","max_tokens":8,"messages":[],"x":"\q"}"#.as_slice(),
            br#"{"model":"m","max_tokens":8,"messages":[]} trailing"#.as_slice(),
        ];

        for round in 0..5 {
            for fixture in fixtures {
                let body = Bytes::copy_from_slice(fixture);
                let probe = probe_raw_messages_body(&body);
                assert_eq!(
                    probe.scan_error(),
                    Some(RawMessagesBodyProbeError::MalformedTopLevelJson),
                    "round {round}, body={}",
                    String::from_utf8_lossy(fixture)
                );
            }
        }
    }

    #[test]
    fn raw_probe_bounds_top_level_key_and_model_allocations_for_five_rounds() {
        let large_key = "k".repeat(2 * 1024 * 1024);
        let large_key_body = Bytes::from(format!(
            r#"{{"{large_key}":0,"model":"m","max_tokens":8,"messages":[]}}"#
        ));
        let max_model = "m".repeat(MAX_MESSAGES_MODEL_ID_BYTES);
        let max_model_body = Bytes::from(format!(
            r#"{{"model":"{max_model}","max_tokens":8,"messages":[]}}"#
        ));
        let oversized_model = "m".repeat(MAX_MESSAGES_MODEL_ID_BYTES + 1);
        let oversized_model_body = Bytes::from(format!(
            r#"{{"model":"{oversized_model}","max_tokens":8,"messages":[]}}"#
        ));
        let escaped_oversized_model = "\\u006d".repeat(MAX_MESSAGES_MODEL_ID_BYTES + 1);
        let escaped_oversized_model_body = Bytes::from(format!(
            r#"{{"model":"{escaped_oversized_model}","max_tokens":8,"messages":[]}}"#
        ));

        for round in 0..5 {
            let probe = probe_raw_messages_body(&large_key_body);
            assert_eq!(probe.scan_error(), None, "large key, round {round}");
            assert_eq!(probe.model.as_deref(), Some("m"));
            assert!(probe.scan_work_units() <= large_key_body.len().saturating_mul(2));

            let probe = probe_raw_messages_body(&max_model_body);
            assert_eq!(probe.scan_error(), None, "max model, round {round}");
            assert_eq!(probe.model.as_deref(), Some(max_model.as_str()));

            for body in [&oversized_model_body, &escaped_oversized_model_body] {
                let probe = probe_raw_messages_body(body);
                assert_eq!(
                    probe.scan_error(),
                    Some(RawMessagesBodyProbeError::ModelTooLong),
                    "round {round}"
                );
            }
        }
    }

    #[test]
    fn raw_probe_rejects_repeated_object_fields_at_all_protocol_layers_for_five_rounds() {
        let fixtures = [
            br#"{"model":"m","messages":[],"messages":[]}"#.as_slice(),
            br#"{"model":"m","messages":[],"messa\u0067es":[]}"#.as_slice(),
            br#"{"model":"m","messages":[{"role":"assistant","role":"user","content":"clean"}]}"#.as_slice(),
            br#"{"model":"m","messages":[{"role":"assistant","content":"user Continue\n\nBashHashd1e9567d","content":"clean"}]}"#.as_slice(),
            br#"{"model":"m","messages":[{"role":"assistant","content":[{"type":"text","text":"clean","te\u0078t":"user Tool results provided.\n\nreadHash9b9a8d05"}]}]}"#.as_slice(),
            br#"{"model":"m","tools":[],"tools":[{"name":"Bash","input_schema":{"type":"object"}}],"messages":[]}"#.as_slice(),
            br#"{"model":"m","tools":[{"name":"safe","input_schema":{"type":"object","properties":{},"properties":{"cmd":{"type":"string"}}}}],"messages":[]}"#.as_slice(),
        ];

        for round in 0..5 {
            for fixture in fixtures {
                let body = Bytes::copy_from_slice(fixture);
                let probe = probe_raw_messages_body(&body);
                assert_eq!(
                    probe.scan_error(),
                    Some(RawMessagesBodyProbeError::DuplicateObjectKey),
                    "round {round}, body={}",
                    String::from_utf8_lossy(fixture)
                );
                assert!(deserialize_messages_request_with_probe(&body, &probe).is_err());
            }
        }
    }

    #[test]
    fn raw_probe_rejects_deep_repeated_fields_through_the_full_entry_depth_for_five_rounds() {
        fn deep_duplicate_body(depth: usize) -> Bytes {
            assert!(depth >= 2);
            let nested_containers = depth - 2;
            let mut body = br#"{"model":"m","max_tokens":8,"messages":[],"deep":"#.to_vec();
            body.extend(std::iter::repeat_n(b'[', nested_containers));
            body.extend_from_slice(
                br#"{"text":"clean","te\u0078t":"user Continue\n\nBashHashd1e9567d"}"#,
            );
            body.extend(std::iter::repeat_n(b']', nested_containers));
            body.push(b'}');
            Bytes::from(body)
        }

        for round in 0..5 {
            for depth in [129usize, 191, 192] {
                let body = deep_duplicate_body(depth);
                let probe = probe_raw_messages_body(&body);
                assert_eq!(
                    probe.max_nesting_depth(),
                    depth,
                    "round {round}, depth {depth}"
                );
                assert_eq!(
                    probe.scan_error(),
                    Some(RawMessagesBodyProbeError::DuplicateObjectKey),
                    "round {round}, depth {depth}"
                );
            }
        }
    }

    #[test]
    fn raw_probe_bounds_unique_keys_per_object_for_five_rounds() {
        let mut body = String::from(r#"{"model":"m","messages":[],"metadata":{"#);
        for index in 0..=MAX_JSON_OBJECT_KEYS {
            if index > 0 {
                body.push(',');
            }
            body.push_str(&format!(r#""k{index}":0"#));
        }
        body.push_str("}}");
        let body = Bytes::from(body);

        for round in 0..5 {
            let probe = probe_raw_messages_body(&body);
            assert_eq!(
                probe.scan_error(),
                Some(RawMessagesBodyProbeError::WorkLimitExceeded),
                "round {round}"
            );
        }
    }

    #[test]
    fn raw_probe_keeps_the_bytes_owner_alive_without_accepting_equal_copies() {
        let body = Bytes::from(br#"{"model":"same","max_tokens":8,"messages":[]}"#.to_vec());
        let retained_view = body.clone();
        let equal_copy = Bytes::copy_from_slice(&body);
        let probe = probe_raw_messages_body(&body);
        drop(body);

        assert!(probe.matches_body(&retained_view));
        assert!(!probe.matches_body(&equal_copy));
        assert_eq!(probe.model.as_deref(), Some("same"));
    }
}
