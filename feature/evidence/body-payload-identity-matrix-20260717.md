# Body / Payload Identity Matrix Evidence - 2026-07-17

Status: `dirty-tree-focused-and-release-probes-pass / final-frozen-candidate-pending`

Scope: body ownership and byte/value identity across payload guard, JSON whitespace compression, request-body limits, remote multimodal preprocessing, external raw/normalized bodies, provider wire bytes, and Kiro CLI/IDE endpoint transforms.

## Build And Safety Boundary

- Toolchain: Rust `1.92.0`.
- Build mode: one debug + release logical batch through `feature/tests/run-cargo-scoped.sh body-payload-identity` with `CARGO_INCREMENTAL=0`.
- Candidate identity: shared dirty working tree; this is not a frozen release binary or tag.
- Network boundary: local unit/fake-upstream tests only. The batch did not access production, active port `9022`, or real credentials.
- The first command entry incorrectly supplied `cargo test --lib` to this binary-only package. Cargo rejected it before compilation. The wrapper still reported `size_kib=32`, `removed=true`, and `reservation_released=true`. The corrected matrix then ran as one shared build batch; this entry error is not counted as passing product evidence.

## Implementation Reverified

- Clean Anthropic requests reuse the original `Bytes` allocation when repair/shaping is unnecessary; the guard performs zero serialization and preserves exact bytes and unknown fields.
- Clean Kiro requests reuse the first required serialization when repair is unnecessary; the guard reports exactly one serialization instead of serializing the same body twice.
- Every full serialization performed by current-fit shaping contributes to `guardSerializations` and serialization timing.
- Leading assistant-only prefixes are counted once and removed with one `drain`, avoiding repeated `remove(0)` moves.
- Current image fitting drops the required batch before the next full serialization. Kiro and Anthropic paths emit one singular/plural summary placeholder rather than one placeholder per image or two placeholders for the final image.
- Lexical JSON whitespace compression preserves key order, duplicate keys, number and escape spellings, and unknown fields. Disabled, invalid, and already compact paths preserve exact bytes; the compacting path reuses the input allocation.

## Debug Matrix

The corrected scoped batch exited `0`. Exact Rust test-function counts from the batch output:

| Group | Result | Repeated behavior covered |
| --- | ---: | --- |
| `anthropic::payload_guard::tests` | 67 passed, 1 ignored | clean Anthropic/Kiro sizes, repair, logical tool turns, documents, schemas, tool results, image decoded-byte boundaries, current-fit and serialization counts |
| `http_client::tests` | 17 passed, 3 ignored | lexical tokens/escapes, invalid/disabled/compact identity, pointer/capacity, 5 MiB, deep/malformed recovery, bounded/timeout response bodies |
| `anthropic::body_processing::tests` | 20 passed | image normalization, clean text, URL/SSRF, redirects, aggregate limits, admission, timeout/cancel and recovery |
| `anthropic::request_body::tests` | 3 passed | exact/chunked request boundary and RPM attribution, each boundary scenario repeated five rounds where specified |
| `anthropic::router::tests` | 5 passed | five Messages routes, 50 MiB normalization, auth/admission order and file boundary |
| ten-route multimodal handler matrix | 1 passed | five Messages + five count_tokens paths, each five rounds; 50 remote-limit rejections, 25 inline count_tokens successes, 25 inline Messages successes |
| external raw/normalized focused matrix | 5 passed | raw exact bytes/SHA, top-level model-only rewrite, unknown/future field overlay, 4,097-message + 2,048-tool tail identity |
| provider raw TCP capture | 1 passed | 80/80 API sends: IDE/CLI, compression off/on, stream/non-stream, profile/no-profile, five rounds per cell |
| CLI endpoint module | 11 passed, 1 ignored | no-op/already-normalized exact bytes, escaped semantic keys, combined mutation, malformed/deep recovery and transport fields |
| IDE endpoint module | 19 passed, 1 ignored | no-op/existing-thinking exact bytes, escaped semantic keys, combined mutation, malformed/deep recovery, API-key/profile/region fields |

