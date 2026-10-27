//! Anthropic thinking signature compatibility helpers.
//!
//! Kiro prompt-steered thinking has no upstream opaque signature.  Claude Code still
//! expects a non-empty signature field on every thinking block, so the Claude Code
//! compatibility profile emits a deterministic transport marker.  The marker is
//! base64 encoded and is recognized only on the request path; it is never sent to
//! Kiro as native reasoning content.

use std::sync::LazyLock;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256, Sha512};

/// Plaintext marker of signatures issued by earlier releases. Still recognized so already-issued
/// signatures replay; never emitted again because it names the proxy after base64 decoding.
const LEGACY_SYNTHETIC_MARKER: &[u8] = b"kiro-rs-compat-thinking-signature-v1\0";

/// Marker of new signatures: opaque bytes derived from a hash, same length as the legacy
/// marker so signature length is unchanged.
static SYNTHETIC_MARKER: LazyLock<[u8; LEGACY_SYNTHETIC_MARKER.len()]> = LazyLock::new(|| {
    let digest = Sha512::digest(b"synthetic thinking signature marker v2");
    let mut marker = [0u8; LEGACY_SYNTHETIC_MARKER.len()];
    marker.copy_from_slice(&digest[..LEGACY_SYNTHETIC_MARKER.len()]);
    marker
});

pub(crate) fn synthetic_thinking_signature(thinking: &str) -> String {
    signature_with_marker(SYNTHETIC_MARKER.as_slice(), thinking)
}

fn signature_with_marker(marker: &[u8], thinking: &str) -> String {
    let digest = Sha256::digest(thinking.as_bytes());
    let mut payload = Vec::with_capacity(marker.len() + digest.len());
    payload.extend_from_slice(marker);
    payload.extend_from_slice(&digest);
    STANDARD.encode(payload)
}

pub(crate) fn synthetic_thinking_signature_matches(thinking: &str, signature: &str) -> bool {
    decode_synthetic(signature)
        .is_some_and(|digest| digest == Sha256::digest(thinking.as_bytes()).as_slice())
}

pub(crate) fn is_synthetic_thinking_signature(signature: &str) -> bool {
    decode_synthetic(signature).is_some()
}

/// Returns the thinking digest of a synthetic signature carrying the current or legacy marker.
fn decode_synthetic(signature: &str) -> Option<Vec<u8>> {
    let decoded = STANDARD.decode(signature).ok()?;
    [SYNTHETIC_MARKER.as_slice(), LEGACY_SYNTHETIC_MARKER]
        .into_iter()
        .find_map(|marker| {
            let digest = decoded.strip_prefix(marker)?;
            (digest.len() == Sha256::output_size()).then(|| digest.to_vec())
        })
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

    #[test]
    fn new_signature_does_not_reveal_the_proxy() {
        let signature = synthetic_thinking_signature("private thought");
        let decoded = STANDARD.decode(&signature).expect("base64");
        let text = String::from_utf8_lossy(&decoded).to_ascii_lowercase();
        for leak in ["kiro", "compat", "signature", "thinking"] {
            assert!(!text.contains(leak), "decoded signature reveals {leak:?}");
        }
        assert!(!decoded.starts_with(LEGACY_SYNTHETIC_MARKER));
        // Same length as signatures issued with the legacy marker.
        assert_eq!(
            signature.len(),
            signature_with_marker(LEGACY_SYNTHETIC_MARKER, "private thought").len()
        );
    }

    #[test]
    fn legacy_marker_signatures_still_replay() {
        let legacy = signature_with_marker(LEGACY_SYNTHETIC_MARKER, "earlier thought");
        assert!(is_synthetic_thinking_signature(&legacy));
        assert!(synthetic_thinking_signature_matches(
            "earlier thought",
            &legacy
        ));
        assert!(!synthetic_thinking_signature_matches(
            "changed thought",
            &legacy
        ));
        assert_ne!(legacy, synthetic_thinking_signature("earlier thought"));

        // Marker alone, or marker with a short digest, is not a signature.
        let truncated = STANDARD.encode(LEGACY_SYNTHETIC_MARKER);
        assert!(!is_synthetic_thinking_signature(&truncated));
        let truncated = STANDARD.encode(SYNTHETIC_MARKER.as_slice());
        assert!(!is_synthetic_thinking_signature(&truncated));
    }
}
