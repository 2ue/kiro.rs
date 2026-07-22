# Evidence Index

Date: 2026-07-12

Last updated: 2026-07-23 for the current dirty-tree final release gate, 2026-07-22 regression rerun, E03 real two-process scheduler/RPM, Redis joint-fault, external takeover, runner contracts and protocol-contamination source-contract pointers.

Role: Durable verification summary for the runtime-correctness and release-gates plan.

Status: Current release-gate evidence exists for the v0.0.109 dirty-tree candidate. Historical 2026-07-12 evidence remains below and must not be confused with the 2026-07-23 candidate.

Source revision note: This historical index did not record the exact Git commit/tree of the validated worktree. Results and binary hashes below must not be attributed to the later `v0.0.102` version-only release commit without separate evidence.

## Scope Boundary

- The implemented scope covers the verified audit findings except `6. P1` production container/TLS/Admin isolation/database-secret hardening.
- No commit, tag, push, or release publication is part of this evidence.
- Raw runtime reports remain under `target/loadtest/runtime-correctness-20260710204231-19707/`; this index records only non-secret outcomes and artifact identifiers.

The later `v0.0.102` release action is a separate event and does not change the evidence boundary above. See [the version-specific release exception](release-exception-v0.0.102.md). The 2026-07-12 schema-key evidence below was produced from a dirty working tree and proves the current local implementation only; it is not an attestation for the already published `v0.0.102` artifact.

## 2026-07-23 Current Dirty-Tree Final Release Gate

This is current dirty-tree evidence for the v0.0.109 remediation branch, not evidence for
the historical 2026-07-12 release artifact below.

- Frozen `kiro-rs` SHA-256:
  `925525419cd48b460217df2568891a40287da0c44d2bf921a38b103c047775ee`.
- Frozen `kiro_loadtest` SHA-256:
  `90babda7388aa93854cbbdb81c132cc436c07f46b0ea22973531b0a7ffb3aff1`.
- Scoped batch `final-c0-release-20260723-r4` passed:
  - `cargo +1.92.0 fmt --all -- --check`;
  - `cargo +1.92.0 test --all-targets` with main `1750 passed / 0 failed / 6 ignored`
    and `kiro_loadtest 31/31`;
  - `cargo +1.92.0 build --release --bins`.
- Scoped cleanup reported
  `size_kib=2516216 available_kib=86414424 removed=true reservation_released=true`.
- `node feature/tests/check-feature-docs.mjs` passed 47 issue documents and 115 links.
- `node --test feature/tests/*.test.mjs` passed
  `283 tests / 261 pass / 22 explicit skips / 0 fail`.
- `git diff --check` passed.
- Build artifact inventory initially failed on a regenerated disposable root `target/`
  containing `debug`, `flycheck0` and `.rustc_info` output. The existing `9022` service was
  not stopped. After deleting only the repository `target/` output, inventory passed with
  `targets=0 reservations=0 target_processes=0 blockers=0`.
- Detailed evidence lives in
  [feature/evidence/final-release-gate-20260723.md](../../../../../feature/evidence/final-release-gate-20260723.md).
- Release publication then used Rust crate mode:
  - work commit `b528ead` (`fix: harden runtime protocol and scheduler gates`);
  - release bump commit `beb9b3420b20776db489461d65392b5b1d6e5d92`
    (`chore(release): 0.0.114`);
  - latest remote semver tag base `v0.0.113`;
  - annotated tag `v0.0.114`, tag object
    `071ccb3975fb1ae2bf6cd27f9875f9dd4b9a24e8`, peeled commit
    `beb9b3420b20776db489461d65392b5b1d6e5d92`;
  - branch push succeeded before tag push, and tag push succeeded.

Docker dynamic execution is explicitly waived by the user for this phase and is not counted
as pass. Existing `127.0.0.1:9022` production/development service was not modified.

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

## 2026-07-21 Current Dirty-Tree E03 Scheduler/RPM Evidence

