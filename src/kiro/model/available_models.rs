//! Kiro ListAvailableModels response models.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct KiroAvailableModelsResponse {
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
