# 159/170 Native WebSearch MCP 错误聚类

Status: `partially-fixed / scheduler fallback fixed / MCP health poisoning fixed / mcp-upstream body evidence pending`

Severity: P0/P1。Native WebSearch 请求在本地 MCP 路径失败时会直接对下游返回 502/503；在配置存在 normalized 外部池时，旧代码还会绕过外部池 fallback。

Last verified: 2026-07-26

## 影响与现象

两台生产机器都出现 native WebSearch 专用路径错误。多数记录对下游表现为通用 502/503 `api_error`，用户只能看到 error ID；usage 内部则分别落为 `websearch_mcp_scheduler_unavailable` 或 `websearch_mcp_upstream_error`。

## 生产机器与版本

只读取证机器：

- 152.53.243.159，服务容器 `kiro-rs-2ue-59137-app`，镜像 `ghcr.io/2ue/kiro-rs:latest`。
- 152.53.194.170，服务容器 `kiro-rs-2ue-59137-app`，镜像 `ghcr.io/2ue/kiro-rs:latest`。

取证根目录：

```text
tmp/prod-evidence/20260725-221257-159-170-errors
```

该目录 raw 下包含未打包的本地只读证据，不应提交或公开。

## 错误类 A：websearch_mcp_scheduler_unavailable

159 最近 12 小时样本：

```text
count: 117
first_seen: 2026-07-25 12:09:40 UTC
last_seen:  2026-07-25 13:59:19 UTC
route: local_credential / local_error_no_fallback
public status/type: 503 / api_error
latencyTrace:
  localAttempts=0
  mcpAttempts=0
  externalAttempts=0
```

代表 usage：

```text
req_01QjVytRXK7sxeXhw1tb87bE
endpoint: /ha/v1/messages
model: claude-opus-4-6 -> claude-opus-4.6
errorMessage: websearch_mcp_scheduler_unavailable
selectionFailure.route: mcp
selectionFailure.stage: account_eligibility
selectionFailure.primaryReason: disabled
selectionFailure.reasonCounts.disabled: 5
```

同期 159 配置/池状态：

```text
externalPoolsEnabled=true
localPoolPreflightEnabled=true
fallbackOnNoAvailableCredentials=true
fallbackOnSchedulerRedisDegraded=true
external_pool_modes:
  raw_passthrough total=4 enabled=0
  normalized total=11 enabled=3
credentials:
  total=753 active_non_deleted=0 active_enabled=0
```

### 根因

该类与 [Native WebSearch 与 normalized 外部池 fallback 断路](websearch-normalized-external-fallback-preflight.md) 相同：

1. 入口 raw external preflight 只覆盖 raw-passthrough 外部池。
2. 外部池只有 normalized 可用时，raw preflight 不 eligible。
3. typed parse 后检测到 `web_search_20250305`，旧代码直接进入 WebSearch MCP。
4. MCP 只能使用本地 Kiro 凭据；本地 MCP 凭据全 disabled/不可调度。
5. WebSearch 分支提前返回 `local_error_no_fallback`，后续 normalized external fallback 没机会执行。

### 当前修复

当前工作树已在 WebSearch MCP 前增加 typed local-pool preflight：

```rust
maybe_local_pool_preflight_external_response(Some(external), request_id, Some(preflight_model))
```

本地池不可调度且 normalized 外部池符合配置时，应直接变成 external fallback，不进入 MCP。

2026-07-26 追加了后置保护：如果 preflight 被关闭、配置 race 或局部状态没有接住，WebSearch MCP acquire 失败后会读取 provider 的 `selectionFailure`，把 no-credentials/all-disabled/capacity/scheduler degraded 分类成真实 local fallback reason，再按 external fallback 配置处理。这样不会把 `primaryReason=disabled` 的场景误记为 Redis degraded，也不会继续返回 `local_error_no_fallback`。

## 错误类 B：websearch_mcp_upstream_error

159 最近 12 小时：

```text
count: 146
first_seen: 2026-07-25 08:26:22 UTC
last_seen:  2026-07-25 12:06:16 UTC
```

170 最近 12 小时：

```text
count: 266
first_seen: 2026-07-25 05:37:43 UTC
last_seen:  2026-07-25 13:59:21 UTC
```

170 代表 usage：

```text
req_01gKtc27UAESgttiEZpfFptW
endpoint: /ha/v1/messages
model: claude-opus-4-6 -> claude-opus-4.6
route: local_credential / local_error_no_fallback
public status/type: 502 / api_error
errorMessage: websearch_mcp_upstream_error
latencyTrace:
  localAttempts=0
  mcpAttempts=0
  externalAttempts=0
```

