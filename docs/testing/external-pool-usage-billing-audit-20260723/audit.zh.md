# 外部池 usage 与 0 计费生产审计记录 - 2026-07-23

## 记录目的

本文独立整理生产环境外部池成功请求出现 `outputTokens=0`、`estimatedCostUsd=0`、缺少 `rawUsage`、缺少 `externalPoolBilling` 的问题。内容包括：

- 已确认的问题现象；
- 生产聚合证据；
- 多轮真实 API 复核证据；
- 下游响应体 usage 到底是透传还是整形的判断；
- 当前源码链路与生产结果的矛盾；
- 结论边界：哪些是已证明，哪些只是推断，哪些仍未知；
- 完整修复方案和验收口径。

本文不把数据库记录反推成“已经抓到历史上游原始 body”。历史成功响应 body 没有被 PostgreSQL、Redis 或现有日志保存，因此历史原始 body 无法从现有数据还原。

## 修复过程记录规则

修复期间如果发现新问题，不把它塞进已有结论里混写。处理规则如下：

```text
1. 如果新问题影响 runtime usage/billing 行为，先判断它是否和 P001 同根因；不同根因就新增独立问题记录。
2. 如果新问题是证据、脱敏、测试或发布流程问题，单独记录为流程问题，不和 kiro.rs runtime bug 混在一起。
3. 如果实现中发现原设计和代码事实不一致，必须回写本文的“完整修复方案”和“测试计划”，不能让文档停留在旧方案。
4. 每个设计变更都要写清楚：发现方式、旧假设、代码事实、新方案、需要补的测试。
5. 修复完成后，必须用模拟上游接入外部池路径验证，不只跑孤立 parser 单测。
```

本轮已按这条规则独立记录一个新问题：

```text
docs/testing/external-pool-usage-billing-audit-20260723/evidence-redaction-gap.md
```

修复代码时又发现一个影响 runtime 行为的新问题，并已独立记录：

```text
docs/testing/external-pool-usage-billing-audit-20260723/nonstream-sse-header-misclassification.md
```

这个新问题更新了原修复方案：不能只补非流式 JSON usage parser。还必须修正响应分支分类，避免非流式请求因为上游错误声明 `text/event-stream` 而绕过非流式 JSON parser 和 billing 生成。

## 本地证据位置

证据根目录：

```text
tmp/prod-evidence/20260723-003327-kiro-prod-usage-zero/
```

默认脱敏归档：

```text
tmp/prod-evidence/20260723-003327-kiro-prod-usage-zero/20260723-003327-kiro-prod-usage-zero-redacted.tar.gz
```

主要问题目录：

```text
tmp/prod-evidence/20260723-003327-kiro-prod-usage-zero/problems/P001-external-pool-nonstream-zero-billing/
tmp/prod-evidence/20260723-003327-kiro-prod-usage-zero/problems/P002-external-pool-upstream-instability/
```

本轮另外把“证据包脱敏缺口”单独记录为安全/流程问题：

```text
docs/testing/external-pool-usage-billing-audit-20260723/evidence-redaction-gap.md
```

长期报告和最终答复不保存生产 SSH 密码、API key、完整请求体、完整响应体、生产 IP 或生产域名。`raw/` 目录只作为本地原始材料保留，默认归档不包含 `raw/`。

## 生产实例上下文

审计对象是一个 Docker Compose 部署的 kiro.rs 生产实例。长期报告中生产主机、公开域名、部署目录、容器名按脱敏口径记录。

运行版本：

```text
image=ghcr.io/2ue/kiro-rs:latest
org.opencontainers.image.revision=c1748265b904aacdbd6fa33f4bd2e86985ad1f53
org.opencontainers.image.version=0.0.112
```

服务健康状态：

```text
/healthz: HTTP 200
/readyz: HTTP 200, postgres=true, redis=true, redisRuntimeEvents=true
```

数据库状态：

```text
usage_records 表约 1355 MB
usage_records.created_at / status / external-pool JSON 字段存在相关索引
```

相关外部池配置：

| pool id | name | enabled | priority | usage_projection_mode | request_body_mode | raw_model_mode |
| ---: | --- | --- | ---: | --- | --- | --- |
| 15 | `apiv3.52codeflow` | true | 2 | `current_path_policy` | `normalized` | `none` |
| 4 | `kkkkyue` | true | 50 | `current_path_policy` | `normalized` | `rewrite_top_level` |

