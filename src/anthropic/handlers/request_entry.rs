use super::*;

const MAX_ENTRY_ERROR_MESSAGE_CHARS: usize = 512;

pub(super) async fn handle_messages_endpoint(
    state: AppState,
    headers: HeaderMap,
    raw_body: Bytes,
    endpoint: String,
    request_api_key_id: Option<String>,
    ingress_id: Option<String>,
    attribution: Option<RequestRejectionAttribution>,
) -> Response {
    let runtime_config = request_runtime_config(&state);
    let inference_attempt_budget = Arc::new(InferenceAttemptBudget::new(
        runtime_config.inference_upstream_max_attempts,
    ));
    inference_attempt_budget.set_dispatch_route(&endpoint);
    inference_attempt_budget.set_dispatch_deadline_after(Duration::from_secs(
        runtime_config
            .accounts
            .effective_dispatch_max_wait_secs()
            .max(1),
    ));
    let raw_probe = probe_raw_messages_body(&raw_body);
    if let Some(probe_error) = raw_probe.scan_error() {
        let request_id = envelope::request_id();
        let error = entry_error_from_raw_probe(probe_error);
        record_entry_request_error(
            attribution.as_ref(),
            &endpoint,
            &request_id,
            &error,
            Some(&raw_probe),
        );
        return error.to_response(&request_id);
    }
    let patches =
        match route_body_patches(&state, &headers, &endpoint, &raw_body, &runtime_config).await {
            Ok(patches) => patches,
            Err(error) => {
                let request_id = envelope::request_id();
                record_entry_request_error(
                    attribution.as_ref(),
                    &endpoint,
                    &request_id,
                    &error,
                    Some(&raw_probe),
                );
                return error.to_response(&request_id);
            }
        };
    let effective_raw_body = patches.apply(&raw_body);
    let effective_raw_probe = Arc::new(probe_raw_messages_body(&effective_raw_body));
    post_messages_inner(
        state,
        headers,
        effective_raw_body,
        effective_raw_probe,
        raw_body,
        endpoint,
        inference_attempt_budget,
        request_api_key_id,
        ingress_id,
        attribution,
    )
    .await
}

pub(super) async fn route_body_patches(
    state: &AppState,
    headers: &HeaderMap,
    endpoint: &str,
    raw: &Bytes,
    runtime: &RequestRuntimeConfig,
) -> Result<super::super::body_patch::BodyPatchSet, EntryRequestError> {
    use super::super::body_patch::BodyPatchSet;
    let features = runtime.route_features_for(endpoint);
    let injections = runtime.injections_for(endpoint);
    let inject = InjectionId::ALL.iter().any(|id| injections.allows(*id));
    let mut patches = BodyPatchSet::default();
    if features.is_empty() && !inject {
        return Ok(patches);
    }
    let mut value: Value = serde_json::from_slice(raw)
        .map_err(|error| EntryRequestError::invalid(error.to_string(), "invalid_json_body"))?;
    let mut previous = value.clone();
    if let Some(config) = features.missing_max_tokens
        && value.get("max_tokens").is_none()
    {
        if config.policy == MissingMaxTokensPolicy::Reject {
            return Err(EntryRequestError::invalid(
                "max_tokens: field is required",
                "missing_max_tokens",
            ));
        }
        value["max_tokens"] = json!(config.normalized().default_value);
    }
    collect_value_patches(&previous, &value, "", &mut patches, "missing_max_tokens");
    previous = value.clone();
    if let Some(compat) = features.reasoning_compat {
        super::super::request_facts::normalize_reasoning_value(
            &mut value,
            &compat,
            value_model_limit(state, runtime, raw),
        )
        .map_err(|error| EntryRequestError::invalid(error, "reasoning_normalization_failed"))?;
        normalize_value_roles_and_content(&mut value, &compat);
    }
    collect_value_patches(&previous, &value, "", &mut patches, "reasoning_compat");
    previous = value.clone();
    if features.history_sanitize {
        let report = super::super::transcript_sanitizer::sanitize_request_history_value(&mut value);
        if runtime.compat_profile.is_strict() && report.blocks > 0 {
            return Err(EntryRequestError::invalid(
                envelope::PUBLIC_INVALID_REQUEST_MESSAGE,
                "strict_request_protocol_contamination",
            ));
        }
    }
    collect_value_patches(&previous, &value, "", &mut patches, "history_sanitize");
    previous = value.clone();
    for (source, model_name, trigger) in [
        (
            "thinking_model_name",
            features.thinking_model_name,
            ThinkingTriggerMode::Off,
        ),
        ("thinking_trigger", false, features.thinking_trigger),
    ] {
        if !model_name && trigger == ThinkingTriggerMode::Off {
            continue;
        }
        let mut payload = feature_payload(&value)?;
        let original_thinking = serde_json::to_value(&payload.thinking).unwrap();
        if model_name {
            override_thinking_from_model_name(&mut payload)
                .map_err(|error| EntryRequestError::invalid(error, "invalid_thinking_model"))?;
        }
        apply_thinking_trigger_mode(&mut payload, trigger);
        if model_name {
            payload.model = payload.model.trim_end_matches("-thinking").to_string();
        }
        value["model"] = json!(payload.model);
        if let Some(thinking) = payload.thinking {
            let processed = serde_json::to_value(thinking).unwrap();
            if value.get("thinking").is_none() {
                value["thinking"] = processed;
            } else if let Some(object) = processed.as_object() {
                for (key, new) in object {
                    if original_thinking.get(key) != Some(new) {
                        value["thinking"][key] = new.clone();
                    }
                }
            }
        }
        collect_value_patches(&previous, &value, "", &mut patches, source);
        previous = value.clone();
    }
    if let Some(config) = features.image_processing {
        let mut payload = feature_payload(&value)?;
        body_processing::prepare_multimodal_sources(
            &mut payload,
            headers
                .get(header::USER_AGENT)
                .and_then(|v| v.to_str().ok()),
            config,
        )
        .await
        .map_err(|error| {
            EntryRequestError::invalid(error.to_string(), "image_processing_failed")
        })?;
        for (index, message) in payload.messages.iter().enumerate() {
            if let (Some(original), Some(processed)) = (
                value.pointer_mut(&format!("/messages/{index}/content")),
                message.content.as_array(),
            ) {
                if let Some(blocks) = original.as_array_mut() {
                    for (block, processed) in blocks.iter_mut().zip(processed) {
                        if let Some(source) = processed.get("source") {
                            block["source"] = source.clone();
                        }
                    }
                }
            }
        }
    }
    collect_value_patches(&previous, &value, "", &mut patches, "image_processing");
    previous = value.clone();
    if inject {
        let mut system = match value.get("system") {
            Some(Value::String(text)) => Some(vec![super::super::types::SystemMessage {
                text: text.clone(),
                cache_control: None,
            }]),
            Some(value) => Some(serde_json::from_value(value.clone()).map_err(|error| {
                EntryRequestError::invalid(error.to_string(), "invalid_system")
            })?),
            None => None,
        };
        super::super::prompt_injection::apply_to_system(&mut system, &injections);
        let mut processed = serde_json::to_value(system).unwrap();
        if let (Some(original), Some(blocks)) = (
            value.get("system").and_then(Value::as_array),
            processed.as_array_mut(),
        ) {
            for block in blocks {
                if let Some(text) = block.get("text") {
                    if let Some(existing) =
                        original.iter().find(|old| old.get("text") == Some(text))
                    {
                        *block = existing.clone();
                    }
                }
            }
        }
        value["system"] = processed;
    }
    collect_value_patches(&previous, &value, "", &mut patches, "prompt_injection");
    previous = value.clone();
    if let Some(config) = features.payload_shaping {
        let mut payload = feature_payload(&value)?;
        let before = payload.clone();
        super::super::payload_guard::shape_route_payload(&mut payload, config);
        for (index, (old, new)) in before.messages.iter().zip(&payload.messages).enumerate() {
            if old.content != new.content {
                value["messages"][index]["content"] = new.content.clone();
            }
        }
        if serde_json::to_value(&before.tools).unwrap()
            != serde_json::to_value(&payload.tools).unwrap()
        {
            if let (Some(original), Some(tools)) = (
                value.get_mut("tools").and_then(Value::as_array_mut),
                payload.tools,
            ) {
                for (original, tool) in original.iter_mut().zip(tools) {
                    let processed = serde_json::to_value(tool).unwrap();
                    for key in ["description", "input_schema"] {
                        if let Some(new) = processed.get(key) {
                            original[key] = new.clone();
                        }
                    }
                }
            }
        }
    }
    collect_value_patches(&previous, &value, "", &mut patches, "payload_shaping");
    previous = value.clone();
    if let Some(config) = features
        .payload_guard
        .filter(|config| config.enabled && config.mode == PayloadGuardMode::Preemptive)
    {
        let guard = route_guard_config(config, None);
        if let Some(messages) = super::super::payload_guard::trim_route_messages(&value, guard) {
            value["messages"] = messages;
        }
    }
    collect_value_patches(&previous, &value, "", &mut patches, "payload_guard");
    Ok(patches)
}

