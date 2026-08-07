# 外部池流式首语义输出前恢复 focused 验证（2026-08-06）

Status: `focused-fake-http-and-routing-validated / normal-routing-rerun-passed-20260807 / storage-ui-wired / frozen-cli-load-passed-20260807 / production-rollout-observation-pending / release-candidate-v0.0.134`

Date: 2026-08-06 Asia/Shanghai

Related:

- [Stream terminal errors and precommit retry](../issues/stream-terminal-errors-and-precommit-retry.md)
- [External pool stream error yuenan sampling](external-pool-stream-error-yuenan-sampling-20260806.md)
- [外部池流式首语义输出前错误恢复](../../docs/plantree/plans/rust-runtime-scheduler-stabilization/topics/external-pool-stream-pre-output-retry-20260806.md)

## Scope

This focused evidence covers the implementation that allows external-pool streaming requests to retry another external pool when the original upstream fails before any downstream semantic output is committed.

It covers:

- global default configuration plus per-pool override;
- PostgreSQL create/update/list/get persistence for `pre_output_stream_retry_mode`;
- real loopback HTTP SSE fake-upstream recovery scenarios;
- final-success usage cleanliness and attempt diagnostics;
- both external-pool UI surfaces and runtime config type/default wiring;
- static Rust and frontend build checks.

It now closes the local frozen-candidate CLI and fake-upstream load/chaos gates for this change. Production rollout observation and a renewed real `yuenan` / `yuenan-1` recurrence check remain pending after publication.

## Implemented Behavior

- `external_pool_stream_pre_output_retry_enabled` is added to global external-pool runtime config and defaults to `true`.
- Each external pool has `pre_output_stream_retry_mode`: `inherit`, `enabled`, or `disabled`.
- When enabled, the external stream 2xx path pre-reads and buffers SSE events before returning a downstream response.
- `message_start`, `ping`, and protocol-only usage preview events can be discarded if the attempt fails before semantic output.
- `error` event, body read error, idle timeout, or EOF without a terminal/semantic event becomes a retryable external-pool failure and re-enters the existing same-request external-pool budget.
- `content_block_start`, valid content/thinking/tool/input JSON deltas, OpenAI-compatible content, finish reason, `[DONE]`, and legal `message_stop` commit the stream; after that point the request is not replayed.
- External direct remains external-only. The focused tests assert `local_attempted=false` and no local credential attempts on recovery.
- Failed pre-output attempts are recorded in attempt diagnostics only. The final successful usage comes from the final successful pool.

## Validation Results

All Cargo commands were run through `feature/tests/run-cargo-scoped.sh`, as required by the project validation contract.

| Gate | Command | Result | Notes |
| --- | --- | --- | --- |
| Unit classifier/effective mode | `feature/tests/run-cargo-scoped.sh external-stream-preoutput-unit-final -- cargo test external_pool_pre_output_stream --locked` | Passed: `2 passed` | Covers global/per-pool effective mode and conservative commit classifier. |
| Real HTTP fake-upstream stream matrix | `feature/tests/run-cargo-scoped.sh external-stream-preoutput-http-matrix -- cargo test external_pool_stream_ --locked` | Passed: `6 passed` | Covers pre-output error event, protocol-only then error, EOF, read error, idle timeout, disabled override, and post-commit no-replay. |
| PostgreSQL persistence | `feature/tests/run-cargo-scoped.sh external-stream-storage -- cargo test postgres_external_pool_list_and_get_preserve_body_modes --locked` | Passed: `1 passed` | Covers create/list/get/update preservation for body modes and `pre_output_stream_retry_mode`. |
| Rust static compile | `feature/tests/run-cargo-scoped.sh external-stream-preoutput-check -- cargo check --all-targets --locked` | Passed | Full all-target compile check. |
| Rust format | `feature/tests/run-cargo-scoped.sh external-stream-preoutput-fmt-check -- cargo fmt --all -- --check` | Passed | Formatting gate. |
| Diff hygiene | `git diff --check` | Passed | No whitespace errors. |
| User UI check | `pnpm --dir ui check` | Passed | Type/check gate for the main UI. |
| User UI build | `pnpm --dir ui build` | Passed | Build passed with an existing chunk-size warning only. |
| Admin UI build | `pnpm --dir admin-ui build` | Passed | Build gate for admin-ui. |

## Normal Output And Scheduling Regression

After the initial implementation validation, the user explicitly asked to verify whether the change affects other normal scheduling logic, especially normal streaming/non-streaming output and local-account versus external-pool direct scheduling across existing documented configurations.