### 当前判断

该类不是上面的 scheduler selection failure，因为 usage metadata 没有 `selectionFailure`。它来自 `src/anthropic/websearch.rs::WebSearchFailure::from_provider_error` 的默认分支：

```rust
Some(McpCallFailureKind::Upstream) | None
  => "websearch_mcp_upstream_error"
```

也就是说，MCP provider 返回了“上游类”错误，或没有附带更精确的 MCP failure kind。当前 usage 没保存 MCP raw response body/top-level shape，因此无法从历史 usage 区分：

- MCP HTTP 200 但 JSON-RPC `error`；
- MCP HTTP 200 但 `result.isError=true`；
- MCP body read 后解析成未知业务错误；
- provider 侧把某类前置失败折叠成 `McpCallFailureKind::Upstream`；
- 旧版本未把 `McpCallAttribution` 写入 usage，导致 `mcpAttempts=0` 但实际是否尝试过不可从 usage 精确判断。

### 与当前 WebSearch preflight 修复的关系

如果本地池不可调度且 normalized external 可用，当前 preflight 修复会在 MCP 前把请求转给 external pool，因此这类 `websearch_mcp_upstream_error` 在“本地不可调度”窗口也会下降。

但如果本地 MCP 凭据 Ready、且确实进入本地 WebSearch MCP 后返回 JSON-RPC/tool/upstream 错误，本修复不会改变该真实 MCP 错误。该类仍需补 body-level 诊断或更细 MCP failure kind。

2026-07-26 还修复了一个与该类相关但不是同一个 usage root cause 的问题：MCP/WebSearch 普通辅助失败以前会把 `mcp_completion upstream_error`、429/5xx/body-read/protocol 等写入主模型凭据 runtime cooldown，导致凭据卡片出现 `server: mcp_completion upstream_error`，并可能把健康模型凭据从主调度中短暂移除。当前工作树已改为：普通 MCP 辅助失败只释放 lease、保留 usage attribution、在本次请求内换号，不写全局 credential cooldown；401/403、明确风控、明确额度耗尽仍保留真实凭据状态更新。

## 根因

本轮确认了两个不同根因：

1. `websearch_mcp_scheduler_unavailable` 的根因是 WebSearch special path 在 typed parse 后直接进入本地 MCP，绕过 normalized external fallback preflight。
2. `websearch_mcp_upstream_error` 的根因仍未完全闭环；代码层它是 MCP provider 错误缺少更细 failure kind 后的默认 upstream 分类，生产 usage 缺少 MCP body/top-level 诊断，无法仅凭历史行恢复具体上游响应形态。
3. 凭据卡片 `server: mcp_completion upstream_error` 的根因是旧 MCP 辅助失败写入主模型 credential runtime health；当前已移除该污染路径。

## 复现方案

### Scheduler unavailable 红绿复现

本地隔离环境：

- 无本地 Kiro credentials；
- external pool `requestBodyMode=normalized` 且 enabled；
- 请求 `/ha/v1/messages`；
- body 包含唯一 `web_search_20250305` server tool；
- stream/non-stream 各 5 轮。

修复前：

```text
HTTP 503
routeSubtype=local_error_no_fallback
fake external hits=0
websearch_mcp_scheduler_unavailable=true
```

修复后：

```text
HTTP 200
routeSubtype=external_fallback_preflight
fake external hits=1
mcp hits=0
```

后置 fallback 补充复现：

- `localPoolPreflightEnabled=false`
- 本地 credentials 为空
- normalized external pool 可用

修复后：

```text
HTTP 200
routeKind=external_pool
localAttempted=true
fallbackReason=local_no_credentials
externalAttempts=1
mcp HTTP hits=0
```

### Upstream error 复现

fake MCP upstream 分别返回：

- JSON-RPC `error`；
- `result.isError=true`；
- invalid envelope id；
- missing result；
- non-text content；
- malformed JSON；
- response body timeout/over-limit。

验收：

- 全部 fail closed；
- 下游不是假成功；
- usage 记录 `websearchFailureReason`；
- `mcpAttempts` 与 actual MCP HTTP hits 对齐；
- 不把 raw query/result/body 写入公开错误。

