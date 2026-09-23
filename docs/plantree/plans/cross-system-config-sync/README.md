# Cross-System Configuration Sync

Role: Cross-system configuration synchronization plan for the current Rust service

Status: `Planning`

Current phase: `architecture shaped; implementation not started`

Last reviewed: 2026-09-23 Asia/Shanghai

Authority:

- This plan owns the target design for pushing selected configuration from one `kiro.rs` system to another.
- It covers receiver acceptance controls, the independent sync key, selectable modules, version compatibility, import records, rollback and restore.
- It does not change the current scheduler, account selection, request execution, usage recording or release process by itself.

Related:

- [Plan Tree](../../README.md)
- [Storage and state baseline](../../baseline/storage-and-state.md)
- [Protocol and API contracts baseline](../../baseline/protocol-and-api-contracts.md)
- [Rust Runtime Scheduler Stabilization](../rust-runtime-scheduler-stabilization/README.md)

## Purpose

Allow an operator to copy selected configuration from one system into another system without silently taking over the receiver. The sender can configure multiple receiver systems and push selected modules to one, many or all targets. The receiver must explicitly enable the feature and decide whether imports are rejected, queued for manual approval or applied automatically.

The first target is a practical, version-gated configuration sync path for:

- local Kiro accounts and their selected scheduling fields;
- external upstream pools;
- proxy resources and account-to-proxy bindings;
- runtime scheduling and request-processing policy;
- selected model capability and pricing catalogs.

## Non-Negotiable Requirements

- Use a new independent sync key, named in product terms as `Admin Sync Key` or `configSyncKey`; never reuse the Admin API key, Request API key, Kiro API key or external-pool API key.
- Receiver mode is explicit: `disabled`, `manualApproval` or `autoAccept`. Disabled mode rejects all sync requests before parsing large payloads.
- Opening receiver sync generates a new key or requires confirming reuse of an existing key. Rotating or disabling the key must immediately stop future sync requests.
- The receiver must reject unsupported or mismatched versions with a clear error. There is no best-effort import across unknown versions.
- Each accepted import must first persist a JSON pre-import snapshot that is sufficient for lossless restoration of every module that will be changed.
- Every import must persist a JSON import record. The record must include the previous target version, the incoming source version, selected modules, content hashes, result and restore pointer.
- A failed import must leave the durable configuration unchanged. If any apply step fails after staging, the system must roll back automatically or mark the import failed before exposing changes.
- A successful import must be restorable by selecting its pre-import snapshot. Restore itself creates a new JSON record and does not erase the original import history.
- Sensitive material is opt-in per send: local account tokens/API keys, proxy credentials and external-pool API keys are excluded by default.
- Sync must not copy request usage history, Redis scheduler state, in-flight leases, cooldowns, sticky bindings, uploaded files, local caches or logs.
- Sync work must not block normal request scheduling. It must run through Admin/control-plane tasks with bounded concurrency and module-level validation.

## Reading Path

1. [Roadmap](roadmap.md)
2. [Sync architecture](topics/sync-architecture.md)
3. [Version gating, import records and restore](topics/version-gating-import-records-and-restore.md)

## Scope

In scope:

- Sender target-system management: name, endpoint, key, enabled state, default module selection and test connection.
- Receiver sync settings: enabled state, acceptance mode, generated key, allowed source systems and allowed modules.
- Push-based synchronization from sender to receiver.
- Dry-run, preview, manual approval and automatic apply.
- Idempotent imports keyed by `syncId`.
- JSON import records and JSON pre-import snapshots.
- Rollback and manual restore to the pre-import configuration.

Out of scope:

- Reusing the existing Admin API key or Request API key for cross-system sync.
- Tenant isolation or multi-operator authorization. The product remains a single-operator system.
- Automatic mesh replication between every system.
- Syncing historical usage records as part of configuration sync.
- Syncing Redis runtime state or any current in-flight request state.
- Changing hot-path scheduler selection semantics as part of this plan.

## Current State

Implementation is not started. The current code already has Admin APIs and storage domains for credentials, proxy resources, external pools, runtime config, model catalogs, usage and audit logs. This plan records the required target behavior before adding a sync surface, because importing the wrong version or partially applied config can break dispatch and account management.

## Completion Criteria

The plan is implementation-ready only when the following are specified in code and tests:

- exact supported version matrix and rejection behavior;
- sync-key storage and middleware separate from Admin authentication;
- export bundle schema and module schema versions;
- pre-import snapshot JSON format;
- import record JSON format;
- dry-run and manual approval workflow;
- transactional or staged apply/rollback semantics;
- restore workflow and audit output;
- no-regression tests proving normal dispatch is not affected.
