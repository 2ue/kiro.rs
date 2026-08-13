# 013: Owner-Transaction Audit Acceptance

Date: 2026-07-12

Status: Accepted

Scope: Atomic Admin mutation/audit acceptance without a generic unit of work or cross-module repository access

Affected requirements/findings: `FUN-021`, `FUN-025`, `INV-006`, `QA-REL-001`, `QA-MAINT-001`-`QA-MAINT-003`, `OPS-003`, `SEC-005`

Refines: [Decision 007](007-domain-oriented-modular-monolith-and-module-ownership.md) and [decision 010](010-fixed-operational-and-acceptance-policies.md)

Related: [Module contracts](../topics/architecture/module-boundaries-and-contracts.md), [State ownership](../topics/architecture/state-ownership-and-consistency.md), [Admin architecture](../topics/architecture/admin-and-frontend-architecture.md), [Work map](../indexes/execution-slice-map.md)

## Context

Decision 010 requires every successful Admin mutation and its sealed audit append, plus any separately required domain outbox/job record, to commit in one PgSQL transaction. Module isolation simultaneously prohibits a domain from importing `MOD-AUDIT`'s private repository/table and prohibits a generic cross-domain unit of work. Without a narrow persistence contract, one of those requirements would have to be violated during implementation.

## Decision

`MOD-AUDIT` owns a sealed, versioned PgSQL append capability and its schema. Each mutable domain owns its business transaction and calls that public capability from its own PgSQL adapter without receiving or exporting a generic transaction object.

### Accepted Audit Envelope

Before persistence, the domain command builds a typed `AcceptedAuditEnvelope` containing:

- stable `audit_id` derived from the authenticated request/operation ID and domain mutation identity;
- actor/key fingerprint and auth epoch, never the reusable key;
- domain/action/object type and redacted object identity;
- expected/current/new domain version where applicable;
- timestamp, request/error correlation and bounded non-secret result metadata;
- schema version and idempotency scope.

The envelope cannot contain arbitrary JSON bodies, request payloads, plaintext secrets, proxy URLs with credentials, tool content or query text.

### Persistence Boundary

`MOD-AUDIT` publishes one versioned database function or equivalent sealed statement contract, for example `audit_append_v1(...)`. `MOD-AUDIT` owns its migration, validation and idempotent insert behavior. The function uses typed arguments, fixed `search_path`, no dynamic SQL and a least-privilege definer role. Direct audit-table privileges and default `PUBLIC` execute are revoked; only registered domain adapter roles may execute the exact version. Function migration is applied before any caller migration that references it. A domain adapter may invoke only that public function inside the same connection-local transaction that mutates its own rows; it cannot query or write audit tables directly.

The transaction order is:

1. validate expected domain version and authorization facts;
2. mutate the domain-owned row/state;
3. invoke the sealed audit append with the accepted envelope;
4. commit once.

Any validation, mutation or audit-append error rolls back the complete transaction. Duplicate `audit_id` with identical canonical content acknowledges idempotently; the same ID with different content fails and rolls back. A post-commit notification may be lossy because the audit row is already durable.

Commands whose external side effect cannot be atomic with PgSQL first commit a domain-owned durable job/outbox request plus audit envelope in this transaction. A supervised worker executes the effect and records a second stable outcome event. Admin transport never performs the external side effect between mutation and audit.

No public contract exposes `sqlx::Transaction`, a generic repository registry, arbitrary SQL, or a callback capable of mutating unrelated state.

## Alternatives

### Let Admin transport append after the command returns

Rejected. A crash between domain commit and audit append loses required evidence.

### Give every domain a private audit table/outbox

Rejected. Event schema, retention, queries and idempotency would drift, and aggregate audit reads would require a new broad store.

### Share one generic unit of work

Rejected. It gives callers cross-domain mutation power and recreates the broad storage/application coupling the rewrite removes.

## Verification

- Inject failure before/after domain mutation, during sealed append and at commit; only mutation-plus-audit or neither is visible.
- Repeat identical/different-content audit IDs and prove idempotent acknowledgement/conflict rollback.
- Static and PgSQL privilege checks reject direct audit-table SQL/permissions outside `MOD-AUDIT`, unsafe `search_path`/dynamic SQL/default execute grants and transaction types in public module contracts.
- Multi-replica duplicate Admin requests converge by operation/audit ID and expected domain version.
- Secret-marker tests prove envelopes, errors, logs and evidence contain no reusable value.
