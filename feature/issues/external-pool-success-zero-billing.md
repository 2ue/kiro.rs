# 外部池成功请求 0 计费与非流式 usage 捕获分裂

Status: `fixed-in-working-tree / targeted + isolated dynamic regression passed / not released`

Severity: `High`

Scope: 外部池 direct/fallback 成功请求的 usage 捕获、费用记录、usage detail、两套 UI API 类型、后续对账与统计报表。

Source branch reviewed: `origin/feature/scheduler-high-concurrency-resilience`

Relevant source commits reviewed:

- `78f0fc7 fix: keep external pool usage billing consistent`
- `3418fdd fix: estimate usage for external success fallbacks`

## 结论

当前 `main` 不能直接合并 `feature/scheduler-high-concurrency-resilience` 整条分支。两边从 `v0.0.109` 后已经分叉：当前 `main` 包含 `v0.0.114` 的协议污染、thinking、scheduler、Redis fault-domain 等修复；该分支包含另一条 scheduler/billing 快速修复线。整分支合并会把两条迭代混在一起，风险过大。

但该分支关于“外部池成功请求经常 0 计费”的两层修复需要进入当前分支。当前 `main` 修复前缺少这些关键能力：

- 非流式请求如果上游错误声明 `content-type: text/event-stream`，会被 `route.is_stream() || response_headers_look_like_sse(...)` 误送进 stream parser。
- 非流式 JSON body 只识别顶层 Anthropic-style `usage`，不识别 nested usage，也不识别 OpenAI-style `prompt_tokens/completion_tokens`。
- HTTP 200 成功 body 如果没有当前 parser 可识别 usage，会生成 `billing=None`，最终记录成 `outputTokens=0`、`estimatedCostUsd=0`、`rawUsage` 缺失、`externalPoolBilling` 缺失。
- billing 结构缺少 `usageEstimated`、`usageEstimateReason`、`usageCandidatePath`、`bodyUsageProjectionApplied`，生产排查无法区分“真实上游 usage”与“内部兜底估算”。

本轮已经将这几项以最小差异方式移植到当前 `main` 工作树，没有并入分支里的其他 scheduler 提交。

## 用户可见现象

生产或复核记录中会出现：

```text
status=success
routeKind=external_pool
stream=false
usageSource=request_estimate
outputTokens=0
estimatedCostUsd=0
rawUsage missing/null
externalPoolBilling missing/null
```

这不是纯 UI 展示问题。它会影响：

- `/api/admin/usage-records` 与 usage detail；
- Dashboard / summary / external pool billing 聚合；
- Redis summary / PostgreSQL usage rollup；
- 按 kiro.rs usage 字段结算的下游账单。

## 修复前源码链

### 路径 A：非流式请求被 SSE header 误分类

修复前外部池成功响应分流条件是：

```rust
route.is_stream() || response_headers_look_like_sse(&response_headers)
```

因此，只要上游把非流式 JSON body 的 `content-type` 错写成 `text/event-stream`，当前请求就会进入 stream branch。普通 JSON 不是 SSE event，stream usage parser 抓不到 usage，最终不会生成 `ExternalPoolBilling`。

### 路径 B：非流式 parser 只识别顶层 Anthropic usage

修复前 `maybe_project_non_stream_usage_with_tools(...)` 只读：

```text
$.usage.input_tokens
$.usage.output_tokens
$.usage.cache_creation_input_tokens
$.usage.cache_read_input_tokens
```

它不处理：

- `$.message.usage`
- `$.delta.usage`
- `$.data.usage`
- `$.response.usage`
- OpenAI-style `prompt_tokens / completion_tokens`
- Anthropic message JSON 但缺 usage
- OpenAI-style `choices[]` wrapper
- 其他 HTTP 200 成功 JSON wrapper
- 非 JSON 成功 body

### 路径 C：billing=None 后被记录为 0 成本

当 `ExternalUsageCapture.raw == None` 时：

```text
external_pool_billing_from_capture(...) -> None
```

后续 `record_external(...)` 在 success 但没有 billing 时只能回退到：

```text
output_tokens=0
estimated_cost_usd=0
pricing_available=false
usageSource=request_estimate
```

所以 0 计费的直接原因不是 PostgreSQL 或 Redis 重新计算错误，而是成功响应没有通过 usage capture/billing 不变量。

## 复现矩阵

最小复现不需要真实生产账号，构造外部池成功响应即可。

