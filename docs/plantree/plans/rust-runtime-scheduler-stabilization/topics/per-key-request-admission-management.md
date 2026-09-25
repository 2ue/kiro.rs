# Per-Key Request Admission Management

- Status: implementation and local verification complete; release publication pending
- Owner: Rust Runtime Scheduler Stabilization
- Last reviewed: 2026-09-25 Asia/Shanghai
- Related: [Local/external route concurrency scenarios](local-external-route-concurrency-scenarios-20260925.md), [Route planner and capacity ledger](route-planner-capacity-ledger.md)

## Goal

Give each downstream request API Key its own configurable RPM, active-request
limit, queue limit, and queue timeout through the existing Admin security/key
management page. Keep the legacy `apiKey` and `apiKeys` fields valid and
preserve their current global `requestAdmission` behavior unless an operator
explicitly manages that key.

The per-key admission limit is an instance-local HTTP admission contract. It is
not a per-upstream-account limit and is not a cross-instance distributed quota.

## Compatibility Contract

1. `apiKey` remains the legacy primary key field. `apiKeys` remains the legacy
   additional-key field. Their order and legacy Admin response field
   `requestApiKey` remain unchanged.
2. Existing configuration with only `apiKey`, with `apiKey + apiKeys`, or with
   no `requestApiKeyPolicies` continues to authenticate those same secrets.
3. A key with no managed policy uses the global `requestAdmission` values,
   exactly as before. Merely upgrading must not rewrite, rotate, disable, or
   re-limit an existing key.
4. Creating a managed key adds it to the legacy key set as well as recording
   its policy. Older clients and scripts that read `apiKey`/`apiKeys` can still
   see the key. The new service applies its managed per-key policy.
5. The policy configuration is never included in the generic runtime-config
   response. Key secrets and policy edits use the existing authenticated
   `/api/admin/security/keys` and `/api/admin/security/request-keys` endpoints.
6. Editing a key value is an explicit rotation: the old value stops
   authenticating and the replacement takes its place. Creating a key does not
   invalidate any existing key. Disabling/deleting a key is explicit and
   affects new authentication immediately after runtime config propagation.
7. At least one request key must remain enabled. Existing in-flight response
   bodies are not cancelled by disable, delete, or a lower concurrency limit.

## Policy Semantics

`requestApiKeyPolicies` is optional persisted configuration. Each policy has:

| Field | Meaning |
| --- | --- |
| `apiKey` | Secret whose digest associates this policy with an authenticated identity |
| `name` | Admin-only display label, trimmed and limited to 80 characters |
| `enabled` | Whether new requests using this key authenticate; defaults to `true` |
| `requestAdmission` | Optional per-key override; absent means use global `requestAdmission` |

The runtime admission controller stores policies by the existing SHA-256 key
identity. It does not store raw secrets in request extensions, logs, or
admission state. Per-key `rpm`, `maxConcurrentRequests`,
`maxQueuedRequests`, and `queueTimeoutMs` use the same bounds and zero-value
semantics as global `requestAdmission`.

Each instance has independent active, queued, RPM, and rejection state for
each key. For a key with `maxConcurrentRequests = 5`, at most five requests
from that key hold admission permits at once on one instance. Additional
requests enter only that key's FIFO queue, subject to that key's queue limit
and timeout. When its queue is unavailable, full, or timed out, the request
receives the existing rate-limit response. It is not silently rerouted to an
external pool.

The admission permit remains held through response-body EOF/error or client
drop. Lowering a limit does not revoke permits already held by valid requests;
new permits wait until active count falls below the new limit. A policy reload
wakes that key's waiters so they re-read current limits.

## Routing And Capacity Boundary

Admission is before request body parsing and route selection. Separate keys
therefore isolate the HTTP entry gate, including its active count, queue, RPM,
and queue rejection state. This does not create separate local credential
pools, separate external-pool managers, or separate Redis scheduler queues.
Different keys can still contend for shared local/external dispatch capacity
after they pass admission.

Route allow/deny rules remain the existing endpoint-level configuration. This
feature does not bind a key to an endpoint or promise per-key local/external
pool isolation. A future scheduler-domain feature must add per-domain capacity
ledger/queue semantics and prove that normal long-running streams are never
preempted before making that guarantee.

## Admin API And UI

- Extend each item in the existing request-key list with name, enabled state,
  and optional per-key `requestAdmission`.
