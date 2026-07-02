//! Kiro ListAvailableModels response models.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct KiroAvailableModelsResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<KiroAvailableModel>,
    #[serde(default)]
    pub models: Vec<KiroAvailableModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct KiroAvailableModel {
    #[serde(default, alias = "id")]
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_input_types: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_limits: Option<KiroModelTokenLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_caching: Option<KiroModelPromptCaching>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_model_request_fields_schema: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_multiplier: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_unit: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct KiroModelTokenLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct KiroModelPromptCaching {
    #[serde(default)]
    pub supports_prompt_caching: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_cache_checkpoints_per_request: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_tokens_per_cache_checkpoint: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_cli_management_model_catalog_fields() {
        let body = r#"{
            "defaultModel": {"modelId": "auto"},
            "models": [{
                "modelId": "claude-opus-4.8",
                "modelName": "Claude Opus 4.8",
                "description": "test model",
                "supportedInputTypes": ["TEXT", "IMAGE"],
                "tokenLimits": {
                    "maxInputTokens": 1000000,
                    "maxOutputTokens": 64000
                },
                "promptCaching": {
                    "supportsPromptCaching": true,
                    "maximumCacheCheckpointsPerRequest": 4,
                    "minimumTokensPerCacheCheckpoint": 1024
                },
                "additionalModelRequestFieldsSchema": {
                    "type": "object",
                    "properties": {
                        "output_config": {"type": "object"}
                    }
                },
                "rateMultiplier": 1,
                "rateUnit": "Credit"
            }]
        }"#;

        let parsed: KiroAvailableModelsResponse = serde_json::from_str(body).unwrap();
        assert_eq!(
            parsed
                .default_model
                .as_ref()
                .map(|model| model.model_id.as_str()),
            Some("auto")
        );
        let model = parsed.models.first().unwrap();
        assert_eq!(model.model_id, "claude-opus-4.8");
        assert_eq!(
            model
                .additional_model_request_fields_schema
                .as_ref()
                .and_then(|schema| schema.pointer("/properties/output_config/type"))
                .and_then(|value| value.as_str()),
            Some("object")
        );
        assert_eq!(model.rate_multiplier, Some(1.0));
        assert_eq!(model.rate_unit.as_deref(), Some("Credit"));
        assert_eq!(
            model
                .prompt_caching
                .as_ref()
                .and_then(|cache| cache.minimum_tokens_per_cache_checkpoint),
            Some(1024)
        );
    }
}