全局运行配置中的相关项：

```text
externalPoolsEnabled=true
externalPoolRetryMaxAttempts=6
externalPoolStreamResponseMode=event_passthrough
externalPoolUsageProjectionUpliftPercent=35
externalPoolUsageProjectionOutputUpliftMinTokens=2000
externalPoolUsageProjectionOutputUpliftPercent=25
externalPoolAutoDisableEnabled=false
reportedUsage.default.enabled=true
reportedUsage.default.skipNonStreamUsageProjection=false
```

这说明 pool 15 和 pool 4 都不是简单的 usage 透传语义。它们配置为 `current_path_policy`，正常情况下下游响应里的 usage 应该是 kiro.rs 按当前路径策略处理后的 reported usage；内部计费也应该使用同一份 reported usage。

## 问题说明

### 问题 A：成功 HTTP 200 可以被记录成 0 计费

生产里存在大量这样的 `usage_records`：

```text
status=success
usageSource=request_estimate
outputTokens=0
estimatedCostUsd=0
pricingAvailable=false
rawUsage missing/null
externalPoolBilling missing/null
```

这不是单纯展示层字段问题。它会影响：

- kiro.rs 自己的 usage 记录；
- 管理后台和统计报表；
- 依赖 `usage_records` 的 rollup 或对账逻辑；
- 按 kiro.rs 内部账单字段结算的下游系统。

### 问题 B：非流式下游 body 有 usage，但 DB/Redis 仍然漏计费

这点是本轮生产复核补强后的核心证据。

之前只有少量调用时，证据确实偏弱。本轮用真实 kiro.rs 生产入口继续做了低频 curl 矩阵复核，结果稳定复现：

```text
pool 15 非流式 HTTP 200：9/9 下游 body 有标准 Anthropic usage，但 DB/Redis billing 缺失。
pool 15 流式 HTTP 200：4/4 SSE usage 存在，DB/Redis billing 存在。
```

这把问题从“上游可能没有返回 usage”推进到更明确的结论：

```text
至少在 pool 15 非流式路径里，即使最终返回给下游的 body 已经包含标准 usage，kiro.rs 仍可能没有把 usage 写入 rawUsage/externalPoolBilling。
```

### 问题 C：非流式下游 usage 不符合 current_path_policy

复核样本显示 pool 15 非流式响应体里的 usage 是可解析的，但它看起来像 raw/pass-through usage，而不是配置要求的 reported/projected usage。

代表性非流式响应 usage：

```json
{
  "cache_creation_input_tokens": 0,
  "cache_read_input_tokens": 734,
  "input_tokens": 81,
  "output_tokens": 1
}
```

同池、同类 prompt 的代表性流式响应 usage：

```json
{
  "cache_creation_input_tokens": 0,
  "cache_read_input_tokens": 0,
  "input_tokens": 817,
  "output_tokens": 1
}
```

对应流式 DB billing 里还能看到：

```text
rawUsage:
  inputTokens=815
  cacheReadInputTokens=734
  outputTokens=1

reportedUsage:
  inputTokens=817
  cacheReadInputTokens=0
  outputTokens=1

externalPoolBilling=present
usageProjectionApplied=true
```

这说明：

- 流式路径确实执行了 `current_path_policy`；
- 非流式最终下游 body 虽然有 usage，但它不是同样的 reported usage；
- 对下游来说，“能按标准字段解析”不等于“计费语义正确”。

因此，之前“对 #15 的复核样本，下游 body 有标准 usage，下游按响应体计费应当正常”必须收紧为：

```text
下游如果只要求标准 Anthropic usage 字段，语法上可以解析；
但如果下游应按 kiro.rs 的 current_path_policy/reported usage 计费，则非流式样本并不正常。
```

### 问题 D：流式和非流式成功路径行为分裂

生产复核展示出清晰分裂：

| 路径 | 下游 usage | usage projection | DB rawUsage | DB externalPoolBilling |
| --- | --- | --- | --- | --- |
| pool 15 stream | 有 | 有 | 有 | 有 |
| pool 15 non-stream | 有 | 看起来没有 | 无 | 无 |