- Return `defaultRequestAdmission` separately so old keys show the inherited
  values without being silently converted to managed policies.
- Create initializes a managed key from the current global limits unless the
  caller supplied explicit limits.
- Update can edit metadata/policy without rotating the key; `apiKey` is
  optional for policy-only updates.
- Keep `requestApiKey` as the first key for existing frontends.
- Update both `ui` and `admin-ui`; retain fallback rendering for older Admin
  responses that only contain `requestApiKey`.

## Reload And Failure Behavior

- Startup and both Redis-notified and periodic runtime-config reload paths must
  replace the authentication set and policy overrides from the same loaded
  config.
- A legacy key absent from the managed-policy list always falls back to the
  global policy.
- A policy with no admission override also falls back to the global policy.
- Invalid bounds, duplicate managed secrets, policy secrets not present in the
  request-key list, or a configuration with no enabled key must fail closed at
  Admin validation; never silently broaden a configured cap.
- In a multi-instance deployment, config propagation remains eventual. Each
  instance independently enforces the same per-key values; aggregate active
  concurrency can reach `instance_count * per_key_limit`.
- Older binaries ignore the new policy metadata. Managed keys remain present
  in the legacy key fields for authentication compatibility, but policy limits
  only take effect after every serving instance runs a version that supports
  them. Do not use mixed-version rollout as a way to enforce new per-key caps.

## Acceptance Criteria

1. An old config containing only `apiKey` still accepts that key after startup
   and reload.
2. Old `apiKey + apiKeys` keys remain accepted after adding or editing another
   key.
3. Key A at its active/queue/RPM limit does not consume Key B's corresponding
   admission state.
4. Key A with a limit of five never receives a sixth concurrent admission on a
   single instance; existing long streams are allowed to finish naturally.
5. Key A queue-full/timeout does not reject Key B; queue overflow returns a
   bounded 429 and never invokes an external fallback.
6. Updating one key's policy does not change other managed keys or legacy
   global defaults.
7. Disabled keys reject new requests, while other keys continue normally.
8. Generic runtime-config responses do not contain raw managed key secrets.
9. Both frontends can create, name, tune, disable, rotate, and delete keys while
   retaining old-response compatibility.
10. Per-instance behavior is explicit in documentation and tests: two independent
    controllers each enforce the configured per-key cap, so aggregate concurrency
    scales with instance count. Local/external dispatch queues remain shared.

## Verification Matrix

### Unit

- Legacy config deserialization and `apiKey`/`apiKeys` merge/dedup.
- Managed-key serialization, normalization, duplicate/bounds validation, and
  legacy fallback for absent policy.
- Independent active concurrency, FIFO queue, RPM bucket, rejection counters,
  and live policy reload for two keys.
- Lowering a limit does not cancel already-held permits.
- Disabled key is excluded from authentication while another key remains valid.

### HTTP / mock

- Two keys traverse the Axum admission middleware in an HTTP mock router with
  independent permits and a stub downstream handler.
- Saturate A, then prove B gets a normal mocked response.
- Fill A's queue, then prove B is admitted and A's queue response is a bounded
  429 rather than external-pool fallback.
- Verify response-body completion and client disconnect release only the
  matching key's permit.
- Verify legacy primary/extra-key config deserialization, store replacement,
  policy reload semantics, and persistence merge behavior with unit tests.

### Frontend / release

- Typecheck and build both frontend surfaces.
- Scoped Rust format, default/no-default full tests, all-target check, release
  build, CI Clippy baseline, both UI builds, fake-upstream loadtest smoke, and
  build-artifact inventory gate.
- The fake-upstream smoke validates the frozen loadtest runner, not a live
  account scheduler. Per-key saturation and recovery are validated by the HTTP
  middleware mock; the operator-configured `19023` service is not load-tested.

## Rollout And Rollback

Upgrade all serving instances before relying on per-key limits. Existing
`apiKey`, `apiKeys`, and global `requestAdmission` remain authoritative for
legacy keys. Create and verify a new managed key with a conservative limit,
then migrate one client at a time. Keep the old key until client cutover is
confirmed; do not rotate it as part of a feature rollout.

Rollback consists of removing the per-key policy entries or restoring the
previous runtime configuration. This returns affected keys to the global
admission policy without cancelling already-running response bodies. Preserve
the prior config snapshot and release tag until all instances confirm reload.