This is current dirty-tree evidence for the v0.0.109 remediation branch, not evidence for
the historical 2026-07-12 release artifact above.

- `rpm-reservation-focused-20260721-r4` passed the Redis storage reservation round-trip and
  two-manager shared-RPM manager test, with scoped target cleanup.
- `rpm-reservation-check-all-20260721-r4` passed `cargo check --all-targets`, with scoped target cleanup.
- Frozen candidate `/tmp/kiro-e03-candidate.T2iG7N/kiro-rs` had SHA-256
  `98e0f79328b49925dc940faaa3b1e8b0c8ae8ef7b9975725eb219635c8957ee7`.
- Real E03 runtime `runId=e03-20260721013242272-88844-36667d` passed `outerRounds=3`.
  Each round had `rpm.firstStatuses=[200,200]`, `rpm.postRestartStatuses=[429,429]`,
  `externalHits=0`, `disabled=0`, and clean process/Redis/port/temp cleanup.
- Durable issue/evidence detail lives in
  [feature/evidence/e03-real-two-process-scheduler-runner-20260720.md](../../../../../feature/evidence/e03-real-two-process-scheduler-runner-20260720.md).

This closes the E03 real two-process scheduler/RPM gap only. Release remains NO-GO until the
remaining external-takeover, two-instance fault/fallback, real-upstream, UI, upgrade, final
inventory and release gates pass.

## 2026-07-21 Current Dirty-Tree Redis Joint-Fault Evidence

This is current dirty-tree evidence for the v0.0.109 remediation branch, not evidence for
the historical 2026-07-12 release artifact above.

- `scheduler-redis-joint-chaos-20260721-r5` on `redis://127.0.0.1:26379/4`
  failed in outer round 2 at WRONGTYPE recovery:
  `wrongtype-round-2: recovery 1/5 failed ... retry_after_secs=4`.
- Root cause: deterministic Redis response/type/script/server errors after
  `arm_redis_commit_unknown()` were treated like timeout/connection-unknown
  failures. That enqueued unnecessary release/tombstone reconciliation for a
  lease that was never created, adding Redis writes and competing with breaker
  half-open recovery.
- Fix: hot-path scheduler Redis failure outcomes now carry `commit_unknown`.
  Deterministic Redis response errors use `commit_unknown=false` and call
  `confirm_redis_not_acquired()`; timeout/connection dropped/I/O and unknown
  failures remain conservative.
- Focused validation passed for the new response-error test, the existing
  consecutive-timeout breaker test, the timeout-streak-reset test, `cargo fmt`,
  `git diff --check`, and `cargo check --all-targets` under scoped targets.
- `scheduler-redis-joint-chaos-20260721-r6` then passed `8 exact × 3 outer`,
  i.e. `24/24`, on `redis://127.0.0.1:26379/5`. Cleanup:
  `databaseEmpty=true`, `childGroupsStopped=true`, `portsReleased=true`,
  `tempRemoved=true`, scoped `size_kib=1710316 removed=true reservation_released=true`.
- Regression reruns also passed:
  `multi-instance-redis-coordination-20260721-r3` on DB7 (`15/15`, scoped
  `1708432 KiB removed=true reservation_released=true`) and
  `redis-fault-domain-product-20260721-r4` on business DB8 / observability DB2
  (`3/3` exact invocations, scoped `1708364 KiB removed=true reservation_released=true`).
- A later RedisStore production role guard and business/observability path-isolation
  contract prevent usage materialization entrypoints from directly using business scheduler
  Redis, keep scheduler/external/runtime-event/health on business Redis only, prevent
  observability Redis startup failure from falling back to business Redis, and keep
  UsageRecorder request handling enqueue/drop-only for observability writes. The source
  contract passed as part of `run-redis-fault-domain-product-validation.contract.test.mjs`
  (`37 pass / 9 skip / 0 fail`),
  the scheduler/fault-domain combined contract passed `53 pass / 21 skip / 0 fail`, and
  scoped `cargo +1.92.0 check --bin kiro-rs` passed with cleanup
  `size_kib=446876 removed=true reservation_released=true`.