fn value_model_limit(state: &AppState, runtime: &RequestRuntimeConfig, raw: &Bytes) -> Option<i32> {
    let probe = probe_raw_messages_body(raw);
    let model = probe.model.as_deref()?;
    let resolution = state.model_capabilities.resolve_model_with_mapping(
        model,
        runtime.model_resolution_mode,
        &runtime.model_mapping,
    );
    resolution
        .upstream_model
        .as_deref()
        .and_then(|model| state.model_capabilities.max_output_tokens_for(model))
        .or_else(|| state.model_capabilities.max_output_tokens_for(model))
}

fn feature_payload(value: &Value) -> Result<MessagesRequest, EntryRequestError> {
    let mut view = value.clone();
    if view.get("max_tokens").is_none() {
        view["max_tokens"] = json!(DEFAULT_MISSING_MAX_TOKENS_VALUE);
    }
    serde_json::from_value(view)
        .map_err(|error| EntryRequestError::invalid(error.to_string(), "invalid_feature_payload"))
}

pub(super) fn route_guard_config(
    config: crate::model::prompt_injection::RoutePayloadGuardConfig,
    shaping: Option<PayloadShapingConfig>,
) -> PayloadGuardConfig {
    let mut shaping_config = shaping.unwrap_or_default();
    shaping_config.enabled = shaping.is_some() && shaping_config.enabled;
    PayloadGuardConfig {
        enabled: config.enabled,
        max_bytes: config.max_bytes.saturating_sub(config.safety_margin_bytes),
        trim_history: config.trim_history,
        shaping: shaping_config,
    }
}

fn collect_value_patches(
    before: &Value,
    after: &Value,
    path: &str,
    patches: &mut super::super::body_patch::BodyPatchSet,
    source: &'static str,
) {
    if before == after {
        return;
    }
    match (before, after) {
        (Value::Object(old), Value::Object(new)) => {
            for (key, value) in new {
                let pointer = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                if let Some(old) = old.get(key) {
                    collect_value_patches(old, value, &pointer, patches, source);
                } else {
                    patches.set(pointer, value.clone(), source);
                }
            }
            for key in old.keys().filter(|key| !new.contains_key(*key)) {
                patches.remove(
                    format!("{path}/{}", key.replace('~', "~0").replace('/', "~1")),
                    source,
                );
            }
        }
        (Value::Array(old), Value::Array(new)) if old.len() == new.len() => {
            for (index, (old, new)) in old.iter().zip(new).enumerate() {
                collect_value_patches(old, new, &format!("{path}/{index}"), patches, source);
            }
        }
        _ => patches.set(path, after.clone(), source),
    }
}

fn normalize_value_roles_and_content(value: &mut Value, config: &LocalRequestCompatConfig) {
    let Some(messages) = value.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    let mut system_texts = Vec::new();
    if config.normalize_message_roles {
        messages.retain(|message| {
            match message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str()
            {
                "user" | "assistant" => true,
                "system" | "developer" => {
                    let text = message_content_text(&message["content"]);
                    if !text.trim().is_empty() {
                        system_texts.push(text);
                    }
                    false
                }
                _ => false,
            }
        });
    }
    if config.normalize_message_content {
        for message in messages {
            let content = &mut message["content"];
            *content = match content.take() {
                Value::Array(blocks) => Value::Array(
                    blocks
                        .into_iter()
                        .filter_map(|block| match block {
                            Value::Object(_) => Some(block),
                            Value::Null => None,
                            other => {
                                Some(json!({"type":"text","text":scalar_content_text(&other)}))
                            }
                        })
                        .collect(),
                ),
                Value::Null => Value::Array(Vec::new()),
                object @ Value::Object(_) => Value::Array(vec![object]),
                text @ Value::String(_) => text,
                other => Value::String(scalar_content_text(&other)),
            };
        }
    }
    if !system_texts.is_empty() {
        let mut system = match value.get("system") {
            Some(Value::Array(blocks)) => blocks.clone(),
            Some(Value::String(text)) => vec![json!({"type":"text","text":text})],
            _ => vec![],
        };
        system.extend(
            system_texts
                .into_iter()
                .map(|text| json!({"type":"text","text":text})),
        );
        value["system"] = Value::Array(system);
    }
}