这说明不能只查“上游是否返回 usage”。真正的问题是：

```text
成功响应最终 body、usage projection、内部 billing 记录没有使用同一个强制不变量。
```

### 问题 E：非流式请求可能被上游 SSE header 误分流

修复过程中代码审查发现，旧代码使用：

```text
route.is_stream() || response_headers_look_like_sse(response_headers)
```

来决定是否走 stream branch。

这会造成：

```text
route.stream=false
upstream content-type=text/event-stream
upstream body=普通 JSON message with usage
=>
kiro.rs 走 stream branch
普通 JSON body 被原样下发
SSE parser 解析不到 usage
DB/Redis billing missing
```

这比单纯 parser 漏字段更能解释“下游 body 有标准 usage，但内部 billing 缺失”的复核样本。

修复后：

```text
非流式请求一律读完整 body；
JSON body 按非流式 usage processor 处理；
header 错标为 SSE 但 body 是 JSON 时，下游 content-type 修正为 application/json；
真正 SSE 文本返回到非流式路径时，归类为 success protocol error，不当作正常成功。
```

### 问题 F：外部上游不稳定单独存在

另一个独立问题是外部上游自身稳定性：

```text
pool 4:
  403 auth/quota/insufficient-balance

pool 15:
  Cloudflare 502
  Cloudflare 524
  403 blocked/security
  model unavailable / no available channel
```

这些问题会影响调度、重试、冷却、延迟和成功率，但它们不能解释“HTTP 200 正常 body 仍然漏写 billing”的问题。修复方案需要把上游失败归类和成功 200 usage 不变量分开处理。

## 聚合证据

最近 24 小时 `usage_records` 聚合：

| pool | stream | status | usageSource | total | outputZero | rawUsagePresent | billingPresent | successZeroMissingBilling | costSum |
| --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| #15 `apiv3.52codeflow` | true | success | `request_estimate` | 28240 | 28240 | 0 | 0 | 28240 | 0 |
| #15 `apiv3.52codeflow` | false | success | `request_estimate` | 12119 | 12119 | 0 | 0 | 12119 | 0 |
| #4 `kkkkyue` | false | success | `request_estimate` | 7159 | 7159 | 0 | 0 | 7159 | 0 |
| #4 `kkkkyue` | true | success | `request_estimate` | 1 | 1 | 0 | 0 | 1 | 0 |
| #15 `apiv3.52codeflow` | true | success | `local_prompt_cache` | 25449 | 0 | 25449 | 25449 | 0 | 5175.787544 |
| #4 `kkkkyue` | true | success | `local_prompt_cache` | 19023 | 0 | 19023 | 19023 | 0 | 4965.352941 |
| #4 `kkkkyue` | false | success | `upstream_metadata` | 4156 | 0 | 4156 | 4156 | 0 | 63.110695 |
| #4 `kkkkyue` | false | success | `local_prompt_cache` | 361 | 0 | 361 | 361 | 0 | 8.293168 |

pool 4 非流式形态：

| class | count | first seen | last seen | p50 max_tokens | p90 max_tokens | max max_tokens | p50 output | max output | costSum |
| --- | ---: | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `zero_missing_billing` | 7159 | 2026-07-21 17:28:42Z | 2026-07-22 16:23:10Z | 8192 | 64000 | 128000 | 0 | 0 | 0 |
| `nonzero_or_billed` | 4517 | 2026-07-21 17:27:28Z | 2026-07-22 16:18:49Z | 50 | 50 | 81920 | 5 | 209 | 71.403863 |

这些聚合证明问题不是一条异常记录，也不是单个 request id 的偶发现象。pool 4 的历史 0 计费还和高 `requestedMaxTokens` 强相关。

## 真实 API 复核证据

复核通过生产公开入口调用 kiro.rs，本身是正常业务路径。调用是低频短 prompt：

```text
Reply with exactly OK.
```

没有修改生产配置、没有写 SQL、没有写 Redis、没有重启服务。API 调用本身自然产生 usage 记录。

第一组有效样本：

