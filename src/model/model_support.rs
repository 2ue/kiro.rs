use crate::anthropic::model_capabilities::resolve_model_with_catalog_and_mode;
use crate::model::config::ModelResolutionMode;

pub fn normalize_model_id(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(value.to_ascii_lowercase())
}

pub fn normalize_supported_models(models: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out = Vec::new();
    for model in models {
        let Some(normalized) = normalize_model_id(&model) else {
            continue;
        };
        if out.iter().any(|existing| existing == &normalized) {
            continue;
        }
        out.push(normalized);
    }
    out
}

pub fn model_is_supported_by_list(models: &[String], candidates: &[Option<&str>]) -> bool {
    if models.is_empty() {
        return true;
    }

    let mut candidates_out = Vec::new();
    for candidate in candidates.iter().flatten() {
        let Some(model) = normalize_model_id(candidate) else {
            continue;
        };
        if models.iter().any(|supported| supported == &model) {
            return true;
        }
        candidates_out.push(model);
    }
    let candidates = candidates_out;
    if candidates.is_empty() {
        return false;
    }

    candidates_resolve_to_supported_models(models, &candidates)
        || supported_models_resolve_to_candidates(models, &candidates)
}

fn candidates_resolve_to_supported_models(models: &[String], candidates: &[String]) -> bool {
    candidates.iter().any(|candidate| {
        let resolution =
            resolve_model_with_catalog_and_mode(candidate, models, ModelResolutionMode::AliasOnly);
        resolution.upstream_model.as_ref().is_some_and(|resolved| {
            models
                .iter()
                .any(|supported| normalize_model_id(supported).as_ref() == Some(resolved))
        })
    })
}

fn supported_models_resolve_to_candidates(models: &[String], candidates: &[String]) -> bool {
    models.iter().any(|supported| {
        let Some(supported) = normalize_model_id(supported) else {
            return false;
        };
        let resolution = resolve_model_with_catalog_and_mode(
            &supported,
            candidates,
            ModelResolutionMode::AliasOnly,
        );
        resolution
            .upstream_model
            .as_ref()
            .is_some_and(|resolved| candidates.iter().any(|candidate| candidate == resolved))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_supported_models() {
        assert_eq!(
            normalize_supported_models(vec![
                " Claude-Sonnet-4 ".to_string(),
                "".to_string(),
                "claude-sonnet-4".to_string(),
                "claude-haiku-4".to_string(),
            ]),
            vec!["claude-sonnet-4", "claude-haiku-4"]
        );
    }

    #[test]
    fn empty_supported_models_allows_any_model() {
        assert!(model_is_supported_by_list(&[], &[Some("anything")]));
    }

    #[test]
    fn supported_models_match_any_candidate() {
        let models = vec!["claude-sonnet-4".to_string()];
        assert!(model_is_supported_by_list(
            &models,
            &[Some("client-alias"), Some("Claude-Sonnet-4")]
        ));
        assert!(!model_is_supported_by_list(
            &models,
            &[Some("claude-haiku-4"), None]
        ));
    }

    #[test]
    fn supported_models_match_version_equivalent_aliases() {
        assert!(model_is_supported_by_list(
            &["claude-opus-4.8".to_string()],
            &[Some("claude-opus-4-8")]
        ));
        assert!(model_is_supported_by_list(
            &["claude-opus-4-8".to_string()],
            &[Some("claude-opus-4.8")]
        ));
    }

    #[test]
    fn supported_models_match_explicit_anthropic_date_aliases() {
        assert!(model_is_supported_by_list(
            &["claude-sonnet-4-20250514".to_string()],
            &[Some("claude-sonnet-4")]
        ));
        assert!(model_is_supported_by_list(
            &["claude-sonnet-4".to_string()],
            &[Some("claude-sonnet-4-20250514")]
        ));
    }

    #[test]
    fn supported_models_do_not_use_family_fallback() {
        assert!(!model_is_supported_by_list(
            &["claude-sonnet-4.6".to_string()],
            &[Some("claude-sonnet-4.5")]
        ));
        assert!(!model_is_supported_by_list(
            &["claude-opus-4.8".to_string()],
            &[Some("claude-sonnet-4.6")]
        ));
    }
}
