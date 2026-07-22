# AWS API Key And Region Lifecycle Evidence

Status: `core-and-malformed-harness-pass / final-candidate-and-browser-pending`

Date: 2026-07-16

Scope: F06 Admin API, JSON file and plain file credential ingestion through PostgreSQL, reload, scheduler-selected inference, region request decoration, duplicate handling, explicit backup, audit, delete and cleanup. This is not UI browser, real AWS/Kiro, multi-instance import or release evidence.

Issue authority: [AWS Kiro API key and region credential lifecycle](../issues/aws-kiro-api-key-region-lifecycle.md)

## Problems Reproduced During Harness Construction

The first safe end-to-end design could not cover API-key credentials because their normalized `cli` endpoint ignored `kiroUpstreamBaseUrl` for inference, MCP, model discovery and balance/overage calls. Direct source inspection confirmed hard-coded official URL construction. The fix makes the configured URL the transport destination while retaining region-derived logical `Host` headers.

The first duplicate design also showed that model discovery ran before the eventual DB uniqueness failure. A process-local import lock plus hash/snapshot preflight now rejects sequential same-instance duplicates before auxiliary work. PostgreSQL remains the final uniqueness authority.

The explicit reusable backup response initially had no anti-cache headers. The body remains deliberately sensitive under the administrator backup contract; the response now sets `no-store, private`, `no-cache` and `nosniff`.

After the first lifecycle passes, static review found that a failed `key|region` split was not treated as invalid. `|us-east-1` remained a non-empty key, multiple pipes became region content, and host-unsafe region characters reached auxiliary calls/storage. The schema-v2 runner reproduced this against old binary `8bf11e58...`: `json_file_invalid_empty_key_1` became healthy rather than being rejected. The runner failed non-zero and cleaned its resources; no pass report was generated for the red run.

## Implementation Under Test

- `src/kiro/endpoint/mod.rs`: shared transport override resolver.
- `src/kiro/endpoint/cli.rs`: CLI inference, MCP and model discovery use the override and preserve logical region headers.
- `src/kiro/token_manager/refresh.rs`: balance and overage calls use the override and preserve `q.<region>.amazonaws.com`, Bearer and `tokentype`.
- `src/admin/service.rs`: canonical hash duplicate preflight under a process-local async import lock; conflict maps to HTTP 409.
- `src/kiro/model/credentials.rs`: bounded pipe-envelope validation and JSON-file rejection before bootstrap persistence; no fixed key prefix or AWS-region allowlist.
- `src/kiro/model/credentials.rs`: the same bounded label rule covers explicit API-key `region`, `authRegion` and `apiRegion`; plain-text parsing keeps its existing `ksk_` entry rule.
- `src/admin/service.rs`: malformed Admin API input is rejected before model discovery, balance or storage.
- `src/admin/handlers.rs`: sensitive backup response headers.
- `feature/tests/aws-api-key-region-lifecycle.mjs`: isolated executable lifecycle runner.

## Safety And Isolation

- All credentials were randomly generated fake `ksk_f06_*` values. No real credential file was read.
- Each full run created a unique `postgres:16-alpine` and `redis:7-alpine` container and random localhost ports.
- Every service port was dynamically selected and asserted not to equal 9022.
- `kiroUpstreamBaseUrl` pointed only to an in-process localhost fake server.
- The runner did not configure a real AWS/Kiro endpoint or usable key. Schema v2 reports distinguish `realAwsOrKiroConfigured=false`, `realAwsOrKiroAccessObserved=false` and `outboundFirewallEnforced=false`; the safety claim is configuration/capture based rather than packet-firewall evidence.
- Reports store only truncated SHA-256 key digests and authorization schemes, never reusable fake keys.

## Commands

Red command against the old fixed binary:

```bash
KIRO_RS_BINARY=target/f06-binaries/kiro-rs-8bf11e5864bf6ab1 \
  KIRO_F06_ROUNDS=3 \
  node feature/tests/aws-api-key-region-lifecycle.mjs
```

Observed result: fail at `json_file_invalid_empty_key_1` because startup was not rejected. The command left no F06 container, process, port or temp directory.