The scenario source was the local plan/issue documentation:

- `scheduler-target-state-machine-and-test-contract.md`
- `scheduler-target-compliance-matrix.md`
- `external-pool-local-first-scheduler.md`
- `external-pool-direct-model-retry-20260804.md`
- `stream-terminal-errors-and-precommit-retry.md`

The focused matrix distilled from those docs was:

| Area | Scenarios checked | Evidence |
| --- | --- | --- |
| Normal external stream | Healthy external SSE stream still commits normally; pre-output retry does not replay after commit; disabled per-pool mode preserves old stream-error behavior | `external_pool_stream_` with real test PgSQL/Redis: `6 passed` |
| Normal external non-stream | Clean non-stream external body remains byte-identical; OpenAI-compatible non-stream usage still normalizes; missing usage still estimates billing | targeted external-pool usage/body tests: `6 x 1 passed` |
| Normal local output | Local stream success still records requested max tokens/downstream stop reason; local non-stream success still commits shared attempt budget before usage | handler tests: `2 x 1 passed` |
| External direct, stream and non-stream | Direct policy routes both stream and non-stream requests to external pool, rewrites the resolved upstream model, records `external_direct_policy`, and does not hit local Kiro upstream | `normalized_external_direct_policy_skips_raw_preparse_without_raw_pool`: `1 passed`; this test now covers both modes |
| Local-first fallback | Request errors do not fall back; capacity/transient/no-credential/config toggle cases keep explicit reasons; normalized external preflight handles stream and non-stream | `external_fallback`: `9 passed`; `native_websearch_normalized_external_preflight_precedes_mcp_for_five_rounds`: `1 passed` |
| Direct/local rescue boundary | External direct and `external_direct_policy` route subtype block local rescue for all error classes; local-first external failure may rescue only when the source and fresh local capacity allow it | `direct_external`: `5 passed`; `external_local_rescue`: `3 passed`; `local_rescue_requires`: `2 passed` |
| Local preflight and fresh local state | Local Ready suppresses external fallback; Redis degraded and configured terminal reasons remain explicit; capacity-gated fallback requires immediate external capacity where documented | `local_pool_preflight_reason`, `local_external_fallback_capacity_gate`, `fresh_local_pool_state`, `classified_scheduler_degraded`: `4 x 1 passed` |
| Route config authority | Built-in route behavior follows runtime cache/prompt/external-pool config instead of hardcoded path semantics | `builtin_routes_follow_runtime_cache_and_prompt_config_matrix`: `1 passed` |

Additional validation commands:

| Gate | Command | Result |
| --- | --- | --- |
| Real PgSQL/Redis external stream rerun | `feature/tests/run-cargo-scoped.sh normal-routing-external-stream-db-rerun -- cargo test external_pool_stream_ --locked` with `KIRO_RS_TEST_POSTGRES_URL` and `KIRO_RS_TEST_REDIS_URL` from existing local Docker containers | Passed: `6 passed` |
| External normal output/usage batch | `feature/tests/run-cargo-scoped.sh normal-output-external-usage-rerun -- bash -lc 'cargo test ...'` | Passed: 8 targeted commands, each `1 passed` |
| Routing/config classifier batch | `feature/tests/run-cargo-scoped.sh routing-config-classifiers-db -- bash -lc 'cargo test ...'` | Passed: 11 targeted commands, total asserted tests listed above |
| Direct stream/non-stream Router batch | `feature/tests/run-cargo-scoped.sh normal-direct-stream-nonstream-router -- bash -lc 'cargo test ...'` | Passed: 4 targeted commands, each `1 passed` |
| Final Rust check | `feature/tests/run-cargo-scoped.sh normal-routing-cargo-check-final -- cargo check --all-targets --locked` | Passed |
| Final Rust format | `feature/tests/run-cargo-scoped.sh normal-routing-fmt-check-final -- cargo fmt --all -- --check` | Passed |
| Final diff hygiene | `git diff --check` | Passed |
| Build artifact inventory | `node feature/tests/inventory-build-artifacts.mjs --gate` | Passed: `targets=0 reservations=0 target_processes=0 blockers=0` |

Notes:

- Two early validation commands used multiple Cargo test filters in one invocation; Cargo rejected them before running tests. They were discarded and rerun as valid single-filter batches.
- Two parallel compile-heavy batches timed out while waiting on build locks. The scoped wrapper later reaped their stale target/reservation entries, and the final artifact inventory passed.
- An earlier inventory run was intentionally discarded because it ran concurrently with an active scoped `cargo check` and correctly reported the active target as a blocker. The final standalone inventory run passed.
- The direct external handler regression was extended in this change to cover both `stream=false` and `stream=true`; it asserts external hits, route subtype, model rewrite, attempt count, and zero local Kiro upstream hits for both modes.

