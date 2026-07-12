# Current Test And Release Gates

Role: Project-wide factual validation baseline

Status: Current commands, workflows, and known gaps

Authority: Checked-in CI/release configuration and registered dated evidence; this file is not proof that a command ran for an unreferenced commit

As of: `v0.0.102`, commit `e9479df71ee0`, 2026-07-11

Read when: Planning validation, changing CI/release behavior, or interpreting historical test evidence

Related: [Deployment and operations](deployment-and-operations.md), [Runtime evidence](../plans/runtime-correctness-and-release-gates/history/evidence-index.md), [Target verification](../plans/system-architecture-modernization/topics/delivery/verification-rollout-and-rollback.md)

## Evidence Rules

- A command list is a gate definition, not evidence of a pass.
- A pass statement must include source commit, date, exact command/config, exit status, and environment/dependency identity when relevant.
- Historical test counts remain historical snapshots; they must not be rewritten as the current suite size.
- Ignored `target/`, `logs/`, Docker builders, temporary databases, Redis prefixes, ports, and CLI homes are ephemeral artifacts, not durable authority.
- Durable evidence stores a sanitized summary, artifact manifest/hashes where useful, cleanup outcome, and links to any temporary raw data while it exists.
- A one-time release exception applies only to the named version and does not weaken future gates.

## Checked-In CI Shape

Current workflows and README require:

- pinned Rust `1.92.0`;
- pinned Node.js `22.23.0` and pnpm `11.11.0` for release/embedded frontend builds;
- both `admin-ui` and `ui` production builds;
- Rust formatting and checked-in Clippy warning baseline;
- real PgSQL and Redis services;
- default-feature and no-default-feature Rust test runs;
- release binary build;
- tag/version agreement between `v<version>` and `Cargo.toml`.

Storage-backed tests are configured to fail rather than silently pass when required test dependencies are missing in the main CI path.

## Static And Build Gates

```bash
cargo +1.92.0 fmt --check
node scripts/ci/check-clippy-baseline.mjs
pnpm --dir admin-ui build
pnpm --dir ui build
cargo +1.92.0 test --locked --all-targets
cargo +1.92.0 test --locked --all-targets --no-default-features
cargo +1.92.0 build --release --locked
git diff --check
```

The exact workflow environment and feature matrix remain authoritative if they differ from this summary.

## Frontend Gates

Current gates build both maintained frontends and compare selected handwritten TypeScript contracts. Current gaps:

- the comparison cannot prove either frontend matches Rust DTOs/routes;
- neither frontend has a checked component test suite;
- neither frontend has a browser E2E suite;
- build success cannot validate form behavior, conflict handling, key rotation, `preservePath`, usage queries, or responsive workflows.

## Protocol Gates

Changes affecting `/cc/v1`, streaming, thinking, tools, Files, models, errors, cache, or usage require:

- direct stream and non-stream smoke tests;
- event-order and final-usage assertions;
- thinking/tool pairing and normalized error fixtures;
- isolated real Claude Code CLI HOME/config sessions when behavior may affect interactive workflows;
- tools, search, MCP, Files, and multi-agent-triggered protocol calls where relevant;
- at least 20 turns per required long-session scenario in the target modernization gate.

The real CLI gate must use isolated configuration and must not mutate the operator's ordinary Claude/ccman state beyond the explicitly selected test configuration.

## Storage And Consistency Gates

Changes to PgSQL, Redis, runtime config, credentials, leases, queues, usage, or audits require real-service tests for:

- transaction/CAS success, conflict, retry, and rollback;
- idempotency and duplicate replay;
- partial Redis/PgSQL failure at each multi-step boundary;
- connection loss, restart, stale lease, and reconciliation;
- multi-replica convergence when that deployment mode is in scope;
- shutdown with pending accepted work;
- isolated schemas and Redis prefixes with deterministic cleanup.

## Load, Chaos, And Resource Gates

`src/bin/kiro_loadtest.rs` provides the fake-upstream/load driver. Existing guidance requires temporary ports and reports status, latency, resource, request, and error IDs. The modernization target expands this to:

- concurrency and sudden bursts;
- slow first byte at 30/60 seconds and active streams longer than 180 seconds;
- partial and widespread 408/429/5xx/protocol/network faults;
- slow/stopped/disconnected clients;
- PgSQL/Redis latency, disconnect, restart, and recovery;
- local/external raw/normalized/direct/fallback/rescue paths;
- large context/tools/schema/media/PDF/tokenizer cases;
- cache creation/read/eviction/shaping/no-cache projection;
- repeated-round RSS/FD/task/queue/file recovery.

Host-safety rules:

- start with the smallest scenario and enforce a concurrency ceiling;
- set outer timeouts, process RSS/FD monitors, and artifact byte/file budgets before the run;
- do not target active production/development ports;
- never retain raw prompt bodies or credentials in reports;
- stop escalation on memory, swap, FD, disk, dependency, or host-responsiveness thresholds;
- clean only validation-owned processes, databases, Redis prefixes, builders, images, ports, homes, and files;
- verify cleanup with inventory rather than repeatedly scanning the whole machine during every test step.

## Docker And Release Gates

The end-to-end Docker gate is not complete when frontend stages pass. It must reach Rust dependency fetch, Rust compilation, final image construction, and image export, then run the expected image smoke/metadata checks.

At the `v0.0.102` source commit, the registered runtime plan recorded a Docker run that built frontends but timed out during `cargo fetch --locked`. The builder and validation residue were cleaned. That result is neither a compilation failure nor a Docker pass.

`v0.0.102` was subsequently published under an explicit one-time instruction to update version/tag/push without local compilation verification. That exception applies only to `v0.0.102`; future releases continue to require the ordinary gates unless another version-specific exception is durably recorded.

## Supply-Chain Gaps

Current releases do not provide the complete target set of:

- SBOM;
- image/binary signature;
- provenance/attestation;
- immutable deployment digest in default examples;
- one manifest linking tag, Cargo version, commit, image digest, binary hashes, and verification evidence.

These are open modernization findings, not current gate claims.

## Performance-Gate Gap

There is no checked benchmark suite or CI threshold for scheduler latency, DB/Redis operations per request, serialization/canonicalization counts, throughput, p95/p99, RSS, FD, task recovery, or artifact growth. Existing load tooling and historical evidence are inputs for a future gate but do not currently prevent regression.

## Release Evidence Checklist

Before a normal release is represented as verified, durable evidence should identify:

1. exact source/tag/version;
2. static, frontend, Rust feature, and real-storage results;
3. protocol and load/chaos scenarios required by changed areas;
4. complete Docker build/export result;
5. binary/image hashes and, when implemented, SBOM/signature/provenance;
6. credential/body scan result for retained evidence;
7. validation artifact manifest and cleanup result;
8. every approved exception, its scope, and its expiry.
