# 外部池成功 0 计费修复验证证据 - 2026-07-23

Status: `targeted regression passed / isolated dynamic gate passed / Claude Code CLI smoke passed`

Branch: `main`

Related issue:

- [外部池成功请求 0 计费与非流式 usage 捕获分裂](../issues/external-pool-success-zero-billing.md)

Reviewed source branch:

- `origin/feature/scheduler-high-concurrency-resilience`
- `78f0fc7 fix: keep external pool usage billing consistent`
- `3418fdd fix: estimate usage for external success fallbacks`

## 代码验证范围

本轮只移植外部池 0 计费相关最小补丁，没有合并整条 `feature/scheduler-high-concurrency-resilience` 分支。

覆盖代码面：

- `src/external_pool.rs`
  - 外部池成功响应 stream/non-stream 分流；
  - non-stream usage candidate selection；
  - OpenAI-style usage 归一化；
  - missing upstream usage 估算；
  - unrecognized success body 估算；
  - `usageEstimated` 优先映射 `usageSource=request_estimate`；
  - `ExternalPoolBilling` 诊断字段填充。
- `src/anthropic/usage.rs`
  - `ExternalPoolBilling` serde 兼容字段；
  - 测试辅助 snapshot。
- `src/storage/postgres.rs`
  - 外部池 billing 测试构造同步新增字段。
- `src/storage/redis_cache.rs`
  - Redis usage summary 测试构造同步新增字段。
- `ui/src/types/api.ts`
- `admin-ui/src/types/api.ts`
  - 两套 UI API 类型同步新增 billing 诊断字段。

## 验证命令与结果

### 1. 格式化

Command:

```bash
feature/tests/run-cargo-scoped.sh external-billing-fmt -- cargo +1.92.0 fmt --all
```

Result:

```text
exit=0
validation-build-cleanup scope=external-billing-fmt size_kib=28 removed=true reservation_released=true
```

### 2. non-stream 精确回归

Command:

```bash
feature/tests/run-cargo-scoped.sh external-billing-tests -- cargo +1.92.0 test --locked non_stream_ -- --nocapture
```

Result:

```text
running 27 tests
27 passed; 0 failed; 0 ignored
kiro_loadtest: 0 tests for this filter
validation-build-cleanup scope=external-billing-tests size_kib=1713712 removed=true reservation_released=true
```

Covered newly added or directly relevant tests:

- `external_pool::tests::openai_usage_is_normalized_for_non_stream_external_pool_body`
- `external_pool::tests::non_stream_missing_usage_injects_estimated_billing_body`
- `external_pool::tests::non_stream_unknown_json_without_usage_injects_estimated_usage_and_billing`
- `external_pool::tests::non_stream_unknown_text_without_usage_records_estimated_billing_without_rewriting_body`
- existing non-stream protocol contamination tests
- existing non-stream usage projection tests
- existing non-stream body bounded/recovery tests

### 3. 格式检查复跑

Command:

```bash
feature/tests/run-cargo-scoped.sh external-billing-fmt-2 -- cargo +1.92.0 fmt --all -- --check
```

Result:

```text
exit=0
validation-build-cleanup scope=external-billing-fmt-2 size_kib=28 removed=true reservation_released=true
```

### 4. OpenAI usage 精确单点复跑

Command:

```bash
feature/tests/run-cargo-scoped.sh external-billing-tests-2 -- cargo +1.92.0 test --locked openai_usage_is_normalized_for_non_stream_external_pool_body -- --nocapture
```

Result:

```text
running 1 test
1 passed; 0 failed; 0 ignored
kiro_loadtest: 0 tests for this filter
validation-build-cleanup scope=external-billing-tests-2 size_kib=1711096 removed=true reservation_released=true
```

### 5. Whitespace gate

Command:

```bash
git diff --check
```

Result:

```text
exit=0
```

### 6. C0 broad test attempt and focused rerun

Command:

```bash
feature/tests/run-cargo-scoped.sh claude-cli-c0 -- \
  bash -lc 'cargo +1.92.0 fmt --check && cargo +1.92.0 test && cargo +1.92.0 build --release && install -m 755 "$CARGO_TARGET_DIR/release/kiro-rs" "$KIRO_FROZEN_BINARY"'
```

Result:

```text
cargo fmt --check: passed
cargo test: 1754 passed / 1 failed / 6 ignored
failed test: kiro::provider::tests::provider_transport_and_body_fault_matrix_is_private_typed_and_bounded
failure symptom: provider_error_chunked_over_limit was classified as upstream_timeout in one run
validation-build-cleanup scope=claude-cli-c0 removed=true reservation_released=true
```

Focused rerun:

```bash
feature/tests/run-cargo-scoped.sh provider-fault-rerun -- \
  cargo +1.92.0 test --locked kiro::provider::tests::provider_transport_and_body_fault_matrix_is_private_typed_and_bounded -- --nocapture
```

Result:

```text
1 passed; 0 failed; 1760 filtered out
validation-build-cleanup scope=provider-fault-rerun removed=true reservation_released=true
```

Interpretation:

- The broad C0 attempt is not recorded as a clean all-suite pass because the first run had one red test.
- The failed test is the existing provider fault/timeout matrix, not the external-pool billing path changed in this issue.
- Its exact rerun passed. This is evidence of an existing timing-sensitive/flaky gate, not proof that the all-suite release gate is green.

### 7. Release binary build for runtime validation

Command:

```bash
feature/tests/run-cargo-scoped.sh claude-cli-release -- \
  bash -lc 'cargo +1.92.0 build --release && install -m 755 "$CARGO_TARGET_DIR/release/kiro-rs" "$KIRO_FROZEN_BINARY"'
```

Result:

```text
exit=0
binary sha256=fe97dd089671af009a1e59f54d976f043d6c5e0cc778a744ed504c44d50f1f31
validation-build-cleanup scope=claude-cli-release removed=true reservation_released=true
```

The frozen binary was removed after the CLI/runtime validation completed.

### 8. Binary warning check after cleanup patch

After runtime validation, release build had surfaced one non-fatal warning:

```text
function `maybe_project_non_stream_usage_with_tools` is never used
```

The helper is test-only, so it was gated with `#[cfg(test)]`.

Command:

```bash
feature/tests/run-cargo-scoped.sh external-billing-warning-check -- \
  cargo +1.92.0 check --locked --bin kiro-rs
```

Result:

```text
exit=0
no new dead-code warning for maybe_project_non_stream_usage_with_tools
validation-build-cleanup scope=external-billing-warning-check removed=true reservation_released=true
```

### 9. Final hygiene after evidence updates

Command:

```bash
feature/tests/run-cargo-scoped.sh external-billing-final-fmt-check -- cargo +1.92.0 fmt --all -- --check
git diff --check
node feature/tests/check-feature-docs.mjs
```

Result:

```text
cargo fmt --check: exit=0
git diff --check: exit=0
feature docs: PASS, 48 issue documents, 116 relative links
validation-build-cleanup scope=external-billing-final-fmt-check removed=true reservation_released=true
```

### 10. Dashed/dotted model pricing alias regression

2026-07-25 追加验证：生产上仍有一类 `success` 记录下游 usage 正常但内部计费为 `0`，原因是计价 catalog 使用 dotted model key，而请求/usage 侧使用 dashed alias。

Command:

```bash
RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh protocol-billing-focused -- \
  bash -lc 'cargo test --locked --all-targets provider_status_and_non_eventstream_matrix_is_private_typed_and_bounded && cargo test --locked --all-targets handler_binary_eventstream_with_json_content_type_is_body_sniffed_for_five_rounds && cargo test --locked --all-targets external_pool_billing_matches_dashed_opus_request_to_dotted_pricing_model'
```

Result:

```text
provider_status_and_non_eventstream_matrix_is_private_typed_and_bounded: passed
handler_binary_eventstream_with_json_content_type_is_body_sniffed_for_five_rounds: passed
external_pool_billing_matches_dashed_opus_request_to_dotted_pricing_model: passed
validation-build-cleanup scope=protocol-billing-focused size_kib=1730452 removed=true reservation_released=true
```

Specific contract:

- request/route model remains `claude-opus-4-8`;
- pricing catalog only contains `claude-opus-4.8`;
- billing selects `pricingModel=claude-opus-4.8`;
- `pricingAvailable=true`;
- `billableCostUsd=0.00101`;
- no route/model mapping behavior is changed outside the pricing lookup path.

## Isolated dynamic gate

Purpose: prove the full request path writes correct PostgreSQL usage records, not only in-memory unit functions.

Runtime setup:

- Kiro service: frozen release binary, `127.0.0.1:19122`.
- PostgreSQL: temporary `postgres:18-alpine` container, `127.0.0.1:39432`, no persistent volume, stopped after validation.
- Redis: existing loopback Redis authority, isolated DB `redis://127.0.0.1:26379/14`.
- Fake external upstream: local Node HTTP server, `127.0.0.1:39221`, stopped after validation.
- Admin runtime config:
  - `externalPoolsEnabled=true`
  - `externalDirectPolicyEnabled=true`
  - `externalPoolLocalRescueEnabled=false`
  - `fallbackOnSchedulerRedisDegraded=true`
- External pool:
  - `id=1`
  - `baseUrl=http://127.0.0.1:39221`
  - `usageProjectionMode=pass_through` for the four non-stream matrix cases
  - switched to `current_path_policy` for the projection case

Cleanup:

```text
127.0.0.1:19122 released
127.0.0.1:39221 released
temporary postgres container stopped and removed by --rm
frozen binary directory removed
raw CLI/runtime temp captures removed after summaries were recorded
```

### Dynamic non-stream matrix

All requests used:

```json
{
  "model": "sonnet",
  "max_tokens": 32,
  "stream": false,
  "messages": [{ "role": "user", "content": "<case marker>" }]
}
```

All records were read back from `/api/admin/usage-records` and matched by request id.

| Case | Request id | Downstream response | Usage record | Billing evidence |
| --- | --- | --- | --- | --- |
| `openai_sse_header` | `req_01FWysj1tBuijEbmN725gXt3` | `200`, `content-type=application/json`; body usage normalized to `input_tokens=11`, `output_tokens=3` while preserving `prompt_tokens=11`, `completion_tokens=3` | `status=success`, `routeKind=external_pool`, `routeSubtype=external_direct_policy`, `usageSource=upstream_metadata`, `usageProjectionApplied=false` | `usageEstimated=false`, `usageCandidatePath=$.usage`, raw/reported usage `11/3`, `billableCostUsd=0.000078` |
| `missing_usage` | `req_01DK9J8GsyRtn1DQ5xDBxqCY` | `200`, JSON body got injected usage `input_tokens=942`, `output_tokens=1` | `status=success`, `routeKind=external_pool`, `usageSource=request_estimate` | `usageEstimated=true`, `usageEstimateReason=missing_upstream_usage`, raw/reported usage `942/1`, `billableCostUsd=0.002841` |
| `unknown_json` | `req_01LvxkUKwkhRJ6Dey48Ca6BP` | `200`, OpenAI-style wrapper without usage got injected usage `input_tokens=942`, `output_tokens=1` | `status=success`, `routeKind=external_pool`, `usageSource=request_estimate` | `usageEstimated=true`, `usageEstimateReason=unrecognized_success_body`, raw/reported usage `942/1`, `billableCostUsd=0.002841` |
| `unknown_text` | `req_01LbSSiUEBxrLSPZRjHpfN8a` | `200`, `content-type=text/plain`; body remained exactly `OK` | `status=success`, `routeKind=external_pool`, `usageSource=request_estimate`, `outputTokens=0` | `usageEstimated=true`, `usageEstimateReason=unrecognized_success_body`, raw/reported usage `942/0`, input-only `billableCostUsd=0.002826` |

Acceptance:

- No successful external-pool non-stream request in the matrix produced missing `externalPoolBilling`.
- No successful external-pool non-stream request in the matrix produced zero billable cost.
- Non-JSON success body was not rewritten.
- Upstream `content-type: text/event-stream` on a non-stream JSON response did not force the stream parser; downstream content-type was corrected to `application/json`.

### Dynamic current-path-policy projection

Pool update:

```json
{ "usageProjectionMode": "current_path_policy" }
```

Request:

```text
req_01CYLrRXfFYa6rga6Kq6R8U4
```

Result:

```text
HTTP status: 200
routeKind: external_pool
routeSubtype: external_direct_policy
usageSource: local_prompt_cache
usageProjectionApplied: true
bodyUsageProjectionApplied: true
usageEstimated: false
usageCandidatePath: $.usage
```

Usage/cost evidence:

```text
rawUsage:      input=11,   output=3, cacheCreation=0
shapedUsage:   input=55,   output=3, cacheCreation=891,  totalInput=946
reportedUsage: input=55,   output=3, cacheCreation=1114, totalInput=1169
rawCostUsd:       0.000078
reportedCostUsd:  0.0043875
billableCostUsd:  0.0043875
```