- Durable issue/evidence detail lives in
  [feature/evidence/scheduler-redis-chaos-nondocker-20260720.md](../../../../../feature/evidence/scheduler-redis-chaos-nondocker-20260720.md),
  [feature/evidence/multi-instance-redis-coordination-20260720.md](../../../../../feature/evidence/multi-instance-redis-coordination-20260720.md), and
  [feature/evidence/business-observability-redis-fault-domain-20260721.md](../../../../../feature/evidence/business-observability-redis-fault-domain-20260721.md).

This closes the single-instance usage-writer/scheduler simultaneous Redis fault gap.
Release remains NO-GO until external-takeover, two-instance fault/fallback, real-upstream,
UI, upgrade, final inventory and release gates pass.

## 2026-07-21 Current Dirty-Tree External Takeover Runner Evidence

This is current dirty-tree evidence for the v0.0.109 remediation branch, not evidence for
the historical 2026-07-12 release artifact above.

- Source review confirmed the `SchedulerRedisDegraded` fallback toggle, fresh local state
  guard and external pool eligibility chain in `src/anthropic/handlers.rs` and
  `src/model/config.rs`.
- `external-takeover-focused-20260721-r2` passed four exact handler/fallback tests:
  scheduler fallback classifier toggles, preflight reason toggles, fresh-state local
  dispatchable guard, and parsed entrypoint model/body-mode eligibility.
- Scoped cleanup for that focused batch reported
  `size_kib=1708372 removed=true reservation_released=true`.
- Added `feature/tests/external-takeover-scheduler-degraded-nondocker.mjs` and
  `feature/tests/external-takeover-scheduler-degraded-nondocker.contract.test.mjs`.
- The contract test passed `8/8`, covering JavaScript syntax, validate-only,
  no Docker/Cargo, no protected `9022` probe, loopback-only PG/Redis, Redis DB1..15,
  unsafe database/prefix rejection and caller-owned artifact/binary inputs.
- Durable issue/evidence detail lives in
  [feature/evidence/external-takeover-scheduler-degraded-20260721.md](../../../../../feature/evidence/external-takeover-scheduler-degraded-20260721.md).

This closes only the focused code-path and runner-contract sub-gates. The dynamic service
gate remains open until a caller-owned empty PostgreSQL database URL is provided and both
fallback-enabled and fallback-disabled runs pass against a frozen candidate binary.

## 2026-07-21 Current Dirty-Tree E01/E02 Runner Contract Evidence

This is current dirty-tree evidence for the v0.0.109 remediation branch, not evidence for
the historical 2026-07-12 release artifact above.

- `feature/tests/scheduler-fairness-sticky-race.mjs` no longer starts Docker-managed
  PostgreSQL/Redis, no longer runs Redis `FLUSHDB`, and no longer creates PostgreSQL
  databases.
- The runner now requires a frozen external binary, an external artifact root, a loopback
  PostgreSQL URL template, `modes × rounds` caller-owned `kiro_e0102_*` databases, a loopback
  Redis DB1..15 URL, and a caller-owned Redis prefix.
- Each dynamic case uses its own Redis `keyPrefix` and cleans only that owned prefix.
- `node --test feature/tests/scheduler-fairness-sticky-race.contract.test.mjs` passed `7/7`.
- `node --test feature/tests/runtime-validation-paths.test.mjs` passed `9/9`.
- The same batch also reran the external-takeover runner contract (`8/8`) and `git diff --check`.
- Durable issue/evidence detail lives in
  [feature/evidence/scheduler-fairness-nondocker-runner-contract-20260721.md](../../../../../feature/evidence/scheduler-fairness-nondocker-runner-contract-20260721.md).

