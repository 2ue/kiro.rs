use crate::model::config::ModelMappingRule;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelProcessingMode {
    Passthrough,
    PassthroughMapping,
    MappingThenProcessed,
    ProcessedThenMapping,
}

#[derive(Debug, Clone, Copy)]
pub struct ModelProcessingInput<'a> {
    pub original_model: Option<&'a str>,
    pub processed_model: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub struct ModelProcessingConfig<'a> {
    pub mode: ModelProcessingMode,
    pub rules: &'a [ModelMappingRule],
    pub require_mapping_match: bool,
    pub fallback_transform: Option<fn(&str) -> String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelProcessingError {
    MissingModel,
    MappingMiss { model: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelProcessingSource {
    Original,
    Processed,
    Mapping,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelProcessingResult {
    pub model: String,
    pub source: ModelProcessingSource,
    pub mapping_applied: bool,
}

pub fn process_model(
    input: ModelProcessingInput<'_>,
    config: ModelProcessingConfig<'_>,
) -> Result<ModelProcessingResult, ModelProcessingError> {
    match config.mode {
        ModelProcessingMode::Passthrough => {
            let (model, source) = original_model(input)
                .map(|model| (model, ModelProcessingSource::Original))
                .or_else(|| {
                    processed_model(input).map(|model| (model, ModelProcessingSource::Processed))
                })
                .ok_or(ModelProcessingError::MissingModel)?;
            Ok(ModelProcessingResult {
                model: model.to_string(),
                source,
                mapping_applied: false,
            })
        }
        ModelProcessingMode::PassthroughMapping => {
            let (model, source) = original_model(input)
                .map(|model| (model, ModelProcessingSource::Original))
                .or_else(|| {
                    processed_model(input).map(|model| (model, ModelProcessingSource::Processed))
                })
                .ok_or(ModelProcessingError::MissingModel)?;
            mapped_or_fallback(
                model,
                source,
                ModelProcessingConfig {
                    fallback_transform: None,
                    ..config
                },
                model,
            )
        }
        ModelProcessingMode::MappingThenProcessed => {
            let original = original_model(input).ok_or(ModelProcessingError::MissingModel)?;
            let (fallback, fallback_source) = processed_model(input)
                .map(|model| (model, ModelProcessingSource::Processed))
                .unwrap_or((original, ModelProcessingSource::Original));
            mapped_or_fallback(original, fallback_source, config, fallback)
        }
        ModelProcessingMode::ProcessedThenMapping => {
            let (model, source) = processed_model(input)
                .map(|model| (model, ModelProcessingSource::Processed))
                .or_else(|| {
                    original_model(input).map(|model| (model, ModelProcessingSource::Original))
                })
                .ok_or(ModelProcessingError::MissingModel)?;
            mapped_or_fallback(model, source, config, model)
        }
    }
}

fn mapped_or_fallback(
    mapping_source: &str,
    fallback_source: ModelProcessingSource,
    config: ModelProcessingConfig<'_>,
    fallback_model: &str,
) -> Result<ModelProcessingResult, ModelProcessingError> {
    if let Some(target) = model_mapping_target(config.rules, mapping_source) {
        return Ok(ModelProcessingResult {
            model: target,
            source: ModelProcessingSource::Mapping,
            mapping_applied: true,
        });
    }
    if config.require_mapping_match {
        return Err(ModelProcessingError::MappingMiss {
            model: mapping_source.to_string(),
        });
    }
    Ok(ModelProcessingResult {
        model: transform_fallback(fallback_model, config),
        source: fallback_source,
        mapping_applied: false,
    })
}

fn original_model(input: ModelProcessingInput<'_>) -> Option<&str> {
    clean_model(input.original_model)
}

fn processed_model(input: ModelProcessingInput<'_>) -> Option<&str> {
    clean_model(input.processed_model)
}

fn clean_model(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|model| !model.is_empty())
}

fn transform_fallback(model: &str, config: ModelProcessingConfig<'_>) -> String {
    match config.fallback_transform {
        Some(transform) => transform(model),
        None => model.to_string(),
    }
}

fn model_mapping_target(rules: &[ModelMappingRule], model: &str) -> Option<String> {
    let source = model.trim().to_ascii_lowercase();
    if source.is_empty() {
        return None;
    }
    rules.iter().find_map(|rule| {
        if !rule.enabled || rule.source.trim().to_ascii_lowercase() != source {
            return None;
        }
        let target = rule.target.trim();
        (!target.is_empty()).then(|| target.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::config::ModelMappingRuleKind;

    fn rule(source: &str, target: &str) -> ModelMappingRule {
        ModelMappingRule {
            enabled: true,
            source: source.to_string(),
            target: target.to_string(),
            kind: ModelMappingRuleKind::Alias,
            note: None,
        }
    }

    fn input<'a>(original_model: &'a str, processed_model: &'a str) -> ModelProcessingInput<'a> {
        ModelProcessingInput {
            original_model: Some(original_model),
            processed_model: Some(processed_model),
        }
    }

    fn dash_numeric_dot(model: &str) -> String {
        model.replace("4.8", "4-8")
    }

    #[test]
    fn passthrough_keeps_original_model_without_mapping_or_transform() {
        let result = process_model(
            input("claude-opus-4.8", "claude-opus-4.7"),
            ModelProcessingConfig {
                mode: ModelProcessingMode::Passthrough,
                rules: &[rule("claude-opus-4.8", "external-opus")],
                require_mapping_match: true,
                fallback_transform: Some(dash_numeric_dot),
            },
        )
        .unwrap();

        assert_eq!(result.model, "claude-opus-4.8");
        assert_eq!(result.source, ModelProcessingSource::Original);
        assert!(!result.mapping_applied);
    }

    #[test]
    fn passthrough_mapping_maps_hit_and_preserves_original_on_miss() {
        let rules = [rule("claude-opus-4.8", "external-opus")];
        let hit = process_model(
            input("claude-opus-4.8", "claude-opus-4.7"),
            ModelProcessingConfig {
                mode: ModelProcessingMode::PassthroughMapping,
                rules: &rules,
                require_mapping_match: false,
                fallback_transform: Some(dash_numeric_dot),
            },
        )
        .unwrap();
        let miss = process_model(
            input("claude-sonnet-4.8", "claude-sonnet-4.7"),
            ModelProcessingConfig {
                mode: ModelProcessingMode::PassthroughMapping,
                rules: &rules,
                require_mapping_match: false,
                fallback_transform: Some(dash_numeric_dot),
            },
        )
        .unwrap();

        assert_eq!(hit.model, "external-opus");
        assert!(hit.mapping_applied);
        assert_eq!(miss.model, "claude-sonnet-4.8");
        assert_eq!(miss.source, ModelProcessingSource::Original);
    }

    #[test]
    fn require_mapping_match_rejects_misses() {
        let err = process_model(
            input("claude-sonnet-4.8", "claude-sonnet-4.7"),
            ModelProcessingConfig {
                mode: ModelProcessingMode::PassthroughMapping,
                rules: &[rule("claude-opus-4.8", "external-opus")],
                require_mapping_match: true,
                fallback_transform: None,
            },
        )
        .unwrap_err();

        assert_eq!(
            err,
            ModelProcessingError::MappingMiss {
                model: "claude-sonnet-4.8".to_string()
            }
        );
    }

    #[test]
    fn processed_then_mapping_maps_processed_model_and_falls_back_to_processed_transform() {
        let rules = [rule("claude-opus-4.7", "external-opus")];
        let hit = process_model(
            input("claude-opus-4.8", "claude-opus-4.7"),
            ModelProcessingConfig {
                mode: ModelProcessingMode::ProcessedThenMapping,
                rules: &rules,
                require_mapping_match: false,
                fallback_transform: Some(dash_numeric_dot),
            },
        )
        .unwrap();
        let miss = process_model(
            input("claude-sonnet-4.8", "claude-sonnet-4.8"),
            ModelProcessingConfig {
                mode: ModelProcessingMode::ProcessedThenMapping,
                rules: &rules,
                require_mapping_match: false,
                fallback_transform: Some(dash_numeric_dot),
            },
        )
        .unwrap();

        assert_eq!(hit.model, "external-opus");
        assert!(hit.mapping_applied);
        assert_eq!(miss.model, "claude-sonnet-4-8");
        assert_eq!(miss.source, ModelProcessingSource::Processed);
    }

    #[test]
    fn mapping_then_processed_maps_original_and_falls_back_to_processed_transform() {
        let rules = [rule("claude-opus-4.8", "external-opus")];
        let hit = process_model(
            input("claude-opus-4.8", "claude-opus-4.7"),
            ModelProcessingConfig {
                mode: ModelProcessingMode::MappingThenProcessed,
                rules: &rules,
                require_mapping_match: false,
                fallback_transform: Some(dash_numeric_dot),
            },
        )
        .unwrap();
        let miss = process_model(
            input("claude-sonnet-4.8", "claude-sonnet-4.8"),
            ModelProcessingConfig {
                mode: ModelProcessingMode::MappingThenProcessed,
                rules: &rules,
                require_mapping_match: false,
                fallback_transform: Some(dash_numeric_dot),
            },
        )
        .unwrap();

        assert_eq!(hit.model, "external-opus");
        assert_eq!(miss.model, "claude-sonnet-4-8");
        assert_eq!(miss.source, ModelProcessingSource::Processed);
    }
}
