//! LiteLLM 不可用时的内置 Anthropic Claude 价格快照。
//!
//! 数据来自 https://docs.anthropic.com/en/docs/about-claude/pricing(2026-01)。
//! 单位:美元 / token,即 美元/百万 token / 1_000_000。
//!
//! Schema 与 LiteLLM `model_prices_and_context_window.json` 兼容。

pub const BUILTIN_SNAPSHOT: &str = r#"{
  "claude-opus-4-7": {
    "max_input_tokens": 200000,
    "max_output_tokens": 32000,
    "input_cost_per_token": 0.000005,
    "output_cost_per_token": 0.000025,
    "cache_read_input_token_cost": 0.0000005,
    "cache_creation_input_token_cost": 0.00000625,
    "litellm_provider": "anthropic",
    "display_name": "Claude Opus 4.7"
  },
  "claude-opus-4-6": {
    "max_input_tokens": 200000,
    "max_output_tokens": 32000,
    "input_cost_per_token": 0.000005,
    "output_cost_per_token": 0.000025,
    "cache_read_input_token_cost": 0.0000005,
    "cache_creation_input_token_cost": 0.00000625,
    "litellm_provider": "anthropic",
    "display_name": "Claude Opus 4.6"
  },
  "claude-sonnet-4-6": {
    "max_input_tokens": 200000,
    "max_output_tokens": 64000,
    "input_cost_per_token": 0.000003,
    "output_cost_per_token": 0.000015,
    "cache_read_input_token_cost": 0.0000003,
    "cache_creation_input_token_cost": 0.00000375,
    "litellm_provider": "anthropic",
    "display_name": "Claude Sonnet 4.6"
  },
  "claude-sonnet-4-5": {
    "max_input_tokens": 200000,
    "max_output_tokens": 64000,
    "input_cost_per_token": 0.000003,
    "output_cost_per_token": 0.000015,
    "cache_read_input_token_cost": 0.0000003,
    "cache_creation_input_token_cost": 0.00000375,
    "litellm_provider": "anthropic",
    "display_name": "Claude Sonnet 4.5"
  },
  "claude-haiku-4-5": {
    "max_input_tokens": 200000,
    "max_output_tokens": 64000,
    "input_cost_per_token": 0.000001,
    "output_cost_per_token": 0.000005,
    "cache_read_input_token_cost": 0.0000001,
    "cache_creation_input_token_cost": 0.00000125,
    "litellm_provider": "anthropic",
    "display_name": "Claude Haiku 4.5"
  },
  "claude-3-5-sonnet-20241022": {
    "max_input_tokens": 200000,
    "max_output_tokens": 8192,
    "input_cost_per_token": 0.000003,
    "output_cost_per_token": 0.000015,
    "cache_read_input_token_cost": 0.0000003,
    "cache_creation_input_token_cost": 0.00000375,
    "litellm_provider": "anthropic",
    "display_name": "Claude 3.5 Sonnet"
  },
  "claude-3-5-haiku-20241022": {
    "max_input_tokens": 200000,
    "max_output_tokens": 8192,
    "input_cost_per_token": 0.000001,
    "output_cost_per_token": 0.000005,
    "cache_read_input_token_cost": 0.0000001,
    "cache_creation_input_token_cost": 0.00000125,
    "litellm_provider": "anthropic",
    "display_name": "Claude 3.5 Haiku"
  }
}"#;
