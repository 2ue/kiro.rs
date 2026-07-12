# Evidence Index

Date: 2026-07-12

Role: Durable verification summary for the runtime-correctness and release-gates plan.

Status: Current through the dirty-tree schema-key compatibility and local release-build evidence on 2026-07-12. Every listed gate passed except the explicitly incomplete end-to-end Docker image build.

Source revision note: This historical index did not record the exact Git commit/tree of the validated worktree. Results and binary hashes below must not be attributed to the later `v0.0.102` version-only release commit without separate evidence.

## Scope Boundary

- The implemented scope covers the verified audit findings except `6. P1` production container/TLS/Admin isolation/database-secret hardening.
- No commit, tag, push, or release publication is part of this evidence.
- Raw runtime reports remain under `target/loadtest/runtime-correctness-20260710204231-19707/`; this index records only non-secret outcomes and artifact identifiers.

The later `v0.0.102` release action is a separate event and does not change the evidence boundary above. See [the version-specific release exception](release-exception-v0.0.102.md). The 2026-07-12 schema-key evidence below was produced from a dirty working tree and proves the current local implementation only; it is not an attestation for the already published `v0.0.102` artifact.

## 2026-07-12 Tool Schema Key Compatibility

- Implemented `bodyConversion.toolSchemaKeyMapping` with `sanitize` (default), `reject`, and `disabled`; implemented configurable `bodyConversion.toolSchemaKeyValidationRegex` defaulting to `^[a-zA-Z0-9_.-]{1,64}$`.
- Default `sanitize` leaves valid schema property keys unchanged and creates request-local mappings only for invalid keys. Invalid keys are mapped to deterministic legal `key<16hex>` ids and reverse-mapped before client-visible `tool_use.input`.
- The map is request-local and grouped by mapped/upstream tool name; no Redis/global state is used.
- Stream and non-stream response paths, plus leaked `<invoke>` extraction, reverse-map tool input keys before Anthropic/Claude Code output.
- Real local-service validation used temporary release service `127.0.0.1:19022` and a real upstream call. The temp process was stopped after validation and no listener remained on `19022`.
- Non-stream `/v1/messages` result: HTTP 200, one `probe` tool block, client-visible input `{"bad key": "alpha", "valid_key": "beta"}`, and no `key<hash>` response key.
- Stream `/cc/v1/messages` result: HTTP 200, one `probe` tool call, combined streamed input `{"bad key":"alpha","valid_key":"beta"}`, and no `key<hash>` response key.
- `reject` and `disabled` behavior is covered by unit tests. A real `reject` service attempt was not counted because runtime PgSQL configuration overrode the temporary file config and left the service in default `sanitize` mode.

## 2026-07-12 Local Release Build With Embedded UIs

- Both maintained frontend bundles were built before the release binary:
  - `(cd admin-ui && pnpm build)`
  - `(cd ui && pnpm build)`
- The shared frontend contract check passed: `node scripts/check-frontend-contracts.mjs`.
- `cargo +1.92.0 build --release --locked` passed after both `admin-ui/dist` and `ui/dist` existed.
- This preserves the release rule that both UI builds are embedded inputs. Missing frontend bundles remain a build failure, not an optional release mode.

## Static, Storage, And Frontend Gates

- Default-feature suite: `1079/1079` main-program tests and `19/19` `kiro_loadtest` tests passed.
- No-default-feature suite: `1079/1079` main-program tests and `19/19` `kiro_loadtest` tests passed.
- Focused real-Redis results: local queue `6/6`, external queue `4/4`, and external cancellation/coordinator `2/2` passed.
- Clippy reported `685` warnings against the checked-in allowance of `711`; no new lint bucket remained.
- `cargo +1.92.0 fmt --all -- --check` and `git diff --check` passed.
- The production TypeScript/build gates for both `admin-ui/` and `ui/` passed.

## Release Artifact

- An isolated Rust `1.92.0` default-feature release build passed with `LTO=false` and `codegen-units=16`.
- `kiro-rs` SHA-256: `ff7b379d980e6a00239d1848c482ed2707aa55df7fc0904470756225ce917c5b`.
- `kiro_loadtest` SHA-256: `eaf85d3d4a41c293685e69973878fd3a0f5f324ce6faa50726e2f8d4596c8871`.
- The isolated release target directory was deleted after validation; the hashes are also recorded in the runtime report's `binary-sha256.txt`.
- Because the source commit/version was not captured in this index, these hashes prove the historical isolated build only. They are not `v0.0.102` release-binary hashes.

## Protocol, Load, And Lifecycle

- Stream/non-stream, thinking, tool-use, invalid-request/error normalization, model alias, usage, and Claude Code CLI cases passed.
- `/dfcache` configured/missing, slow-first-byte, slow-thinking, idle-timeout, 429/500 bursts and recovery, invalid tool, malformed SSE, mixed chaos, client disconnect, concurrency/RPM saturation and recovery, and restart recovery passed on temporary ports.
- `run-summary.json` records `cleanupComplete: true` and resource recovery from baseline RSS/FD `35696 KiB/32` through immediate `42144 KiB/31` to idle `30848 KiB/31`.
- `sigterm-inflight-evidence.json` records `2/2` successful in-flight requests, approximately `2796 ms` graceful drain, and drained usage/storage writers.
- Final lifecycle closure added a deadline-aware multi-round stats/runtime drain with non-zero shutdown failure on residue, plus bounded external-pool lease touch and critical release retry/fallback. Focused real-PgSQL/Redis tests and the two full feature suites passed after these changes.

## Docker Gate: Incomplete

- The isolated run used verified official Buildx `v0.35.0`; plugin SHA-256 was `fedbcbd488dcdb46414c6119920d8186d406531a1157ceede4e857e25af77ff1`.
- Both `admin-ui/` and `ui/` production build stages passed.
- The builder later remained in `RUN ... cargo fetch --locked ...` until the 1800-second outer timeout because crates.io downloads were too slow.
- The run never reached Rust compilation or final image export. Therefore the end-to-end Docker gate is not recorded as passed.
- The unique builder/container, newly pulled BuildKit image, temporary config, and `target/docker-validation-final` directory were precisely removed; the output image tag was absent because export was never reached, and no global Docker pruning was used.

## Subsequent Release Exception

- `v0.0.102` was later committed and tagged at `e9479df71ee0044cfa0da8acbf69d98c2259a66f` under a one-time explicit instruction to skip local compilation verification for that release action.
- The exception did not convert the incomplete Docker result into a pass and does not apply to any later version.
- The durable scope, consequence, and future-gate rule are recorded in [release-exception-v0.0.102.md](release-exception-v0.0.102.md).

## Cleanup And Preservation

- The final runtime evidence set contains 143 files (`2668 KiB`). Structured JSON/JSONL and high-signal text scans found no sensitive key assignments, Bearer credentials, embedded-authentication URLs, private keys, JWTs, or common API-key shapes.
- Plain `token` mentions in `proxy.log` were non-secret runtime vocabulary without assigned credential values; the 26 logged URLs were local validation endpoints without credentials or sensitive query parameters.
- Post-runtime checks found no listeners on validation ports `19280/19281`, no validation databases, no `kiro_rs:validate:*` or `kiro_rs:test:*` Redis keys, and no `/private/tmp/kiro-runtime-*` residue.
- The protected services on ports `9022` and `19422` retained their original PIDs throughout validation.
- Generated frontend bundles/build metadata, shared `target/debug`, isolated static/release targets, and known validation temporary files were removed after use.
- `target/worktrees` was preserved because it contains registered Git worktrees rather than disposable test output.