#[derive(Debug, Clone)]
pub(super) struct EntryRequestError {
    status: StatusCode,
    error_type: &'static str,
    message: String,
    #[cfg_attr(not(test), allow(dead_code))]
    reason: &'static str,
    /// The client's `output_config.effort` (truncated) when it caused the rejection, so the
    /// value can be seen in the rejection record without logging the request body.
    original_effort: Option<String>,
}

impl EntryRequestError {
    fn invalid(message: impl Into<String>, reason: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            error_type: "invalid_request_error",
            message: message.into(),
            reason,
            original_effort: None,
        }
    }

    fn with_original_effort(mut self, original_effort: Option<String>) -> Self {
        self.original_effort = original_effort;
        self
    }

    pub(super) fn to_response(&self, request_id: &str) -> Response {
        envelope::error_response_with_id_and_headers(
            self.status,
            self.error_type,
            envelope::public_message_with_error_id(&self.message, request_id),
            request_id,
            [("x-error-id", request_id.to_string())],
        )
    }

    fn rejection_reason(&self) -> RequestRejectionReason {
        match self.reason {
            "strict_request_protocol_contamination" => {
                RequestRejectionReason::StrictRequestProtocolContamination
            }
            _ => RequestRejectionReason::RequestEntryInvalid,
        }
    }
}

#[cfg(test)]
fn apply_missing_max_tokens_policy(
    raw_body: &mut Bytes,
    config: MissingMaxTokensConfig,
    defaulted_max_tokens: &mut Option<i32>,
) -> Result<(), EntryRequestError> {
    let probe = probe_raw_messages_body(raw_body);
    apply_missing_max_tokens_policy_with_probe(raw_body, &probe, config, defaulted_max_tokens)
}

fn apply_missing_max_tokens_policy_with_probe(
    raw_body: &mut Bytes,
    probe: &RawMessagesBodyProbe,
    config: MissingMaxTokensConfig,
    defaulted_max_tokens: &mut Option<i32>,
) -> Result<(), EntryRequestError> {
    if probe.max_tokens_present || !probe.complete_top_level_object {
        return Ok(());
    }

    match config.normalized().policy {
        MissingMaxTokensPolicy::Reject => Err(EntryRequestError::invalid(
            "max_tokens: field is required",
            "missing_max_tokens",
        )),
        MissingMaxTokensPolicy::DefaultValue => {
            let default_value = config.normalized().default_value;
            match rewrite_raw_missing_top_level_max_tokens_with_probe(
                raw_body,
                probe,
                default_value,
            ) {
                Ok(Some(rewritten)) => {
                    *raw_body = rewritten;
                    *defaulted_max_tokens = Some(default_value);
                    Ok(())
                }
                Ok(None) => Ok(()),
                Err(err) => Err(EntryRequestError::invalid(
                    format!("Invalid JSON body: {}", err),
                    "missing_max_tokens_rewrite_failed",
                )),
            }
        }
    }
}

fn entry_error_from_raw_probe(
    error: super::super::request_facts::RawMessagesBodyProbeError,
) -> EntryRequestError {
    let reason = match error {
        super::super::request_facts::RawMessagesBodyProbeError::NestingTooDeep => {
            "json_nesting_too_deep"
        }
        super::super::request_facts::RawMessagesBodyProbeError::BodyTooLarge => {
            "json_body_too_large"
        }
        super::super::request_facts::RawMessagesBodyProbeError::ModelTooLong => "model_too_long",
        super::super::request_facts::RawMessagesBodyProbeError::WorkLimitExceeded => {
            "json_probe_work_limit"
        }
        super::super::request_facts::RawMessagesBodyProbeError::DuplicateObjectKey => {
            "duplicate_json_object_key"
        }
        super::super::request_facts::RawMessagesBodyProbeError::MalformedTopLevelJson => {
            "invalid_json_body"
        }
    };
    EntryRequestError::invalid(error.to_string(), reason)
}

#[cfg(test)]
pub(super) fn parse_messages_payload(
    raw_body: &Bytes,
    request_id: &str,
) -> Result<MessagesRequest, EntryRequestError> {
    let probe = probe_raw_messages_body(raw_body);
    parse_messages_payload_with_probe(
        raw_body,
        &probe,
        request_id,
        &crate::model::config::LocalRequestCompatConfig::default(),
    )
}

pub(super) fn parse_messages_payload_with_probe(
    raw_body: &Bytes,
    probe: &RawMessagesBodyProbe,
    _request_id: &str,
    compat: &crate::model::config::LocalRequestCompatConfig,
) -> Result<MessagesRequest, EntryRequestError> {
    if !probe.matches_body(raw_body) {
        return Err(EntryRequestError::invalid(
            "raw request probe does not match the request body snapshot",
            "raw_probe_body_mismatch",
        ));
    }
    if let Some(error) = probe.scan_error() {
        return Err(entry_error_from_raw_probe(error));
    }
    let mut payload = deserialize_messages_request_with_probe(raw_body, probe)
        .map_err(|message| EntryRequestError::invalid(message, "invalid_json_body"))?;
    if payload.model.trim().is_empty() {
        return Err(EntryRequestError::invalid(
            "model: field is required and cannot be empty",
            "empty_model",
        ));
    }
    normalize_message_roles_and_content(&mut payload, compat)?;
    validate_typed_reasoning_protocol(&payload)?;
    Ok(payload)
}

