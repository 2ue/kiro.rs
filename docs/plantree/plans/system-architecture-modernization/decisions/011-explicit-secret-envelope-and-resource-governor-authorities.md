# 011: Explicit Secret-Envelope And Resource-Governor Authorities

Date: 2026-07-12

Status: Accepted

Scope: Application-level secret encryption, master-key lifecycle, process resource admission, weighted memory permits, shared queue/connection ceilings, and previously unspecified bounded production defaults

Affected requirements/findings: `FUN-025`, `FUN-042`, `QA-RES-001`-`QA-RES-005`, `QA-PERF-004`, `QA-SEC-003`, `QA-SEC-008`, `RES-001`, `RES-003`, `RES-005`, `SEC-005`, `SEC-006`

Refines: [Decision 007](007-domain-oriented-modular-monolith-and-module-ownership.md) by registering two missing technical authorities and [decision 010](010-fixed-operational-and-acceptance-policies.md) by fixing the production ceilings that its resolved policies require

Related: [Target module ledger](../indexes/target-module-ledger.md), [Modular work map](../indexes/execution-slice-map.md), [State ownership](../topics/architecture/state-ownership-and-consistency.md), [Resource baseline](../../../baseline/resource-and-concurrency-model.md), [Performance contract](../topics/delivery/performance-contract-and-workloads.md), [Verification](../topics/delivery/verification-rollout-and-rollback.md)

## Context

Decision 010 requires a versioned application-level secret envelope and one combined process resource envelope, but the prior boundary draft assigned neither responsibility to one technical authority. Secret-owning domains could therefore invent incompatible cryptography, while the two schedulers and media/token modules could each admit work against separate limits and exceed the same process memory budget.

The fixed `64 MiB` heavy-work divisor was also insufficient by itself: normalized requests may retain inbound JSON, parsed values, downloaded bytes, base64/materialized data, rewritten payloads and buffered response data at the same time. A container with no bytes remaining after reserve must never receive one permit merely because a formula clamps the result to one.

## Decision

The target has two additional modules and therefore 50 technical authority modules in total.

### `MOD-SECRET-ENVELOPE`

`MOD-SECRET-ENVELOPE` owns only cryptographic and key-provider mechanics:

- a versioned envelope containing format version, algorithm, key ID, random nonce and ciphertext;
- XChaCha20-Poly1305 through a maintained audited library, a 256-bit key, a CSPRNG-generated 192-bit nonce, and associated data binding domain/module, record identity, field identity and envelope version;
- an operator-mounted key-ring file or secret-manager mount with an active key and decrypt-only prior keys; production rejects environment/config/database/image/frontend/log/evidence storage of the key ring;
- restrictive path/owner/mode checks, redacted non-`Debug` secret wrappers, explicit zeroization on drop where the language/runtime permits, and no generic serialization of plaintext;
- `seal`, `open`, `rewrap`, constant-time keyed-verifier, key-status and backup-key-manifest contracts; it owns no credential, proxy, pool, runtime-config or auth lifecycle and never enumerates their tables;
- activation of a new key ID before new writes, domain-owned CAS rewrap jobs, verification before retirement, and retention of decrypt-only keys through the complete database/WAL backup-retention window;
- recovery of key material from an operator-controlled secret manager or at least two encrypted off-host key-ring backups with separated access. Ordinary database/WAL/image/evidence backups never contain the wrapping secret. Restore drills mount the recovered provider and actually decrypt/verify restored secret rows; durable evidence records only key IDs, encrypted-backup manifest/hash and pass/fail, never key bytes.

`MOD-CREDENTIALS`, `MOD-PROXY-RESOURCES`, `MOD-EXTERNAL-POOLS`, `MOD-RUNTIME-CONFIG` and `MOD-AUTH` retain business ownership of their secrets and rows. Reusable upstream/proxy/runtime values that must be replayed use `seal/open`. High-entropy request/Admin API keys that are only verified store a versioned keyed HMAC/verifier, fingerprint and auth epoch; their plaintext exists only in the create/rotate input/result lifetime and is never stored as reversible ciphertext. `MOD-AUTH` owns verification/revocation semantics while the key provider owns the pepper-derived primitive. Domain owners call the narrow contract through owner adapters and own migration, enumeration, mutation and audit semantics. `MOD-MAINTENANCE-JOBS` executes bounded owner-defined rewrap jobs; `MOD-RECOVERY` verifies the external key manifest during backup/restore without reading key bytes into evidence.

