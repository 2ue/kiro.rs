# Downstream standard usage field over 1m - 2026-07-31

Status: `analysis-recorded / production-evidence-collected / standard-field-guard-implemented / focused-tests-passed / scoped-release-gate-passed / production-recurrence-pending`

Severity: P1. A single downstream-standard usage field can exceed 1,000,000 in persisted usage data and, for successful requests, can match the final Anthropic-compatible `usage` object returned downstream. This is not reasonable for consumer-facing usage fields even when diagnostic request estimates are large.

Last observed: 2026-07-31 Asia/Shanghai

Evidence archive: `tmp/prod-evidence/20260731-174810-usage-1m-anomaly/20260731-174810-usage-1m-anomaly-redacted.tar.gz`

## 范围与结论

This issue tracks production rows where one standard downstream usage field exceeds 1,000,000:

- `input_tokens`
- `output_tokens`
- `cache_creation_input_tokens`
- `cache_read_input_tokens`

Mapping from persisted columns:

- `usage_records.compat_input_tokens` = downstream `input_tokens`
- `usage_records.output_tokens` = downstream `output_tokens`
- `usage_records.cache_creation_input_tokens` = downstream `cache_creation_input_tokens`
- `usage_records.cache_read_input_tokens` = downstream `cache_read_input_tokens`

Current conclusion:

- The anomaly is real on all three inspected production deployments.
- `output_tokens > 1,000,000` was not observed.
- Success cases are mostly usage projection issues, not upstream raw usage: `rawUsage.cache*` can be zero while the final standard cache field is over 1m.
- Error cases are a separate class: failure records store request-estimated input in the standard `input_tokens` column even though downstream error responses normally do not include a normal Anthropic `usage` object.
- 2026-07-31 focused fix: `cache_creation_input_tokens` now has a reported-usage final guard, parallel to the existing cache-read guard. The config fields are `finalCacheCreationMaxTokens`, `finalCacheCreationJitterMinTokens`, and `finalCacheCreationJitterMaxTokens`; defaults are `400000`, `20000`, and `45000`, so the effective cap is deterministic per usage shape and never above 400k.
- The focused fix covers current-high-cache success projection where input sampling moves the removed input delta into `cache_creation_input_tokens`, including stream, non-stream/record shaping, and external-pool `current_path_policy` uplift re-guarding.
- 2026-08-01 residual fix: routes with no full `reportedUsage` policy now still apply final standard cache read/write guards on local prompt-cache projected usage for `CurrentHighCache` and `KiroRsTool`; this covers the observed `/dfcache/team` `kiro_rs_tool` class without forcing full reported-usage projection.
- 2026-08-01 residual fix: local credential failure records and external pool failure records now keep large request estimates in diagnostic fields (`total_input_tokens`, raw/original usage where present) and write zero into downstream-standard usage fields for non-success statuses.
- 2026-08-01 scoped release gate passed in [Final release gate - 2026-08-01](../evidence/final-release-gate-20260801.md): full Rust default/no-default all-target tests, release build, UI/admin-ui build, Node contracts, real Claude CLI fake-upstream suite, feature docs, diff hygiene, fmt, and artifact inventory all passed for the current batch.
- No production writes, restarts, migrations, Redis writes, or broad container logs were used during analysis.

## 现象与影响

Recent 2h bounded aggregation:

| Host label | Rows over threshold | `input_tokens` | `cache_creation_input_tokens` | `cache_read_input_tokens` | `output_tokens` | Max input | Max creation | Max read | Max output |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Host A | 22 | 21 | 1 | 0 | 0 | 1,023,350 | 1,012,902 | 0 | 365 |
| Host B | 47 | 43 | 3 | 1 | 0 | 2,648,439 | 2,645,419 | 2,646,152 | 5,876 |
| Host C | 11 | 10 | 1 | 0 | 0 | 1,023,350 | 1,011,991 | 0 | 288 |

Representative rows:

| Request | Endpoint | Status | Route | Usage source | Standard usage shape |
| --- | --- | --- | --- | --- | --- |
| `req_01pjMDDpjARSfL9EWwC1kkiW` | `/cc/v1/messages` | success | local credential | local prompt cache | raw input 984,996; final `input_tokens=24`, `cache_creation_input_tokens=1,012,902` |
| `req_01fDr2p4AX7RKeqKGYfDg6ix` | `/cc/v1/messages` | success | local credential | local prompt cache | raw input 975,899; final `input_tokens=330`, `cache_creation_input_tokens=1,023,902` |
| `req_01z6vHKXkgRp3EENywdjTjE7` | `/cc/v1/messages` | success | local credential | local prompt cache | raw input 984,280; final `input_tokens=63`, `cache_creation_input_tokens=1,011,991` |
| `req_01dUjpoQo465ahPVJ7Xmis8y` | `/dfcache/team/v1/messages` | success | local credential | local prompt cache | raw input 304,883; final `input_tokens=349`, `cache_read_input_tokens=2,646,152` |
| `req_01oYrhnKQSYvfyBTkjaBS2N6` | `/dfcache/team/v1/messages` | success | local credential | local prompt cache | raw input 306,598; final `input_tokens=806`, `cache_creation_input_tokens=2,645,419` |
| `req_01p3VNze1ELJ8fJC9xjM4kag` | `/dfcache/team/v1/messages` | error | local credential | none | error record `input_tokens=2,648,439` |
| `req_01BCs1kmQHgZuCZuhiMSgDWD` | `/cc/v1/messages` | error | local credential | none | error record `input_tokens=1,720,038`; request body about 6.6 MiB |

Impact:

- Downstream clients, dashboards, billing views, and rollups can see implausibly large single-field values.
- Successful response usage can violate expected per-field sanity bounds even when raw upstream usage is lower.
- Error rows can be mistaken for final downstream usage if consumers do not distinguish diagnostic failure accounting from response usage.

## 根因与源码链

### Class 1: `/cc` and `/ha` high-cache success projection

Runtime route policy on the inspected deployments:

- `/cc` and `/ha` use `cacheType=current_high_cache`.
- Simulation uses `tokenScale=2.05`, `targetReadRatio=0.99`, and `maxSimulatedInputTokens=300000`.
- `/cc` and `/ha` reported usage sets `input.mode=sample-max`, `input.maxTokens=600`, and `input.moveDeltaToCacheRead=true`.
- `/cc` cache creation target is `55,000`; `/ha` target is `150,000`.

Relevant code:

- `CacheUsage::to_anthropic_usage_json` emits only `input_tokens`, `output_tokens`, `cache_creation_input_tokens`, and `cache_read_input_tokens`: `src/anthropic/cache.rs`.
- `CacheUsage::with_reported_cache_usage_policy_and_raw_evidence` samples/cache-projects fields, then samples input and moves the removed input delta to cache when configured.
- `add_input_delta_to_reported_cache` adds the input delta to `cache_read_input_tokens` only when there was cache-read evidence; otherwise it adds the delta to `cache_creation_input_tokens`.
- Therefore a raw input near one million can become `input_tokens <= 600` and `cache_creation_input_tokens` near one million when the cache-read field is zero.
- Fixed on 2026-07-31 for reported-usage-enabled paths: `ReportedCacheUsagePolicy::apply_final_standard_cache_guards` now applies both `apply_final_cache_read_guard` and `apply_final_cache_creation_guard` after field projection and input-delta movement. Handler and stream final usage paths call the combined guard after `apply_final_input_guard`; external-pool `current_path_policy` calls it before and after usage uplift.

Observed example:

- Raw input: 984,996.
- Final standard usage: `input_tokens=24`, `cache_creation_input_tokens=1,012,902`.
- `rawUsage.cacheCreationInputTokens=0`, so the large cache creation field is local projection, not upstream cache metadata.

### Class 2: `/dfcache/team` `kiro_rs_tool` uncapped projection

Runtime route policy:

- `/dfcache/team` uses `cacheType=kiro_rs_tool`.
- `reportedUsage` is null for that route.

Effect:

- The normal `/cc` and `/ha` reported-usage guard does not apply.
- Tool-cache simulated usage can directly produce final `cache_creation_input_tokens` or `cache_read_input_tokens` above 1m.
- Fixed on 2026-08-01 for local handler/stream downstream-standard fields: `apply_final_standard_cache_guards_for_standard_fields()` applies the default final cache-read and cache-creation caps even when full reported-usage projection is disabled, but only for local prompt-cache usage with cache fields under `CurrentHighCache` or `KiroRsTool`.

Observed examples:

- Raw input 304,883; final `cache_read_input_tokens=2,646,152`.
- Raw input 306,598; final `cache_creation_input_tokens=2,645,419`.

### Class 3: failures persist request estimate as standard input

