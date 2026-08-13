use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub cache_read_input_tokens: i32,
    pub cache_creation_input_tokens: i32,
}

impl TokenUsage {
    pub fn total_input_tokens(self) -> i32 {
        self.input_tokens
            .saturating_add(self.cache_read_input_tokens)
            .saturating_add(self.cache_creation_input_tokens)
            .max(0)
    }

    pub fn normalized(self) -> Self {
        Self {
            input_tokens: self.input_tokens.max(0),
            output_tokens: self.output_tokens.max(0),
            cache_read_input_tokens: self.cache_read_input_tokens.max(0),
            cache_creation_input_tokens: self.cache_creation_input_tokens.max(0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageConfidence {
    Exact,
    ProviderReported,
    Estimated,
    Partial,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawUsageFacts {
    pub usage: Option<TokenUsage>,
    pub confidence: UsageConfidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl RawUsageFacts {
    pub fn unknown() -> Self {
        Self {
            usage: None,
            confidence: UsageConfidence::Unknown,
            source: None,
        }
    }

    pub fn provider_reported(usage: TokenUsage, source: impl Into<String>) -> Self {
        Self {
            usage: Some(usage.normalized()),
            confidence: UsageConfidence::ProviderReported,
            source: Some(source.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_usage_normalizes_negative_values() {
        let usage = TokenUsage {
            input_tokens: -1,
            output_tokens: 2,
            cache_read_input_tokens: -3,
            cache_creation_input_tokens: 4,
        }
        .normalized();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 2);
        assert_eq!(usage.cache_read_input_tokens, 0);
        assert_eq!(usage.cache_creation_input_tokens, 4);
        assert_eq!(usage.total_input_tokens(), 4);
    }

    #[test]
    fn unknown_usage_keeps_absent_usage_distinct_from_zero() {
        let usage = RawUsageFacts::unknown();
        assert_eq!(usage.usage, None);
        assert_eq!(usage.confidence, UsageConfidence::Unknown);
    }
}
