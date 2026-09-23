# Version Gating, Import Records And Restore

Role: Hard safety contract for imports, version compatibility, JSON records and lossless restore

Status: Planning

Related: [sync architecture](sync-architecture.md), [storage baseline](../../../baseline/storage-and-state.md)

## Core Rule

The receiver must prove that it understands the incoming bundle before it accepts or applies anything.

If the receiver cannot prove compatibility, it must reject the import with a structured error. The receiver must not attempt a partial or best-effort import across unknown versions, because current runtime configuration, credential fields, external-pool fields and model catalog semantics can change between releases.

For the first implementation, the safest rule is:

- require exact `protocolVersion`;
- require exact `configSchemaVersion`;
- require every selected module's schema version to be supported;
- require `runtimeConfigMigrationVersion` compatibility for any bundle containing `runtimeStrategy`;
- require exact package version match unless the target code contains an explicit compatibility allowlist for the source package version.

Because this project is still `0.0.x`, a patch version cannot automatically be treated as wire-compatible.

## Version Fields

Every bundle must include version metadata at the top level:

```json
{
  "protocolVersion": 1,
  "configSchemaVersion": "kiro-rs-config-sync/1",
  "source": {
    "instanceId": "01J...",
    "name": "system-a",
    "packageName": "kiro-rs",
    "packageVersion": "0.0.169",
    "gitSha": "37663e3a147a5cd8bedf6c42c375b5d90deebb21",
    "runtimeConfigMigrationVersion": 10
  },
  "compatibility": {
    "minReceiverPackageVersion": "0.0.169",
    "maxReceiverPackageVersion": "0.0.169",
    "moduleSchemaVersions": {
      "proxyResources": 1,
      "localAccounts": 1,
      "externalPools": 1,
      "runtimeStrategy": 1,
      "modelCatalogs": 1
    }
  },
  "syncId": "018f3f93-9d11-7d48-8b35-90cb2f0c4e17",
  "createdAt": "2026-09-23T00:00:00Z",
  "expiresAt": "2026-09-23T00:10:00Z",
  "modules": ["proxyResources", "localAccounts"],
  "payloadSha256": "..."
}
```

The receiver compares that metadata with its own local version snapshot:

```json
{
  "packageName": "kiro-rs",
  "packageVersion": "0.0.169",
  "gitSha": "37663e3a147a5cd8bedf6c42c375b5d90deebb21",
  "runtimeConfigVersion": 42,
  "runtimeConfigMigrationVersion": 10,
  "configSchemaVersion": "kiro-rs-config-sync/1",
  "supportedProtocolVersions": [1],
  "supportedModuleSchemaVersions": {
    "proxyResources": [1],
    "localAccounts": [1],
    "externalPools": [1],
    "runtimeStrategy": [1],
    "modelCatalogs": [1]
  }
}
```

## Rejection Response

Version rejection must be explicit and machine-readable:

```json
{
  "ok": false,
  "status": "rejected",
  "code": "VERSION_MISMATCH",
  "message": "配置同步版本不兼容，已拒绝导入",
  "details": {
    "reason": "receiverPackageVersionNotAllowed",
    "sourcePackageVersion": "0.0.169",
    "receiverPackageVersion": "0.0.170",
    "sourceConfigSchemaVersion": "kiro-rs-config-sync/1",
    "receiverConfigSchemaVersion": "kiro-rs-config-sync/2"
  },
  "syncId": "018f3f93-9d11-7d48-8b35-90cb2f0c4e17"
}
```

Rejected imports still create a bounded JSON import record with no module payload and no configuration mutation.

## Import Record

Every receiver-side import attempt creates one JSON record. The record is the durable audit and restore index, not a UI-only log.

Minimum shape:

```json
{
  "recordVersion": 1,
  "importId": "imp_018f3f99",
  "syncId": "018f3f93-9d11-7d48-8b35-90cb2f0c4e17",
  "status": "applied",
  "mode": "manualApproval",
  "source": {
    "instanceId": "01J...",
    "name": "system-a",
    "packageVersion": "0.0.169",
    "gitSha": "37663e3a147a5cd8bedf6c42c375b5d90deebb21",
    "runtimeConfigMigrationVersion": 10
  },
  "targetBefore": {
    "instanceId": "01K...",
    "packageVersion": "0.0.169",
    "gitSha": "37663e3a147a5cd8bedf6c42c375b5d90deebb21",
    "runtimeConfigVersion": 42,
    "runtimeConfigMigrationVersion": 10,
    "moduleVersions": {
      "proxyResources": 17,
      "localAccounts": 103,
      "externalPools": 9,
      "runtimeStrategy": 42,
      "modelCatalogs": 31
    }
  },
  "targetAfter": {
    "runtimeConfigVersion": 43,
    "moduleVersions": {
      "proxyResources": 18,
      "localAccounts": 104,
      "externalPools": 9,
      "runtimeStrategy": 42,
      "modelCatalogs": 31
    }
  },
  "selectedModules": ["proxyResources", "localAccounts"],
  "includeSecrets": {
    "accountSecrets": false,
    "proxySecrets": true,
    "externalPoolSecrets": false
  },
  "bundle": {
    "payloadSha256": "...",
    "moduleHashes": {
      "proxyResources": "...",
      "localAccounts": "..."
    }
  },
  "result": {
    "created": { "proxyResources": 5, "localAccounts": 2 },
    "updated": { "proxyResources": 3, "localAccounts": 10 },
    "skipped": { "proxyResources": 0, "localAccounts": 1 },
    "failed": {}
  },
  "snapshot": {
    "snapshotId": "snap_018f3f98",
    "snapshotSha256": "...",
    "restorable": true
  },
  "createdAt": "2026-09-23T00:00:03Z",
  "appliedAt": "2026-09-23T00:00:09Z",
  "approvedBy": "local-admin"
}
```