Green schema-v2 command, executed three independent times:

```bash
cargo build --bin kiro-rs
cp target/debug/kiro-rs target/f06-binaries/kiro-rs-56232fd937c0cb3b
KIRO_RS_BINARY=target/f06-binaries/kiro-rs-56232fd937c0cb3b \
  KIRO_F06_ROUNDS=3 \
  node feature/tests/aws-api-key-region-lifecycle.mjs
```

Focused source checks used during implementation:

```bash
cargo test cli_upstream_override_changes_transport_but_preserves_region_headers -- --nocapture
cargo test api_key_usage_limits_honors_transport_override_and_region_headers -- --nocapture
cargo test sensitive_credential_export_is_not_cacheable_or_content_sniffable -- --nocapture
cargo test api_key_pipe -- --nocapture
cargo test explicit_api_key_regions_reject_host_unsafe_values_for_three_rounds -- --nocapture
cargo test plain_credentials_reject_malformed_pipe_forms_for_three_rounds -- --nocapture
cargo test kiro::model::credentials::tests:: -- --nocapture
node --check feature/tests/aws-api-key-region-lifecycle.mjs
git diff --check
```

Current provisional-source result: transport 1/1, usage-limit transport/header 1/1, export headers 1/1, `api_key_pipe` 4/4, explicit region 1/1, plain malformed 1/1, and the complete credential module 61/61. Critical invalid/compatible test bodies each run three rounds and assert load errors do not echo input. `cargo build --bin kiro-rs`, runner syntax, scoped rustfmt, Node syntax and diff checks passed. All must still be rerun against the frozen final candidate; provisional success is not substituted for that gate.

## Report Identity

All reports are generated artifacts under `target/f06-reports/`; the committed evidence records hashes and summaries rather than reusable secrets.

| Report | Schema | Binary SHA-256 | Dirty diff SHA-256 | Report SHA-256 | Result |
| --- | ---: | --- | --- | --- | --- |
| `f06-20260715222836809-50424-c969a7.json` | 1 | `2fc60ec91cb3f22f4848dff34985e83666d033b92e070397ac354401ac39391b` | `102172c672fb8b0159d7010784861569859c3d52a549ab120b4dc7b5034c03b6` | `e52827a8f76ccd0cc90c8b9816836b655bb3c8cf2827c3d606a4c39081f8b174` | pass |
| `f06-20260715223135473-69034-880c0b.json` | 1 | `2fc60ec91cb3f22f4848dff34985e83666d033b92e070397ac354401ac39391b` | `e4fb51dffd7a14024d18559ee98c365b76e6b010c794d711229e44349e57e54c` | `33837659fcf6fb5a3ce352b7c1919ee87e7c291b94d3100304dd3cb42c81ec94` | pass |
| `f06-20260715223517418-93742-9525b9.json` | 1 | `8bf11e5864bf6ab1781d18969a1871766ded1f0cd46831221e8f01f0e3849edd` | `e4fb51dffd7a14024d18559ee98c365b76e6b010c794d711229e44349e57e54c` | `a813a66c3e1267c5cfac7fb8a33d5f985dcfb9741d778b90bf4c11918e23abaa` | pass, provisional |
| `f06-20260715223549220-96804-b28ee5.json` | 1 | `8bf11e5864bf6ab1781d18969a1871766ded1f0cd46831221e8f01f0e3849edd` | `e4fb51dffd7a14024d18559ee98c365b76e6b010c794d711229e44349e57e54c` | `818a8317fd9648d792010b16e662e9e1e192acec01bff8cd81fc3b4253456a0b` | pass, provisional |
| `f06-20260715223648569-4936-7bc6d8.json` | 1 | `8bf11e5864bf6ab1781d18969a1871766ded1f0cd46831221e8f01f0e3849edd` | `8eb5e90698bc955ba6d2f444049f705ec6b78b0879a51e4c007cfb194768ce82` | `672788fee2a332e6f843188d27937f4304ea92f90b74fcbe385a58f6107c3e1c` | pass, provisional |
| `f06-20260715223733740-11976-88fc96.json` | 1 | `8bf11e5864bf6ab1781d18969a1871766ded1f0cd46831221e8f01f0e3849edd` | `6fd43a67403e30a913b98aec39c7926e921001a06218ac2aa98b2f31a1ce0d9b` | `fd5fadf237ade1eadac23ce444c57b36ad3f358f3d3b2688935598f894dc2d7f` | pass, provisional |
| `f06-20260715230517133-12606-a76759.json` | 2 | `56232fd937c0cb3b5674085385b10809ffa42aa775d82ff370a5007d1d08260e` | `ef8ec6c501cacf5b2dccad14211e91ea02ad95123499eddd381755109f1554fe` | `dfeeda12d2fbe8e4560c88523f758769a6df1ac8799c4716ce292c39d8538782` | pass, malformed fix provisional |
| `f06-20260715230710811-25653-54b076.json` | 2 | `56232fd937c0cb3b5674085385b10809ffa42aa775d82ff370a5007d1d08260e` | `ef8ec6c501cacf5b2dccad14211e91ea02ad95123499eddd381755109f1554fe` | `866737badd8a13cb17b0f6c6d83a8ed026427f437dbf7b1adb33940da0cd6925` | pass, malformed fix provisional |
| `f06-20260715230814127-33576-9bf55a.json` | 2 | `56232fd937c0cb3b5674085385b10809ffa42aa775d82ff370a5007d1d08260e` | `ef8ec6c501cacf5b2dccad14211e91ea02ad95123499eddd381755109f1554fe` | `b52cbb77d13d8aaf6e170bbab8a9b85cbe472ff57bca26c6310daec8a838bff7` | pass, malformed fix provisional |
| `f06-20260715232346174-33729-e1bfca.json` | 3 | `1ab25d6443b80c12076112d570c5e64a5fb24be7d43884b4fc10dcb998b33f2b` | `86ba8857a33c40c4ad9e804659980509c4052946885d328207625c2c21c9579a` | `33ccaf838fc7dede1e204d9b09390f525f693f2fecbe5af743632e3bb4982278` | pass, explicit-region/plain fix provisional |

