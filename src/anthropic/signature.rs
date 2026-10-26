//! Anthropic thinking signature compatibility helpers.
//!
//! Kiro prompt-steered thinking has no upstream opaque signature.  Claude Code still
//! expects a non-empty signature field on every thinking block, so the Claude Code
//! compatibility profile emits a deterministic transport marker.  The marker is
//! base64 encoded and is recognized only on the request path; it is never sent to
//! Kiro as native reasoning content.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};

const SYNTHETIC_MARKER: &[u8] = b"kiro-rs-compat-thinking-signature-v1\0";

pub(crate) fn synthetic_thinking_signature(thinking: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(thinking.as_bytes());

    let digest = hasher.finalize();
    let mut payload = Vec::with_capacity(SYNTHETIC_MARKER.len() + digest.len());
    payload.extend_from_slice(SYNTHETIC_MARKER);
    payload.extend_from_slice(&digest);
    STANDARD.encode(payload)
}

pub(crate) fn synthetic_thinking_signature_matches(thinking: &str, signature: &str) -> bool {
    is_synthetic_thinking_signature(signature)
        && synthetic_thinking_signature(thinking) == signature
}

pub(crate) fn is_synthetic_thinking_signature(signature: &str) -> bool {
    let Ok(decoded) = STANDARD.decode(signature) else {
        return false;
    };
    decoded.starts_with(SYNTHETIC_MARKER)
        && decoded.len() == SYNTHETIC_MARKER.len() + Sha256::output_size()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_signature_is_deterministic_and_recognizable() {
        let first = synthetic_thinking_signature("private thought");
        let second = synthetic_thinking_signature("private thought");
        assert_eq!(first, second);
        assert!(is_synthetic_thinking_signature(&first));
        assert!(synthetic_thinking_signature_matches(
            "private thought",
            &first
        ));
        assert!(!synthetic_thinking_signature_matches(
            "tampered thought",
            &first
        ));
        assert!(!is_synthetic_thinking_signature(
            "opaque-upstream-signature"
        ));
        assert_ne!(first, synthetic_thinking_signature("different thought"));
    }
}