| Case | 上游响应 | 修复前结果 | 修复后结果 |
| --- | --- | --- | --- |
| A | non-stream request + `content-type: text/event-stream` + 普通 JSON message with usage | 误入 stream branch，billing 可能缺失 | 按 non-stream 处理；如果 body 是 JSON，下游 `content-type` 修正为 `application/json` |
| B | `{"type":"message","content":[...],"stop_reason":"end_turn"}`，无 usage | `billing=None`，0 成本 | 注入估算 usage；`usageEstimated=true`，`usageEstimateReason=missing_upstream_usage` |
| C | `{"usage":{"prompt_tokens":11,"completion_tokens":3}}` | parser 不识别，billing 缺失 | 归一化为 Anthropic-style usage；`usageCandidatePath=$.usage` |
| D | OpenAI-style `choices[].message.content` wrapper，无 usage | `billing=None`，0 成本 | 注入估算 usage；output tokens 从 choices 文本估算 |
| E | HTTP 200 纯文本 body | `billing=None`，0 成本 | 不改下游 body；内部按请求 input 生成 billing，output 记 0 |

## 已落地修复

### 1. 成功响应分流只看请求语义

当前工作树把成功响应是否走 stream branch 改为：

```rust
let response_is_stream = route.is_stream();
```

非流式分支额外处理：

- 如果响应头像 SSE 且 body 不是 JSON：作为非流式协议错误返回，不当 success 计费。
- 如果响应头像 SSE 但 body 是 JSON：继续 non-stream parser，并把下游 `content-type` 修正为 `application/json`。

### 2. route-aware non-stream usage processor

新增/扩展 `process_non_stream_response_usage(...)`：

- 先执行现有 response sanitizer，保留协议污染 fail-closed 行为；
- 识别候选路径：
  - `$.usage`
  - `$.message.usage`
  - `$.delta.usage`
  - `$.data.usage`
  - `$.response.usage`
- 归一化 OpenAI-style `prompt_tokens/completion_tokens`；
- 对 nested usage 注入顶层 Anthropic-style `usage`，便于下游标准 parser；
- 对缺 usage 的正常 Anthropic message 生成估算 usage；
- 对未识别 JSON 成功体生成估算 usage；
- 对非 JSON 成功体不改 body，但内部生成 input 成本 billing。

### 3. billing 诊断字段

`ExternalPoolBilling` 新增：

```text
usageEstimated
usageEstimateReason
usageCandidatePath
bodyUsageProjectionApplied
```

两套 UI API 类型同步新增这些字段。

### 4. usageSource 分类修正

如果 `billing.usageEstimated=true`，`record_external(...)` 优先记录：

```text
usageSource=request_estimate
```

即使该估算 usage 又经过了 current path policy projection，也不会被误标为 `local_prompt_cache` 或 `upstream_metadata`。

### 5. 模型计价 key 兼容修正

2026-07-25 生产复核补充发现：部分成功请求 usage 返回给下游正常，但系统内部计费仍为 `0`，原因不是 usage 缺失，而是计价模型 key 匹配失败。

典型样本：

```text
requested model / usage model: claude-opus-4-8
pricing catalog key:          claude-opus-4.8
```

修复目标：

- 只在计价匹配阶段兼容 dashed/dotted patch alias，例如 `opus-4-8` 能匹配 `opus-4.8`。
- 不改变请求路由、上游模型映射、下游返回 model 字段或 external pool raw model policy。
- 如果已有精确 pricing key，精确 key 仍优先。

新增验证：

```text
external_pool_billing_matches_dashed_opus_request_to_dotted_pricing_model
```

该测试构造 `route.model=claude-opus-4-8`，只配置 `claude-opus-4.8` 价格，确认：

- `pricingAvailable=true`
- `pricingModel=claude-opus-4.8`
- `billableCostUsd > 0`
- 费用等于 `100 input * 0.000007 + 10 output * 0.000031 = 0.00101`

## 验证结果

本轮验证记录见：

- [外部池成功 0 计费修复验证证据](../evidence/external-pool-success-zero-billing-20260723.md)

已通过：

