# Implementation Status

Date: 2026-07-17

Current Phase: Scheduler persistence/admission degradation fix implemented and locally validated on `fix/scheduler-redis-degradation-isolation`; prior usage/error and Docker release gates remain open.

Next Target: Review and land the scheduler branch, run a 40-credential full-proxy canary without touching production ports, then resume the existing usage/error and Docker release gates.

Last Landed: No commit was created for the 2026-07-17 scheduler work. The dirty branch now separates best-effort success persistence from correctness-critical quarantine, isolates Redis admission health from sticky/state-sync latency, releases successful stream capacity before terminal persistence, batches Redis sticky cleanup, and avoids unnecessary external-pool scans. The earlier 2026-07-13 usage/error follow-up and incomplete Docker image gate remain recorded in [history/evidence-index.md](history/evidence-index.md).

Active TODO:

1. Review/commit the scheduler branch and run an isolated full-proxy 40-credential canary with an external fallback sentinel; do not touch live `9022` or `19422`.
2. Run the remaining C0 gates for the combined dirty tree: release build and both UI production builds. Formatting, diff hygiene, test compilation, main tests, and loadtest tests already passed for the scheduler branch.
3. Validate the earlier usage/error fixes through a temporary local release service and real direct `/v1` + `/cc/v1` calls.
4. Run Claude CLI `--output-format=stream-json` for tool/schema key mapping, official-upstream error message shape, external masking, and final usage fields.
5. Run low-concurrency long-context/resource smoke and capture RSS/FD before/after.
6. Rerun the end-to-end Docker Buildx gate; do not accept a frontend-only result.

Blocked By: The scheduler implementation itself has no local test blocker. Production deployment still requires review/landing and a controlled full-proxy canary. Full Docker evidence remains incomplete because slow crates.io downloads caused the earlier isolated run's 1800-second outer timeout during `cargo fetch --locked`; the usage/error follow-up still needs real local-service and Claude CLI validation before any release claim.

Last Verified:

- On 2026-07-17, `cargo fmt --all -- --check`, `git diff --check`, and `cargo check --tests --locked` passed on `fix/scheduler-redis-degradation-isolation`.
- With dedicated real dependencies, `cargo test --locked --bin kiro-rs -- --test-threads=4` passed `1169/1169`; `cargo test --locked --bin kiro_loadtest` passed `26/26`.
- A two-connection PgSQL pool was fully occupied while 40 credentials concurrently completed success handling. All 40 remained `Ready`, `available=40`, `disabled=0`, and all queued mutations replayed afterward.
- A real Redis test removed 10,000 sticky bindings in 64-member atomic batches while 64 admission probes succeeded; cleanup stayed off the request path, did not open the admission breaker, and left no binding backlog.
- Isolated release-loadtest fake scenarios passed: normal stream `40/40`, normal non-stream `40/40`, injected 1500 ms slow-first-byte `24/24` without extra queueing, slow-thinking `20/20`, and recovery-after-burst with the expected first 9 failures followed by 21 consecutive successes.
- Default and no-default full suites each passed `1079/1079` main tests and `19/19` loadtest tests after the final shutdown and external-pool lease fixes.
- Focused local/external Redis queue, cancellation, and coordinator tests passed against the real dependencies.
- A real-PgSQL shutdown test proved that frozen stats, newly accumulated deltas, and runtime mutations spanning multiple flush rounds drain within one deadline; a real-Redis test proved external-pool touch/release tasks are tracked and release capacity before TTL.
- Formatting, diff hygiene, checked-in lint baseline, and isolated default-feature release build passed.
- Temporary-port protocol/load/chaos, restart, recovery, resource, and graceful SIGTERM validation passed with cleanup complete.
- The final 143-file runtime evidence set passed a structured and high-signal credential scan; generated frontend/debug artifacts and validation-owned Redis, database, port, Docker, and temporary-file residue were absent after cleanup.
- The Docker run built both frontend production bundles, then timed out only at the locked Cargo dependency fetch; its unique builder, container, newly pulled BuildKit image, config, and validation directory were cleaned, and no temporary output image existed.
- Current dirty-tree schema-key mapping validation passed with default `sanitize`: invalid schema property key `bad key` was sent upstream through a legal generated key and returned to clients as `bad key` on both `/v1/messages` non-stream and `/cc/v1/messages` stream real local-service calls.
- Current dirty-tree local release build passed after building both maintained UI bundles (`admin-ui/dist` and `ui/dist`); both bundles remain required embedded release inputs and are not optional.

Handoff Notes:

- The incident was not a persistent `credentials.disabled=true` event. PgSQL runtime-persistence pressure first set process-local entries to disabled/quarantined; slow Redis sticky cleanup then amplified it into admission degradation. Preserve that distinction in alerts and incident summaries.
- Redis capacity acquire, queue admission, and queue renewal must remain fail-closed. Do not replace the fix with per-process local counting when Redis coordination is uncertain.
- High-cardinality sticky cleanup is still best-effort: queue rejection is logged, and a task exceeding the generic five-second storage deadline can leave stale bindings for their six-hour TTL. This does not disable credentials, but a future cleanup retry/coalescing worker would strengthen hygiene.
- Pending success mutations are in-memory. A hard process kill during a long PgSQL outage can lose unapplied warmup decrements even though normal timeout/replay is idempotent and generation-fenced.
- Keep the production container/TLS/Admin isolation/database-secret hardening scope (`6. P1`) deferred.
- Preserve registered `target/worktrees`; they are not disposable validation output.
- Do not disturb the existing services on ports `9022` and `19422` while completing any remaining validation.