Failure rows can store request-estimated input in `compat_input_tokens`.

Observed examples:

- Error record `input_tokens=2,648,439`, route `local_error_no_fallback`, upstream rate limit.
- Error record `input_tokens=1,720,038`, route `local_error_no_fallback`, queue full; payload body about 6.6 MiB.

This is useful diagnostic evidence, but it should not be treated as final downstream response usage unless the error protocol explicitly returns usage.

Fixed on 2026-08-01:

- Local credential failure records call `standard_usage_for_status()` before persisting standard fields. Non-success statuses write zero for `compat_input_tokens`, `billable_input_tokens`, output, and cache fields. The request estimate remains in diagnostic `total_input_tokens`; success rows keep prior standard usage behavior.
- External pool failure records call `external_standard_usage_for_status()` before persisting standard fields. Non-success statuses write zero for the same downstream-standard fields while preserving request-estimate diagnostics in `total_input_tokens`.

## 复现

No production mutation is needed to reproduce locally.

For the `/cc` / `/ha` class:

1. Configure a route with `current_high_cache`.
2. Enable reported usage with:
   - `input.mode=sample-max`
   - `input.maxTokens=600`
   - `input.moveDeltaToCacheRead=true`
   - `cacheRead.mode=preserve`
   - no cache-read evidence on the request.
3. Send a request whose estimated input is near or above 1,000,000 tokens.
4. Expected final usage shape:
   - low `input_tokens`
   - `cache_creation_input_tokens` near the removed raw input delta

For the `/dfcache/team` class:

1. Use `cacheType=kiro_rs_tool`.
2. Do not configure `reportedUsage` for that route.
3. Use a large stable tool/prompt prefix so tool-cache coverage is high.
4. Expected final usage can preserve large cache creation/read values directly.

## 方案与取舍

Selected focused fix, implemented 2026-07-31:

1. Add `reportedUsage.*.finalCacheCreationMaxTokens`; default `400000`, `0` disables the guard.
2. Add `reportedUsage.*.finalCacheCreationJitterMinTokens` / `finalCacheCreationJitterMaxTokens`; defaults `20000..45000`.
3. Compute the effective cap as `finalCacheCreationMaxTokens - deterministic_jitter(min..max)`. This mirrors the cache-read/output guard style: it only clips downward and never raises small values or exceeds the configured maximum.
4. Re-cap `cache_creation_5m_input_tokens` and `cache_creation_1h_input_tokens` after the total creation field is clipped.
5. Include creation-cap-only cases in `should_rewrite_local_prompt_cache_usage`, so persisted local prompt-cache usage records are rewritten even when no other field policy changes.
6. Apply the combined cache guards to stream final usage, non-stream/downstream record shaping, and external-pool `current_path_policy` before and after external usage uplift.

Selected residual fix, implemented 2026-08-01:

1. Add a standard-field-only cache guard that is independent of full reported-usage projection. This deliberately ignores `reportedUsage.enabled`, clips only downstream-standard cache read/write fields, and preserves raw/diagnostic usage snapshots.
2. Apply that guard to local handler and stream paths when the usage is local prompt-cache projected, has cache fields, and the strategy is `CurrentHighCache` or `KiroRsTool`.
3. Split failure request estimates from downstream-standard fields for local credential records. Non-success rows keep the request estimate in `total_input_tokens` and raw/original diagnostic fields, while standard usage fields are zero.
4. Apply the same failure standard-field split to external pool records, so external failures cannot persist a large request estimate into `compat_input_tokens` or `billable_input_tokens`.
5. Keep `output_tokens` under the existing output final guard; no production row had `output_tokens > 1m` in the sampled evidence.

Important behavior decision:

- A hard cap prevents impossible-looking downstream fields, but it changes billing/projection semantics for intentionally large cache simulation.
- A display-only/dashboard cap hides the symptom but leaves downstream API responses and persisted standard fields inconsistent.
- The safest code direction is a final standard-field guard with raw diagnostic fields preserved separately.

## 验证与证据

Commands/evidence:

- Production read-only SSH inventory for three hosts.
- PostgreSQL metadata for `usage_records` schema/indexes/table sizes.
- Bounded 2h aggregation of standard fields over 1m.
- Exact request-id samples for selected non-sensitive fields:
  - `rawUsage`
  - `simulated`
  - `externalPoolBilling`
  - `payloadGuardReport`
  - cache/reporting config subset