```bash
feature/tests/run-cargo-scoped.sh external-billing-fmt -- cargo +1.92.0 fmt --all
feature/tests/run-cargo-scoped.sh external-billing-tests -- cargo +1.92.0 test --locked non_stream_ -- --nocapture
feature/tests/run-cargo-scoped.sh external-billing-fmt-2 -- cargo +1.92.0 fmt --all -- --check
feature/tests/run-cargo-scoped.sh external-billing-tests-2 -- cargo +1.92.0 test --locked openai_usage_is_normalized_for_non_stream_external_pool_body -- --nocapture
feature/tests/run-cargo-scoped.sh provider-fault-rerun -- cargo +1.92.0 test --locked kiro::provider::tests::provider_transport_and_body_fault_matrix_is_private_typed_and_bounded -- --nocapture
feature/tests/run-cargo-scoped.sh claude-cli-release -- bash -lc 'cargo +1.92.0 build --release && install -m 755 "$CARGO_TARGET_DIR/release/kiro-rs" "$KIRO_FROZEN_BINARY"'
feature/tests/run-cargo-scoped.sh external-billing-warning-check -- cargo +1.92.0 check --locked --bin kiro-rs
git diff --check
```

关键结果：

- `non_stream_` 过滤：`27 passed / 0 failed`
- 精确 OpenAI usage 归一化过滤：`1 passed / 0 failed`
- provider fault matrix 初次出现在全量 C0 中红一次，精确复跑 `1 passed / 0 failed`；该红项不是外部池 success billing 路径，但发布前不能伪装为“全量一次绿”。
- release binary build 通过，sha256 `fe97dd089671af009a1e59f54d976f043d6c5e0cc778a744ed504c44d50f1f31`。
- `cargo check --bin kiro-rs` 通过；test-only helper 已加 `#[cfg(test)]`，消除了 release build 暴露的新 dead-code warning。
- 所有 Cargo scoped target 均 `removed=true / reservation_released=true`
- `git diff --check` 通过

已完成隔离动态 gate：

- 临时服务：`127.0.0.1:19122`。
- 临时 PostgreSQL：一次性 `postgres:18-alpine` 容器，`127.0.0.1:39432`，无持久卷，验证后停止删除。
- Redis：当前项目 loopback Redis 的独立 DB `redis://127.0.0.1:26379/14`。
- fake external upstream：`127.0.0.1:39221`，验证后停止。
- 外部池：`routeKind=external_pool`、`routeSubtype=external_direct_policy`。

动态复核结果：

| Case | 结果 |
| --- | --- |
| non-stream + upstream `content-type:text/event-stream` + JSON + OpenAI usage | downstream `content-type=application/json`；body usage 归一为 `input_tokens=11/output_tokens=3`；usage record `usageSource=upstream_metadata`；`externalPoolBilling.usageEstimated=false`；`billableCostUsd=0.000078` |
| Anthropic message JSON 缺 usage | body 注入估算 usage `942/1`；usage record `usageSource=request_estimate`；`usageEstimateReason=missing_upstream_usage`；`billableCostUsd=0.002841` |
| OpenAI choices wrapper 缺 usage | body 注入估算 usage `942/1`；usage record `usageSource=request_estimate`；`usageEstimateReason=unrecognized_success_body`；`billableCostUsd=0.002841` |
| HTTP 200 text/plain | body 保持 `OK` 不改写；usage record `usageSource=request_estimate`；reported usage `942/0`；input-only `billableCostUsd=0.002826` |
| `current_path_policy` | raw `11/3` 被整形成 shaped `946/3`、reported `1169/3`；body usage 与 record 一致；`usageSource=local_prompt_cache`；`bodyUsageProjectionApplied=true`；`billableCostUsd=0.0043875` |

已完成真实 Claude Code CLI smoke：

- Claude CLI：`2.1.197 (Claude Code)`。
- 通过 `ANTHROPIC_BASE_URL=http://127.0.0.1:19122/cc` 命中当前修复后的服务。
- `--output-format=stream-json` 返回正常 `system/stream_event/assistant/result`。
- final usage 非零：`input_tokens=1`、`cache_creation_input_tokens=4823`、`output_tokens=2`。
- stdout/stderr 未出现内部泄漏标记：`credential`、`fallback pool`、`upstream pool`、`private scheduler`、`bashHash`、`readHash`、`Tool results provided`、`function_results`。

## 性能与兼容性

- 常规已有 usage path 只多做固定数量 JSON pointer 检查，候选路径为 5 个，复杂度是 body JSON tree 的局部 pointer 查找，不引入额外网络/Redis/PostgreSQL IO。
- 只有 non-stream 成功 body 已被完整读入后才做 JSON parse/估算；不会改变 stream hot path。
- 非 JSON 成功体不重写 body，避免把文本/二进制响应错误包装成 JSON。
- 新增字段均带 serde default，旧记录反序列化兼容。
- UI type 字段为 optional，兼容旧 API 响应。

