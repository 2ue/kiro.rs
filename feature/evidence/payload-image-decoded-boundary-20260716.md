# Payload Image Decoded-Byte Boundary Evidence

Date: 2026-07-16

Status: focused implementation evidence; not a full payload, multimodal, CLI, or release gate

Build identity: dirty working tree based on `401473c` / `v0.0.109`; final revision and release binary hash are not assigned yet

## Reproduced Defect

The focused test constructs valid standard-base64 payloads from a requested decoded byte count. Before the fix:

```text
cargo test image_source_size_uses_decoded_base64_bytes -- --nocapture
FAILED
left: 6990508
right: 5242879
```

The left value is the encoded base64 length for a `5 MiB - 1` byte source. This proves the 5 MiB image rule was applied to encoded characters rather than decoded image bytes. It was therefore possible to reject a valid image roughly one quarter earlier than the documented upstream boundary.

## Implementation

- Compute exact decoded length in one scan without allocating a decoded copy.
- Support standard padded/unpadded base64, ASCII whitespace and inline `data:*;base64,` payloads.
- Keep invalid input fail-closed by falling back to encoded source size; normal local conversion already rejects invalid base64 and malformed image bytes before routing.
- Apply the same decoded-byte contract to normalized Anthropic and converted Kiro image sources.
- Collapse multiple oversized images in one Anthropic message to one ordered singular/plural summary placeholder while preserving surrounding text blocks.
- Keep `payloadGuardMaxBytes` as the documented soft shaping target; this change does not turn it into an unrelated hard reject.

## Focused Verification

```text
cargo test image_source_size_uses_decoded_base64_bytes -- --nocapture
1/1 passed

cargo test anthropic::payload_guard::tests -- --nocapture
58/58 passed
```

Covered in the focused suite:

- decoded `5 MiB - 1`, exactly `5 MiB`, and `5 MiB + 1`;
- plain base64 and data URL with whitespace;
- Anthropic and Kiro source representations;
- exact 5 MiB acceptance for three rounds;
- oversized history/current drop and reject policies;
- two oversized current images produce one plural summary while text before/after remains.

## Remaining Gates

- Real structurally valid PNG/JPEG/WebP/GIF fixtures at boundary sizes, five rounds each.
- Invalid/truncated/mismatched MIME and empty image end-to-end checks with zero upstream hits.
- Raw/direct/external/fallback routes and actual Claude Code CLI image input.
- 50 MiB router body limit `413`, zero-upstream and burst-memory verification.
- CPU/RSS comparison for 1/5/50 MiB body handling and combined payload shaping.