| request id | endpoint | stream | model | max_tokens | 下游 body usage | DB billing |
| --- | --- | --- | --- | ---: | --- | --- |
| `req_01MWSzCj1NfoaixzzPkgVqzn` | `/v1/messages` | false | haiku | 1 | present | missing |
| `req_01PxztmgfkWZQWComggKTtSr` | `/v1/messages` | false | haiku | 32 | present | missing |
| `req_01HkFDzEaqT348ueGoAjy7zX` | `/ha/v1/messages` | false | opus | 8192 | present | missing |
| `req_016RAGjvJrkjYo9JiyJyktxd` | `/v1/messages` | true | haiku | 16 | present | present |

扩展 curl 矩阵：

| class | count | HTTP result | body usage | DB billing |
| --- | ---: | --- | --- | --- |
| `/v1/messages` haiku non-stream max 1 | 2 | 2/2 HTTP 200 JSON | 2/2 top-level usage | 0/2 present |
| `/v1/messages` haiku non-stream max 32 | 2 | 2/2 HTTP 200 JSON | 2/2 top-level usage | 0/2 present |
| `/ha/v1/messages` opus non-stream max 8192 | 2 | 2/2 HTTP 200 JSON | 2/2 top-level usage | 0/2 present |
| `/v1/messages` haiku stream max 16 | 2 | 2/2 HTTP 200 SSE | 2/2 SSE usage events | 2/2 present |
| `/ha/v1/messages` opus stream max 64 | 1 | 1/1 HTTP 200 SSE | 1/1 SSE usage events | 1/1 present |

复核里的一个重要方法细节：

```text
Python urllib 矩阵曾全部返回 403；
这些 403 被排除在 usage 结论之外；
因为 curl 客户端形态是已知可成功的真实路径，后续矩阵用 curl 重跑。
```

这避免把客户端形态导致的 403 混入 usage 结论。

## 是否透传还是整形

结论要拆成三层：

### 1. 是否证明拿到了上游原始 body

没有。

当前证据没有 HTTPS 明文上游响应，也没有应用层上游 body 采样。历史成功响应 body 没有存库，所以不能证明“上游原始 body 等于最终下游 body”。

### 2. 最终下游 body 是否像透传 raw usage

强烈像。

原因是 pool 15 配置为 `current_path_policy`，流式路径同样 prompt 下会把 raw usage：

```text
inputTokens=815
cacheReadInputTokens=734
```

处理成 reported usage：

```text
inputTokens=817
cacheReadInputTokens=0
```

但非流式最终下游 body 仍出现：

```text
input_tokens=81
cache_read_input_tokens=734
```

这不是 stream 路径的 reported usage 形态。

### 3. 按配置是否应该整形

应该。

pool 15 和 pool 4 的 `usage_projection_mode=current_path_policy`，且 `reportedUsage.default.skipNonStreamUsageProjection=false`。因此正常非流式响应不应该只返回 raw/pass-through usage。

最准确说法是：

```text
非流式最终下游 body 是可解析 usage，但不是配置语义下应返回的 reported usage。
是否完全字节级透传上游 body，仍需要应用层采样才能证明。
```

## 当前源码链路

本地源码版本与生产 image label revision 对齐：

```text
c1748265b904aacdbd6fa33f4bd2e86985ad1f53
```

关键源码入口：

```text
src/external_pool.rs
  forward_once
  maybe_project_non_stream_usage
  process_usage_slots_in_sse_value
  cache_usage_from_value
  external_pool_billing_from_capture
  record_external
```

源码期望的非流式链路：

```text
HTTP 200
-> response.bytes()
-> success_response_looks_like_html / success_response_looks_like_error_body
-> maybe_project_non_stream_usage(bytes, projection_context)
-> external_pool_billing_from_capture(route, pool, projected.usage_capture)
-> record_external_success(..., billing)
-> 返回 projected.body 给下游
```

当前 `maybe_project_non_stream_usage` 的明显限制：

```text
只解析 JSON；
只找顶层 $.usage；
不扫 $.message.usage / $.data.usage / $.response.usage 等 wrapper；
只认 Anthropic snake_case usage 字段；
capture.raw 缺失时 external_pool_billing_from_capture 直接返回 None。
```

但本轮生产复核出现了更强的矛盾：

