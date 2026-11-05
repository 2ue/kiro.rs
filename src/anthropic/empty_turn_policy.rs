//! 空回（上游 200 但整条响应没有任何文本、推理或工具调用）的对外语义与重试节奏。
//!
//! 空回只在上游流结束时按整条流判定一次，见 `stream::StreamContext::upstream_terminal_failure_detail`；
//! 这里只决定判定为空回之后：对外返回什么状态码，以及换号重试之间等多久。

use std::time::Duration;

use axum::http::StatusCode;

use crate::model::config::Config;

/// 空回对外的状态码与 error type。只允许 502 / 503 / 529，其它值按 502 处理。
pub(crate) fn empty_turn_status(config: &Config) -> (StatusCode, &'static str) {
    match config.empty_turn_error_status_code {
        529 => (
            StatusCode::from_u16(529).expect("529 is a valid status code"),
            "overloaded_error",
        ),
        503 => (StatusCode::SERVICE_UNAVAILABLE, "api_error"),
        _ => (StatusCode::BAD_GATEWAY, "api_error"),
    }
}

/// 空回换号重试的等待方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EmptyTurnBackoffMode {
    None,
    Fixed,
    Exponential,
}

impl EmptyTurnBackoffMode {
    pub(crate) fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "fixed" => Self::Fixed,
            "exponential" => Self::Exponential,
            _ => Self::None,
        }
    }
}

/// 第 `retry_index` 次空回重试（从 1 开始）前要等多久。抖动在 `±jitter%` 内均匀分布，
/// `jitter_roll` 取值 0~1（调用方传随机数，测试传定值）。
pub(crate) fn empty_turn_retry_delay(
    config: &Config,
    retry_index: u32,
    jitter_roll: f64,
) -> Duration {
    let base = config.empty_turn_retry_base_delay_ms.min(10_000);
    let max = config.empty_turn_retry_max_delay_ms.min(30_000);
    let raw_ms = match EmptyTurnBackoffMode::parse(&config.empty_turn_retry_backoff_mode) {
        EmptyTurnBackoffMode::None => return Duration::ZERO,
        EmptyTurnBackoffMode::Fixed => base,
        EmptyTurnBackoffMode::Exponential => {
            let shift = retry_index.saturating_sub(1).min(20);
            base.saturating_mul(1u64 << shift).min(max.max(base))
        }
    };
    if raw_ms == 0 {
        return Duration::ZERO;
    }
    let jitter = f64::from(config.empty_turn_retry_jitter_percent.min(100)) / 100.0;
    let factor = 1.0 + jitter * (jitter_roll.clamp(0.0, 1.0) * 2.0 - 1.0);
    Duration::from_millis(((raw_ms as f64) * factor).round().max(0.0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(mode: &str, base: u64, max: u64, jitter: u32) -> Config {
        let mut config = Config::default();
        config.empty_turn_retry_backoff_mode = mode.to_string();
        config.empty_turn_retry_base_delay_ms = base;
        config.empty_turn_retry_max_delay_ms = max;
        config.empty_turn_retry_jitter_percent = jitter;
        config
    }

    #[test]
    fn default_keeps_previous_behaviour() {
        let config = Config::default();
        assert_eq!(empty_turn_status(&config).0, StatusCode::BAD_GATEWAY);
        assert_eq!(empty_turn_retry_delay(&config, 1, 0.5), Duration::ZERO);
    }

    #[test]
    fn status_code_is_limited_to_allowed_values() {
        let mut config = Config::default();
        for (code, expected, error_type) in [
            (529, 529, "overloaded_error"),
            (503, 503, "api_error"),
            (502, 502, "api_error"),
            (200, 502, "api_error"),
            (418, 502, "api_error"),
        ] {
            config.empty_turn_error_status_code = code;
            let (status, kind) = empty_turn_status(&config);
            assert_eq!(status.as_u16(), expected, "config {code}");
            assert_eq!(kind, error_type);
        }
    }

    #[test]
    fn fixed_and_exponential_delays() {
        let fixed = config("fixed", 100, 2000, 0);
        assert_eq!(
            empty_turn_retry_delay(&fixed, 1, 0.5),
            Duration::from_millis(100)
        );
        assert_eq!(
            empty_turn_retry_delay(&fixed, 5, 0.5),
            Duration::from_millis(100)
        );

        let exp = config("exponential", 100, 1000, 0);
        let delays: Vec<u64> = (1..=6)
            .map(|index| empty_turn_retry_delay(&exp, index, 0.5).as_millis() as u64)
            .collect();
        assert_eq!(delays, vec![100, 200, 400, 800, 1000, 1000]);
    }

    #[test]
    fn jitter_stays_within_bounds() {
        let exp = config("fixed", 1000, 1000, 20);
        assert_eq!(
            empty_turn_retry_delay(&exp, 1, 0.0),
            Duration::from_millis(800)
        );
        assert_eq!(
            empty_turn_retry_delay(&exp, 1, 1.0),
            Duration::from_millis(1200)
        );
        assert_eq!(
            empty_turn_retry_delay(&exp, 1, 0.5),
            Duration::from_millis(1000)
        );
    }

    #[test]
    fn bad_values_are_clamped() {
        let huge = config("exponential", 1_000_000, 1_000_000, 500);
        let delay = empty_turn_retry_delay(&huge, 30, 1.0);
        assert!(delay <= Duration::from_millis(30_000 * 2), "{delay:?}");
        assert_eq!(
            empty_turn_retry_delay(&config("bogus", 100, 100, 0), 1, 0.5),
            Duration::ZERO
        );
    }
}