This closes only the E01/E02 runner safety-contract sub-gate. The actual distribution,
sticky and lease-race dynamic gate remains open until frozen-candidate service execution passes
against caller-owned empty PostgreSQL databases and an isolated Redis prefix.

## 2026-07-21 Current Dirty-Tree E05 Non-Docker Runner Contract Evidence

This is current dirty-tree evidence for the v0.0.109 remediation branch, not evidence for
the historical 2026-07-12 release artifact above.

- `feature/tests/strict-local-first-routing.mjs` now uses caller-owned PostgreSQL/Redis
  inputs instead of the old Docker/Toxiproxy-managed runtime.
- The runner requires a frozen external binary, external artifact root, `modes × rounds`
  pre-created `kiro_e05_*` databases, a loopback Redis DB1..15 URL, and a caller-owned
  Redis prefix.
- It does not start Docker, create databases, `FLUSHDB` Redis, call Cargo, or probe
  protected `9022`; Redis fault injection uses `feature/tests/redis-chaos-proxy.mjs`.
- `node --test feature/tests/strict-local-first-routing.contract.test.mjs` passed `6/6`.
- The combined runner-contract batch passed `30/30` across E05 (`6/6`),
  `runtime-validation-paths` (`9/9`), external takeover contract (`8/8`) and E01/E02
  runner contract (`7/7`); `git diff --check` passed.
- Build-artifact inventory initially failed after an external root `target/` reappeared;
  after confirming `target/debug`/`target/flycheck0` had no `lsof` references, those
  reproducible artifacts were deleted and inventory passed with
  `targets=0 reservations=0 target_processes=0 blockers=0`. This is not residue from
  the E05 contract batch.
- Durable issue/evidence detail lives in
  [feature/evidence/strict-local-first-nondocker-runner-contract-20260721.md](../../../../../feature/evidence/strict-local-first-nondocker-runner-contract-20260721.md).

This closes only the E05 runner safety-contract sub-gate. It is not an E05 product pass;
dynamic service execution remains open until a frozen binary and caller-owned empty
PostgreSQL databases are supplied and the full matrix passes.

## 2026-07-21 Current Dirty-Tree F06 Non-Docker Runner Contract Evidence

This is current dirty-tree evidence for the v0.0.109 remediation branch, not evidence for
the historical 2026-07-12 release artifact above.

- `feature/tests/aws-api-key-region-lifecycle.mjs` now requires caller-owned runtime
  inputs instead of starting Docker-managed PostgreSQL/Redis.
- The runner requires a frozen external binary, external artifact root, loopback
  PostgreSQL URL whose database matches `kiro_f06_*`, loopback Redis DB1..15, and a
  caller-owned Redis prefix.
- It does not start Docker, create PostgreSQL databases, `FLUSHDB`/`FLUSHALL` Redis,
  call Cargo, probe protected `9022`, or inherit the caller's full `process.env` into
  service children.
- Redis cleanup is prefix-scoped to `${KIRO_F06_REDIS_PREFIX}:*`; PostgreSQL access is
  limited to caller-owned database reads/checks through local `psql`.
- `node --check feature/tests/aws-api-key-region-lifecycle.mjs` passed.
- `node --test feature/tests/aws-api-key-region-lifecycle.contract.test.mjs` passed
  `6/6`.
- The combined non-Docker runner-contract batch passed `41/41` across request-admission
  (`5/5`), F06 (`6/6`), runtime validation paths (`9/9`), external takeover (`8/8`),
  E01/E02 (`7/7`) and E05 (`6/6`).
- Durable issue/evidence detail lives in
  [feature/evidence/aws-api-key-region-nondocker-runner-contract-20260721.md](../../../../../feature/evidence/aws-api-key-region-nondocker-runner-contract-20260721.md).

This closes only the F06 runner safety-contract sub-gate. The F06 AWS API key + region
dynamic service gate remains open until a frozen binary, caller-owned empty PostgreSQL
database and isolated Redis prefix are supplied and the lifecycle run passes.

