//! Protocol-level auto-compaction signal for Claude Code clients.
//!
//! Claude Code decides to compact from the usage it receives. Route policies may reshape the
//! reported usage fields for billing, so the client can stop seeing how full the real context
//! is. The usage fields stay untouched; instead the real context size observed from Kiro for a
//! conversation is remembered here, and once it crosses the configured share of the model window
//! the next request of that conversation is answered with the Claude Code protocol
//! `prompt is too long: N tokens > M maximum` error. Claude Code reacts by compacting and
//! retrying, and the compaction request itself is always let through.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::types::MessagesRequest;

/// Opening sentence of Claude Code's compaction prompt. Requests carrying it must never be
/// rejected by the signal, otherwise compaction could never make progress.
const CLAUDE_CODE_COMPACTION_MARKER: &str = "create a detailed summary of the conversation";

const MAX_TRACKED_CONVERSATIONS: usize = 20_000;
const OBSERVATION_TTL: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Debug, Clone, Copy)]
struct Observation {
    context_tokens: i32,
    context_window_tokens: i32,
    observed_at: Instant,
}

fn compaction_request_ids() -> &'static Mutex<HashMap<String, Instant>> {
    static IDS: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    IDS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Marks a request as Claude Code's compaction request. Its own (large) context is not recorded,
/// and the conversation's observation is cleared once it completes.
pub(crate) fn mark_compaction_request(request_id: &str) {
    let now = Instant::now();
    let mut ids = compaction_request_ids()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    ids.retain(|_, marked_at| now.duration_since(*marked_at) < OBSERVATION_TTL);
    ids.insert(request_id.to_string(), now);
}

fn observations() -> &'static Mutex<HashMap<String, Observation>> {
    static OBSERVATIONS: OnceLock<Mutex<HashMap<String, Observation>>> = OnceLock::new();
    OBSERVATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Records the real context size Kiro reported for the latest successful turn of a conversation.
pub(crate) fn observe(
    conversation_id: &str,
    request_id: &str,
    context_tokens: i32,
    context_window_tokens: i32,
) {
    if compaction_request_ids()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(request_id)
        .is_some()
    {
        reset(conversation_id);
        return;
    }
    if conversation_id.is_empty() || context_tokens <= 0 || context_window_tokens <= 0 {
        return;
    }
    let now = Instant::now();
    let mut map = observations()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if map.len() >= MAX_TRACKED_CONVERSATIONS && !map.contains_key(conversation_id) {
        map.retain(|_, observation| now.duration_since(observation.observed_at) < OBSERVATION_TTL);
        if map.len() >= MAX_TRACKED_CONVERSATIONS {
            if let Some(oldest) = map
                .iter()
                .min_by_key(|(_, observation)| observation.observed_at)
                .map(|(key, _)| key.clone())
            {
                map.remove(&oldest);
            }
        }
    }
    map.insert(
        conversation_id.to_string(),
        Observation {
            context_tokens,
            context_window_tokens,
            observed_at: now,
        },
    );
}

/// Forgets a conversation, used after a compaction request so the next turn starts fresh.
pub(crate) fn reset(conversation_id: &str) {
    observations()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(conversation_id);
}

/// Whether the request is Claude Code's own compaction request.
pub(crate) fn is_compaction_request(payload: &MessagesRequest) -> bool {
    payload.messages.last().is_some_and(|message| {
        message.role == "user"
            && message
                .content
                .to_string()
                .to_ascii_lowercase()
                .contains(CLAUDE_CODE_COMPACTION_MARKER)
    })
}

/// Token figures for the `prompt is too long` signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CompactSignal {
    pub observed_context_tokens: i32,
    pub signal_threshold_tokens: i32,
}

/// Returns the signal to send when the conversation's last observed real context already
/// reaches `trigger_ratio` of the model window. Single-message requests are never signalled
/// because the client has nothing to compact.
pub(crate) fn pending_signal(
    conversation_id: &str,
    payload: &MessagesRequest,
    context_window_tokens: i32,
    trigger_ratio: f64,
) -> Option<CompactSignal> {
    if payload.messages.len() < 2 || is_compaction_request(payload) {
        return None;
    }
    if !(trigger_ratio.is_finite() && trigger_ratio > 0.0 && trigger_ratio < 1.0) {
        return None;
    }
    let observation = *observations()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(conversation_id)?;
    if observation.observed_at.elapsed() >= OBSERVATION_TTL {
        return None;
    }
    let window = if context_window_tokens > 0 {
        context_window_tokens
    } else {
        observation.context_window_tokens
    };
    let threshold = (f64::from(window) * trigger_ratio).floor() as i32;
    (threshold > 0 && observation.context_tokens >= threshold).then_some(CompactSignal {
        observed_context_tokens: observation.context_tokens,
        signal_threshold_tokens: threshold,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(messages: serde_json::Value) -> MessagesRequest {
        serde_json::from_value(serde_json::json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 16,
            "messages": messages
        }))
        .unwrap()
    }

    fn conversation() -> serde_json::Value {
        serde_json::json!([
            {"role": "user", "content": "first"},
            {"role": "assistant", "content": "ok"},
            {"role": "user", "content": "next"}
        ])
    }

    #[test]
    fn signals_only_after_real_context_crosses_the_threshold_for_five_rounds() {
        for round in 0..5 {
            let id = format!("compact-signal-threshold-{round}");
            let payload = request(conversation());
            assert_eq!(
                pending_signal(&id, &payload, 200_000, 0.9),
                None,
                "round {round}"
            );
            observe(&id, "req-a", 150_000, 200_000);
            assert_eq!(
                pending_signal(&id, &payload, 200_000, 0.9),
                None,
                "round {round}"
            );
            observe(&id, "req-b", 185_000, 200_000);
            assert_eq!(
                pending_signal(&id, &payload, 200_000, 0.9),
                Some(CompactSignal {
                    observed_context_tokens: 185_000,
                    signal_threshold_tokens: 180_000,
                }),
                "round {round}"
            );
            reset(&id);
            assert_eq!(
                pending_signal(&id, &payload, 200_000, 0.9),
                None,
                "round {round}"
            );
        }
    }

    #[test]
    fn completed_compaction_request_clears_the_conversation() {
        let id = "compact-signal-after-compaction";
        observe(id, "req-before", 190_000, 200_000);
        mark_compaction_request("req-compaction");
        observe(id, "req-compaction", 195_000, 200_000);
        assert_eq!(
            pending_signal(id, &request(conversation()), 200_000, 0.9),
            None
        );
    }

    #[test]
    fn compaction_and_single_message_requests_are_never_signalled() {
        let id = "compact-signal-exempt";
        observe(id, "req-c", 990_000, 1_000_000);
        let compaction = request(serde_json::json!([
            {"role": "user", "content": "first"},
            {"role": "assistant", "content": "ok"},
            {"role": "user", "content": [{"type": "text", "text": "Your task is to create a detailed summary of the conversation so far."}]}
        ]));
        assert!(is_compaction_request(&compaction));
        assert_eq!(pending_signal(id, &compaction, 1_000_000, 0.9), None);
        let single = request(serde_json::json!([{"role": "user", "content": "hi"}]));
        assert_eq!(pending_signal(id, &single, 1_000_000, 0.9), None);
        assert_eq!(
            pending_signal(id, &request(conversation()), 1_000_000, 1.5),
            None
        );
        assert!(pending_signal(id, &request(conversation()), 1_000_000, 0.9).is_some());
    }
}