- Redacted archive:
  - `tmp/prod-evidence/20260731-174810-usage-1m-anomaly/20260731-174810-usage-1m-anomaly-redacted.tar.gz`

Validation still needed after code changes:

- Focused unit tests for `moveDeltaToCacheRead` when raw input exceeds the final creation field cap: done 2026-07-31.
- Stream final `message_delta.usage` creation-cap test: done 2026-07-31.
- Non-stream/record policy test for creation-cap-only rewrite: done 2026-07-31.
- External-pool `current_path_policy` usage-uplift re-guard test for cache creation: done 2026-07-31.
- `/dfcache/team` / `kiro_rs_tool` style unreported cache-field standard guard test: done 2026-08-01.
- Local failure-record test proving diagnostic estimates are not confused with final downstream usage: done 2026-08-01.
- External pool failure-record standard field split test: done 2026-08-01.
- Full isolated service/fake-upstream usage-shape smoke for `/cc`, `/ha`, and `/dfcache/team`.
- Dashboard/API rollup check proving standard fields and diagnostic fields are distinguishable.
- Production read-only recheck on the same bounded query after rollout.

Focused validation completed on 2026-07-31:

- `feature/tests/run-cargo-scoped.sh usage-cache-creation-cap -- cargo test anthropic::cache`: `46 passed / 0 failed`.
- `feature/tests/run-cargo-scoped.sh usage-cache-creation-cap -- cargo test reported_usage`: `46 passed / 0 failed` in main tests plus `1 passed / 0 failed` in loadtest unit tests.
- `feature/tests/run-cargo-scoped.sh usage-cache-creation-cap -- cargo test usage_projection_final_cache`: `2 passed / 0 failed`, covering read and creation external-pool final cache guards after uplift.
- `feature/tests/run-cargo-scoped.sh usage-cache-creation-cap -- cargo test test_stream_final_usage_caps_cache_creation_after_input_delta`: `1 passed / 0 failed`.
- `pnpm --dir ui check`: TypeScript check passed.

Focused validation completed on 2026-08-01:

- [Usage standard field guard focused validation](../evidence/usage-standard-field-guard-20260801.md) records the local command matrix and results.
- `feature/tests/run-cargo-scoped.sh usage-standard-cache-field-final -- cargo test --bin kiro-rs standard_cache_field -- --nocapture`: `3 passed / 0 failed`.
- `feature/tests/run-cargo-scoped.sh usage-record-filter-final -- cargo test --bin kiro-rs usage_record`: `13 passed / 0 failed`.
- `feature/tests/run-cargo-scoped.sh usage-projection-final-cache-final -- cargo test --bin kiro-rs usage_projection_final_cache`: `2 passed / 0 failed`.
- `feature/tests/run-cargo-scoped.sh external-failure-standard-usage -- cargo test --bin kiro-rs external_failure_standard_usage_fields_are_zeroed_for_all_non_success_statuses -- --nocapture`: `1 passed / 0 failed`.
- `feature/tests/run-cargo-scoped.sh external-error-filter-final -- cargo test --bin kiro-rs external_error`: `4 passed / 0 failed`.
- `feature/tests/run-cargo-scoped.sh usage-standard-guard-fmt-final -- cargo fmt --check`: passed.
- `git diff --check`: passed before documentation edits.

## 残余风险与边界

Residual risk:

- The 24h grouped query timed out on two large deployments, so exact 24h counts are not recorded for all hosts. The 2h bounded window and request-id samples are sufficient to prove the issue class.
- `usage_records` raw JSON can contain sensitive operational details. The redacted archive excludes raw evidence by default.
- The 2026-07-31 and 2026-08-01 focused fixes have not been rolled out to production; recurrence evidence is still pending.
- Focused unit tests cover the known local `/dfcache/team` `kiro_rs_tool` no-reportedUsage shape, but full isolated service/fake-upstream smoke is still pending.
- Local credential and external pool failure records now zero downstream-standard fields for non-success statuses, but dashboard/API consumers still need a rollup check to verify diagnostic `total_input_tokens` is not displayed as final response usage.
- External-pool success paths are covered for `current_path_policy` creation/read uplift re-guarding; any external-pool route that intentionally bypasses that projection policy still needs route-specific validation before it can be claimed safe.

Rollback boundary:

- If a final hard cap is added and billing/projection consumers require uncapped values, keep uncapped values only in explicit diagnostic/raw fields, not in the downstream-standard usage fields.
