# Native WebSearch 与 normalized 外部池 fallback 断路

Status: `fixed / focused + broad WebSearch regression passed / production recurrence pending`

Severity: P0/P1。对启用 Anthropic 原生 `web_search_20250305` 的请求，如果本地凭据池不可调度且外部池仅支持 `normalized` body，旧路径会在 fallback 之前进入 WebSearch MCP，本地 MCP 调度失败后直接向下游返回 503。

Last verified: 2026-07-26

## 用户可见现象

代表性现网记录来自 152.53.243.159，版本 `0.0.117`：

- endpoint: `/ha/v1/messages`
- stream: true
- request model: `claude-opus-5`
- route: `local_credential / local_error_no_fallback`
- public status: 503
- internal reason: `websearch_mcp_scheduler_unavailable`
- `latencyTrace`: `mcpAttempts=0`, `localAttempts=0`, `externalAttempts=0`

下游看到的是通用 `api_error`：

```text
The request could not be completed right now. Please retry shortly.
```

如果同一服务仍有可用外部池，用户预期是：本地池不可用时应按 external fallback 配置交给外部池，而不是在 WebSearch MCP 分支直接失败。

## 已确认不是同一问题的情况

- Claude Code CLI 2.1.197 的 `--bare --print` 真实请求不会发送 Anthropic 原生 server-side WebSearch tool。即使传 `--tools=WebSearch --allowedTools=WebSearch`，实际 body 中 `tools` 仍没有 `web_search_20250305`；`--tools=default` 只出现 `Bash/Edit/Read` 等客户端工具。
- 因此本问题不能用 Claude CLI 内置工具直接复现。应使用官方 Anthropic Messages 协议 body：

```json
{
  "model": "claude-opus-5",
  "max_tokens": 64,
  "stream": true,
  "tools": [
    { "type": "web_search_20250305", "name": "web_search", "max_uses": 1 }
  ],
  "messages": [
    { "role": "user", "content": "Search web once and answer one short sentence." }
  ]
}
```

2026-07-29 本地账号复验进一步确认：Claude Code CLI `2.1.220` 在当前 `ccman` 本地配置下，`--tools WebSearch --allowedTools WebSearch` 仍没有产生真实 `tool_use` 或 `server_tool_use`，只生成了 `<search_web>` 伪 XML 文本；纯 native `web_search_20250305` 单工具 direct body 可以通过本地账号 credential `8` 成功返回 `server_tool_use` / `web_search_tool_result`；但 native WebSearch 与普通 tool 混用时不会进入本文件修复的 pure native 分支，而是落成普通 `tool_use name="web_search"`。这段是 2026-07-29 的修复前记录，2026-07-31 的 focused native version-family / mixed / CLI WebSearch 结果已记录在 [Claude Code local-account WebSearch/tools/image analysis - 2026-07-29](claude-code-local-accounts-websearch-tools-image-analysis-20260729.md) 并应作为当前状态优先参考。

## 根因

请求入口已有一层 raw external preflight：

```rust
maybe_raw_external_preflight_response(...)
```

但它只接受 `ExternalPoolRequestBodyMode::RawPassthrough` 外部池，因为此时还没有完成 typed parse，只能安全透传原始 body。

旧流程顺序是：

1. raw direct/preflight 尝试外部池；
2. 如果外部池是 `normalized`，raw preflight 不 eligible；
3. typed parse 成 `MessagesRequest`；
4. 检测到 `tools` 只有一个原生 `web_search_20250305`；
5. 直接进入 `websearch::handle_websearch_request(...)`；
6. WebSearch MCP 需要本地 Kiro 凭据；
7. 本地凭据池不可调度时，MCP 分支返回 `websearch_mcp_scheduler_unavailable`；
8. 由于 WebSearch 分支提前 `return`，后续 normalized external fallback 没有机会执行。

这个问题只在“外部池存在但不是 raw-passthrough”时暴露。raw-passthrough 外部池在入口层已经可以接住，所以旧代码不会复现 503。

## 修复方案

在 native WebSearch MCP 分支调用 MCP 前，复用 typed parse 后已有的 external fallback context：

```rust
maybe_local_pool_preflight_external_response(
    Some(external),
    &request_id,
    Some(preflight_model),
)
```

这一步发生在模型解析完成之后，`ExternalFallbackContext` 已有 normalized payload 和 model resolution，可使用与普通 parsed fallback 相同的外部池 eligibility 规则：