The real HTTP fake-upstream tests used pre-existing local Docker services:

- PostgreSQL: `kiro-rs-postgres-local` on `127.0.0.1:25432`
- Redis: `kiro-rs-redis-local` on `127.0.0.1:26379`

The tests use existing isolated test helpers to create and drop PostgreSQL schemas and random Redis prefixes. No production service was modified, no local `9022` service was restarted, and no new long-running process was left running by this validation.

## 2026-08-07 User-Requested Rerun

The user asked for an additional check that the stream pre-output retry change does not affect other normal scheduling logic, especially normal stream/non-stream output and local-account versus external-pool direct scheduling under the documented configurations.

Environment:

- Rust: `cargo +1.92.0`, through `feature/tests/run-cargo-scoped.sh`.
- PostgreSQL integration container: `kiro-rs-postgres-local` on `127.0.0.1:25432`; password was read from Docker env and not printed.
- Redis integration container: `kiro-rs-redis-local` on `127.0.0.1:26379/0`.
- Node: `v22.23.1`; pnpm: `10.33.4`. This differs from the baseline pnpm `11.11.0`, so these frontend results are local rerun evidence, not a substitute for the pinned CI gate.

Focused scheduler/output matrix:

```text
feature/tests/run-cargo-scoped.sh normal-routing-scheduler-matrix-20260807 -- bash -lc '
  cargo +1.92.0 test external_pool_stream_ --locked
  cargo +1.92.0 test external_pool_pre_output_stream --locked
  cargo +1.92.0 test postgres_external_pool_list_and_get_preserve_body_modes --locked
  cargo +1.92.0 test external_fallback --locked
  cargo +1.92.0 test direct_external --locked
  cargo +1.92.0 test local_pool_preflight_reason --locked
  cargo +1.92.0 test local_external_fallback_capacity_gate --locked
  cargo +1.92.0 test fresh_local_pool_state --locked
  cargo +1.92.0 test classified_scheduler_degraded --locked
  cargo +1.92.0 test external_local_rescue --locked
  cargo +1.92.0 test local_rescue_requires --locked
  cargo +1.92.0 test normalized_external_direct_policy_skips_raw_preparse_without_raw_pool --locked
  cargo +1.92.0 test stream_success_records_requested_max_tokens_and_downstream_stop_reason --locked
  cargo +1.92.0 test local_non_stream_success_commits_shared_attempt_budget_before_usage_for_five_rounds --locked
  cargo +1.92.0 test native_websearch_normalized_external_preflight_precedes_mcp_for_five_rounds --locked
  cargo +1.92.0 test builtin_routes_follow_runtime_cache_and_prompt_config_matrix --locked
'
```

Result: passed. The batch covered:

- external stream pre-output recovery, disabled override and post-commit no-replay: `6 passed`;
- pre-output effective-mode/classifier: `2 passed`;
- PostgreSQL `pre_output_stream_retry_mode` persistence with existing body-mode preservation: `1 passed`;
- local-first fallback classifiers: `9 passed`;
- external direct policy and direct-to-local-rescue block: `5 passed`;
- local preflight toggles, capacity fallback gate, fresh local Ready and Redis degraded classifier: `4 x 1 passed`;
- external local rescue classifiers and shared-attempt constraints: `3 passed` plus `2 passed`;
- normalized external direct Router path covers both `stream=false` and `stream=true`: `1 passed`, asserting external hits, rewritten upstream model, route subtype `external_direct_policy`, and zero local Kiro upstream hits;
- normal local stream and non-stream output: `2 x 1 passed`;
- normalized external preflight before WebSearch/MCP and built-in route config authority: `2 x 1 passed`.

External normal output and usage matrix:

```text
feature/tests/run-cargo-scoped.sh normal-output-external-usage-matrix-20260807 -- bash -lc '
  cargo +1.92.0 test openai_usage_is_normalized_for_non_stream_external_pool_body --locked
  cargo +1.92.0 test openai_usage_is_captured_for_stream_external_pool_billing --locked
  cargo +1.92.0 test openai_stream_usage_keeps_local_shaping_separate_from_raw_billing --locked
  cargo +1.92.0 test non_stream_missing_usage_injects_estimated_billing_body --locked
  cargo +1.92.0 test external_pool_billing_matches_dashed_opus_request_to_dotted_pricing_model --locked
  cargo +1.92.0 test non_stream_unknown_json_without_usage_injects_estimated_usage_and_billing --locked
  cargo +1.92.0 test non_stream_unknown_text_without_usage_records_estimated_billing_without_rewriting_body --locked
  cargo +1.92.0 test external_non_stream_clean_response_remains_byte_identical --locked
'
```

