# 凭据卡片 `server: mcp_completion upstream_error` 来源追踪

Status: `fixed-in-current-tree / focused regression passed / production recurrence pending`

Severity: P1/P2。该错误显示在凭据卡片“最近错误”区域，可能误导为账号不可用或正式推理失败；但它不一定对应 usage_records 中的某条用户请求。

Last verified: 2026-07-26

## 用户可见现象

前端凭据卡片显示类似：

```text
最近错误 (约5分钟前)：server: mcp_completion upstream_error
```

用户在 159 上观察到企业账号也有较多这类报错。

## 源码链

前端位置：

- [ui/src/features/credentials/credential-card.tsx](/Users/yuanfeijie/Desktop/procode/kiro.rs/ui/src/features/credentials/credential-card.tsx)
  - 卡片读取 `lastErrorKind` / `lastErrorReason` 并拼成最近错误。

Admin API：

- [ui/src/api/credentials.ts](/Users/yuanfeijie/Desktop/procode/kiro.rs/ui/src/api/credentials.ts)
  - `GET /api/admin/credentials/runtime`
- [src/admin/handlers.rs](/Users/yuanfeijie/Desktop/procode/kiro.rs/src/admin/handlers.rs)
  - `get_credentials_runtime`
- [src/admin/service.rs](/Users/yuanfeijie/Desktop/procode/kiro.rs/src/admin/service.rs)
  - 从 token manager runtime snapshot 映射 `last_error_kind` / `last_error_reason`。

旧写入来源：

- [src/kiro/provider.rs](/Users/yuanfeijie/Desktop/procode/kiro.rs/src/kiro/provider.rs)
  - MCP completion failure 曾调用 `report_transient_failure_kind(...)`，reason 格式：

```rust
format!("mcp_completion {}", kind.scheduler_reason())
```

当 `kind=McpCallFailureKind::Upstream` 时，卡片可显示：

```text
server: mcp_completion upstream_error
```

当前工作树已移除该写入路径。MCP/WebSearch 属于辅助路径，普通 MCP completion failure 现在只释放 in-flight lease 并记录请求内 attribution，不再把 `mcp_completion upstream_error` 写进主模型凭据 runtime health。明确认证错误、明确风控、明确额度耗尽仍按真实凭据状态处理。

## 本轮生产验证与证据

159 只读证据：

```text
usage_records data/text search for mcp_completion: 0
credential_events search for mcp_completion: 0
credential_runtime_state has no last_error_kind/last_error_reason columns
Admin /api/admin/credentials/runtime at取证时刻:
  total=0
  errors=[]
Redis scheduler health前 20000 个 key value search:
  mcp_completion/upstream_error: 0
targeted app logs last 2h grep:
  mcp_completion/websearch/thinking/external coordinator: 0
```

因此，本轮没有在当前 159 持久数据中找到与卡片完全一致的证据。可能原因：

- 用户看到的是另一台机器、另一个刷新时刻或浏览器缓存；
- 卡片来自进程内 token manager snapshot，取证时本地凭据已全部删除/禁用，runtime API 返回 0；
- Redis key TTL 已过或只扫描到当前 health key 的一部分；
- 该错误对应的是 MCP completion 辅助调用，不写 usage_records，也不写 credential_events。

## 当前判断

`server: mcp_completion upstream_error` 不是 WebSearch usage 错误的同义词。它表示旧版本中模型能力/补全/辅助 MCP completion 调用的上游错误被写到了凭据 runtime health 中。

这类卡片错误应作为“凭据 runtime health 最近错误”看待，不能直接等价为：

- 本次用户请求失败；
- 凭据被持久禁用；
- Kiro token 刷新失败；
- usage 计费异常。

## 复现方案

旧版本可用 fake MCP completion upstream 复现：

1. 构造一个 credential runtime；
2. 让 `call_mcp...` 或相关 completion 辅助路径返回 `McpCallFailureKind::Upstream`；
3. 确认 token manager snapshot 中：

```text
last_error_kind=server
last_error_reason=mcp_completion upstream_error
```

4. 查询 `/api/admin/credentials/runtime`，前端卡片应显示同样文案；
5. 确认 usage_records 不一定存在对应行。

当前工作树复现/验证：

1. 对所有 `McpCallFailureKind` 调用 `McpCallCompletion::report_failure(...)`；
2. 断言 lease 释放、attempt attribution 保留；
3. 断言 token manager snapshot 中 `cooled_down=false`、`last_error_kind=None`、`last_error_reason=None`；
4. 对真实 `call_mcp` 错误路径再跑 5 轮，覆盖 MCP 500、body too large、chunked over-limit、429/timeout 类换号重试，断言同样不污染主模型凭据 runtime health。

## 建议优化

1. 前端卡片把 runtime health 错误标注为“调度健康最近错误”，不要让用户误解为“最近请求 usage 错误”。
2. Admin runtime API 增加 `lastErrorSource`，例如：
   - `inference`
   - `mcp_completion`
   - `profile_discovery`
   - `token_refresh`
   - `scheduler`
3. MCP completion failure 写入轻量事件或 usage auxiliary diagnostics：
   - credential id；
   - model；
   - failure kind；
   - reason；
   - 不保存 prompt/body。
4. 为 Redis scheduler health 提供只读 admin debug endpoint，按 credential id 精确读，不需要生产手工 SCAN。

## 2026-07-26 当前修复

代码变更：

- [src/kiro/provider.rs](/Users/yuanfeijie/Desktop/procode/kiro.rs/src/kiro/provider.rs)
  - 移除 `McpCallFailureKind::transient_failure_kind()` 对主调度 cooldown 的映射。
  - `McpCallCompletion::report_failure(...)` 不再调用 `token_manager.report_transient_failure_kind(...)`。
  - `call_mcp_with_retry(...)` 的普通发送失败、非 2xx body 读取失败、408/429/5xx、协议兜底错误不再写全局 credential cooldown；只在本次请求内排除当前 credential 并按显式分类换号重试。
  - 保留 401/403、明确风控、明确 quota exhausted 等真实凭据状态更新。

已执行测试：

```text
mcp_completion_failure_types_release_without_poisoning_core_credentials_for_five_rounds ... ok
mcp_real_sends_share_request_budget_for_1_20_60_accounts_over_five_rounds ... ok
mcp_error_response_body_is_bounded_while_reading_for_five_rounds ... ok
websearch_mcp_error_resource_and_recovery_matrix_is_fail_closed_for_five_rounds ... ok
```

验收结论：

- MCP 辅助失败不会再把健康的模型凭据写成 `server: mcp_completion upstream_error`。
- MCP 真实发送数仍被 shared inference/auxiliary attempt budget 限制；1/20/60 账号池均不突破请求预算。
- 普通 MCP 协议/上游错误仍 fail-closed，不假成功、不泄漏 raw query/result/body。

## 残余风险

- 当前没有 exact 生产样本证明 159 用户看到的卡片就是本轮扫描机器/时刻产生；但旧源码写入路径已经确认存在并已移除。
- 发布后仍需要观察 `/api/admin/credentials/runtime` 是否再出现 `mcp_completion` 来源；若出现，应采集对应版本和 runtime snapshot。
