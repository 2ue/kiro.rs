# AWS Kiro API Key And Region Credential Lifecycle

Status: `core-and-malformed-lifecycle-verified-on-provisional-build / non-Docker runner contract verified / final-build-and-browser-gates-pending`

Severity: P1

Last verified: 2026-07-16

Evidence: [AWS API key and region lifecycle](../evidence/aws-api-key-region-lifecycle-20260716.md), [non-Docker runner contract](../evidence/aws-api-key-region-nondocker-runner-contract-20260721.md)

## Scope And Impact

This issue covers `ksk_*|region` credentials from ingestion through normalization, PostgreSQL persistence, scheduler selection, region-specific request decoration, duplicate handling, explicit backup export, disable/delete, restart and reload. It also covers the auxiliary model-discovery and balance traffic created by imports, because those calls contribute to the internal-RPM amplification reported by the user.

Affected entry paths are the Admin API, JSON credential file and plain-text credential file. The two browser UIs call the same Admin API, but browser interaction and page-level warning behavior remain a separate F05/F06 release gate and are not claimed by the core lifecycle evidence.

## User-Visible Symptoms And Fingerprints

The defects do not have one stable response fingerprint:

- An API-key credential uses the CLI endpoint family by default. Before the fix, `kiroUpstreamBaseUrl` redirected IDE transport but not CLI inference, model discovery, MCP or balance/overage calls. Local validation could unexpectedly attempt official Kiro/AWS hosts instead of the configured fake upstream.
- Re-importing an existing API key eventually returned a duplicate error, but model discovery could run before the database uniqueness check. A duplicate client retry therefore produced internal auxiliary RPM even though no credential was added.
- An explicit administrator backup intentionally contains reusable credentials, but its HTTP response was cacheable by default and had no `nosniff` protection.
- Malformed pipe input such as `|us-east-1`, multiple `|`, or a host-unsafe region could survive normalization. Admin import then performed model discovery/balance and could persist a credential that would later fail while constructing Authorization/Host headers.
- Region or auth normalization regressions can instead appear as the wrong `Host`, missing `tokentype: API_KEY`, OAuth fields surviving on an API-key row, an unselectable credential after restart, or a request signed/routed as the wrong endpoint. None of those variants requires a `Hashxxxxxxxx` tool-name fingerprint.

## Source And Runtime Chain

```text
Admin API / JSON file / plain file
  -> credential parser and canonicalization
  -> API key and region normalization
  -> duplicate preflight
  -> optional model discovery and balance lookup
  -> TokenManager and PostgreSQL credential row
  -> restart/reload
  -> scheduler selects credential
  -> CLI inference/model-management/balance endpoint
  -> transport override + logical region Host + Bearer + tokentype
```

The database remains the authority for uniqueness. The in-process duplicate preflight exists only to reject known duplicates before auxiliary network calls.

## Root Causes

### 1. Transport override was endpoint-family specific

CLI inference, MCP and model discovery constructed official URLs directly in `src/kiro/endpoint/cli.rs`. Balance and overage calls did the same in `src/kiro/token_manager/refresh.rs`. The existing `kiroUpstreamBaseUrl` contract therefore did not cover an API-key credential whose normalized endpoint is `cli`.

### 2. Duplicate rejection happened after auxiliary work

`add_credential` attempted model discovery before `TokenManager::add_credential` reached PostgreSQL uniqueness enforcement. The final conflict was correct, but it was too late to prevent duplicate-triggered upstream traffic. Independent request paths could also enter this section concurrently within one process.

### 3. Sensitive export lacked response controls

The explicit full-backup endpoint was authenticated and audited, but its response builder did not set anti-cache or content-sniffing headers. The product contract deliberately preserves a reusable full backup; silently masking that body would break restore semantics.

### 4. Parse failure was not a validation failure

`split_kiro_api_key_and_region` returned `None` for an empty key, but `normalize_api_key_defaults` kept the original non-empty string. `TokenManager` only checked `trim().is_empty()`, so `|region` passed as a key. `split_once` also accepted a second pipe as region content, and region text was not checked before becoming a logical HTTP Host label. JSON credentials were validated only after bootstrap work, and Admin API auxiliary calls happened before final storage rejection.

## Reproduction

### Minimal transport/region reproduction

1. Configure a localhost fake upstream through `kiroUpstreamBaseUrl`.
2. Add a fake `ksk_*|eu-west-3` credential with no OAuth fields required.
3. Trigger model discovery, balance and one Messages inference.
4. Before the fix, one or more CLI-family calls bypass the fake server. After the fix, every call reaches the fake server while retaining the logical region host (`management.<region>.kiro.dev`, `runtime.<region>.kiro.dev` or `q.<region>.amazonaws.com`), `Authorization: Bearer ...` and `tokentype: API_KEY`.

### Minimal duplicate-RPM reproduction