- `requires_normalized_body=false` 时不强制 body mode，raw/normalized 外部池均可 eligible；
- `requires_normalized_body=true` 时仍只允许 normalized；
- `fallbackOnNoAvailableCredentials` / `fallbackOnLocalCapacityExhausted` / `fallbackOnSchedulerRedisDegraded` / `fallbackOnLocalTransientExhausted` 等开关仍由 `local_pool_fallback_reason_for_fresh_state` 统一控制；
- 如果本地池 Ready，不会触发外部池，仍走原 WebSearch MCP。

这不是“所有 WebSearch 都走外部池”，只是在本地池不可调度且外部池 fallback policy 允许时，把请求交给外部池，避免 MCP 特殊分支吞掉 fallback。

2026-07-26 追加修复了第二个分叉：如果 local pool preflight 被关闭或 race 中没有接住，WebSearch MCP 路径进入 provider 后仍可能在 acquire 阶段失败。旧后置 fallback 没有覆盖 WebSearch failure 分支，且曾把所有 `websearch_mcp_scheduler_unavailable` 粗暴当成 Redis degraded。当前工作树会读取 provider 携带的 `selectionFailure`：

- `NoAccounts` -> `local_no_credentials`
- `Disabled/MissingAuth/HealthBlocked` -> `local_all_disabled` 或对应 no-available 状态
- `RpmLimited/CooldownActive/CapacityFull` -> 对应 transient/capacity fallback
- `DispatchQueue + Unknown` -> `local_scheduler_redis_degraded`

因此本地无凭证、全禁用、容量满、Redis degraded 不再混成同一个错误原因。

## 代码变更

- [src/anthropic/handlers.rs](/Users/yuanfeijie/Desktop/procode/kiro.rs/src/anthropic/handlers.rs): native WebSearch MCP 分支前新增 typed local-pool preflight external takeover。
- [src/anthropic/handlers.rs](/Users/yuanfeijie/Desktop/procode/kiro.rs/src/anthropic/handlers.rs): WebSearch MCP failure 后置 external fallback 新增 `selectionFailure` 分类，避免把 no-credentials/all-disabled/capacity full 误写成 Redis degraded。
- [src/anthropic/handlers/tests.rs](/Users/yuanfeijie/Desktop/procode/kiro.rs/src/anthropic/handlers/tests.rs): 新增 handler 级真实 HTTP 回归：
  - fake Kiro MCP upstream；
  - fake external Anthropic `/v1/messages` upstream；
  - 本地 Kiro credentials 为空；
  - 外部池 `requestBodyMode=normalized`；
  - `/ha/v1/messages` + `web_search_20250305`；
  - stream/non-stream 各 5 轮。
  - preflight 关闭 + 本地无凭证 + normalized 外部池可用时，WebSearch 后置 fallback 仍返回 external success，usage `fallbackReason=local_no_credentials`。

## 红绿复现

### 旧二进制复现

旧候选二进制：`d89e32725fea0672281c73b12dbdaa90cb7f8c3642201c3cb64519a0c7033ba1`

隔离环境：

- 复用本地项目 Postgres 容器，只新建临时 database；
- 复用本地项目 Redis 容器，只用临时空 DB index；
- 不改现有 9022 服务；
- fake external pool 是本机临时 HTTP server。

结果：

```text
OLD_NORMALIZED ws_status 503
fake_hits 0
LOG_HAS detected native WebSearch True
LOG_HAS websearch_mcp_scheduler_unavailable True
LOG_HAS routing raw request directly to external pool before parsing body False
```

这证明旧代码在 normalized-only external pool 下不会 fallback，而是进入 MCP 并失败。

### 当前工作树验证

当前候选二进制 SHA:

```text
d456fac80a8742a56af8640ea21009a6d526f01bce552bb8ab9820f04949bd1b
```

同一隔离环境、同一请求：

```text
NEW_NORMALIZED ws_status 200
fake_hits 1
LOG_HAS detected native WebSearch True
LOG_HAS native WebSearch MCP skipped because local pool preflight routed request to external pool True
LOG_HAS websearch_mcp_scheduler_unavailable False
LOG_HAS routing raw request directly to external pool before parsing body False
```

## 已执行回归

命令均通过 `feature/tests/run-cargo-scoped.sh` 执行，结束后 target 自动清理。

```bash
KIRO_RS_TEST_POSTGRES_URL='postgres://kiro_rs:kiro_rs_dev_password@127.0.0.1:25432/kiro_rs' \
KIRO_RS_TEST_REDIS_URL='redis://127.0.0.1:26379/0' \
feature/tests/run-cargo-scoped.sh websearch-normalized-test -- \
  cargo test -q native_websearch_normalized_external_preflight_precedes_mcp_for_five_rounds -- --nocapture
```

结果：

```text
1 passed; finished in 1.16s
```

相关回归批次：

