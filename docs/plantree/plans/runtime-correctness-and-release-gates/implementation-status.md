# Implementation Status

Date: 2026-07-13

Current Phase: Runtime usage/error correctness follow-up before v0.0.103 release validation

Next Target: Finish C0 + temporary local-service real-call validation for `docs/feature` fixes, then rerun the isolated Docker gate through Rust compilation and final image export.

Last Landed: 2026-07-13 feature follow-up analysis recorded in [../../../feature/runtime-usage-error-followup-2026-07-13.md](../../../feature/runtime-usage-error-followup-2026-07-13.md); current dirty-tree code now has schema diagnostics false-positive fix, `prompt is too long` classification, official Kiro upstream public message extraction, external parsed-route request input token estimation, and usage-only stream completion diagnostics. 2026-07-12 schema-key compatibility fix passed static/unit/frontend/local-release and real local-service stream/non-stream validation. The end-to-end Docker image gate remains incomplete. See [history/evidence-index.md](history/evidence-index.md).

Active TODO:

1. Run C0 gates for the current dirty tree: `cargo fmt --check`, `git diff --check`, `cargo check --all-targets`, `cargo test --all-targets`, release build, and both UI production builds.
2. Validate current fixes through a temporary local release service and real direct `/v1` + `/cc/v1` calls without touching live `9022`.
3. Run Claude CLI `--output-format=stream-json` for tool/schema key mapping, official-upstream error message shape, external masking, and final usage fields.
4. Run low-concurrency long-context/resource smoke and capture RSS/FD before/after.
5. Rerun the end-to-end Docker Buildx gate; do not accept a frontend-only result.

Blocked By: Full Docker evidence is incomplete because slow crates.io downloads caused the isolated run's 1800-second outer timeout during `cargo fetch --locked`. Current usage/error follow-up also still needs real local-service and Claude CLI validation before any release claim.

Last Verified:

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

- Keep the production container/TLS/Admin isolation/database-secret hardening scope (`6. P1`) deferred.
- Preserve registered `target/worktrees`; they are not disposable validation output.
- Do not disturb the existing services on ports `9022` and `19422` while completing any remaining validation.