During the 24-hour whole-system rollback window, a reviewed compatibility projection may retain and update the legacy plaintext columns needed by the immutable previous binary. This is a temporary, access-restricted exception, never target read authority. Manual secret/key/config/catalog mutations are frozen during that window; automatic credential refresh may dual-write only the minimum old projection in the same owner transaction. The projection is deleted, dual-write stops, and the secret-at-rest gate passes only during post-window contraction. No new release may declare `Complete` while a plaintext compatibility projection remains. Decision 014 owns the full per-authority rollback matrix, including state the previous binary cannot read.

### `MOD-RESOURCE-GOVERNOR`

`MOD-RESOURCE-GOVERNOR` is the single process-local authority for pre-heavy-work admission, weighted live-byte permits, the combined local/external waiting ceiling, outbound-connection permits and resource recovery metrics. It does not rank credentials, own Redis leases, choose an upstream, parse business payloads or replace owner-specific byte limits.

Public and Admin transports acquire a base admission/body reservation after header authentication, endpoint/profile resolution and one runtime capture but before collecting or parsing a request body. `Content-Length` can reject early; chunked uploads atomically grow the reservation before each retained chunk and stop reading on limit/permit failure. The handoff is a `BoundedRawBody` tied to its admission token, never an unconstrained `Bytes`. Messages and downstream owners may upgrade/release stage weight but cannot create a second global ledger. Admin uses a lower sublimit in the same ledger. Health/readiness uses a small reserved control channel and cannot be starved by public overload.

The supported production profile applies these rules:

1. Detect an explicit process/container memory limit. Reserve `max(512 MiB, 25% of the limit)` for runtime, code, pools and background work.
2. Require at least 64 MiB beyond that reserve. If `limit <= reserve + 64 MiB`, production profile validation fails and readiness remains closed; there is no minimum-one-permit override.
3. For every request, charge `max(class_floor, ceil(1.5 * live_bytes))`. The class floor is 8 MiB for raw/streaming, 16 MiB for normalized work without remote/media stages, and 64 MiB for remote/media/PDF/tokenizer/non-stream-heavy work. `live_bytes` is the profile's simultaneous inbound body, downloaded, decoded/materialized, rewritten/serialized, buffered response and codec working-set maximum; a 50-MiB ordinary body therefore cannot receive only the 16-MiB floor. Unknown lengths use the accepted maximum, not zero.
4. Total outstanding weight, including stage upgrades, never exceeds `memory_limit - reserve`. A stage upgrade atomically replaces or increments the request's current reservation before allocation and releases superseded weight; it is neither double-counted nor allowed outside the one global ledger.
5. Actual measured class peaks may only increase charges or reduce concurrency without a superseding decision and full capacity evidence.
6. `admitted_capacity` is the lesser of 256 request tokens and the number of minimum-class reservations that fit the current weighted byte ledger. The combined local/external wait queue is `min(2 * admitted_capacity, 256)`. Unset or zero is invalid in production.

The following initial hard ceilings are also binding; lower values are allowed when the same profile remains functional and passes capacity gates:

| Resource | Per-replica supported ceiling |
| --- | ---: |
| PgSQL pool | 32 connections; 2-second acquire deadline |
| Redis pool | 32 connections; 500-millisecond acquire deadline |
| Outbound sockets | 256 total and 64 per origin; scheduler/provider limits may be lower |
| Named long-lived supervised tasks | 256; request tasks remain bounded by admission |
| One critical writer ingress | 1,024 records and 16 MiB |
| All critical writer ingresses combined | 4,096 records and 64 MiB |
| Durable maintenance jobs | 1,024 pending and 8 running per replica |
| Inbound TCP connections | 512 total; 256 active public streams, 32 active Admin streams and 8 reserved health/control requests |
| HTTP headers | 128 fields and 32 KiB total; 10-second read deadline |
| Public request body | 50 MiB; 15-second idle and 120-second total read deadlines |
| Admin request/response body | 8 MiB request and 32 MiB response; bulk operations page/stream inside these limits |
| HTTP keepalive / HTTP2 | 30-second idle, 10-minute connection age, 1,000 requests/connection and 64 concurrent HTTP2 streams/connection within global limits |

Critical queue saturation either performs the required durable accept synchronously within its operation deadline or rejects/becomes unready; it never drops accepted work. Reconstructable derived work may coalesce/drop only with an explicit rebuild path and metric.