```text
最终下游 body 明明有顶层 Anthropic usage，
DB/Redis 仍然没有 rawUsage/externalPoolBilling，
非流式 body 还没有按 current_path_policy 改写。
```

所以修复不能只补几个 parser path。必须在“最终要返回给下游的 body”和“内部要记录的 billing”之间建立同源不变量。

## 已证明、推断、未知

已证明：

```text
pool 15 非流式 HTTP 200 能返回标准顶层 usage，但 DB/Redis billing 缺失。
pool 15 流式 HTTP 200 能返回 SSE usage，并正确写 DB/Redis billing。
pool 15 非流式 body usage 与 stream reported usage 形态不一致。
pool 4 非流式历史 0 计费大量存在，并与高 max_tokens 强相关。
usage_repair_loop 没有覆盖或修复复核样本；Redis 和 Postgres 状态一致。
外部上游 403/502/524/模型不可用是单独问题。
```

强推断：

```text
非流式 usage accounting 路径在成功 200 下丢失或没有使用最终 body usage。
非流式 usage projection 没有稳定作用到最终下游 body。
对 current_path_policy 池，非流式下游 usage 可能导致下游按错误语义计费。
```

仍未知：

```text
为什么生产中最终 body 有顶层 usage 时 maybe_project_non_stream_usage 没有留下 billing。
body 在 parser 看到时和下游收到时是否被其他运行时分支改变。
pool 4 历史 0 计费成功响应的原始 body 到底是 HTML、JSON wrapper、无 usage JSON、SSE 文本，还是字段名不兼容。
```

这些未知项需要应用层窄采样或修复后的诊断字段来闭环。

## 下游计费语义

必须区分三类 usage：

| usage 类型 | 含义 |
| --- | --- |
| raw usage | 上游外部池返回的原始 usage |
| reported usage | kiro.rs 返回给下游客户端的 usage |
| billable usage | kiro.rs 内部计费用 usage |

对于 `pass_through`：

```text
reported usage = raw usage
billable usage = raw usage
```

对于 `current_path_policy`：

```text
raw usage      = upstream observed usage
reported usage = current path cache/output policy 后的 usage
billable usage = reported usage
```

对于正常 200 但上游 usage 缺失或不可用：

```text
raw usage      = absent 或 estimated snapshot
reported usage = kiro.rs 合成的保守 usage
billable usage = 合成的 reported usage
metadata       = usageEstimated=true / response_estimate
```

核心要求：

```text
返回 HTTP 200 可以继续保持正常成功；
但是只要是正常模型响应，就不能让下游或内部看到不可计费、语义错误或缺失的 usage。
```

## 修复不变量

每一个外部池成功模型响应都必须满足：

```text
HTTP status is 200
AND response is a normal model response
=>
downstream response contains parseable Anthropic-compatible usage
AND downstream usage respects pool.usage_projection_mode
AND UsageRecord contains raw/reported usage evidence
AND UsageRecord contains externalPoolBilling
AND UsageRecord token/cost fields are derived from reported usage
```

允许例外：

```text
pricing catalog 缺少模型价格。
```

即使价格缺失，也应该保留 `externalPoolBilling`、`rawUsage`、`reportedUsage`，并设置：

```text
pricingAvailable=false
cost=0
```

不能因为价格缺失或 usage 是估算，就把整条成功记录退化成：

```text
outputTokens=0
externalPoolBilling missing
```

## 完整修复方案

### 1. 引入最终响应 usage 处理器

新增一个 stream/non-stream 共用的 usage 处理模块。它的职责不是“顺手 parse 一下 usage”，而是对最终响应和内部 billing 建立同源结果。

建议数据结构：

```rust
struct ExternalResponseUsageResult {
    body: Bytes,
    raw_usage: Option<CacheUsage>,
    shaped_usage: Option<CacheUsage>,
    reported_usage: Option<CacheUsage>,
    usage_projection_applied: bool,
    usage_estimated: bool,
    diagnostics: ExternalResponseUsageDiagnostics,
}
```

非流式调用位置：

```text
response.bytes()
-> classify success body
-> process_external_non_stream_response_usage(...)
-> 用 result.body 返回下游
-> 用 result.raw/shaped/reported 生成 externalPoolBilling
-> record_external_success
```