```text
cargo fmt --check
native_websearch_normalized_external_preflight_precedes_mcp_for_five_rounds
websearch_canonical_detection_and_current_long_history_query_are_exact_for_five_rounds
websearch_mcp_error_resource_and_recovery_matrix_is_fail_closed_for_five_rounds
handler_thinking_signature_retry_accepts_json_labeled_eventstream_success_for_five_rounds
thinking_signature_retry_success_is_lazy_same_credential_and_bounded_five_rounds
```

结果：全部通过。

2026-07-26 当前工作树补充回归：

```text
native_websearch_normalized_external_preflight_precedes_mcp_for_five_rounds ... ok
native_websearch_scheduler_failure_falls_back_to_external_after_mcp_path_for_five_rounds ... ok
websearch_mcp_error_resource_and_recovery_matrix_is_fail_closed_for_five_rounds ... ok
websearch 全量 29 tests ... ok
```

新增后置 fallback 测试覆盖：

- 外部池 `requestBodyMode=normalized`；
- 本地 credentials 为空；
- `localPoolPreflightEnabled=false`，强制走 WebSearch MCP acquire 失败路径；
- stream/non-stream 各 5 轮；
- 下游 HTTP 200；
- fake external hits = 10；
- fake MCP HTTP hits = 0；
- usage `routeKind=external_pool`、`localAttempted=true`、`fallbackReason=local_no_credentials`、`externalAttempts=1`。

Clippy baseline：

```text
rustup run 1.92.0 node scripts/ci/check-clippy-baseline.mjs
Clippy emitted 817 warnings; the checked-in baseline allows 849.
```

Artifact gate：

```text
node feature/tests/inventory-build-artifacts.mjs --gate
release-gate result=pass
targets=0 reservations=0 target_processes=0 blockers=0
```

## 真实 Claude CLI 协议验证

Claude CLI version:

```text
2.1.197 (Claude Code)
```

抓包事实：

- `--tools=WebSearch --allowedTools=WebSearch` 不会让 Claude CLI 发送 `web_search_20250305`；
- `--tools=default` 只发送 `Bash/Edit/Read`；
- WebSearch 原生 server tool 必须用 Anthropic Messages API body 直接验证。

当前候选二进制 + 临时 app + fake raw external pool 跑真实 Claude CLI 普通请求：

```text
CLI returncode 0
fake_hits 2
result: fake-cli-ok
final usage: input_tokens=12 output_tokens=4
```

fake external 捕获到 Claude CLI 真实 body：

```text
FAKE_HIT 1:
model=claude-sonnet-5
stream=true
tools=0
thinking={"type":"adaptive"}
output_config={"effort":"high"}

FAKE_HIT 2:
model=claude-sonnet-5
stream=true
tools=0
thinking={"type":"disabled"}
output_config={"effort":"high","format":{"type":"json_schema",...}}
```

这条验证说明当前候选可跑真实 Claude CLI 协议；但它不覆盖原生 WebSearch，因为当前 Claude CLI 不发该 tool。

## 性能与异常边界

修复只增加一次 typed WebSearch 分支内的 local pool state preflight：

- 仅在 `websearch::has_web_search_tool(&payload)` 为 true 时执行；
- 只在 external pools enabled 且存在 fallback context 时执行；
- 使用已有 `local_pool_route_state_fresh(model)` 和 external pool eligibility 逻辑；
- 本地池 Ready 时不外部转发，不增加 MCP 请求；
- 本地池不可调度时减少一次必失败的 MCP 调度路径，并避免下游 503。

该路径不会引入额外上游 Kiro 调用；相反，在 normalized external pool 可用时，它会跳过本来会失败的 MCP 调度。

## 后续生产验证

发布后应在 152.53.243.159 观察：

- `websearch_mcp_scheduler_unavailable` 是否下降；
- 对同类 `/ha/v1/messages` + `web_search_20250305` 请求，route 是否变为 `external_pool / external_fallback_preflight`；
- externalAttempts 是否为 1，mcpAttempts 是否为 0；
- raw-passthrough 外部池行为不应变化；
- 本地凭据池 Ready 时 WebSearch MCP 成功路径不应变化。
- 如果 preflight 被关闭或 race 未接住，后置 fallback 应仍能把 no-credentials/all-disabled/capacity/scheduler degraded 分类成准确 fallback reason。

## 残余风险

- 如果 external pool 不支持该模型、被禁用、满并发或自身失败，仍可能返回 external pool 错误；这不是本问题的回归。
- 如果生产请求不是原生 `web_search_20250305` 单工具，而是普通工具/MCP 工具，本修复不会改变其路径。
- Claude CLI 当前无法直接生成原生 WebSearch body；未来 CLI 若支持该 tool，需要把本测试扩展到 C2/C3 真实 CLI 场景。