1. Count fake-upstream requests by type.
2. Import a new fake API key and wait for the normal discovery/balance calls.
3. Import the same normalized key again.
4. Before the fix, the rejected duplicate may add model-discovery hits. After the fix, it returns HTTP 409 and adds zero discovery, balance, inference, overage or unknown hits.

### Minimal export reproduction

1. As an authenticated administrator, request each explicit backup format: `json`, `backup-json`, and `jsonl`.
2. Verify the body contains the reusable fake key by design.
3. Verify `Cache-Control: no-store, private`, `Pragma: no-cache`, and `X-Content-Type-Options: nosniff`.
4. Verify ordinary list responses, service logs and audit details do not contain the full key.

### Malformed key/region reproduction

For both Admin API and JSON-file bootstrap, run each shape three times:

```text
|us-east-1                     # empty key
ksk_fake|us-east-1|extra       # multiple pipe separators
ksk_fake|us east-1             # whitespace in region
ksk_fake|us-east-1\nextra     # control character in region
ksk_fake|us-east-1.example     # region escapes one DNS label
```

Admin API and JSON additionally test the same unsafe values in explicit `region`, `authRegion` and `apiRegion` fields while the key itself has no pipe. Plain text has no explicit-field syntax, so its applicable matrix is the five pipe forms.

The old fixed binary `8bf11e58...` failed the first JSON case by becoming healthy instead of rejecting it. The repaired contract is Admin HTTP 400 or JSON startup rejection before listener readiness, zero model-discovery/balance/inference/unknown hits, zero active PostgreSQL credential rows, and no raw input in response/log/report.

### Multi-round isolated reproduction

```bash
KIRO_RS_BINARY=/abs/outside/repo/frozen/kiro-rs \
KIRO_VALIDATION_ARTIFACT_DIR=/abs/outside/repo/artifacts \
KIRO_F06_POSTGRES_URL='postgres://...@127.0.0.1:<pg-port>/kiro_f06_<owned_empty_db>' \
KIRO_F06_REDIS_URL='redis://127.0.0.1:<redis-port>/<nonzero-db-1-15>' \
KIRO_F06_REDIS_PREFIX='kiro_rs:f06:<unique-owned-prefix>' \
KIRO_F06_ROUNDS=3 \
node feature/tests/aws-api-key-region-lifecycle.mjs
```

The runner no longer creates Docker PostgreSQL/Redis containers. It now requires caller-owned loopback PostgreSQL/Redis inputs, a frozen external binary and an owned artifact root. It runs Admin API, JSON file and plain file entries three times each, including a service restart in every case. It must not use port 9022 or a real AWS/Kiro credential, and it must not reuse or flush a shared Redis database.

## Selected Fix

1. Add one `configured_upstream_url` helper and use it for CLI inference, MCP, model discovery, balance and overage transport URLs. The override changes the TCP destination only; endpoint decorators still derive the logical `Host` from the credential region and preserve API-key headers.
2. Serialize imports within an `AdminService` instance, normalize first, hash the canonical API key or refresh token, and reject a snapshot-known duplicate before any discovery/balance work. Keep PostgreSQL partial unique indexes as the final authority and classify duplicates as HTTP 409.
3. Keep the explicit administrator full-backup body unchanged, but mark every format `no-store, private`, `no-cache` and `nosniff`. Continue masking ordinary credential-list responses and excluding reusable keys from logs/audit detail.
4. Validate the optional `key|region` envelope and explicit `region`/`authRegion`/`apiRegion` fields before canonicalization side effects, Admin auxiliary calls or file bootstrap persistence. Reject an empty key, multiple separators, and region values that are not one safe DNS label. Do not require `ksk_` or a fixed AWS region allowlist on Admin/JSON, preserving future key/region compatibility; plain text keeps its existing `ksk_` entry rule.

## Alternatives And Tradeoffs

- Masking the explicit backup would be safer for accidental disclosure but would make the documented restore artifact unusable. A future masked GET plus explicit reveal/POST workflow is a product/API redesign, not a compatible patch.
- Moving all import auxiliary calls after database insertion would avoid pre-insert traffic, but introduces partially initialized credentials and rollback complexity. Preflight plus final DB uniqueness keeps the existing lifecycle while eliminating sequential duplicate amplification.
- A process-local lock is intentionally low overhead and removes the common same-instance race. It does not coordinate different service instances; cross-instance admission requires Redis/PostgreSQL-backed import ownership or an outbox/job design.
- A fixed AWS-region allowlist would reject newly launched or partition-specific regions and would turn an input-safety patch into product policy. The selected check only enforces a host-safe label (ASCII alphanumeric/hyphen, no leading/trailing hyphen, at most 63 bytes).

## Compatibility And Performance Boundaries