## 残余风险

- 本轮没有执行真实生产外部池动态调用，也没有发布新版本；当前状态是 working tree 修复通过 targeted gate 和隔离动态 gate。
- `unrecognized_success_body` 的 output tokens 是估算，不等价于上游真实 output usage。它的目标是防止 success 记录完全 0 计费，并提供诊断字段方便后续对账。
- 如果外部池返回非常规成功体且 output 无法从 `choices` 或常见文本字段识别，仍只计 input 成本，output 记 0；这比 `externalPoolBilling` 缺失更安全，但不是精确 usage。
- 发布前仍需要项目最终发布 gate：全量测试一次性绿、release inventory、版本提交和 tag。当前证据只关闭“外部池成功 usage/billing 逻辑”这一个问题。

## 2026-07-25 生产复核：0.0.113 仍大量 0 计费

只读证据目录：

```text
tmp/prod-evidence/20260725-044228-rpm-slow-142-159
```

机器 `152.53.243.159` 当前运行：

```text
version=0.0.113
revision=36b65ce509809120ba53bb46c6b536e3658a6129
```

最近 2 小时外部池成功请求仍大量 `externalPoolBilling.totalCostUsd=0`：

```text
zero_billing|claude-opus-4-8|jinnyapi|883 total|882 success|882 zero
zero_billing|claude-opus-4-8|apiv3.52codeflow|675 total|670 success|670 zero
zero_billing|claude-opus-4-8|kkkkyue|568 total|566 success|566 zero
zero_billing|claude-sonnet-4-6|jinnyapi|517 total|516 success|516 zero
zero_billing|claude-opus-4-6|jinnyapi|374 total|372 success|372 zero
```

抽样 `billing_sample` 显示这不是下游没有返回成功，而是 billing 字段没有算出费用：

```text
routeKind=external_pool
routeSubtype=external_direct_policy
status=success
model=claude-opus-4-8
externalPoolName=apiv3.52codeflow / jinnyapi / kkkkyue
externalPoolBilling.totalCostUsd=<empty>
externalPoolBilling.pricingModel=claude-opus-4-8 或 <empty>
externalPoolUsage.input_tokens/output_tokens=<empty>
```

这与本专题根因吻合：旧版本既有“成功 body usage 未捕获/未估算”的路径，也有“计价模型 key 不兼容”的路径。尤其是用户指出的：

```text
request/usage model: claude-opus-4-8
pricing catalog:      claude-opus-4.8
```

当前分支新增的计价阶段 dotted/dashed fallback 只影响 pricing lookup，不改变：

- 请求模型；
- 上游模型映射；
- 下游返回 model；
- external pool raw model policy。

因此上线当前分支后，类似 `claude-opus-4-8` 应能在计价阶段匹配 `claude-opus-4.8`，但必须以发布后生产 usage 复核为准：

```sql
-- 发布后只读核验口径
select model,
       data->>'externalPoolName' as pool,
       count(*) filter (where status='success') as success,
       count(*) filter (
         where status='success'
           and coalesce(nullif(data#>>'{externalPoolBilling,totalCostUsd}', ''), '0')::numeric = 0
       ) as zero_billing
from usage_records
where deleted_at is null
  and created_at >= now() - interval '30 minutes'
  and data->>'routeKind' = 'external_pool'
group by model, data->>'externalPoolName'
order by success desc;
```

预期结果：

- 能识别 usage 的成功请求：`externalPoolBilling.totalCostUsd > 0`。
- 缺 usage 但 body 可估算的成功请求：`usageSource=request_estimate` 且至少 input 成本非 0。
- `sampled request rejection` 这类 admission 采样记录仍然 usage 为 0，不能纳入外部池成功 0 计费统计。

## 2026-07-23 最终候选复核

v0.0.117 候选 default/no-default 全量 Rust gate 已包含 external pool usage/billing 相关测试，其中 `postgres_rolls_up_external_pool_billing_for_large_samples_and_removes_after_cleanup` 通过；同时本轮移植后的非流式 usage projection、nested/OpenAI-style usage 识别、缺 usage 成功估算、`usageEstimated/usageEstimateReason/usageCandidatePath/bodyUsageProjectionApplied` 诊断字段和 `usageSource=request_estimate` 分类均由专题证据与全量 gate 覆盖。最终发布门禁还记录了冻结候选 SHA 与通过范围，见 [最终发布门禁证据](../evidence/final-release-gate-20260723.md)。