Result: passed, `8 x 1 passed`. This specifically rechecked normal external non-stream body identity, OpenAI-compatible non-stream usage normalization, stream billing capture, raw-vs-shaped billing separation, missing-usage estimate injection, text body no-rewrite behavior and pricing-model matching.

Static and UI gates rerun:

| Gate | Result |
| --- | --- |
| `feature/tests/run-cargo-scoped.sh normal-routing-static-final-20260807 -- bash -lc 'cargo +1.92.0 fmt --all -- --check; cargo +1.92.0 check --all-targets --locked'` | Passed |
| `git diff --check` | Passed |
| `node feature/tests/check-feature-docs.mjs` | Passed: `75` issue docs, `354` relative links |
| `node feature/tests/inventory-build-artifacts.mjs --gate` | Passed: `targets=0 reservations=0 target_processes=0 blockers=0`; Docker scan timed out in read-only inventory mode, cleanup remains manual-only |
| `pnpm --dir ui check` | Passed |
| `pnpm --dir ui build` | Passed with the existing chunk-size warning |
| `pnpm --dir admin-ui build` | Passed |

Conclusion for this rerun:

- No regression was found in the focused normal stream/non-stream output paths covered by the tests.
- No regression was found in external direct scheduling: direct requests remain external-only in both stream and non-stream modes.
- No regression was found in the focused local-first fallback/rescue classifier matrix: local Ready suppresses external fallback, Redis degraded and terminal local states keep explicit reasons, and local rescue remains bounded.
- This still does not close real Claude Code CLI, load/chaos, production observation, or renewed `yuenan` / `yuenan-1` recurrence gates.

## 2026-08-07 Final Frozen Candidate Gates

The final local candidate was rebuilt outside Cargo target and bound to the following immutable binaries:

| Binary | SHA-256 |
| --- | --- |
| `kiro-rs` | `eec71c67ce49ee9003d2cd70fae0d8ebfef1d44f72ee56bda8bb7c7ee592b688` |
| `kiro_loadtest` | `023f3e961cdbc56e32f46f896ac66494b1a92d0e182728ddaddbeb5b8ed90e4d` |

Scoped C0 Rust gate:

```text
feature/tests/run-cargo-scoped.sh final-candidate-003 -- bash -lc '
  cargo +1.92.0 fmt --all -- --check
  cargo +1.92.0 test --locked
  cargo +1.92.0 check --all-targets --locked
  cargo +1.92.0 build --release --locked --bin kiro-rs --bin kiro_loadtest
'
```

Result: passed. The earlier `final-candidate-002` full test run failed only because an old local `9022` gateway process held the PostgreSQL runtime lifecycle fence; after verifying and stopping that exact local `kiro-rs` listener, `final-candidate-003` passed and the scoped target cleanup reported `removed=true` / `reservation_released=true`.

Real Claude Code CLI fake-upstream gates used Claude Code CLI `2.1.221` with an isolated HOME / `CLAUDE_CONFIG_DIR` and the frozen `kiro-rs` binary above:

| Gate | Result | Evidence summary |
| --- | --- | --- |
| Bare invoke | Passed | `20/20` cases; `15` negative and `5` structured cases; `25` inference hits; `5` tool_use and `5` tool_result; cleanup true. |
| Long session | Passed on rerun | `5` sessions, `110` turns, `105` continue turns, `100` tool turns, `100` tool_use / `100` tool_result, `leakMatches=0`; cleanup true. One earlier run timed out at `round=3 turn=9`; the rerun completed the same point successfully, with two slow turns around `41s` but below the gate timeout. |
| Thinking wire | Passed on rerun | `60/60` cases across CLI/IDE endpoints and `absent/low/medium/high/xhigh/max` effort values; `violations=0`; cleanup true. One earlier run had `ide-max-4` time out before any ingress/wire record was captured; rerun with the same frozen binary passed all cases. |

Load/chaos gates used the frozen `kiro-rs` and `kiro_loadtest` binaries above, caller-owned PostgreSQL databases, Redis DB `12` with random caller-owned prefixes, and the repository `frozen-load-chaos-runner.mjs`. The runner removed raw runtime/log/report trees after writing summary JSON; the outer harness dropped its PostgreSQL databases and the runner deleted owned Redis prefixes.