The `targetBefore` object is required. It is the "previous version" trace for this import and must be preserved as JSON for every import, including rejected and failed attempts where available.

## Pre-Import Snapshot

Before applying any mutation, the receiver persists a snapshot of every selected module that may change.

Minimum shape:

```json
{
  "snapshotVersion": 1,
  "snapshotId": "snap_018f3f98",
  "importId": "imp_018f3f99",
  "syncId": "018f3f93-9d11-7d48-8b35-90cb2f0c4e17",
  "createdAt": "2026-09-23T00:00:02Z",
  "targetVersion": {
    "packageVersion": "0.0.169",
    "gitSha": "37663e3a147a5cd8bedf6c42c375b5d90deebb21",
    "runtimeConfigVersion": 42,
    "runtimeConfigMigrationVersion": 10
  },
  "modules": {
    "proxyResources": {
      "rows": [],
      "objectLinks": []
    },
    "localAccounts": {
      "rows": [],
      "runtimeConfigRefs": [],
      "objectLinks": []
    },
    "runtimeStrategy": {
      "runtimeConfigJson": {},
      "runtimeConfigVersion": 42
    }
  },
  "integrity": {
    "canonicalJsonSha256": "...",
    "containsEncryptedSecrets": true
  }
}
```

The snapshot must contain the exact JSON/storage representation needed to restore selected modules without information loss. If a selected module contains secrets and the import would overwrite them, the snapshot must preserve those secrets using the same at-rest secret protection policy as the original storage. A redacted snapshot is not sufficient for lossless restore.

## Apply Semantics

The receiver must apply imports through a staged flow:

1. Insert or find the idempotency record for `syncId`.
2. Write the initial import record with status `validating`.
3. Validate versions, selected modules, allowed source and receiver mode.
4. Compute the dry-run diff.
5. In manual mode, store pending proposal and stop before snapshot/apply.
6. Immediately before apply, read the current target version and write the pre-import snapshot.
7. Apply all module changes in one transaction where possible.
8. If a module cannot share one transaction with all others, apply it through a staged record that is not made active until all selected modules pass verification.
9. Verify resulting module hashes and runtime-config version.
10. Mark import `applied` and publish local reload events.

If any step after snapshot creation fails, the receiver must either roll back the transaction or restore from the snapshot before returning success. A response must not say `applied` unless the post-apply state is verified.

## Restore Semantics

Restore uses the pre-import snapshot from an applied import record.

Rules:

- Restore requires local Admin approval.
- Restore validates that the snapshot schema and current package version are restorable.
- Restore writes a new import record with `status=restored` or `type=restore`.
- Restore preserves the original import record and snapshot.
- Restore updates only the modules captured by that snapshot.
- Restore does not delete newer unrelated import records.

Restore record example:

```json
{
  "recordVersion": 1,
  "importId": "imp_restore_018f4010",
  "type": "restore",
  "status": "restored",
  "restoresImportId": "imp_018f3f99",
  "restoresSnapshotId": "snap_018f3f98",
  "targetBefore": {
    "packageVersion": "0.0.169",
    "runtimeConfigVersion": 47
  },
  "targetAfter": {
    "packageVersion": "0.0.169",
    "runtimeConfigVersion": 48
  },
  "selectedModules": ["proxyResources", "localAccounts"],
  "createdAt": "2026-09-23T01:00:00Z",
  "approvedBy": "local-admin"
}
```

## What Must Not Be In The Snapshot

Configuration sync restore is not a database backup. It must not snapshot:

- `usage_records` or usage rollups;
- Redis leases, queues, cooldowns or sticky bindings;
- current in-flight request state;
- process-local caches;
- uploaded Files objects;
- ordinary logs.

Those domains are either high-volume, derived or runtime-only. Including them would make imports heavy and could recreate stale scheduling state.

## Tests Required Before Implementation Is Accepted

- Version mismatch for package, protocol, runtime migration and module schema returns `VERSION_MISMATCH` and mutates no module state.
- Rejected import creates a bounded import record with `targetBefore` when available.
- Manual approval creates no pre-import snapshot until approval time, because the target state can change while pending.
- Snapshot is taken from the immediate pre-apply state, not from initial preview time.
- Apply failure after snapshot restores or rolls back to the pre-apply state.
- Reapplying the same `syncId` is idempotent.
- Restore from an import snapshot returns selected modules to their exact previous JSON representation.
- Snapshot and import record logs do not leak raw secrets in plaintext output.