流式路径也应复用同一套 projection 和 billing 决策，避免 stream 与 non-stream 继续分裂。

### 2. 对 HTTP 200 body 做明确分类

成功响应 body 先分类：

```text
html_success_protocol_error
error_envelope_success_protocol_error
anthropic_message_json
anthropic_wrapper_json
sse_text_on_non_stream
unknown_text_or_binary
```

已有 HTML / error envelope 识别应该保留。明确 HTML 或错误 envelope 不应该被硬合成为正常 200 usage；这类响应应该继续走协议错误、重试或失败归类。

正常模型 JSON 响应才进入 usage 抽取、projection、补齐、计费。

修复过程中新增的分支规则：

```text
非流式请求不再因为上游 response header 看起来像 SSE 就进入 stream branch。
route.stream=false 时先读完整 body：
  JSON model body -> 非流式处理、必要时修正 content-type；
  真正 SSE text -> success protocol error。
```

### 3. 支持多路径 usage 抽取

非流式 usage 抽取不能只看顶层 `$.usage`，需要支持：

```text
$.usage
$.message.usage
$.delta.usage
$.data.usage
$.response.usage
```

识别 Anthropic 字段：

```text
input_tokens
output_tokens
cache_creation_input_tokens
cache_read_input_tokens
cache_creation.ephemeral_5m_input_tokens
cache_creation.ephemeral_1h_input_tokens
```

兼容 OpenAI 字段：

```text
prompt_tokens -> input_tokens
completion_tokens -> output_tokens
total_tokens -> 只做诊断，除非 input/output 缺失时用于兜底估算
```

候选选择顺序：

```text
1. final message 顶层 usage
2. message.usage
3. final delta usage
4. wrapper usage
```

诊断里记录：

```text
usage_candidate_paths
selected_usage_path
parser_rejection_reason
```

### 4. projection 必须统一作用到下游 body 和内部 billing

处理逻辑：

```text
if pool.usage_projection_mode == pass_through:
    reported_usage = raw_or_estimated_usage
    body_usage = reported_usage
    usage_projection_applied = false

if pool.usage_projection_mode == current_path_policy:
    shaped_usage = apply current path policy(raw_or_estimated_usage)
    reported_usage = apply uplift/output guard/final policy(shaped_usage)
    body_usage = reported_usage
    usage_projection_applied = true
```

非流式 body 在 `current_path_policy` 下必须重写 usage 字段。不能出现 DB 用 reported usage、下游 body 用 raw usage，或下游有 usage、DB 没 billing 的状态。

### 5. billing 从同一份 reported usage 生成

`ExternalPoolBilling` 应该在 usage 已知或合成时总是生成：

```text
rawUsage       = 上游原始 usage；如果没有，则保存 estimated raw snapshot
shapedUsage    = policy 后 usage
reportedUsage  = 最终返回下游的 usage
billableCost   = price(reportedUsage)
reportedCost   = price(reportedUsage)
rawCost         = price(rawUsage)，如果 raw 是估算则标记
pricingAvailable = price lookup 是否成功
usageEstimated = 是否发生估算
```

如果 pricing 不可用：

```text
externalPoolBilling 仍然存在
usage snapshots 仍然存在
pricingAvailable=false
cost=0
```

不能把 billing 对象整体丢掉。

### 6. 正常 200 缺 usage 时合成 usage

如果响应是正常模型响应，但没有可用 usage：

输入 token：

```text
使用 route.request_input_tokens 或现有请求侧估算
```

输出 token：

```text
从 response content text 做估算
如果有非空文本但估算为 0，output_tokens 至少记 1
如果 stop_reason 存在但没有文本，output_tokens 可以是 0
```

然后注入标准 Anthropic usage：

```json
{
  "usage": {
    "input_tokens": 0,
    "cache_creation_input_tokens": 0,
    "cache_read_input_tokens": 0,
    "output_tokens": 0
  }
}
```

实际值来自 estimated/reported usage。注入后：

```text
HTTP 200 保持成功
downstream body 有 usage
UsageRecord 有 externalPoolBilling
usageEstimated=true
```

### 7. stream 缺 final usage 时也要兜底

流式处理应记录：

```text
raw usage event
reported usage event
text delta bytes/chars/token estimate
message_stop seen
```