- The URL override is inactive when unset, so production endpoint construction remains unchanged.
- The duplicate preflight hashes one normalized secret and scans the in-memory credential snapshot under an async process-local lock. It avoids the substantially more expensive network calls on duplicates but serializes imports in one process. Inference and credential selection do not acquire this lock.
- Pipe validation is one bounded scan of the credential string and runs before network/storage work. It does not parse successful request bodies or affect inference hot paths.
- The current lifecycle harness restarts the service for each case and samples RSS/FD. It is not a high-concurrency import benchmark or L5 soak proof.
- Logical `Host` differs from the localhost transport host under fake-upstream validation by design. Proxies or test servers must accept this split-host contract.

## Acceptance Matrix

| Area | Required result | Current state |
| --- | --- | --- |
| Admin API import | normalized row; exactly one discovery and one balance call | pass, 3 rounds per full run |
| JSON file import | normalized row; no balance call; at most one bootstrap discovery | pass, 3 rounds per full run |
| Plain file import | normalized row; no balance call; at most one bootstrap discovery | pass, 3 rounds per full run |
| PostgreSQL | key stored without `|region`; hash present; OAuth fields cleared | pass |
| Scheduler/reload | selected key digest and region preserved before/after restart | pass |
| Request decoration | valid logical Host, Bearer scheme and `API_KEY` token type | pass |
| Duplicate | HTTP 409 and zero auxiliary hits | pass within one instance |
| Delete | active delete rejected; disable then soft-delete succeeds | pass |
| Export/list/audit | explicit backup reusable and non-cacheable; ordinary surfaces masked | pass for API paths |
| Malformed Admin input | eight pipe/explicit-region classes x 3: 400, 0 auxiliary/PG, no reflection | pass on provisional build |
| Malformed JSON bootstrap | eight pipe/explicit-region classes x 3: pre-health reject, 0 auxiliary/PG, no log leak | pass on provisional build |
| Explicit region fields | Admin/JSON `region`/`authRegion`/`apiRegion` unsafe values x 3 | pass on provisional build |
| Malformed plain file | five applicable pipe classes x 3: reject whole file, 0 auxiliary/PG | pass on provisional build |
| Resources/cleanup | bounded per-case RSS/FD; owned Redis prefix/temp secrets removed; random service port released | pass for lifecycle harness |
| Two UIs | import and backup warning through real browser interaction | pending parent F05/F06 gate |
| Final candidate | rerun against frozen release-candidate SHA | pending scheduler/model-filter stabilization |

## Verified Results

Ten complete pass reports currently cover 90 positive lifecycle cases: three entry modes x three rounds x ten full runs. Six schema-v1 reports cover the original lifecycle; three schema-v2 reports on provisional binary `56232fd9...` cover 45 Admin and 45 JSON pipe-malformed cases; one schema-v3 report on `1ab25d64...` expands to explicit region fields and plain files, adding 24 Admin, 24 JSON and 15 plain malformed cases. Across the reports this is 153/153 malformed rejections. Every duplicate generated zero auxiliary hits, every malformed case generated zero upstream hits and zero active PG rows, every captured positive upstream request was classified and valid, and all cleanup flags are true. On 2026-07-21 the runner itself was converted to a non-Docker caller-owned PG/Redis contract and that contract passed 6/6 plus the shared 36/36 runtime contract batch.

The red run against old binary `8bf11e58...` failed at `json_file_invalid_empty_key_1` because the process was not rejected before startup. It exited the runner non-zero and cleaned all resources; it is evidence of the defect, not one of the ten pass reports.

A second red run used the pipe-only `56232fd9...` build with schema v3 and failed at `json_file_invalid_explicit_region_whitespace_1`, proving the explicit-field matrix detects a real bypass rather than only repeating the pipe parser tests.

These results establish the core API/file/PG/reload/scheduler/header lifecycle for the tested dirty-tree builds. They do not yet establish the final release candidate or browser UI behavior.

## Residual Risks

- Two different service instances can both pass their local snapshot preflight before one PostgreSQL insert wins. The losing insert is still rejected, but both instances may have already issued discovery calls. E06 must cover cross-instance auxiliary admission before release.
- `update_credential_auth` has a separate post-update discovery path and is not covered by the duplicate-import lock contract.
- Non-pipe API keys remain deliberately prefix-agnostic for compatibility. This patch validates envelope/Host safety, not whether a key is genuine; fake/expired keys still fail through ordinary upstream validation/use behavior.
- Existing PostgreSQL rows created before this validation are not backfilled or deleted by the patch. Invalid legacy rows need operator review/migration evidence; the change prevents new Admin/file imports from creating them.
- The explicit backup remains sensitive by design. Administrator authentication, audit, TLS/deployment controls and UI warning text remain required.
- The harness uses fake keys and a fake upstream without an outbound-deny network namespace. It proves captured transport/header behavior and avoids usable credentials; it is not a packet-level proof that no unrelated process contacted AWS/Kiro.
- No UI browser interaction, high-concurrency import burst, multi-instance race, long soak or real AWS/Kiro call is claimed here.