All runs were based on Git revision `401473ca1649997bdeccf4468e3add1bdb187248` plus a dirty remediation tree. The binary and diff identities therefore matter; the reports are not release evidence merely because the Git revision is the same.

## Executed Matrix And Aggregate Results

Each full run executed:

```text
admin_api  rounds 1, 2, 3
json_file  rounds 1, 2, 3
plain_file rounds 1, 2, 3
```

Ten complete runs produced 90/90 passing positive lifecycle cases. The three schema-v2 runs produced 45/45 Admin API and 45/45 JSON pipe-malformed rejections. Schema v3 added explicit `region`/`authRegion`/`apiRegion` bypasses and plain files: 24/24 Admin, 24/24 JSON and 15/15 plain cases. Total malformed evidence is 153/153. Every applicable class ran three rounds inside each report.

Aggregate HTTP statuses were 720 x 200, 159 x 400 and 90 x 409. Of the 400 responses, 90 were intentional active-delete rejections and 69 were malformed Admin imports. JSON/plain malformed cases reject before listener health and therefore have process-exit evidence rather than an HTTP status.

Aggregate fake-upstream calls were 180 model-discovery, 30 balance, 180 inference, zero overage and zero unknown. All of those calls belong to positive lifecycle work; all 153 malformed cases produced zero calls of every kind. Per positive lifecycle case:

- Admin API initial import: exactly one discovery and one balance request.
- JSON/plain bootstrap: at most one discovery and zero balance requests.
- Initial scheduler inference: exactly one inference and no auxiliary request.
- Reload scheduler inference: exactly one inference and no unexpected auxiliary request in the measured slice.
- Duplicate import: zero discovery, balance, inference, overage and unknown requests in all 90 cases.

Malformed cases also queried PostgreSQL after rejection and required zero active rows. Admin responses, JSON startup logs and generated reports were checked not to contain the malformed input or generated key.

## Persistence And Normalization Evidence

Every case queried PostgreSQL directly and required:

- exactly one active credential before delete;
- `auth_kind=api_key` and `authMethod=api_key`;
- API-key hash present;
- stored API key present for runtime use but without the `|region` suffix;
- `region`, `authRegion` and `apiRegion` equal to the imported region;
- normalized endpoint `cli`;
- stale access token, refresh token, client ID and client secret cleared.

After stopping and restarting the service, ordinary listing still returned the same region/endpoint without the full key, and scheduler inference returned text containing only the expected selected-key digest. This ties the selected runtime credential to the persisted row without exposing the key.

## Request Decoration Evidence

Every captured fake-upstream request required `valid=true` and recorded only the authorization scheme and key digest. The logical host matrix was:

| Call | Logical Host | Authorization | `tokentype` |
| --- | --- | --- | --- |
| model discovery | `management.<region>.kiro.dev` | Bearer | `API_KEY` |
| inference | `runtime.<region>.kiro.dev` | Bearer | `API_KEY` |
| balance | `q.<region>.amazonaws.com` | Bearer | `API_KEY` |

Unknown request count was zero in every report. The transport destination remained the localhost fake upstream even though the logical Host reflected the imported region.

## Duplicate, Delete, Export And Audit Evidence

- Every duplicate returned 409 and generated zero auxiliary hits.
- Deleting an active credential returned 400 by product contract.
- Disabling then deleting succeeded and left zero active rows plus at least one soft-deleted row.
- Ordinary list responses and service logs were asserted not to contain the full fake key.
- Explicit administrator backups rotated across `json`, `backup-json` and `jsonl`; the body contained the reusable fake key by design.
- Every backup response contained `Cache-Control: no-store, private`, `Pragma: no-cache` and `X-Content-Type-Options: nosniff`.
- Audit tables contained at least one export event and zero rows whose detail or error message matched `ksk_`.

## Latency And Resources

Across the nine reports, measured positive-request TTFB p50 was 29.09-41.25 ms, p95 was 171.25-215.68 ms and p99 was 180.34-264.93 ms. Total latency stayed close to TTFB for these local fake responses. Reports with first-text instrumentation recorded first-text p50 26.88-38.97 ms and p95 38.12-103.52 ms. Malformed JSON process-start rejection latency is intentionally not mixed into HTTP TTFB.

Per-case debug service RSS started at approximately 41-45 MiB and the sampled peak was no higher than about 64.5 MiB. FD count started at 26-30 and the sampled peak/end was 32. A service process is restarted within each case, and the runner samples at lifecycle checkpoints rather than continuously, so these values establish bounded sampled behavior, not a 15-30 minute L5 soak or absence of a short-lived or slow leak.

## Cleanup And Secret Scan

All ten pass reports record:

```text
containersRemoved=true
tempSecretsRemoved=true
portsReleased=true
```

Post-run checks found no F06 Docker container or service process. One empty directory left by an early runner-development failure was removed with `rmdir`; the current top-level `finally` cleanup is exercised by every successful report. Failure injection into every runner setup step remains a tooling-hardening gap, not an application lifecycle failure.

A report-content scan found zero `ksk_` markers, admin/request test secrets, stale OAuth fixture strings or Authorization values. Report-level digests are intentionally retained. Schema v2 also turns this into an in-run hard assertion over every generated fake key and fixed secret fixture before the report can be written as a pass.

## Limitations And Remaining Gates

- All report binaries are provisional. The schema-v3 report includes pipe, explicit-region and plain-file fixes, but PostgreSQL migration and final formatting/Clippy work were still changing the tree. F06 must run again against the frozen release-candidate SHA.
- The import lock is process local. A two-instance simultaneous duplicate can still produce auxiliary calls before PostgreSQL rejects one insert.
- No browser interaction was performed. Static parser/API evidence must not be represented as `ui` or `admin-ui` browser evidence.
- No real AWS/Kiro call was performed, and none is needed for the fake-upstream contract. Official-service behavior remains outside this evidence.
- No high-concurrency import burst, outbound-firewall proof, L5 soak or multi-instance auxiliary-admission test was performed.