## 2026-07-21 Current Dirty-Tree Request Admission Non-Docker Runner Contract Evidence

This is current dirty-tree evidence for the v0.0.109 remediation branch, not evidence for
the historical 2026-07-12 release artifact above.

- `feature/tests/request-api-key-admission-multi-instance.mjs` now requires caller-owned
  PostgreSQL/Redis and uses `feature/tests/redis-chaos-proxy.mjs` instead of Docker
  Toxiproxy.
- The runner requires a frozen external binary, external artifact root, loopback
  PostgreSQL URL template with exactly one `{database}` placeholder, `rounds` pre-created
  databases matching `kiro_request_admission_*`, loopback Redis DB1..15, and a
  caller-owned Redis prefix.
- It does not start Docker, create PostgreSQL databases, `FLUSHDB`/`FLUSHALL` Redis, call
  Cargo, use `host.docker.internal`, probe protected `9022`, or inherit the caller's full
  `process.env` into service children.
- Each round uses a round-specific Redis `keyPrefix`; cleanup only deletes
  `${KIRO_REQUEST_ADMISSION_REDIS_PREFIX}:*`.
- `node --check feature/tests/request-api-key-admission-multi-instance.mjs` passed.
- `node --test feature/tests/request-api-key-admission-multi-instance.contract.test.mjs`
  passed `5/5`.
- The combined non-Docker runner-contract batch passed `41/41` across request-admission
  (`5/5`), F06 (`6/6`), runtime validation paths (`9/9`), external takeover (`8/8`),
  E01/E02 (`7/7`) and E05 (`6/6`).
- Durable issue/evidence detail lives in
  [feature/evidence/request-api-key-admission-nondocker-runner-contract-20260721.md](../../../../../feature/evidence/request-api-key-admission-nondocker-runner-contract-20260721.md).

This closes only the request-admission runner safety-contract sub-gate. The dynamic
multi-instance admission gate remains open until a frozen binary, caller-owned empty
PostgreSQL databases and isolated Redis prefix are supplied and the full run passes.

## 2026-07-21 Current Dirty-Tree Frozen Load/Chaos Non-Docker Runner Contract Evidence

This is current dirty-tree evidence for the v0.0.109 remediation branch, not evidence for
the historical 2026-07-12 release artifact above.

- `feature/tests/frozen-load-chaos-runner.mjs` now requires caller-owned PostgreSQL/Redis
  inputs instead of using Docker-managed PostgreSQL/Redis.
- The runner requires `KIRO_RS_BINARY`, `KIRO_LOADTEST_BINARY`, an external artifact root,
  a loopback PostgreSQL URL template with exactly one `{database}` placeholder, tier-sized
  pre-created databases matching `kiro_load_chaos_*`, loopback Redis DB1..15, and a
  caller-owned Redis prefix.
- L3 requires three caller-owned databases, L4 requires six, and L5 requires one.
- It does not start Docker, create/drop PostgreSQL databases, `FLUSHDB`/`FLUSHALL` Redis,
  call Cargo, probe protected `9022`, or inherit the caller's full `process.env` into
  fake upstream, proxy or loadtest children.
- Redis cleanup is prefix-scoped through `SCAN`/`DEL` for this run's owned keys only.
- `node --test feature/tests/frozen-load-chaos-runner.contract.test.mjs` passed `6/6`.
- The combined runner-path batch passed `15/15` across frozen-load-chaos (`6/6`) and the
  shared runtime validation path contract (`9/9`); `git diff --check` passed.
- Durable issue/evidence detail lives in
  [feature/evidence/frozen-load-chaos-nondocker-runner-contract-20260721.md](../../../../../feature/evidence/frozen-load-chaos-nondocker-runner-contract-20260721.md).

This closes only the load/chaos runner safety-contract sub-gate. The final L3/L4/L5
dynamic service gate remains open until the current frozen release candidate is rerun with
caller-owned empty PostgreSQL databases and an isolated Redis prefix.

