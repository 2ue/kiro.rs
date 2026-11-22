use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::anthropic::cache::CacheUsage;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawUsageSource {
    #[default]
    LegacyMax,
    UpstreamLast,
}

impl RawUsageSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LegacyMax => "legacy_max",
            Self::UpstreamLast => "upstream_last",
        }
    }

    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "legacy_max" => Ok(Self::LegacyMax),
            "upstream_last" => Ok(Self::UpstreamLast),
            _ => anyhow::bail!("raw_usage_source 值无效: {value}"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct UpstreamUsage {
    usage: Option<CacheUsage>,
}

fn tokens(value: &Value, key: &str) -> Option<i32> {
    value
        .get(key)?
        .as_u64()
        .map(|n| n.min(i32::MAX as u64) as i32)
}

impl UpstreamUsage {
    pub(crate) fn snapshot(&self) -> Option<CacheUsage> {
        self.usage
    }

    pub(crate) fn observe(&mut self, value: &Value) {
        let input = tokens(value, "input_tokens").or_else(|| tokens(value, "prompt_tokens"));
        let output = tokens(value, "output_tokens").or_else(|| tokens(value, "completion_tokens"));
        let read = tokens(value, "cache_read_input_tokens");
        let total = tokens(value, "cache_creation_input_tokens");
        let breakdown = &value["cache_creation"];
        let short = tokens(breakdown, "ephemeral_5m_input_tokens")
            .or_else(|| tokens(value, "cache_creation_5m_input_tokens"));
        let long = tokens(breakdown, "ephemeral_1h_input_tokens")
            .or_else(|| tokens(value, "cache_creation_1h_input_tokens"));
        if [input, output, read, total, short, long]
            .iter()
            .all(Option::is_none)
        {
            return;
        }
        let usage = self.usage.get_or_insert(CacheUsage {
            total_input_tokens: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        });
        if let Some(n) = input {
            usage.input_tokens = n;
        }
        if let Some(n) = output {
            usage.output_tokens = n;
        }
        if let Some(n) = read {
            usage.cache_read_input_tokens = n;
        }
        if total.is_some() || short.is_some() || long.is_some() {
            // A revised total invalidates old TTL evidence; an unchanged total retains missing TTL fields.
            if total.is_some_and(|n| n != usage.cache_creation_input_tokens) {
                usage.cache_creation_5m_input_tokens = short.unwrap_or(0);
                usage.cache_creation_1h_input_tokens = long.unwrap_or(0);
            } else {
                if let Some(n) = short {
                    usage.cache_creation_5m_input_tokens = n;
                }
                if let Some(n) = long {
                    usage.cache_creation_1h_input_tokens = n;
                }
            }
            usage.cache_creation_input_tokens = total.unwrap_or_else(|| {
                usage
                    .cache_creation_5m_input_tokens
                    .saturating_add(usage.cache_creation_1h_input_tokens)
            });
            usage.cache_creation_5m_input_tokens = usage
                .cache_creation_5m_input_tokens
                .min(usage.cache_creation_input_tokens);
            usage.cache_creation_1h_input_tokens = usage
                .cache_creation_1h_input_tokens
                .min(usage.cache_creation_input_tokens - usage.cache_creation_5m_input_tokens);
        }
        usage.total_input_tokens = usage
            .input_tokens
            .saturating_add(usage.cache_read_input_tokens)
            .saturating_add(usage.cache_creation_input_tokens);
    }

    pub(crate) fn observe_response(&mut self, value: &Value) {
        for path in [
            "/usage",
            "/message/usage",
            "/delta/usage",
            "/data/usage",
            "/response/usage",
        ] {
            if let Some(usage) = value.pointer(path) {
                let mut candidate = Self::default();
                candidate.observe(usage);
                if candidate.usage.is_some() {
                    self.observe(usage);
                    return;
                }
            }
        }
    }

    pub(crate) fn observe_sse(&mut self, event: &[u8]) {
        if !event.windows(7).any(|part| part == b"\"usage\"") {
            return;
        }
        let Ok(text) = std::str::from_utf8(event) else {
            return;
        };
        let data = text
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(|line| line.strip_prefix(' ').unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n");
        if let Ok(value) = serde_json::from_str::<Value>(&data) {
            self.observe_response(&value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn upstream_last_keeps_missing_and_accepts_explicit_zero() {
        let mut usage = UpstreamUsage::default();
        usage.observe(&json!({"input_tokens":262393,"output_tokens":0}));
        usage.observe(&json!({"input_tokens":1,"output_tokens":75,"cache_read_input_tokens":97321,
            "cache_creation_input_tokens":20816,"cache_creation":{"ephemeral_1h_input_tokens":20816,"ephemeral_5m_input_tokens":0}}));
        let final_usage = usage.snapshot().unwrap();
        assert_eq!(final_usage.input_tokens, 1);
        assert_eq!(final_usage.total_input_tokens, 118138);
        usage.observe(&json!({"output_tokens":0,"input_tokens":null,"cache_read_input_tokens":-1}));
        let final_usage = usage.snapshot().unwrap();
        assert_eq!(final_usage.input_tokens, 1);
        assert_eq!(final_usage.output_tokens, 0);
        assert_eq!(final_usage.cache_read_input_tokens, 97321);
        usage.observe(&json!({"input_tokens":0}));
        assert_eq!(usage.snapshot().unwrap().input_tokens, 0);
        let mut zero = UpstreamUsage::default();
        zero.observe(&json!({"input_tokens":0,"output_tokens":0}));
        assert!(zero.snapshot().is_some());
    }

    #[test]
    fn upstream_last_cache_revisions_remove_stale_ttl_evidence() {
        let mut usage = UpstreamUsage::default();
        usage.observe(&json!({"cache_creation":{"ephemeral_5m_input_tokens":10,"ephemeral_1h_input_tokens":20}}));
        usage.observe(&json!({"cache_creation_input_tokens":30}));
        assert_eq!(usage.snapshot().unwrap().cache_creation_1h_input_tokens, 20);
        usage.observe(&json!({"cache_creation":{"ephemeral_1h_input_tokens":5}}));
        assert_eq!(usage.snapshot().unwrap().cache_creation_input_tokens, 15);
        usage.observe(&json!({"cache_creation_input_tokens":3}));
        let snapshot = usage.snapshot().unwrap();
        assert_eq!(snapshot.cache_creation_input_tokens, 3);
        assert_eq!(snapshot.cache_creation_1h_input_tokens, 0);
        assert_eq!(snapshot.cache_creation_5m_input_tokens, 0);
        usage.observe(&json!({"cache_creation_input_tokens":2,"cache_creation":{"ephemeral_1h_input_tokens":10}}));
        assert_eq!(usage.snapshot().unwrap().cache_creation_1h_input_tokens, 2);
    }

    #[test]
    fn upstream_last_supported_slots_do_not_descend_into_iterations() {
        let mut usage = UpstreamUsage::default();
        usage.observe_sse(b"event: message_start\r\ndata: {\"message\":{\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":1}}}\r\n\r\n");
        usage.observe_response(&json!({"usage":{"input_tokens":1,"output_tokens":5,"iterations":[{"input_tokens":999999}]}}));
        usage.observe_response(
            &json!({"usage":{"input_tokens":"bad"},"response":{"usage":{"output_tokens":8}}}),
        );
        assert_eq!(usage.snapshot().unwrap().input_tokens, 1);
        assert_eq!(usage.snapshot().unwrap().output_tokens, 8);
        let mut other_attempt = UpstreamUsage::default();
        other_attempt.observe_response(&json!({"message":{"usage":{"input_tokens":0}}}));
        assert_eq!(other_attempt.snapshot().unwrap().input_tokens, 0);
        assert_eq!(usage.snapshot().unwrap().input_tokens, 1);
    }
}

#[cfg(test)]
mod replay_tests {
    use super::*;

    #[test]
    fn upstream_last_replays_171_redacted_real_responses() {
        let samples: Vec<Value> =
            serde_json::from_str(include_str!("fixtures/upstream-usage-171.json")).unwrap();
        assert_eq!(samples.len(), 171);
        let mut changed = 0;
        for sample in samples {
            let mut capture = UpstreamUsage::default();
            for usage in sample["usage"].as_array().unwrap() {
                capture.observe(usage);
            }
            let final_usage = capture.snapshot().unwrap();
            let saved = &sample["legacy"];
            let fields = [
                ("inputTokens", final_usage.input_tokens),
                ("outputTokens", final_usage.output_tokens),
                ("cacheReadInputTokens", final_usage.cache_read_input_tokens),
                (
                    "cacheCreationInputTokens",
                    final_usage.cache_creation_input_tokens,
                ),
                (
                    "cacheCreation5mInputTokens",
                    final_usage.cache_creation_5m_input_tokens,
                ),
                (
                    "cacheCreation1hInputTokens",
                    final_usage.cache_creation_1h_input_tokens,
                ),
            ];
            let differs = fields
                .iter()
                .any(|(key, value)| saved[*key].as_i64().unwrap_or(0) != i64::from(*value));
            if differs {
                changed += 1;
                assert_eq!(final_usage.input_tokens, 1);
                for (key, value) in &fields[1..] {
                    assert_eq!(
                        saved[*key].as_i64().unwrap_or(0),
                        i64::from(*value),
                        "{key}"
                    );
                }
            }
        }
        assert_eq!(changed, 16);
    }
}
