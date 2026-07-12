# Implementation Status

Date: 2026-07-12

Current Phase: Final validation and evidence closure

Next Target: Rerun the isolated Docker gate through Rust compilation and final image export after the crates.io fetch timeout.

Last Landed: 2026-07-12 current dirty-tree schema-key compatibility fix passed static/unit/frontend/local-release and real local-service stream/non-stream validation; 2026-07-10/11 static, real-storage, isolated Rust release build, protocol, load/chaos, resource-recovery, and in-flight SIGTERM validation passed. The end-to-end Docker image gate remains incomplete. See [history/evidence-index.md](history/evidence-index.md).

Active TODO:

1. Rerun the end-to-end Docker Buildx gate; do not accept a frontend-only result.

Blocked By: Full Docker evidence is incomplete because slow crates.io downloads caused the isolated run's 1800-second outer timeout during `cargo fetch --locked`. The run did not reach Rust compilation or image export; this is not evidence that the Docker gate passed or that compilation failed.

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