## 2026-07-21 Current Dirty-Tree Redis Storage Runner No-FLUSH Contract Evidence

This is current dirty-tree evidence for the v0.0.109 remediation branch, not evidence for
the historical 2026-07-12 release artifact above.

- `feature/tests/run-token-refresh-cluster-validation.mjs` and
  `feature/tests/run-multi-instance-redis-coordination-validation.mjs` no longer run
  runner-level `FLUSHDB` cleanup.
- Both runners still require caller-confirmed empty loopback Redis DB1..15 before Cargo.
- After a run, leftover Redis keys are reported as `residualKeyCount` and fail the gate;
  the runner does not clear the whole database.
- `node --test feature/tests/run-token-refresh-cluster-validation.contract.test.mjs
  feature/tests/run-multi-instance-redis-coordination-validation.contract.test.mjs` passed
  `19 pass / 1 skip / 0 fail`; the skip is explicit live nonempty Redis opt-in.
- `node --check` passed for both runner files.
- Source scanning showed no `FLUSHDB/FLUSHALL` in those runner and contract files;
  `git diff --check` passed.
- Durable issue/evidence detail lives in
  [feature/evidence/redis-storage-runner-no-flush-contract-20260721.md](../../../../../feature/evidence/redis-storage-runner-no-flush-contract-20260721.md).

This closes only the Redis runner cleanup safety-contract sub-gate. Token-refresh cluster
and multi-instance Redis dynamic service reruns remain open.

## 2026-07-21 Current Dirty-Tree Runner Child Environment Isolation Evidence

This is current dirty-tree validation-runner evidence for the v0.0.109 remediation branch,
not evidence for the historical 2026-07-12 release artifact above and not a product dynamic
gate pass.

- Added `feature/tests/validation-child-env.mjs` and routed real validation child processes
  through a whitelist environment instead of inheriting the caller's full `process.env`.
- Updated runner children include Claude CLI/bare invoke, long-session continue, thinking
  effort capture, external takeover, E03 two-process scheduler, E01/E02 scheduler fairness
  and business/observability Redis fault-domain runners.
- `node --test feature/tests/runtime-validation-paths.test.mjs` passed `11/11`, including
  checks that `DATABASE_URL`, `REDIS_URL`, Anthropic/OpenAI keys, `KIRO_API_KEY`,
  `KIRO_RS_TEST_REDIS_URL` and arbitrary unpassed `KIRO_*` state are not inherited by child
  processes.
- `node --check` passed for the updated helper and runner files.
- Source scanning showed remaining `...process.env` matches only in `.test.mjs` fixture
  launchers, not in non-test validation runners.
- No Docker, Cargo or `kiro-rs` service was started for this evidence.
- Durable evidence detail lives in
  [feature/evidence/runner-child-environment-isolation-20260721.md](../../../../../feature/evidence/runner-child-environment-isolation-20260721.md).

This closes only the validation contamination safety-contract sub-gate. Dynamic service,
real-upstream Claude CLI, UI, upgrade, load/chaos and final inventory gates remain open.

## 2026-07-21 Protocol Contamination Source Contract Evidence

This is current dirty-tree source-contract evidence for the v0.0.109 remediation branch,
not evidence for the historical 2026-07-12 release artifact above.

- Added `feature/tests/protocol-contamination-source-contract.test.mjs`.
- The contract is pure Node source inspection: it does not start Docker, does not run Cargo,
  and does not start a `kiro.rs` service.
- Standalone run passed `10 tests / 10 pass / 0 fail`.
- Combined run with the business/observability Redis fault-domain contract passed
  `56 tests / 47 pass / 9 explicit live-signal skips / 0 fail`; the skips are inherited
  live Redis signal fixtures and are not protocol-contamination skips.