| Gate | Result | Evidence summary |
| --- | --- | --- |
| L3 burst/recovery | Passed | `9/9` cases. Normal stream c1/c5/c10 and c40 spike were all `200`; post-spike recovery `10/10`; recovery-after-error-burst had bounded mixed 200/429/502 and normal recovery `12/12`; invalid-tool burst produced expected `502` and normal recovery `12/12`. |
| L4 restart/chaos | Passed | `12/12` cases. Proxy restart during long stream produced bounded transport errors and recovery `12/12`; 429, 500, invalid-tool, client-drop and mixed-chaos bursts all matched expected error/mixed outcomes and each normal recovery case was `12/12`. |
| L5 soak | Passed | `900s` long-stream soak at concurrency `20`: `6820/6820` success, `0` errors, TTFB p95 `359ms`, total latency p95 `2733ms`. After `300s` idle cooldown, `rssReturnedWithin32MiB=true`, `idleRssSettled=true`, `fdReturnedWithin5=true`; post-soak normal recovery `12/12`. |

Post-run cleanup checks:

- no `9022` listener was left running;
- no new `kiro-rs`, `kiro_loadtest`, release-suite, or load-chaos process remained, aside from the active Codex/Claude session itself;
- the runner deleted this run's Redis prefixes;
- the outer harness dropped this run's PostgreSQL databases;
- raw CLI/load artifact roots contained only small summary/report JSON after extraction and were removed before release.

## Final Pre-Release Gates

After the evidence/status document update, the final pre-release local gates passed:

| Gate | Result |
| --- | --- |
| `feature/tests/run-cargo-scoped.sh final-static-after-docs-20260807 -- bash -lc 'cargo +1.92.0 fmt --all -- --check && cargo +1.92.0 check --all-targets --locked'` | Passed; scoped target cleanup `removed=true` / `reservation_released=true` |
| `git diff --check` | Passed |
| `node feature/tests/check-feature-docs.mjs` | Passed: `75` issue docs, `354` relative links |
| `node feature/tests/inventory-build-artifacts.mjs --gate` | Passed: `targets=0 reservations=0 target_processes=0 blockers=0`; Docker scan timed out in read-only inventory mode, cleanup remains manual-only |
| `pnpm --dir ui check` | Passed |
| `pnpm --dir ui build` | Passed with the existing chunk-size warning |
| `pnpm --dir admin-ui build` | Passed |

## Release CI Recovery

The first remote tag publish attempt for `v0.0.134` reached GitHub Actions `Publish Docker Images #165` and failed in the `quality / Frontend and Rust quality gate` before image publication. Local reproduction with the release-quality Clippy baseline found only lint-bucket regressions in `src/external_pool.rs`: `clippy::derivable_impls` for the new `ExternalPoolStreamRetryMode` default implementation and `clippy::single_match` in the new pre-output stream commit classifier.

The repair is behavior-neutral:

- `ExternalPoolStreamRetryMode` now uses `#[derive(Default)]` with `Inherit` marked as `#[default]`.
- The single-pattern `match` in `external_sse_event_commits_pre_output_stream` is now an equivalent `if let`.

Local release-quality rerun:

| Gate | Result |
| --- | --- |
| `feature/tests/run-cargo-scoped.sh release-clippy-baseline-fix-20260807 -- rustup run 1.92.0 node scripts/ci/check-clippy-baseline.mjs` | Passed: Clippy emitted `813` warnings; checked-in baseline allows `849` |
| `feature/tests/run-cargo-scoped.sh final-static-after-clippy-fix-20260807b -- bash -lc 'cargo +1.92.0 fmt --all -- --check && cargo +1.92.0 check --all-targets --locked'` | Passed; scoped target cleanup `removed=true` / `reservation_released=true` |
| `git diff --check` | Passed |
| `node feature/tests/check-feature-docs.mjs` | Passed: `75` issue docs, `354` relative links |
| `node feature/tests/inventory-build-artifacts.mjs --gate` | Passed: `targets=0 reservations=0 target_processes=0 blockers=0`; Docker inspected read-only, cleanup remains manual-only |
| `pnpm --dir ui check` | Passed |
| `pnpm --dir ui build` | Passed with the existing chunk-size warning |
| `pnpm --dir admin-ui build` | Passed |

## Remaining Gates

- Production rollout observation remains pending.
- A renewed real `yuenan` / `yuenan-1` sampling pass after rollout remains useful but is not claimed here.