/// Some clients put `system`/`developer` turns inside `messages`. Their text is moved into the
/// system prompt (in order), and messages with any other non-user/assistant role are dropped,
/// matching how earlier releases served these requests instead of rejecting them.
fn normalize_message_roles(payload: &mut MessagesRequest) {
    if payload
        .messages
        .iter()
        .all(|message| matches!(message.role.as_str(), "user" | "assistant"))
    {
        return;
    }
    let mut moved = 0usize;
    let mut dropped = 0usize;
    let mut kept = Vec::with_capacity(payload.messages.len());
    for message in std::mem::take(&mut payload.messages) {
        match message.role.to_ascii_lowercase().as_str() {
            "user" | "assistant" => kept.push(message),
            "system" | "developer" => {
                let text = message_content_text(&message.content);
                if !text.trim().is_empty() {
                    payload.system.get_or_insert_with(Vec::new).push(
                        crate::anthropic::types::SystemMessage {
                            text,
                            cache_control: None,
                        },
                    );
                }
                moved += 1;
            }
            _ => dropped += 1,
        }
    }
    payload.messages = kept;
    tracing::info!(
        moved_to_system = moved,
        dropped_unknown_roles = dropped,
        "normalized non-standard message roles"
    );
}

fn message_content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                Value::String(text) => Some(text.clone()),
                Value::Object(map) => map.get("text").and_then(Value::as_str).map(str::to_string),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => scalar_content_text(other),
    }
}

/// Rejects roles the Anthropic API rejects, and normalizes content shapes Claude Code
/// clients may send but 上游 cannot take directly: scalar content becomes text, scalar list
/// items become text blocks, and nulls are dropped. Normalizing here keeps every later stage
/// (conversion, history, token counting) on the same string-or-block-list shape.
fn normalize_message_roles_and_content(
    payload: &mut MessagesRequest,
    compat: &crate::model::config::LocalRequestCompatConfig,
) -> Result<(), EntryRequestError> {
    if compat.normalize_message_roles {
        normalize_message_roles(payload);
    }
    if !compat.normalize_message_content {
        return Ok(());
    }
    for (index, message) in payload.messages.iter_mut().enumerate() {
        let replacement = match &mut message.content {
            Value::String(_) => None,
            Value::Array(blocks) => {
                if blocks.iter().all(Value::is_object) {
                    continue;
                }
                let original_len = blocks.len();
                let normalized = std::mem::take(blocks)
                    .into_iter()
                    .filter_map(|block| match block {
                        Value::Object(_) => Some(block),
                        Value::Null => None,
                        other => Some(json!({"type": "text", "text": scalar_content_text(&other)})),
                    })
                    .collect::<Vec<_>>();
                tracing::info!(
                    message_index = index,
                    original_blocks = original_len,
                    kept_blocks = normalized.len(),
                    "消息 content 数组含非对象元素，已转为 text 块（null 已丢弃）"
                );
                *blocks = normalized;
                None
            }
            Value::Null => {
                tracing::info!(message_index = index, "消息 content 为 null，按空内容处理");
                Some(Value::Array(Vec::new()))
            }
            Value::Object(_) => {
                tracing::info!(
                    message_index = index,
                    "消息 content 为单个对象，按单元素 content 块列表处理"
                );
                Some(Value::Array(vec![message.content.take()]))
            }
            other => {
                let kind = if other.is_number() {
                    "number"
                } else {
                    "boolean"
                };
                tracing::info!(
                    message_index = index,
                    content_kind = kind,
                    "消息 content 为标量，已转为文本内容"
                );
                Some(Value::String(scalar_content_text(other)))
            }
        };
        if let Some(replacement) = replacement {
            message.content = replacement;
        }
    }
    Ok(())
}

/// Text form of a non-object content value: strings verbatim, other values as compact JSON.
fn scalar_content_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn validate_typed_reasoning_protocol(payload: &MessagesRequest) -> Result<(), EntryRequestError> {
    if payload
        .output_config
        .as_ref()
        .and_then(|output_config| output_config.effort.as_deref())
        .is_some_and(|effort| super::super::types::parse_thinking_effort(effort).is_none())
    {
        return Err(EntryRequestError::invalid(
            format!(
                "output_config.effort must be one of: {}",
                super::super::types::THINKING_EFFORT_VALUES.join(", ")
            ),
            "invalid_thinking_effort",
        ));
    }
    let Some(thinking) = payload.thinking.as_ref() else {
        return Ok(());
    };
    if !matches!(
        thinking.thinking_type.as_str(),
        "enabled" | "adaptive" | "disabled"
    ) {
        return Err(EntryRequestError::invalid(
            "thinking.type must be one of: enabled, adaptive, disabled",
            "invalid_thinking_type",
        ));
    }
    if thinking.thinking_type == "enabled" {
        if thinking.budget_tokens < 1_024 {
            return Err(EntryRequestError::invalid(
                "thinking.budget_tokens is required and must be at least 1024",
                "invalid_thinking_budget",
            ));
        }
        if thinking.budget_tokens >= payload.max_tokens {
            return Err(EntryRequestError::invalid(
                "thinking.budget_tokens must be less than max_tokens",
                "invalid_thinking_budget",
            ));
        }
    }
    Ok(())
}

fn record_entry_request_error(
    attribution: Option<&RequestRejectionAttribution>,
    endpoint: &str,
    request_id: &str,
    error: &EntryRequestError,
    raw_probe: Option<&RawMessagesBodyProbe>,
) {
    if let Some(attribution) = attribution {
        attribution.record_with_metadata_and_context(
            error.rejection_reason(),
            "request_entry",
            error.status,
            request_id,
            endpoint,
            Some(match error.original_effort.as_deref() {
                Some(original_effort) => serde_json::json!({
                    "entryReason": error.reason,
                    "entryMessage": bounded_entry_error_message(&error.message),
                    "originalEffort": original_effort,
                }),
                None => serde_json::json!({
                    "entryReason": error.reason,
                    "entryMessage": bounded_entry_error_message(&error.message),
                }),
            }),
            raw_probe.and_then(request_rejection_usage_context_from_probe),
        );
    }
}

fn request_rejection_usage_context_from_probe(
    probe: &RawMessagesBodyProbe,
) -> Option<super::super::usage::RequestRejectionUsageContext> {
    let context = super::super::usage::RequestRejectionUsageContext {
        model: probe
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_string),
        stream: probe.stream,
        requested_max_tokens: probe
            .max_tokens
            .and_then(|tokens| i32::try_from(tokens).ok())
            .filter(|tokens| *tokens > 0),
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
    };
    (!context.is_empty()).then_some(context)
}