- The contract locks that transcript suppression is not an arbitrary `Hashxxxxxxxx`
  matcher. It requires current request tool names plus deterministic/legacy mapped names,
  preserves marker-free raw request bytes before DOM parse, keeps user/tool data out of
  cleanup, keeps signed/redacted thinking atomic, blocks strict request contamination before
  upstream, and requires stream/non-stream/external contamination to fail closed rather than
  produce blank or partial success terminal events.
- Durable evidence detail lives in
  [feature/evidence/protocol-contamination-source-contract-20260721.md](../../../../../feature/evidence/protocol-contamination-source-contract-20260721.md).

This closes only the source-regression sub-gate. Real native Kiro upstream, active/passive
thinking long sessions, MCP/search/image/agent, fault recovery, UI, upgrade and final release
gates remain open.

## 2026-07-21 Protocol Marker Inventory Source Contract Evidence

This is current dirty-tree source-contract evidence only. It does not start Docker, does not run
Cargo, does not start kiro.rs and does not invoke Claude Code CLI.

- `node --test feature/tests/protocol-marker-inventory-source-contract.test.mjs` passed
  `4/4`.
- The contract inventories production Rust source after excluding Rust test modules and
  `*/tests.rs` fixtures. It locks that `user Continue` and `user Tool results provided` are
  confined to `transcript_sanitizer`, `Tool results:` is confined to sanitizer plus stream
  bounded observability, function-results/function-calls markers are confined to the stream
  protocol adapter, and production code has no bare `<invoke>` / `</invoke>` literal or old
  `[previous output]` / `[trimmed output]` / `[duplicate output]` placeholders.
- It also locks that tool-result-only user-message placeholders are inert dots and duplicate or
  orphan tool-result repair does not textify rejected content into ordinary user text.
- Durable evidence detail lives in
  [feature/evidence/protocol-marker-inventory-source-contract-20260721.md](../../../../../feature/evidence/protocol-marker-inventory-source-contract-20260721.md).

This closes only the production-source marker inventory sub-gate. Dynamic native CLI/upstream,
fault/load, UI, upgrade and final release gates remain open.

## 2026-07-21 Lightweight Contract Regression Evidence

This is current dirty-tree evidence for low-artifact validation after the E05 runner
rewrite. It does not start Docker, does not run Cargo and does not start kiro.rs.

- Feature docs check first passed 47/47 issue documents and 102 relative links; after the
  runner child-environment evidence was added, the docs check reran as 47/47 and 104 links.
- UI/config contracts passed: cost format, MCP attempt channel, request API key ID,
  prompt control independence and prompt default parity.
- Runner contracts passed or skipped only explicit live fixtures:
  - E03/token-refresh/multi-instance: `70 pass / 1 skip`.
  - Scheduler chaos/fault-domain: initially `44 pass / 21 skip`; after adding business/observability Redis production source-contract checks, the combined batch passed `49 pass / 21 skip / 0 fail`; after adding RedisStore production role-guard checks, it passed `50 pass / 21 skip / 0 fail`; after adding business/observability path-isolation checks, it passed `53 pass / 21 skip / 0 fail`.
  - Thinking wire: `45/45`.
  - Claude capture/bare signal: `5/5`.
  - `run-cargo-scoped-lifecycle`: `21/21`, without invoking Cargo.
- A later continuation rerun passed UI/prompt contracts again, non-Docker runner/path
  `49/49`, E03/token-refresh/multi-instance/scheduler/fault-domain `124/146` with
  22 explicit fixture skips, and thinking/Claude signal `50/50` after rerunning the
  long signal matrix with a 120s timeout.
- The same continuation added the protocol-contamination source contract: standalone
  `10/10` pass, and combined with business/observability Redis fault-domain
  `47/56` with 9 explicit live-signal skips.
- A follow-up continuation added the production marker inventory source contract:
  standalone `4/4` pass.
- The earlier non-Docker runner/path batch passed `68/69` with one explicit live nonempty
  Redis opt-in skip; `node --check` passed for the updated runner helper/files.
