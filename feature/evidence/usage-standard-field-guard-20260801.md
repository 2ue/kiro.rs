# Usage standard field guard focused validation - 2026-08-01

Status: `focused-pass / final-candidate-pending / production-recurrence-pending`

Related issue:

- [Downstream standard usage field over 1m](../issues/downstream-usage-standard-field-over-1m-20260731.md)

## Scope

This evidence covers the focused fixes for persisted and downstream-standard usage fields that can exceed 1,000,000 in a single standard field:

- `input_tokens`
- `cache_creation_input_tokens`
- `cache_read_input_tokens`

The validation was local. It did not restart or modify the existing `127.0.0.1:9022` service, production PostgreSQL, production Redis, or production credentials.

## Code path validated

- `ReportedCacheUsagePolicy::apply_final_standard_cache_guards_for_standard_fields()` applies only the final downstream-standard cache-read and cache-creation caps while ignoring `reportedUsage.enabled`.
- Local handler non-stream and persisted-record paths apply that standard cache guard when:
  - `usage_source == LocalPromptCache`
  - cache fields are present
  - strategy is `CurrentHighCache` or `KiroRsTool`
  - the route has no full reported-usage policy.
- Stream final `message_delta.usage` applies the same standard cache guard when local prompt-cache projection is enabled but no reported-usage policy exists.
- Local credential failure records now keep the request input estimate in diagnostic `total_input_tokens` / `rawUsage` paths and write zero to downstream-standard fields.
- External pool failure records now keep request-estimate diagnostics in `total_input_tokens` and write zero to downstream-standard `compat_input_tokens`, `billable_input_tokens`, output, and cache fields.
- Existing reported-usage-enabled paths still apply the final cache-read and cache-creation guards after input-delta movement and external-pool uplift.

## Commands

```bash
feature/tests/run-cargo-scoped.sh usage-standard-cache-field-final -- cargo test --bin kiro-rs standard_cache_field -- --nocapture
feature/tests/run-cargo-scoped.sh usage-record-filter-final -- cargo test --bin kiro-rs usage_record
feature/tests/run-cargo-scoped.sh usage-projection-final-cache-final -- cargo test --bin kiro-rs usage_projection_final_cache
feature/tests/run-cargo-scoped.sh external-failure-standard-usage -- cargo test --bin kiro-rs external_failure_standard_usage_fields_are_zeroed_for_all_non_success_statuses -- --nocapture
feature/tests/run-cargo-scoped.sh external-error-filter-final -- cargo test --bin kiro-rs external_error
feature/tests/run-cargo-scoped.sh usage-standard-guard-fmt-final -- cargo fmt --check
git diff --check
```

Earlier focused usage guard commands from the same work sequence:

```bash
feature/tests/run-cargo-scoped.sh usage-cache-module -- cargo test --bin kiro-rs anthropic::cache
feature/tests/run-cargo-scoped.sh usage-reported-filter -- cargo test --bin kiro-rs reported_usage
feature/tests/run-cargo-scoped.sh usage-failure-standard-zero -- cargo test --bin kiro-rs large_request_estimate -- --nocapture
feature/tests/run-cargo-scoped.sh usage-failure-record-focused -- cargo test --bin kiro-rs failure_usage_record -- --nocapture
```

## Results

- `standard_cache_field`: `3 passed / 0 failed`, covering:
  - standard cache field caps without full reported-usage projection;
  - local handler `/dfcache` / `kiro_rs_tool` style unreported cache usage;
  - stream final usage with local prompt-cache projection and no reported-usage policy.
- `usage_record`: `13 passed / 0 failed`, including `failure_usage_record_keeps_large_request_estimate_out_of_standard_fields`.
- `usage_projection_final_cache`: `2 passed / 0 failed`, covering cache-read and cache-creation guards after external-pool uplift.
- `external_failure_standard_usage_fields_are_zeroed_for_all_non_success_statuses`: `1 passed / 0 failed`, covering `Error`, `StreamError`, `UpstreamTimeout`, and `ClientDropped`.
- `external_error`: `4 passed / 0 failed`, covering nearby external-pool error classification/diagnostics and the preflight external error path.
- `cargo fmt --check`: passed after formatting.
- `git diff --check`: passed before documentation edits.

Earlier focused usage guard results:

- `anthropic::cache`: `47 passed / 0 failed`.
- `reported_usage`: `47 passed / 0 failed`.
- `usage_projection_final_cache`: `2 passed / 0 failed`.
- `large_request_estimate`: `1 passed / 0 failed`.
- `failure_usage_record`: `1 passed / 0 failed`.

## Release meaning

This closes the known code residuals recorded in the issue for:

- unreported local prompt-cache cache fields, including `kiro_rs_tool` standard cache read/write caps;
- local credential failure rows using request estimates as downstream-standard fields;
- external pool failure rows using request estimates as downstream-standard fields.

It does not close the release gate by itself.

Remaining gates:

- final frozen candidate binding;
- full isolated service/fake-upstream smoke for `/cc`, `/ha`, and `/dfcache/team` usage shapes;
- dashboard/API rollup check that standard fields and diagnostic fields are displayed distinctly;
- production recurrence observation on the affected hosts after rollout.
