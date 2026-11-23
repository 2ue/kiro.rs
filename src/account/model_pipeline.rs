use super::*;

#[allow(clippy::result_large_err)]
pub(super) fn outbound_model_for_raw(
    route: &AccountRouteRequest,
    pool: &Account,
    raw_model: Option<&str>,
) -> Result<Option<String>, AccountError> {
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
    process_account_model(pool, original_model, processed_model)
}

#[allow(clippy::result_large_err)]
fn process_account_model(
    pool: &Account,
    original_model: Option<&str>,
    processed_model: Option<&str>,
) -> Result<Option<String>, AccountError> {
    let result = process_model(
        ModelProcessingInput {
            original_model,
            processed_model,
        },
        ModelProcessingConfig {
            mode: pool.model_mapping_mode.processing_mode(),
            rules: &pool.model_mapping_rules,
            require_mapping_match: pool.model_mapping_require_match,
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

fn model_processing_error(pool: &Account, err: ModelProcessingError) -> AccountError {
    match err {
        ModelProcessingError::MissingModel => AccountError {
            status: Some(StatusCode::BAD_REQUEST),
            message: format!("account #{} model is missing", pool.id),
            retryable: false,
            auto_disable_reason: None,
            cooldown: None,
            protocol_error: None,
            raw_upstream_error: None,
        },
        ModelProcessingError::MappingMiss { model } => AccountError {
            status: Some(StatusCode::BAD_GATEWAY),
            message: format!(
                "account #{} requires model mapping match, but no rule matched model {}",
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
