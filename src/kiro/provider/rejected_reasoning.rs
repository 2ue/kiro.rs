//! Historical thinking signatures the upstream rejected, remembered per conversation.
//!
//! When a request fails with `THINKING_SIGNATURE_INVALID`, the provider retries once on the same
//! credential with every historical `reasoningContent` removed. The client keeps sending the same
//! history, so without memory every later turn of that conversation repeats the failed send
//! before the stripped retry: one wasted upstream call per turn, and a run of errors on the
//! account the session is bound to. The signatures present when the upstream rejected the
//! history are remembered here, and later requests of the conversation drop exactly those
//! reasoning blocks before the first send. Signatures issued after the rejection are kept.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::Value;

/// Conversations remembered at once; the least recently touched one is dropped beyond this.
const MAX_CONVERSATIONS: usize = 4096;
/// A conversation untouched for this long is forgotten.
const CONVERSATION_TTL: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Default)]
pub(super) struct RejectedReasoningSignatures {
    conversations: Mutex<HashMap<String, RejectedConversation>>,
}

struct RejectedConversation {
    signatures: HashSet<u64>,
    touched_at: Instant,
}

impl RejectedReasoningSignatures {
    /// Remembers every historical reasoning signature of `request_body`, a Kiro request the
    /// upstream rejected with `THINKING_SIGNATURE_INVALID`. Returns how many were added.
    pub(super) fn remember(&self, conversation_id: &str, request_body: &str) -> usize {
        let Ok(body) = serde_json::from_str::<Value>(request_body) else {
            return 0;
        };
        let signatures: Vec<u64> = history_reasoning_signatures(&body)
            .map(signature_key)
            .collect();
        if signatures.is_empty() {
            return 0;
        }
        let now = Instant::now();
        let mut conversations = self.conversations.lock();
        if !conversations.contains_key(conversation_id) {
            conversations
                .retain(|_, entry| now.duration_since(entry.touched_at) < CONVERSATION_TTL);
            if conversations.len() >= MAX_CONVERSATIONS
                && let Some(oldest) = conversations
                    .iter()
                    .min_by_key(|(_, entry)| entry.touched_at)
                    .map(|(id, _)| id.clone())
            {
                conversations.remove(&oldest);
            }
        }
        let entry = conversations
            .entry(conversation_id.to_string())
            .or_insert_with(|| RejectedConversation {
                signatures: HashSet::new(),
                touched_at: now,
            });
        entry.touched_at = now;
        signatures
            .into_iter()
            .filter(|signature| entry.signatures.insert(*signature))
            .count()
    }

    /// `request_body` without the historical reasoning blocks whose signatures the upstream
    /// already rejected in this conversation; `None` when nothing has to be removed.
    pub(super) fn strip(&self, conversation_id: &str, request_body: &str) -> Option<String> {
        let rejected = {
            let mut conversations = self.conversations.lock();
            let entry = conversations.get_mut(conversation_id)?;
            if entry.touched_at.elapsed() >= CONVERSATION_TTL {
                conversations.remove(conversation_id);
                return None;
            }
            entry.touched_at = Instant::now();
            entry.signatures.clone()
        };
        let mut body = serde_json::from_str::<Value>(request_body).ok()?;
        let history = body
            .pointer_mut("/conversationState/history")
            .and_then(Value::as_array_mut)?;
        let mut removed = 0usize;
        for message in history {
            let Some(assistant) = message
                .get_mut("assistantResponseMessage")
                .and_then(Value::as_object_mut)
            else {
                continue;
            };
            let rejected_here = assistant
                .get("reasoningContent")
                .and_then(reasoning_signature)
                .is_some_and(|signature| rejected.contains(&signature_key(signature)));
            if rejected_here {
                assistant.remove("reasoningContent");
                removed += 1;
            }
        }
        if removed == 0 {
            return None;
        }
        tracing::debug!(
            conversation_id,
            removed,
            "dropped historical reasoning whose signatures the upstream already rejected"
        );
        serde_json::to_string(&body).ok()
    }
}

fn history_reasoning_signatures(body: &Value) -> impl Iterator<Item = &str> {
    body.pointer("/conversationState/history")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|message| {
            message
                .pointer("/assistantResponseMessage/reasoningContent")
                .and_then(reasoning_signature)
        })
}

fn reasoning_signature(reasoning: &Value) -> Option<&str> {
    reasoning
        .pointer("/reasoningText/signature")
        .and_then(Value::as_str)
        .filter(|signature| !signature.is_empty())
}

fn signature_key(signature: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    signature.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body(signatures: &[Option<&str>]) -> String {
        let mut history = vec![json!({"userInputMessage": {"content": "hi", "modelId": "m"}})];
        for (index, signature) in signatures.iter().enumerate() {
            let mut assistant = json!({"content": format!("answer {index}")});
            if let Some(signature) = signature {
                assistant["reasoningContent"] = json!({"reasoningText": {"text": format!("thought {index}"), "signature": signature}});
            }
            history.push(json!({"assistantResponseMessage": assistant}));
            history.push(json!({"userInputMessage": {"content": "next", "modelId": "m"}}));
        }
        json!({"conversationState": {"conversationId": "c", "history": history}}).to_string()
    }

    fn signatures_of(body: &str) -> Vec<String> {
        let value: Value = serde_json::from_str(body).unwrap();
        history_reasoning_signatures(&value)
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn later_turns_drop_only_the_rejected_signatures() {
        let memory = RejectedReasoningSignatures::default();
        let rejected = body(&[Some("old-a"), None, Some("old-b")]);
        assert!(memory.strip("c", &rejected).is_none());
        assert_eq!(memory.remember("c", &rejected), 2);

        // The next turn carries the same history plus a signature issued after the rejection.
        let next = body(&[Some("old-a"), None, Some("old-b"), Some("new-c")]);
        let stripped = memory
            .strip("c", &next)
            .expect("rejected signatures are dropped");
        assert_eq!(signatures_of(&stripped), vec!["new-c".to_string()]);
        let value: Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(
            value["conversationState"]["history"]
                .as_array()
                .unwrap()
                .len(),
            9
        );

        // Other conversations, and bodies without rejected signatures, are left alone.
        assert!(memory.strip("other", &next).is_none());
        assert!(memory.strip("c", &body(&[Some("new-c")])).is_none());
        assert_eq!(memory.remember("c", &next), 1);
    }

    #[test]
    fn bodies_without_signed_history_are_not_remembered() {
        let memory = RejectedReasoningSignatures::default();
        assert_eq!(memory.remember("c", &body(&[None, None])), 0);
        assert_eq!(memory.remember("c", "not json"), 0);
        assert!(memory.strip("c", &body(&[Some("x")])).is_none());
    }

    #[test]
    fn memory_is_bounded() {
        let memory = RejectedReasoningSignatures::default();
        let rejected = body(&[Some("sig")]);
        for index in 0..MAX_CONVERSATIONS + 10 {
            memory.remember(&format!("c{index}"), &rejected);
        }
        assert_eq!(memory.conversations.lock().len(), MAX_CONVERSATIONS);
        assert!(
            memory
                .strip(&format!("c{}", MAX_CONVERSATIONS + 9), &rejected)
                .is_some()
        );
    }
}