该矩阵历史已在 [WebSearch/MCP 协议、错误、usage、attempt 与隐私边界](websearch-mcp-protocol-usage-and-privacy.md) 中实现，但生产 `websearch_mcp_upstream_error` 仍缺历史 body-level 证据，不能倒推出具体 upstream body。

## 建议优化

1. 保留当前 typed preflight 修复，先解决 normalized external 被 WebSearch 分支吞掉的问题。
2. 对 `websearch_mcp_upstream_error` 增加脱敏诊断：
   - response status；
   - content-type；
   - body bytes；
   - JSON top-level keys；
   - JSON-RPC `error.code`/`error.message` 的安全分类；
   - `result.isError` 布尔值；
   - 不保存 query/result 原文。
3. usage 的 `mcpAttempts` 必须与真实 MCP HTTP send 对齐；如果 provider attribution 缺失，应记录 `attributionMissing=true`，不要默默显示 0。
4. 如果 local MCP credentials 不可调度且 external 不可用，错误应明确是 local MCP capacity/scheduler；如果 external 可用，按 fallback 配置接管。

## 发布后验证

```sql
select coalesce(error_message,error_detail,''), count(*), min(created_at), max(created_at)
from usage_records
where created_at >= now() - interval '2 hours'
  and coalesce(error_message,error_detail,'') ilike 'websearch_mcp%'
group by 1
order by count(*) desc;
```

期望：

- `websearch_mcp_scheduler_unavailable` 在 normalized external 可用且本地池不可调度时不再新增；
- 同类请求 route 变成 `external_pool / external_fallback_preflight`；
- `websearch_mcp_upstream_error` 若继续新增，需要采集新增脱敏 MCP body diagnostics 后再归因。
- 凭据 runtime 卡片不应再新增 `mcp_completion upstream_error` 来源；如果出现，需确认是否仍为旧版本实例或另一个写入路径。

## 2026-07-26 本地验证结果

命令均通过 `feature/tests/run-cargo-scoped.sh` 执行，target 自动清理。

```text
native_websearch_normalized_external_preflight_precedes_mcp_for_five_rounds ... ok
native_websearch_scheduler_failure_falls_back_to_external_after_mcp_path_for_five_rounds ... ok
websearch_mcp_error_resource_and_recovery_matrix_is_fail_closed_for_five_rounds ... ok
websearch 全量 29 tests ... ok
mcp_completion_failure_types_release_without_poisoning_core_credentials_for_five_rounds ... ok
mcp_real_sends_share_request_budget_for_1_20_60_accounts_over_five_rounds ... ok
mcp_error_response_body_is_bounded_while_reading_for_five_rounds ... ok
request_admission 25 tests ... ok
local_pool_fast_fail 2 tests ... ok
external_pool scheduler degraded fallback/config/migration selected tests ... ok
```

## 残余风险

- Claude Code CLI 2.1.197 当前不会发送 native `web_search_20250305`，因此真实 CLI 不能覆盖这个 server-tool 分支；必须用官方 Anthropic Messages body 直接验证。
- 若部署没有外部池，且本地 MCP 凭据不可用，WebSearch 仍应失败；这是资源不可用，不是 fallback bug。
- 若外部池本身不支持 server-side WebSearch，转发后可能返回 external upstream error；这是外部池能力问题，需从 external attempts 诊断。

## 2026-07-26 当前候选补证

当前冻结候选：

```text
kiro-rs sha256=7268b3e722f03a40179d205e7b5917b86d696cd8bf1d5f6533d3b1347ea30bec
```

补充验证：

- C0 静态/完整 Rust/release build/clippy baseline 已通过。
- fake-upstream L3/L4/L5 负载/异常恢复通过，错误爆发和 mixed chaos 后 normal recovery 均成功；见 [candidate-c0-load-chaos-20260726](../evidence/candidate-c0-load-chaos-20260726.md)。
- 真实 Claude Code CLI fake-upstream 协议通过，说明本次改动没有重新引入 transcript/tool/thinking wire 问题；见 [candidate-c0-claude-cli-real-protocol-20260726](../evidence/candidate-c0-claude-cli-real-protocol-20260726.md)。

上线后必须用生产 usage 验证两个不同预期：

1. `websearch_mcp_scheduler_unavailable` 在 normalized external 可用且本地池不可调度时应下降/消失，并改为 external route。
2. `websearch_mcp_upstream_error` 如果仍新增，不能直接视为 scheduler fallback 未修；需要看新增的 MCP body/top-level 诊断或至少看 externalAttempts/mcpAttempts 是否对齐。