The supported deployment has at most 8 replicas unless a superseding capacity profile is accepted. The release-generation manifest fixes the exact replica set and per-replica pool allocation. Before readiness, `replicas * per_replica_pool + 16 PgSQL/64 Redis administrative-migration-recovery reserve` must fit the dependency's advertised/configured capacity; otherwise bootstrap lowers the per-replica value without going below 4 or fails the production profile. Durable maintenance claims also enforce a global maximum of 32 running jobs across replicas. Multiplying a per-replica default by an unbounded replica count is prohibited.

If a trusted reverse proxy terminates inbound connections, its checked-in/deployed manifest must enforce limits at least as strict and the application still enforces body/admission limits. Slow-header, slow/chunked upload, idle keepalive and HTTP2 stream saturation are mandatory fault cases.

### Exact Stateful Ceilings

| State | Binding supported policy |
| --- | --- |
| Shared Files | 50 MiB/file, 128 live objects, 256 MiB total, 7-day idle TTL, 30-day absolute age; quota overflow rejects, explicit delete removes payload immediately and bounded metadata/tombstone cleanup completes within 5 minutes |
| Prompt-cache evidence | 32,768 records, 2 KiB/record, 64 MiB aggregate, 2-hour TTL; overflow/Redis loss degrades to unknown/no-evidence and never fabricates an upstream cache fact |
| Admin sessions | 1,024 global and 256 per accepted origin/auth epoch; 15-minute idle and 8-hour absolute lifetime; zero/unset is invalid |
| CSRF tokens | Random 256-bit token, server stores only hash; at most 4 active tokens/session, 30-minute token TTL; same-origin mint endpoint validates `Origin`/Fetch Metadata, reload/new tabs mint independently, and sensitive auth/reveal actions rotate the calling token |
| Structured request cardinality | 4,096 messages, 16,384 content blocks, 128 tools, 64 schema depth, 10,000 schema nodes, 4,096 aggregate properties, 4,096 required/dependency entries, 1,024 reference edges, 10,000 characters/description and 1 MiB aggregate tool descriptions |

## Dependency And Integration Rules

- `MOD-RUNTIME-CONFIG` publishes immutable resource policy views but cannot own semaphore/queue state.
- `MOD-TRANSPORT-PUBLIC` and `MOD-TRANSPORT-ADMIN` own server/protocol limits but consume the one governor for connection/stream/body admission; `MOD-TRANSPORT-HEALTH` uses only the reserved control channel.
- `MOD-MESSAGES`, `MOD-MEDIA`, `MOD-FILES`, `MOD-TOKEN-COUNT`, both schedulers and upstream adapters consume scoped handles issued by the governor; they retain their own semantic limits and cleanup, not permit state.
- Scheduler queue/lease authority begins only after process admission and remains separate from the combined process resource queue.
- `MOD-BOOTSTRAP` validates the memory/key-provider profiles and constructs both modules; `MOD-READINESS` consumes their health contracts.
- Resource and secret-envelope failures are fail-closed where correctness, durability or confidentiality cannot be proven.

## Alternatives

### Let every secret-owning domain choose cryptography

Rejected. It duplicates envelope formats, key loading, rotation and recovery semantics and makes cross-domain audits unreliable.

### Put mutable admission in `MOD-KERNEL` or either scheduler

Rejected. The kernel must remain dependency-light and state-free. Either scheduler sees only one route class and cannot own the combined process budget used by media, tokenization and response buffering.

### Keep one constant 64-MiB permit per heavy request

Rejected. It ignores profile-specific simultaneous copies and can over-admit memory-heavy requests while unnecessarily limiting small ordinary calls.

## Verification

- Golden/tamper/wrong-associated-data/wrong-key/nonce/key-rotation/rewrap/zeroization and restore-key-manifest tests pass without plaintext artifacts.
- Every secret-bearing table and config source has an adoption/rewrap/rollback-window/contraction row; plaintext searches and database probes pass after contraction.
- Admission race/cancel/overload/recovery tests prove one combined queue and no permit leak across every terminal path.
- Load/chaos covers each cost class at its maximum declared bytes/cardinality, invalid/absent memory limits, slow headers/uploads, chunked upgrade, HTTP2/pool/socket saturation and 60-second idle recovery.
- Static checks reject direct crypto/key access outside `MOD-SECRET-ENVELOPE` and independent global admission semaphores outside `MOD-RESOURCE-GOVERNOR`.
