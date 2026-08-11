use super::*;

pub(super) fn outbound_model_for_raw(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    raw_model: Option<&str>,
) -> Result<Option<String>, ExternalPoolError> {
    let original_model = raw_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .or(route.model_hint.as_deref());
    let processed_model = route
        .upstream_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .or(original_model);
    process_external_pool_model(pool, original_model, processed_model)
}

fn process_external_pool_model(
    pool: &ExternalPool,
    original_model: Option<&str>,
    processed_model: Option<&str>,
) -> Result<Option<String>, ExternalPoolError> {
    let fallback_transform = (pool.normalize_model_version_dots
        && matches!(
            pool.model_mapping_mode,
            ExternalPoolModelMappingMode::DirectMapping
                | ExternalPoolModelMappingMode::ProcessedMapping
        ))
    .then_some(normalize_outbound_model as fn(&str) -> String);
    let result = process_model(
        ModelProcessingInput {
            original_model,
            processed_model,
        },
        ModelProcessingConfig {
            mode: pool.model_mapping_mode.processing_mode(),
            rules: &pool.model_mapping_rules,
            require_mapping_match: pool.model_mapping_require_match,
            fallback_transform,
        },
    )
    .map_err(|err| model_processing_error(pool, err))?;
    Ok(Some(result.model))
}

pub(crate) fn normalize_mapping_rules(rules: Vec<ModelMappingRule>) -> Vec<ModelMappingRule> {
    rules
        .into_iter()
        .filter_map(|mut rule| {
            rule.source = rule.source.trim().to_ascii_lowercase();
            rule.target = rule.target.trim().to_string();
            rule.note = rule.note.and_then(|value| {
                let value = value.trim().to_string();
                (!value.is_empty()).then_some(value)
            });
            (!rule.source.is_empty() && !rule.target.is_empty()).then_some(rule)
        })
        .collect()
}

pub(super) fn normalize_outbound_model(model: &str) -> String {
    let trimmed = model.trim();
    if !trimmed.starts_with("claude-") || !trimmed.contains('.') {
        return trimmed.to_string();
    }

    let chars: Vec<char> = trimmed.chars().collect();
    let mut out = String::with_capacity(trimmed.len());
    for (idx, ch) in chars.iter().enumerate() {
        if *ch == '.'
            && idx > 0
            && idx + 1 < chars.len()
            && chars[idx - 1].is_ascii_digit()
            && chars[idx + 1].is_ascii_digit()
        {
            out.push('-');
        } else {
            out.push(*ch);
        }
    }
    out
}

fn model_processing_error(pool: &ExternalPool, err: ModelProcessingError) -> ExternalPoolError {
    match err {
        ModelProcessingError::MissingModel => ExternalPoolError {
            status: Some(StatusCode::BAD_REQUEST),
            message: format!("external pool #{} model is missing", pool.id),
            retryable: false,
            auto_disable_reason: None,
            cooldown: None,
            protocol_error: None,
            raw_upstream_error: None,
        },
        ModelProcessingError::MappingMiss { model } => ExternalPoolError {
            status: Some(StatusCode::BAD_GATEWAY),
            message: format!(
                "external pool #{} requires model mapping match, but no rule matched model {}",
                pool.id, model
            ),
            retryable: true,
            auto_disable_reason: None,
            cooldown: Some((Duration::ZERO, "model_mapping_miss".to_string())),
            protocol_error: None,
            raw_upstream_error: None,
        },
    }
}
