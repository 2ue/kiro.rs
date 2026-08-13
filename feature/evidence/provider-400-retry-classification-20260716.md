# Provider 400 Retry Classification Evidence

Status: `focused-real-http-harness-pass / cli-and-load-gates-pending`

Date: 2026-07-16

Source authority: HEAD `401473c` (`v0.0.109`) plus the dirty-tree remediation changes. Current test binary: `target/debug/deps/kiro_rs-d0eac30c038749e6`, SHA-256 `c8de4b66d935a89d0378598e28ce775b5212dfbb94e55ac5cab98b1fc77e4d61`. This hash identifies a test binary, not a release artifact.

## Reproduced Defect

The old classifier treated generic `REQUEST_BODY_INVALID` as `tool_use_format_bad_request`. With prompt-logic retry enabled, deterministic inputs such as `Image data cannot be empty` could rotate credentials. A second defect recorded the final budgeted tool failure as `prompt_logic_retry_next` even though no later HTTP send was possible.

## Selected Fix

Classification order is now prefill, profile ARN, explicit model unavailable, invalid model, invalid image, explicit tool/tool-schema, malformed body, generic request-body-invalid, then generic bad request. Model-unavailable retry requires model-specific wording or exact quoted JSON reasons `MODEL_UNAVAILABLE`/`MODEL_NOT_AVAILABLE`. Tool retry requires explicit tool-use or tool-schema semantics. The final available loop iteration is recorded as `fail`.

The implementation does not parse or rewrite successful request bodies. These checks execute only after a provider 400 body has already been read.

## Executed Matrix

The test starts an Axum server on a random localhost port and sends through the real `MultiTokenManager -> KiroProvider -> reqwest -> /generateAssistantResponse` path.

```text
cargo test bad_request_retry_matrix_bounds_real_provider_http_hits -- --nocapture
PASS

10 response classes x pools 1/20/60 x 5 rounds
150 provider requests
1,636 runtime assertions
240 exact inference HTTP hits
```

Seven deterministic classes, including invalid model, empty/unsupported image, generic body invalid, malformed body, and non-model endpoint wording, produced exactly one hit in every pool size. Explicit model-unavailable, tool-use, and tool-schema cases produced one hit for a one-account pool and at most four hits for larger pools. Every run checked the shared budget snapshot, local/external channel counts, unique credential count, attempt reason/action, and absence of auxiliary HTTP hits.

Additional focused checks:

```text
cargo test classifies_bad_request_protocol_reasons -- --nocapture
PASS: 24 fingerprints x 5 rounds = 120 assertions

cargo test prompt_logic_retry_only_applies_to_enabled_protocol_reasons -- --nocapture
PASS: 40 policy assertions

cargo check --tests
PASS

rustfmt --edition 2024 --check src/kiro/provider.rs
PASS

git diff --check -- src/kiro/provider.rs
PASS
```

The first full matrix run found the final-action bug on `invalid_tool`, pool 20, round 0. That failure is part of the evidence chain; the matrix passed only after the remaining-iteration guard was added.

## Remaining Gates

This is a real local HTTP provider harness, not official Kiro or Claude Code CLI evidence. D05 still requires the same classes through the isolated release service and real CLI, followed by recovery requests. Unknown future model-unavailable wording intentionally fails conservatively instead of rotating accounts. Client-originated retries, request-key multi-instance admission, auxiliary-call attribution, 429/500/partial failures, and load amplification remain separate gates.