如果看到 usage event：

```text
按统一 projection 处理，记录 billing。
```

如果没看到 usage event，但已经输出文本：

```text
在 message_stop 前注入 final message_delta usage event；
usageEstimated=true；
record externalPoolBilling。
```

如果无文本但响应是合法空模型响应：

```text
注入/记录 input estimate + output 0；
usageEstimated=true。
```

### 8. 加成功 usage 守卫

在写成功记录前加守卫：

```rust
if status == success && route_kind == external_pool {
    assert_or_repair_success_usage_invariant(...)
}
```

生产里不能 panic。守卫行为：

```text
能修就修；
不能修就打 bounded diagnostic；
只要最终 body 有正常 usage，就不能让 billing=None；
正常模型 200 缺 usage 时必须估算并注入。
```

诊断字段：

```text
request_id
endpoint
stream
pool_id
pool_name
usage_projection_mode
response_content_type
body_len
body_sha256
json_parse_ok
body_class
top_level_keys
usage_candidate_paths
selected_usage_path
raw_usage_present
reported_usage_present
billing_present_before_guard
billing_present_after_guard
usage_estimated
body_prefix_redacted_512
```

禁止记录：

```text
prompt
API key
完整输出
完整 body
```

### 9. 持久化估算元数据

给 `ExternalPoolBilling` 或相邻诊断对象增加：

```text
usageEstimated: bool
usageEstimateReason:
  missing_upstream_usage
  unparseable_usage
  stream_missing_final_usage
usageCandidatePath: string | null
bodyUsageProjectionApplied: bool
```

可以考虑新增 usage source：

```text
response_estimate
```

如果第一版不想动枚举兼容性，可以暂时保留 `usageSource=request_estimate`，但必须保留：

```text
externalPoolBilling
usageEstimated=true
reportedUsage
rawUsage/estimatedRawUsage
```

长期上应该把“只有请求侧估算”和“根据响应内容估算”区分开。

## 历史数据处理方案

历史精确回填不可行，因为历史成功响应 body 没有保存。

允许做的历史处理：

```text
标记 affected records 为 external_pool_success_missing_billing；
按 pool/model/stream/max_tokens/input_tokens 计算 exposure 区间；
不声称历史 exact output usage；
不静默把历史 0 记录改成权威已计费记录。
```

可选保守回填：

```text
如果未来诊断采样里捕获到同 request 的下游 usage，可精确回填；
没有 body 的历史记录，只加 anomaly marker 和 estimated exposure 字段；
是否把 estimated exposure 计入账单，需要业务确认。
```

## 测试计划

### 单元测试

覆盖最终响应 usage 处理器：

```text
1. 非流式顶层 Anthropic usage -> billing present, rawUsage present。
2. 非流式 current_path_policy -> 返回 body usage 被 projection，DB reportedUsage 等于 body usage。
3. 非流式 pass_through -> body usage 等于 raw usage，billing present。
4. 非流式正常 content 缺 usage -> 注入 usage，billing present，usageEstimated=true。
5. 非流式 wrapper usage -> message.usage/data.usage 能识别并标准化。
6. OpenAI-style usage -> prompt_tokens/completion_tokens 正确归一。
7. HTML success -> 仍归类 protocol error，不合成 usage。
8. HTTP 200 error envelope -> 仍归类 protocol error，不合成 usage。
```

### 流式测试

```text
1. SSE 正常 usage -> billing present，final downstream usage 等于 reported usage。
2. SSE 缺 usage 但有文本 -> 注入 final usage event，usageEstimated=true。
3. message_start 与 message_delta 都带 usage -> final message_delta authoritative，DB 与最终下游一致。
4. pass_through/current_path_policy 两种模式都覆盖。
```

### 集成测试

使用 fake external pool server 覆盖：

```text
1. non-stream JSON with usage
2. non-stream JSON without usage
3. stream SSE with usage
4. stream SSE without usage
5. upstream HTML 200
6. upstream error envelope 200
7. non-stream JSON body with erroneous text/event-stream header
```

每个成功 case 断言：

```text
HTTP 200 returned downstream
downstream usage present
UsageRecord status=success
externalPoolBilling present
outputTokens matches reported usage
estimatedCostUsd > 0 when pricing available and input/output nonzero
```

