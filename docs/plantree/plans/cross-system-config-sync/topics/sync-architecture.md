# Sync Architecture

Role: Target architecture for cross-system configuration synchronization

Status: Planning

Related: [plan root](../README.md), [version gating and restore](version-gating-import-records-and-restore.md)

## Model

The feature uses a push model:

```text
Sender Admin UI
  -> selected target systems
  -> export selected modules into one versioned bundle per target
  -> POST bundle with the receiver's configSyncKey

Receiver sync endpoint
  -> authenticate sync key
  -> validate version and schema
  -> dry-run diff
  -> reject, queue for approval or apply
  -> write import record and snapshot
```

The receiver is authoritative for whether a bundle is accepted. The sender cannot bypass the receiver's mode, allowed modules, source allowlist or version gate.

## Key Boundary

Add one independent sync credential type:

- product name: `Admin Sync Key`;
- internal name: `configSyncKey`;
- authentication header: `x-config-sync-key` or `Authorization: Bearer <configSyncKey>`;
- storage: hash only on the receiver, encrypted secret on the sender target-system record;
- authority: only `GET /api/config-sync/v1/capabilities` and `POST /api/config-sync/v1/push`;
- no access to `/api/admin`, request-plane routes or upstream provider keys.

The sync key must not be accepted by the existing Admin middleware. The existing Admin key remains only for local operator control of the sender and receiver UIs.

## Receiver Modes

| Mode | Behavior |
| --- | --- |
| `disabled` | Reject all sync requests. Do not parse module payloads beyond the minimum needed to return a bounded error. |
| `manualApproval` | Validate the bundle, create a pending proposal and precompute a diff. Do not mutate configuration until an Admin user approves it locally. |
| `autoAccept` | Validate and apply allowed modules automatically. Still write the import record, pre-import snapshot and audit entries. |

Receiver settings must also include allowed modules and optional allowed source IDs. If a sender asks to apply a module not allowed by the receiver, that module is rejected before apply.

## Sender Target Systems

The sender stores multiple target systems:

- target ID and display name;
- base URL;
- encrypted receiver `configSyncKey`;
- enabled flag;
- default selected modules;
- last capability probe;
- last sync result;
- last failure reason.

Sending to multiple targets produces one job per target. A failed target does not fail the other targets.

## Module Boundaries

First-class modules:

| Module | Includes | Default sensitive behavior |
| --- | --- | --- |
| `proxyResources` | proxy URL, protocol, username/password, enabled flag, notes | credentials excluded unless `includeProxySecrets=true` |
| `localAccounts` | selected credential configuration, supported models, scheduling fields, proxy binding | tokens/API keys excluded unless `includeAccountSecrets=true` |
| `externalPools` | base URL, auth type, routing/body/model/usage settings, priority, concurrency, supported models | API keys excluded unless `includeExternalPoolSecrets=true` |
| `runtimeStrategy` | load balancing, cooldown, retry, admission, payload, body conversion, cache and model mapping settings | no user secrets expected, but version-gated strictly |
| `modelCatalogs` | model capabilities and pricing records | no user secrets expected |

Do not sync:

- request usage records and rollups;
- Redis leases, queues, cooldowns, sticky bindings or runtime coordination state;
- credential runtime failures, current in-flight counts and transient scheduler health;
- Admin API keys or Request API keys;
- local logs, uploaded files, process-local caches or tool debug JSONL.

## Bundle Flow

1. Sender probes receiver capabilities.
2. Sender builds one bundle with selected modules and module schema versions.
3. Sender submits the bundle with a unique `syncId`.
4. Receiver authenticates the sync key.
5. Receiver checks protocol, app, config and module versions.
6. Receiver computes a dry-run diff and stores a pending record when required.
7. Receiver persists the pre-import snapshot before any apply.
8. Receiver applies module patches with idempotency and local conflict rules.
9. Receiver verifies the resulting state and records success or failure.
10. Receiver emits Admin audit summaries and invalidates local runtime snapshots where needed.

## Conflict Policy

Default behavior is non-destructive upsert:

- create missing receiver objects;
- update matched receiver objects only for selected fields;
- preserve receiver-only fields when not part of the selected module;
- do not delete receiver objects that are absent from the sender;
- do not reset scheduler runtime state;
- do not overwrite Admin or Request API keys.

Optional destructive modes require a separate design and preview because deleting receiver-only accounts, proxies or pools can immediately change production routing.

## Storage Surfaces

Proposed new durable tables:

| Table | Role |
| --- | --- |
| `config_sync_receiver_settings` | receiver mode, key hash, key metadata, allowed modules and allowed source IDs |
| `config_sync_targets` | sender-side target systems and encrypted target keys |
| `config_sync_jobs` | outbound send jobs and per-target results |
| `config_sync_import_records` | receiver-side import records, including rejected/manual/applied/restored states |
| `config_sync_pending_imports` | manual approval proposals and preview diffs |
| `config_sync_object_links` | source object to receiver object mappings for idempotent upserts |
| `config_sync_snapshots` | encrypted or JSONB pre-import snapshots used for restore |

Existing `admin_audit_logs` may record summaries but must not be the only source of import/restore truth.

## UI Surface

Add one Admin page with three sections:

- Receiver settings: enable switch, acceptance mode, key generation/rotation/revocation, allowed modules and allowed sources.
- Target systems: target CRUD, connection test, capability probe and last result.
- Send config: choose targets, choose modules, choose whether to include secrets, dry-run, send and per-target result view.

Manual approval mode adds a receiver-side pending import view with source, selected modules, version result, diff summary, sensitive-field flags and approve/reject actions.

## Hot-Path Isolation

Sync must remain a control-plane task:

- no sync operation runs inside request execution or account selection;
- module apply tasks use bounded concurrency;
- runtime reload happens after durable commit;
- existing requests keep their current snapshots;
- scheduler Redis state is neither imported nor cleared;
- usage history is not scanned or copied by configuration sync.