fn bounded_entry_error_message(message: &str) -> String {
    message
        .chars()
        .take(MAX_ENTRY_ERROR_MESSAGE_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::prompt_cache::PromptCacheTracker;
    use crate::anthropic::prompt_cache_creation_control::PromptCacheCreationController;
    use crate::anthropic::request_admission::RequestAdmissionController;
    use crate::anthropic::usage::{
        UsageRecordQuery, UsageRecordStatus, UsageRecorder, UsageSource,
    };
    use crate::common::auth::RequestApiKeyStore;
    use crate::model::config::{DEFAULT_MISSING_MAX_TOKENS_VALUE, RequestAdmissionConfig};

    fn test_state(recorder: Arc<UsageRecorder>) -> AppState {
        AppState::new(
            Arc::new(RequestApiKeyStore::new(["test-key"])),
            recorder,
            Arc::new(PromptCacheTracker::default()),
            Arc::new(PromptCacheCreationController::default()),
            PromptCacheSimulationMode::HighCache,
            0.98,
            CompatProfile::ClaudeCode,
        )
    }

    fn test_attribution(recorder: Arc<UsageRecorder>) -> RequestRejectionAttribution {
        let store = RequestApiKeyStore::new(["test-key"]);
        let identity = store.authenticate("test-key").unwrap();
        RequestRejectionAttribution::detached(
            Arc::new(RequestAdmissionController::new(
                RequestAdmissionConfig::disabled(),
            )),
            recorder,
            identity,
        )
    }

    fn nested_json_value(depth: usize) -> Value {
        let mut value = json!({"leaf": true});
        for level in (0..depth).rev() {
            value = json!({"level": level, "children": [value]});
        }
        value
    }

    #[test]
    fn missing_max_tokens_default_value_rewrites_body_for_typed_parse() {
        let state = test_state(Arc::new(UsageRecorder::new(10)));
        let runtime_config = RequestRuntimeConfig::from_app_state(&state);
        let cache_route = runtime_config.cache_policy_for_path("/cc/v1/messages");
        let client = Bytes::from_static(
            br#" {"model":"claude-sonnet-4-5","messages":[{"role":"user","content":"hello"}],"stream":false} "#,
        );
        let expected = Bytes::from_static(
            br#" {"model":"claude-sonnet-4-5","messages":[{"role":"user","content":"hello"}],"stream":false,"max_tokens":20480} "#,
        );

        for _round in 0..5 {
            let mut effective = client.clone();
            let mut defaulted = None;
            apply_missing_max_tokens_policy(
                &mut effective,
                MissingMaxTokensConfig::default(),
                &mut defaulted,
            )
            .expect("default missing max_tokens");
            let parsed = parse_messages_payload(&effective, "req_test_missing_default")
                .expect("typed parse");

            assert_eq!(effective, expected, "only max_tokens may be appended");
            assert_eq!(defaulted, Some(DEFAULT_MISSING_MAX_TOKENS_VALUE));
            assert_eq!(parsed.max_tokens, DEFAULT_MISSING_MAX_TOKENS_VALUE);
            assert_eq!(parsed.model, "claude-sonnet-4-5");

            let route = external_route_request_raw(
                &state,
                &runtime_config,
                &cache_route,
                HeaderMap::new(),
                effective.clone(),
                "/cc/v1/messages",
                "req_missing_max_raw_route".to_string(),
                Arc::new(InferenceAttemptBudget::new(4)),
                None,
                None,
                Arc::new(probe_raw_messages_body(&effective)),
            );
            assert_eq!(route.effective_raw_body, expected);
            assert_eq!(route.raw_body, expected);
        }
    }

    #[test]
    fn non_standard_message_roles_are_normalized_for_five_rounds() {
        for round in 0..5 {
            let raw = Bytes::from_static(
                br#"{"model":"claude-sonnet-4-5","max_tokens":16,"system":"base","messages":[{"role":"user","content":"hi"},{"role":"system","content":"be brief"},{"role":"developer","content":[{"type":"text","text":"answer in English"}]},{"role":"robot","content":"ignored"},{"role":"assistant","content":"ok"},{"role":"user","content":"again"}]}"#,
            );
            let payload = parse_messages_payload(&raw, "req_roles")
                .unwrap_or_else(|error| panic!("round {round}: {}", error.message));
            let roles: Vec<&str> = payload.messages.iter().map(|m| m.role.as_str()).collect();
            assert_eq!(roles, ["user", "assistant", "user"], "round {round}");
            let system: Vec<&str> = payload
                .system
                .as_ref()
                .expect("system")
                .iter()
                .map(|s| s.text.as_str())
                .collect();
            assert_eq!(
                system,
                ["base", "be brief", "answer in English"],
                "round {round}"
            );
        }
    }

    #[test]
    fn non_block_message_content_is_normalized_instead_of_rejected_for_five_rounds() {
        let cases: [(&[u8], Value); 6] = [
            (
                br#"{"model":"m","max_tokens":16,"messages":[{"role":"user","content":42}]}"#,
                json!("42"),
            ),
            (
                br#"{"model":"m","max_tokens":16,"messages":[{"role":"user","content":true}]}"#,
                json!("true"),
            ),
            (
                br#"{"model":"m","max_tokens":16,"messages":[{"role":"user","content":null}]}"#,
                json!([]),
            ),
            (
                br#"{"model":"m","max_tokens":16,"messages":[{"role":"user","content":{"type":"text","text":"one"}}]}"#,
                json!([{"type": "text", "text": "one"}]),
            ),
            (
                br#"{"model":"m","max_tokens":16,"messages":[{"role":"user","content":["plain",{"type":"text","text":"ok"},7,false,null,[1]]}]}"#,
                json!([
                    {"type": "text", "text": "plain"},
                    {"type": "text", "text": "ok"},
                    {"type": "text", "text": "7"},
                    {"type": "text", "text": "false"},
                    {"type": "text", "text": "[1]"}
                ]),
            ),
            (
                br#"{"model":"m","max_tokens":16,"messages":[{"role":"user","content":"hi"},{"role":"assistant","content":[{"type":"text","text":"ok"}]},{"role":"user","content":[]}]}"#,
                json!([]),
            ),
        ];
        for round in 0..5 {
            for (raw, expected_last_content) in &cases {
                let raw = Bytes::copy_from_slice(raw);
                let payload = parse_messages_payload(&raw, "req_normalized_content")
                    .unwrap_or_else(|error| panic!("round {round}: {}", error.message));
                assert_eq!(
                    &payload.messages.last().unwrap().content,
                    expected_last_content,
                    "round {round}"
                );
            }
        }
    }

    #[test]
    fn typed_parse_accepts_deep_tool_input_within_explicit_limit() {
        let raw = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "claude-sonnet-4-5",
                "max_tokens": 16,
                "messages": [{
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "toolu_deep",
                        "name": "echo",
                        "input": nested_json_value(80)
                    }]
                }]
            }))
            .expect("serialize deep request"),
        );

        let parsed = parse_messages_payload(&raw, "req_deep_json").expect("deep request parses");

        assert_eq!(parsed.model, "claude-sonnet-4-5");
        assert_eq!(parsed.messages.len(), 1);
    }

    #[test]
    fn typed_parse_rejects_json_beyond_explicit_nesting_limit() {
        let raw = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "claude-sonnet-4-5",
                "max_tokens": 16,
                "messages": [{
                    "role": "user",
                    "content": nested_json_value(MAX_MESSAGES_JSON_NESTING_DEPTH)
                }]
            }))
            .expect("serialize over-deep request"),
        );

        let error = parse_messages_payload(&raw, "req_too_deep_json")
            .expect_err("over-deep request is rejected");

        assert_eq!(error.reason, "json_nesting_too_deep");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn missing_max_tokens_reject_records_entry_error_without_body_content() {
        let recorder = Arc::new(UsageRecorder::new(10));
        let attribution = test_attribution(recorder.clone());
        let error =
            EntryRequestError::invalid("max_tokens: field is required", "missing_max_tokens");
        let raw = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5","stream":true,"messages":[{"role":"user","content":"secret-body"}]}"#,
        );
        let raw_probe = probe_raw_messages_body(&raw);

        for count in 1..=5 {
            record_entry_request_error(
                Some(&attribution),
                "/cc/v1/messages",
                &format!("req_entry_missing_max_tokens_{count}"),
                &error,
                Some(&raw_probe),
            );
        }

        let records = recorder.query(UsageRecordQuery::default()).records;
        assert_eq!(records.len(), 5);
        let mut observed_counts = records
            .iter()
            .map(|record| {
                assert_eq!(record.status, UsageRecordStatus::Error);
                assert_eq!(record.usage_source, UsageSource::None);
                assert_eq!(record.model, "claude-sonnet-4-5");
                assert!(record.stream);
                assert_eq!(record.requested_max_tokens, None);
                assert_eq!(record.error_source.as_deref(), Some("request_rejection"));
                assert_eq!(record.error_status_code, Some(400));
                let metadata = record.error_metadata.as_ref().expect("metadata");
                assert_eq!(metadata["stage"], "request_entry");
                assert_eq!(metadata["reason"], "request_entry_invalid");
                assert_eq!(metadata["entryReason"], "missing_max_tokens");
                assert_eq!(metadata["entryMessage"], "max_tokens: field is required");
                assert_eq!(metadata["sampled"], true);
                assert_eq!(metadata["observedCountIsExact"], false);
                assert!(!metadata.to_string().contains("secret-body"));
                metadata["observedCount"].as_u64().unwrap()
            })
            .collect::<Vec<_>>();
        observed_counts.sort_unstable();
        assert_eq!(observed_counts, vec![1, 2, 3, 4, 5]);
    }

    /// dropUnknownEffort 关掉时，被拒的 effort 原值进拒绝记录（截断，不含请求正文）。
    #[test]
    fn rejected_effort_records_original_value_without_body_content() {
        let recorder = Arc::new(UsageRecorder::new(10));
        let attribution = test_attribution(recorder.clone());
        let raw = Bytes::from_static(
            br#"{"model":"claude-opus-5-5","max_tokens":64,"output_config":{"effort":"Turbo"},"messages":[{"role":"user","content":"secret-body"}]}"#,
        );
        let raw_probe = probe_raw_messages_body(&raw);
        let message = validate_raw_reasoning_protocol_with_probe(&raw, &raw_probe)
            .expect_err("strict validator rejects the effort");
        assert!(message.starts_with("output_config.effort"), "{message}");
        let original =
            super::super::super::request_facts::raw_output_effort_for_record(&raw, &raw_probe);
        assert_eq!(original.as_deref(), Some("Turbo"));
        let error = EntryRequestError::invalid(message, "invalid_reasoning_protocol")
            .with_original_effort(original);
        record_entry_request_error(
            Some(&attribution),
            "/dfcache/minea/v1/messages",
            "req_entry_bad_effort",
            &error,
            Some(&raw_probe),
        );
        let records = recorder.query(UsageRecordQuery::default()).records;
        let metadata = records[0].error_metadata.as_ref().expect("metadata");
        assert_eq!(metadata["entryReason"], "invalid_reasoning_protocol");
        assert_eq!(metadata["originalEffort"], "Turbo");
        assert!(!metadata.to_string().contains("secret-body"));

        // Errors unrelated to effort carry no originalEffort.
        let other =
            EntryRequestError::invalid("max_tokens: field is required", "missing_max_tokens");
        record_entry_request_error(
            Some(&attribution),
            "/dfcache/minea/v1/messages",
            "req_entry_other",
            &other,
            Some(&raw_probe),
        );
        let records = recorder.query(UsageRecordQuery::default()).records;
        assert!(
            records
                .iter()
                .filter_map(|record| record.error_metadata.as_ref())
                .any(|metadata| metadata.get("originalEffort").is_none())
        );
    }

    /// 角色与 content 归一化各自可关；关掉时请求原样交给后续阶段。默认开启，行为与之前一致。
    #[test]
    fn role_and_content_normalization_follow_their_switches() {
        let raw = Bytes::from_static(
            br#"{"model":"m","max_tokens":64,"messages":[{"role":"system","content":"be brief"},{"role":"user","content":42}]}"#,
        );
        let probe = probe_raw_messages_body(&raw);
        let defaults = crate::model::config::LocalRequestCompatConfig::default();
        let payload =
            parse_messages_payload_with_probe(&raw, &probe, "req_compat_default", &defaults)
                .expect("parses");
        assert_eq!(payload.messages.len(), 1);
        assert_eq!(payload.messages[0].content, serde_json::json!("42"));
        assert_eq!(
            payload
                .system
                .as_ref()
                .map(|system| system[0].text.as_str()),
            Some("be brief")
        );

        let off = crate::model::config::LocalRequestCompatConfig {
            normalize_message_roles: false,
            normalize_message_content: false,
            ..defaults
        };
        let payload = parse_messages_payload_with_probe(&raw, &probe, "req_compat_off", &off)
            .expect("parses");
        assert_eq!(payload.messages.len(), 2);
        assert_eq!(payload.messages[0].role, "system");
        assert_eq!(payload.messages[1].content, serde_json::json!(42));
    }

    #[test]
    fn malformed_json_parse_error_can_be_recorded_at_entry() {
        let recorder = Arc::new(UsageRecorder::new(10));
        let attribution = test_attribution(recorder.clone());
        let raw = Bytes::from_static(br#"{"model":"claude-sonnet-4-5","max_tokens":16"#);
        let error = parse_messages_payload(&raw, "req_entry_bad_json").expect_err("malformed json");

        for count in 1..=5 {
            record_entry_request_error(
                Some(&attribution),
                "/v1/messages",
                &format!("req_entry_bad_json_{count}"),
                &error,
                None,
            );
        }

        let records = recorder.query(UsageRecordQuery::default()).records;
        assert_eq!(records.len(), 5);
        assert!(records.iter().all(|record| {
            record.error_source.as_deref() == Some("request_rejection")
                && record
                    .error_metadata
                    .as_ref()
                    .and_then(|value| value.get("reason"))
                    .and_then(serde_json::Value::as_str)
                    == Some("request_entry_invalid")
                && record
                    .error_metadata
                    .as_ref()
                    .and_then(|value| value.get("entryReason"))
                    .and_then(serde_json::Value::as_str)
                    == Some("invalid_json_body")
                && record
                    .error_metadata
                    .as_ref()
                    .and_then(|value| value.get("entryMessage"))
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|message| {
                        !message.is_empty()
                            && message.chars().count() <= MAX_ENTRY_ERROR_MESSAGE_CHARS
                            && !message.contains("claude-sonnet-4-5")
                    })
        }));
    }

    #[test]
    fn repeated_json_fields_fail_at_entry_without_exposing_hidden_transcript_for_five_rounds() {
        let fixtures = [
            br#"{"model":"m","max_tokens":64,"messages":[{"role":"assistant","content":"user Continue\n\nBashHashd1e9567d"}],"messages":[{"role":"user","content":"clean"}]}"#.as_slice(),
            br#"{"model":"m","max_tokens":64,"messages":[{"role":"assistant","content":[{"type":"text","text":"clean","te\u0078t":"user Tool results provided.\n\nreadHash9b9a8d05"}]}]}"#.as_slice(),
        ];

        for round in 0..5 {
            for fixture in fixtures {
                let error = parse_messages_payload(
                    &Bytes::copy_from_slice(fixture),
                    "req_duplicate_json_field",
                )
                .expect_err("repeated object fields must fail before routing");
                assert_eq!(error.status, StatusCode::BAD_REQUEST, "round {round}");
                assert_eq!(error.reason, "duplicate_json_object_key", "round {round}");
                assert!(!error.message.contains("BashHash"), "round {round}");
                assert!(!error.message.contains("readHash"), "round {round}");
            }
        }
    }

    #[test]
    fn typed_reasoning_protocol_preserves_large_budget_and_max_effort_for_five_rounds() {
        let raw = Bytes::from_static(
            br#"{"model":"claude-opus-4.8","max_tokens":128000,"messages":[{"role":"user","content":"hi"}],"thinking":{"type":"enabled","budget_tokens":100000}}"#,
        );
        let adaptive = Bytes::from_static(
            br#"{"model":"claude-opus-4.8","max_tokens":128000,"messages":[{"role":"user","content":"hi"}],"thinking":{"type":"adaptive"},"output_config":{"effort":"max"}}"#,
        );
        let omitted = Bytes::from_static(
            br#"{"model":"claude-opus-4.8","max_tokens":128000,"messages":[{"role":"user","content":"hi"}],"thinking":{"type":"adaptive"},"output_config":{}}"#,
        );

        for round in 0..5 {
            let parsed = parse_messages_payload(&raw, "req_budget")
                .unwrap_or_else(|error| panic!("round {round}: {error:?}"));
            assert_eq!(parsed.thinking.unwrap().budget_tokens, 100_000);
            let parsed = parse_messages_payload(&adaptive, "req_max")
                .unwrap_or_else(|error| panic!("round {round}: {error:?}"));
            assert_eq!(parsed.output_config.unwrap().effort.as_deref(), Some("max"));
            let parsed = parse_messages_payload(&omitted, "req_omitted")
                .unwrap_or_else(|error| panic!("round {round}: {error:?}"));
            assert_eq!(
                parsed
                    .output_config
                    .as_ref()
                    .expect("output_config container")
                    .effort,
                None,
                "round {round}: omitted effort must remain omitted"
            );
            assert!(
                serde_json::to_value(parsed)
                    .expect("serialize parsed request")
                    .pointer("/output_config/effort")
                    .is_none(),
                "round {round}: typed serialization must not add effort"
            );
        }
    }

    #[tokio::test]
    async fn messages_entry_normalizes_omitted_or_conflicting_thinking_controls_before_dispatch() {
        let state = test_state(Arc::new(UsageRecorder::new(10)));
        state.runtime.install_local(|config| {
            config.route_features.defaults.missing_max_tokens =
                Some(MissingMaxTokensConfig::default());
            config.route_features.defaults.reasoning_compat =
                Some(LocalRequestCompatConfig::default());
        });
        let cases = [
            (
                "both controls omitted",
                br#"{"model":"claude-sonnet-4","messages":[{"role":"user","content":"hello"}],"thinking":{"type":"enabled"}}"#
                    as &[u8],
            ),
            (
                "budget exceeds max",
                br#"{"model":"claude-sonnet-4","max_tokens":2048,"messages":[{"role":"user","content":"hello"}],"thinking":{"type":"enabled","budget_tokens":4096}}"#
                    as &[u8],
            ),
            (
                "small explicit max",
                br#"{"model":"claude-sonnet-4","max_tokens":512,"messages":[{"role":"user","content":"hello"}],"thinking":{"type":"enabled","budget_tokens":20000}}"#
                    as &[u8],
            ),
        ];
        let endpoints = [
            "/v1/messages",
            "/cc/v1/messages",
            "/na/v1/messages",
            "/ha/v1/messages",
            "/dfcache/demo/v1/messages",
        ];

        for round in 0..5 {
            for (case_name, body) in cases {
                for endpoint in endpoints {
                    let response = handle_messages_endpoint(
                        state.clone(),
                        HeaderMap::new(),
                        Bytes::copy_from_slice(body),
                        endpoint.to_string(),
                        None,
                        None,
                        None,
                    )
                    .await;
                    assert_eq!(
                        response.status(),
                        StatusCode::from_u16(529).unwrap(),
                        "round {round}, {case_name}, {endpoint}: request must pass entry validation before the intentionally providerless dispatch"
                    );
                    let response_body = axum::body::to_bytes(response.into_body(), 32 * 1024)
                        .await
                        .expect("read provider-not-ready response");
                    let response_body = String::from_utf8(response_body.to_vec())
                        .expect("provider-not-ready response UTF-8");
                    assert!(
                        !response_body
                            .contains("thinking.budget_tokens must be less than max_tokens"),
                        "round {round}, {case_name}, {endpoint}: {response_body}"
                    );
                }
            }
        }
    }

    #[test]
    fn omitted_output_effort_keeps_enabled_and_disabled_thinking_authoritative_five_rounds() {
        let enabled = Bytes::from_static(
            br#"{"model":"claude-opus-4.8","max_tokens":64000,"messages":[{"role":"user","content":"hi"}],"thinking":{"type":"enabled","budget_tokens":32000},"output_config":{}}"#,
        );
        let disabled = Bytes::from_static(
            br#"{"model":"claude-opus-4.8","max_tokens":4096,"messages":[{"role":"user","content":"hi"}],"thinking":{"type":"disabled"},"output_config":{}}"#,
        );
        let disabled_with_effort = Bytes::from_static(
            br#"{"model":"claude-opus-4.8","max_tokens":4096,"messages":[{"role":"user","content":"hi"}],"thinking":{"type":"disabled"},"output_config":{"effort":"max"}}"#,
        );
        let enabled_with_effort = Bytes::from_static(
            br#"{"model":"claude-opus-4.5","max_tokens":4096,"messages":[{"role":"user","content":"hi"}],"thinking":{"type":"enabled","budget_tokens":2048},"output_config":{"effort":"high"}}"#,
        );

        for round in 0..5 {
            let parsed = parse_messages_payload(&enabled, "req_enabled_omitted")
                .unwrap_or_else(|error| panic!("round {round}: {error:?}"));
            assert_eq!(
                parsed
                    .thinking
                    .as_ref()
                    .map(|thinking| thinking.thinking_type.as_str()),
                Some("enabled"),
                "round {round}"
            );
            assert_eq!(
                parsed
                    .output_config
                    .as_ref()
                    .and_then(|config| config.effort.as_deref()),
                None,
                "round {round}"
            );

            let parsed = parse_messages_payload(&disabled, "req_disabled_omitted")
                .unwrap_or_else(|error| panic!("round {round}: {error:?}"));
            assert_eq!(
                parsed
                    .thinking
                    .as_ref()
                    .map(|thinking| thinking.thinking_type.as_str()),
                Some("disabled"),
                "round {round}"
            );
            assert_eq!(
                parsed
                    .output_config
                    .as_ref()
                    .and_then(|config| config.effort.as_deref()),
                None,
                "round {round}"
            );

            let parsed = parse_messages_payload(&disabled_with_effort, "req_disabled_effort")
                .unwrap_or_else(|error| panic!("round {round}: {error:?}"));
            assert_eq!(
                parsed
                    .thinking
                    .as_ref()
                    .map(|thinking| thinking.thinking_type.as_str()),
                Some("disabled"),
                "round {round}"
            );
            assert_eq!(
                parsed
                    .output_config
                    .as_ref()
                    .and_then(|config| config.effort.as_deref()),
                Some("max"),
                "round {round}: Claude CLI disabled thinking must not discard explicit effort"
            );

            let parsed = parse_messages_payload(&enabled_with_effort, "req_enabled_effort")
                .unwrap_or_else(|error| panic!("round {round}: {error:?}"));
            assert_eq!(
                parsed
                    .thinking
                    .as_ref()
                    .map(|thinking| thinking.thinking_type.as_str()),
                Some("enabled"),
                "round {round}"
            );
            assert_eq!(
                parsed
                    .output_config
                    .as_ref()
                    .and_then(|config| config.effort.as_deref()),
                Some("high"),
                "round {round}: valid budget plus explicit effort is a protocol-level supported form"
            );
        }
    }

    #[test]
    fn typed_reasoning_protocol_rejects_unknown_and_ambiguous_controls_for_five_rounds() {
        let fixtures = [
            br#"{"model":"m","max_tokens":4096,"messages":[],"thinking":{"type":"mystery"}}"#.as_slice(),
            br#"{"model":"m","max_tokens":4096,"messages":[],"thinking":{"type":"enabled","budget_tokens":1023}}"#.as_slice(),
            br#"{"model":"m","max_tokens":4096,"messages":[],"thinking":{"type":"enabled"}}"#.as_slice(),
            br#"{"model":"m","max_tokens":4096,"messages":[],"thinking":{"type":"enabled","budget_tokens":4096}}"#.as_slice(),
            br#"{"model":"m","max_tokens":4096,"messages":[],"thinking":{"type":"enabled","budget_tokens":4096},"output_config":{"effort":"low"}}"#.as_slice(),
            br#"{"model":"m","max_tokens":4096,"messages":[],"output_config":{"effort":"MAX"}}"#.as_slice(),
            br#"{"model":"m","max_tokens":4096,"messages":[],"output_config":{"effort":"unknown"}}"#.as_slice(),
        ];
        for round in 0..5 {
            for fixture in fixtures {
                let error = parse_messages_payload(&Bytes::copy_from_slice(fixture), "req_invalid")
                    .expect_err("invalid reasoning controls must be rejected");
                assert_eq!(error.status, StatusCode::BAD_REQUEST, "round {round}");
            }
        }
    }
}