Downstream body usage was projected consistently with the record:

```json
{
  "input_tokens": 55,
  "output_tokens": 3,
  "cache_creation_input_tokens": 1114,
  "cache_read_input_tokens": 0,
  "cache_creation": {
    "ephemeral_5m_input_tokens": 1114,
    "ephemeral_1h_input_tokens": 0
  }
}
```

This validates the user-facing requirement: external-pool success is normally identified, normally billed, and output usage is shaped according to config when `current_path_policy` is enabled.

## Real Claude Code CLI smoke

Claude CLI version:

```text
2.1.197 (Claude Code)
```

Command shape:

```bash
HOME=<isolated-home> \
CLAUDE_CONFIG_DIR=<isolated-config> \
ANTHROPIC_BASE_URL=http://127.0.0.1:19122/cc \
ANTHROPIC_API_KEY=<redacted> \
CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 \
claude --bare --print --verbose \
  --output-format=stream-json \
  --include-partial-messages \
  --no-session-persistence \
  --model sonnet \
  'Reply with exactly: pong. CASE_STREAM_OK'
```

Result:

```text
exit=0
parsed stream-json lines=10
event types: system, stream_event, assistant, result
final text included: pong
final usage: input_tokens=1, cache_creation_input_tokens=4823, cache_read_input_tokens=0, output_tokens=2
internal leak scan: false
```

Leak scan patterns included:

```text
credential
fallback pool
upstream pool
private scheduler
bashHash
readHash
Tool results provided
function_results
```

No match appeared in stdout/stderr.

## Case-level acceptance

| Case | Acceptance | Status |
| --- | --- | --- |
| Anthropic-style usage present | Body stays byte-identical when no projection/sanitization is needed; billing captures raw/reported usage | Passed by existing non-stream projection/pass-through tests |
| OpenAI-style usage present | `prompt_tokens/completion_tokens` normalized to `input_tokens/output_tokens`; `usageCandidatePath=$.usage`; not marked estimated | Passed |
| Standard message JSON missing usage | Top-level usage injected; billing present; `usageEstimated=true`; `usageEstimateReason=missing_upstream_usage` | Passed |
| OpenAI choices wrapper missing usage | Top-level usage injected; input and output both estimated; `usageEstimateReason=unrecognized_success_body` | Passed |
| Plain text HTTP 200 success | Body unchanged; internal billing present; input cost can be billed; output stays 0 | Passed |
| Estimated billing source classification | `usageEstimated=true` takes precedence and maps to `UsageSource::RequestEstimate` | Passed by unit tests and isolated PostgreSQL usage record readback |
| SSE header + JSON non-stream body | Request streamness no longer depends on upstream response header; JSON body is processed as non-stream and downstream content-type is corrected | Passed by isolated fake upstream dynamic gate |
| Current-path-policy projection | Body usage and internal billing are projected according to config while raw upstream usage remains recorded | Passed by isolated fake upstream dynamic gate |
| Real Claude Code CLI stream success | Claude CLI receives normal stream-json output, final usage is non-zero, and internal markers do not leak | Passed by isolated CLI smoke |

## Artifact lifecycle

All Cargo commands were executed through `feature/tests/run-cargo-scoped.sh`.

Observed cleanup:

- `external-billing-fmt`: `removed=true / reservation_released=true`
- `external-billing-tests`: `removed=true / reservation_released=true`
- `external-billing-fmt-2`: `removed=true / reservation_released=true`
- `external-billing-tests-2`: `removed=true / reservation_released=true`
- `provider-fault-rerun`: `removed=true / reservation_released=true`
- `claude-cli-release`: `removed=true / reservation_released=true`
- `external-billing-warning-check`: `removed=true / reservation_released=true`
- `external-billing-final-fmt-check`: `removed=true / reservation_released=true`

No root `target/` cleanup was required during this evidence run. Runtime temp dirs, fake upstream, and frozen binary dirs created by this validation were removed.

## Non-goals for this evidence

- This evidence does not claim production recurrence is closed.
- This evidence does not claim a new release tag exists.
- This evidence does not merge or validate unrelated scheduler changes from `feature/scheduler-high-concurrency-resilience`.
- This evidence does not claim the entire repository all-suite gate is green in one clean run; one unrelated provider fault matrix test failed once and passed on focused rerun.