- After deleting unreferenced reproducible `target/debug`, `target/flycheck0` and
  `target/.rustc_info.json`, build artifact inventory passed with
  `targets=0 reservations=0 target_processes=0 blockers=0`. Docker read-only inspection
  timed out and remained a manual-only hint; no Docker cleanup was performed.
- After protocol documentation updates, feature docs passed again with 47/47 issue documents
  and 106 links, and `git diff --check` passed. A subsequent inventory first failed on a
  root `target/` around 710 MiB with PID 84264 referencing `target/release/kiro-rs` and its
  local verification log; only unreferenced debug/flycheck/.rustc_info artifacts were removed,
  the service process was not stopped, and inventory then passed with
  `targets=0 reservations=0 target_processes=0 blockers=0` and `target=0B`.
- After marker-inventory evidence, protocol contamination plus marker inventory source
  contracts passed `14/14`; feature docs passed 47/47 issue documents and 108 links;
  `git diff --check` passed. Root `target/` was then rebuilt again around 710 MiB by
  rust-analyzer/flycheck style activity; only current visible debug/flycheck/.rustc_info entries
  with no `lsof +D target` references were removed, PID 84264 was not stopped, and inventory
  returned to `targets=0 reservations=0 target_processes=0 blockers=0`. Disk available was
  about 69 GiB.
- Disk available was about `71-73GiB`. No new scoped target/reservation residue was produced.
- Durable evidence detail lives in
  [feature/evidence/lightweight-contract-regression-20260721.md](../../../../../feature/evidence/lightweight-contract-regression-20260721.md).

This closes only a low-artifact regression sub-gate. It is not a substitute for dynamic
service, real upstream, UI browser/build, upgrade or final inventory gates.

## 2026-07-22 Token Refresh Cluster Revalidation

This is current dirty-tree evidence for the token-refresh two-manager Redis/PostgreSQL
coordination gate. It uses caller-owned local services and does not start Docker or touch
the protected `9022` service.

- Pre-fix cluster run reproduced a fast-failure amplification red:
  `token_refresh_two_manager_failure_replay_and_cancelled_leader_recover_without_send_amplification_for_five_rounds`
  failed with `endpoint hits left=2 right=1` for `failure-replay-1`.
- Root cause: Redis waiters poll every 500ms, while the first negative replay window was
  jittered to 400-500ms. A fast 500 leader could therefore expire the failed outcome before
  a waiter observed it, electing a second leader and sending a second OAuth refresh request.
- Fix: Redis token-refresh failure delay is now floored at
  `TOKEN_REFRESH_POLL_AFTER_MS + 250`, and the Redis backoff unit test asserts that both
  no-Retry-After and tiny-Retry-After failures outlive waiter polling.
- Focused red/green batch `failure-replay-fix` passed the Redis backoff exact test and the
  cluster failure replay exact test; scoped target `1713280 KiB` was removed and reservation
  released.
- Default cluster matrix then passed with
  `KIRO_TOKEN_REFRESH_CLUSTER_OUTER_ROUNDS=3`, isolated Redis DB `2` and PostgreSQL database
  `kiro_rs`: 7 exact tests × 3 outer rounds × 5 internal rounds = 105 internal scenario
  rounds. Cleanup reported `childGroupsStopped=true`, `redisDatabaseEmpty=true`,
  `residualKeyCount=0`, `tempRemoved=true`; scoped target `1715296 KiB` was removed and
  reservation released.
- Durable evidence detail lives in
  [feature/evidence/token-refresh-failure-wave-and-cluster-rpm-20260717.md](../../../../../feature/evidence/token-refresh-failure-wave-and-cluster-rpm-20260717.md).

This closes the ordinary token-refresh two-manager Redis/PostgreSQL cluster sub-gate. It does
not close the full invalid-bearer provider matrix, token-refresh-specific Redis slow/error/restart,
real upstream/native CLI, UI/browser, upgrade or final release gates.

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