### 生产回归复核

补丁部署后，用同一类 curl 矩阵低频复核：

```text
/v1/messages haiku non-stream max 1
/v1/messages haiku non-stream max 32
/ha/v1/messages opus non-stream max 8192
/v1/messages haiku stream max 16
/ha/v1/messages opus stream max 64
```

期望：

```text
non-stream:
  HTTP body usage present
  current_path_policy 下 body usage 已 projection
  DB rawUsage present
  DB externalPoolBilling present
  DB reportedUsage equals downstream body usage

stream:
  同样满足上述条件
```

## 实施触点

主要代码文件：

```text
src/external_pool.rs
  forward_once non-stream branch
  maybe_project_non_stream_usage
  process_usage_slots_in_sse_value
  external_pool_billing_from_capture
  record_external

src/external_pool/usage_projection.rs
  shared projection context
  cache commit behavior

src/anthropic/usage.rs
  usage source / billing metadata

src/storage/postgres.rs
src/storage/redis_cache.rs
  新 billing metadata 的序列化兼容
```

实现不要针对 pool 15 或 pool 4 写特判。它是外部池成功响应 usage 不变量，应该覆盖所有 external pool。

## 最终要求

修复完成后，每个外部池成功模型响应都应满足：

```text
正常 HTTP 200 继续返回 HTTP 200。
下游 body 一定有 Anthropic-compatible usage。
current_path_policy 下，下游 usage 是 reported/projected usage。
pass_through 下，下游 usage 是 raw usage。
内部 UsageRecord 有 raw/reported usage evidence。
内部 UsageRecord 有 externalPoolBilling。
内部 token/cost 字段来自 reported usage。
上游 usage 缺失时，kiro.rs 合成、注入、计费，并明确标记 usageEstimated=true。
```

一句话结论：

```text
问题不是“HTTP 200 要不要返回成功”。
HTTP 200 正常模型响应应该继续成功返回；
真正必须修的是：成功返回时，下游 usage 和内部 billing 必须同源、可解析、符合 pool 配置、不可静默退化成 0 计费。
```

## 修复完成与验证结果

本轮实现已经把上面的设计落到代码里，且没有改动正常业务请求的主流程：

```text
1. 非流式分支不再因为 upstream 响应头像 SSE 就被误判成 stream branch。
2. 非流式 JSON 响应继续按完整 body 处理；如果 header 错标为 text/event-stream，但 body 仍是 JSON，会修正下游 content-type 为 application/json。
3. 非流式成功响应支持顶层 usage、wrapper usage、OpenAI-style usage；缺 usage 时会合成保守 billing，而不是静默落成 0 计费。
4. 流式尾包缺 usage 时会补 synthetic usage，保证 final billing 不悬空。
5. pass_through 仍保持 body 原样，不会被强制整形。
6. current_path_policy 仍按已配置策略整形 body 和 billing。
```

新增并通过的验证包括：

```text
cargo fmt --check
git diff --check
cargo test external_pool:: -- --nocapture
cargo test
cargo build --release
```

全量测试结果：

```text
main tests: 1286 passed
kiro_loadtest tests: 26 passed
external_pool 子集: 140 passed
```

模拟上游接入备用池的集成测试也已通过：

```text
external_pool_fake_upstream_non_stream_json_with_sse_header_records_billing
```

这个测试使用本地 fake upstream，返回：

```text
HTTP 200
content-type: text/event-stream
body: 标准 Anthropic JSON message with usage
route.stream=false
pool.usage_projection_mode=current_path_policy
```

断言结果：

```text
downstream HTTP 200
downstream content-type=application/json
downstream usage 被 projection
UsageRecord.status=success
rawUsage present
externalPoolBilling present
```

此外，本轮还在 release build 里发现并修掉了一个编译可见性问题：非流式输入 token 估算 helper 原本只在测试态可见，release 构建会失败。这个问题已修复，`cargo build --release` 已通过。

对“不能影响其他业务逻辑”的覆盖，已经由现有测试和本轮新增测试共同兜住：

```text
pass_through 不改 body
current_path_policy 按预期整形
普通模型调用仍返回正常 200
stream 路径仍能产出 usage
```
