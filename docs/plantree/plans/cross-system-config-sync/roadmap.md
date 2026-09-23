# Roadmap

Last reviewed: 2026-09-23 Asia/Shanghai

## Done

- Captured the user requirement for explicit receiver acceptance rather than silent synchronization.
- Captured the requirement for sender-side multiple target systems and one/many/all target dispatch.
- Captured selectable sync modules for local accounts, external pools, proxy resources, runtime strategies and catalogs.
- Captured the hard requirement that mismatched or unsupported versions must be explicitly rejected.
- Captured the hard requirement that every import preserves a JSON pre-import snapshot and a JSON import record, including the previous target version.

## In Progress

- Architecture shaping for the current Rust service. No implementation has started.

## Next

1. Define the concrete `config_sync_*` PostgreSQL tables and indexes.
2. Define the version compatibility contract and the first supported bundle schema version.
3. Implement receiver settings and key lifecycle before adding any import apply path.
4. Implement export and dry-run preview for proxy resources first, because it is lower risk than account secrets and runtime config.
5. Add local accounts, external pools and account-to-proxy bindings after idempotency and rollback are proven.
6. Add runtime strategy and model catalog modules only after module-level versioning and restore are covered.

## Deferred

- Cross-system pull mode.
- Mesh replication.
- Public-key encryption of payload fields beyond HTTPS transport and local secret encryption.
- Historical usage migration. If needed, it must be a separate migration feature with batching, retention policy and resumable jobs.
- Automatic deletion of receiver objects that are missing from the sender. First implementation must default to non-destructive upsert.

## Acceptance Gates

- Receiver disabled mode rejects requests before body apply and leaves no module changes.
- Receiver manual mode creates a pending proposal and leaves configuration unchanged until approval.
- Receiver auto mode applies only allowed modules after version and schema checks pass.
- Unsupported protocol, app or module schema versions return a structured rejection and create only a rejected import record.
- Retrying the same `syncId` returns the prior result without duplicating accounts, proxies or pools.
- Every applied import has a pre-import snapshot that can restore the changed modules byte-for-byte at the JSON storage boundary.
- Restore creates its own record and preserves the original import record.
- Logs, audit entries and import records do not expose raw tokens, proxy passwords or external-pool API keys unless stored inside encrypted snapshot payloads.
- Existing local request scheduling continues while sync jobs run.