Total debug test functions executed successfully: `149`; normal debug discovery also reported `6` intentionally ignored release/isolation probes.

Important repeated payload cases include:

- clean Anthropic and Kiro at 1 KiB, 100 KiB, 1 MiB and 5 MiB, 100 rounds per size;
- clean Anthropic exact `Bytes` pointer identity, zero guard serializations and unknown-field preservation;
- leading-assistant repair at 1,000, 4,000 and 16,000 messages, five rounds per size;
- converted 20-cycle and 100-cycle tool histories, five rounds each;
- current four-image fitting for Kiro and Anthropic, five rounds, dropping three images, retaining one, producing one placeholder and reporting three serializations;
- decoded image sizes `5 MiB - 1`, `5 MiB`, and `5 MiB + 1`, plus exact-5-MiB acceptance, five rounds;
- tool result, document and tool-schema compression ON/OFF, five rounds.

## Release Probe Matrix

The same scoped target then executed `35` ignored probes explicitly under `--release`; all returned success before the batch exited `0`:

| Probe | Matrix |
| --- | --- |
| payload guard size/serialization | clean Anthropic, dirty Anthropic and clean Kiro; 1 KiB, 100 KiB, 1 MiB and 5 MiB; five rounds per cell |
| JSON whitespace size/mode | 1 KiB, 100 KiB, 1 MiB and 5 MiB x valid/invalid/disabled/compact x five rounds |
| JSON whitespace burst/recovery | 5 MiB x concurrency 8 x five rounds = 40 concurrent transforms, then five recovery transforms |
| JSON whitespace abort/recovery | 5 MiB x five abort observations, each followed by recovery |
| CLI/IDE endpoint transform | both endpoints x 1 KiB/100 KiB/1 MiB/5 MiB x escaped-no-marker/mutation x five rounds |

The release probes' assertions prove their identity/mutation, serialization-count, allocation-reuse and recovery contracts passed. The execution transport retained the beginning and end of the 3,600-line output but truncated its middle. Therefore this run does **not** claim a complete new set of exact p50/p95/p99 or allocation vectors. Existing earlier exact metrics remain attributed to their documented binaries; they are not silently rebound to this dirty-tree run. No Cargo rerun was started solely to recover display output, because doing so would create another cold build and conflict with the build-artifact lifecycle requirement.

## Artifact Cleanup Evidence

Successful-batch wrapper output:

```text
validation-build-cleanup scope=body-payload-identity size_kib=2410696 available_kib=50244676 removed=true reservation_released=true
```

Independent checks after wrapper exit showed:

- `target/.validation-build-body-payload-identity*`: no matching path;
- `.git/kiro-validation-build-state`: no reservation file;
- wrapper/batch/Cargo/rustc PIDs `34875`, `34929`, `11354`, and `36197`: absent;
- data-volume available space: `50,213,760 KiB`;
- focused `git diff --check`: pass.

This confirms the branch removed approximately 2.30 GiB of owned debug/release artifacts immediately after the logical batch. Concurrent building was not the root cause of the earlier disk exhaustion; missing post-build cleanup was. Concurrency only changes how quickly uncollected targets accumulate.

## Evidence Boundary And Remaining Gates

- This batch proves the listed unit/fake-upstream body contracts on the tested dirty-tree state. It does not prove that every future input is unchanged or that no upstream protocol change can introduce a new class.
- Exact current-candidate performance distributions were not recoverable from the truncated output and are not stated.
- A frozen release candidate must still bind the relevant protocol/runtime gates to one binary SHA.
- Raw MCP wire capture, real Claude Code C2/C3/C4 long sessions, 50 MiB concurrent request RSS/event-loop behavior, and L5 soak remain separate release gates.
- Any later source change affecting these paths invalidates this batch as final release evidence and requires a new scoped candidate gate with the same mandatory cleanup.
