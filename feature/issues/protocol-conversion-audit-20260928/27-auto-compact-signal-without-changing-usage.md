# P27 Claude Code 不触发自动压缩（usage 按路由整形后客户端看不到真实上下文）

Status: fixed-in-working-tree (context compaction signal; not released)
Severity: High
Area: response + scheduling
Discovered: 2026-09-29
Verified-against: 2026-09-29 工作树（`context_compact_signal`）

## 问题与影响

Claude Code 依据响应 usage（`input_tokens + cache_read_input_tokens + cache_creation_input_tokens`）判断何时压缩。按路由挂载的 usage 整形策略会改写这些字段，它们是计费字段，必须保持不变，因此客户端可能看不到真实的上下文占用。上下文真正超出 Kiro 上限时，旧版本返回的报错文案也不是 `prompt is too long`，客户端的被动压缩同样不会触发，会话就此卡死。

真实复现（2026-09-29，`127.0.0.1:19023`，旧二进制，真实 Claude Code CLI 2.1.283，隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，`claude-sonnet-4-5`，200k 窗口）：

- 每轮追加约 150 KB。第 1 轮 200，真实上下文 119,034 token。
- 第 2 轮起每一轮都失败：`API Error: 400 Request input content length exceeded the request threshold...`。CLI 没有任何 compact 事件，会话之后的每一轮都失败。

## 官方协议对照

- Claude Code 协议：上下文超限时返回 400 `invalid_request_error`，message 为 `prompt is too long: N tokens > M maximum`。客户端收到后会自动压缩再重试。已用本地 fake 服务端和真实 CLI 2.1.283 确认，会出现 `compact_boundary`，`trigger=auto`。
- Kiro 协议：`contextUsageEvent.contextUsagePercentage` 和 metadata token 用量反映的是上游真实计算的上下文，与下游整形后上报的 usage 相互独立。

## 源码链与根因

- 上报给客户端的 usage 由路由策略整形，见 `reported_usage` 和 prompt cache 模拟，本次不改动。
- 真实上下文记录在 usage 记录的 `rawUsage` 中，来源依次为 metadata、contextUsagePercentage，都缺失时才用本地估算。
- 根因有两点：客户端能用来判断压缩的只有 usage；旧版超限报错不是协议格式（P01）。

## 复现

### 最小复现（单测）

`src/anthropic/context_compact_signal.rs`：

- `signals_only_after_real_context_crosses_the_threshold_for_five_rounds`
- `completed_compaction_request_clears_the_conversation`
- `compaction_and_single_message_requests_are_never_signalled`

### 端到端复现

脚本 `tmp/thinking-budget-local/fix-evidence-20260929/compact/grow.sh`，用真实 CLI 按轮追加内容：`TURNS=6 CHUNK=150000` 复现被动超限；`TURNS=9 CHUNK=40000` 复现逐步增长。

## 修复方案

### 候选方案

- A. 修改上报的 usage，让客户端自己算出需要压缩。这会破坏计费字段，已否决。
- B.（采用）usage 字段保持原样，额外通过协议错误给客户端发信号：
  1. 被动信号（P01，`439e5ce`）：上游超限时返回协议格式的 `prompt is too long`。
  2. 主动信号（本次）：每轮成功后，按会话记录 Kiro 返回的真实上下文，取值为 raw 的 `total_input_tokens + output_tokens`。下一次请求如果该值已达到模型窗口的 `triggerRatio`（默认 0.8），直接返回 400 `prompt is too long: <真实 token> tokens > <阈值> maximum`，并带上 `x-kiro-too-long-kind: context_compact_signal` 头，不发往上游。
  3. 以下请求永远放行，避免压缩死循环：Claude Code 的压缩请求（最后一条 user 包含 `create a detailed summary of the conversation`），以及只有一条消息的请求。压缩请求完成后清除该会话的记录。

### 推荐方案

采用 B，配置为 `contextCompactSignal.enabled`（默认 true）和 `contextCompactSignal.triggerRatio`（默认 0.8）。默认取 0.8 而不是 0.9，是因为实测 Kiro 在名义窗口之前就会拒绝：sonnet-4.5 在 169k 时接受、191k 时拒绝。判断依据只用上游真实值，不用整形后的 usage，也不用本地估算。只有上游没给 metadata 和 contextUsage 时，raw 才会退回本地估算。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)。

- 单测：模块 3 个用例，全量 `cargo test --bin kiro-rs` 为 2231 passed / 0 failed。
- 真实 CLI（sonnet-4.5，200k 窗口），新二进制：
  - 被动路径（每轮 150 KB）：第 2 至 4 轮都先收到 `prompt is too long`，CLI 自动压缩（`trigger=auto`，例如 pre 158,749 降到 post 38,947），然后返回 `turn-N-ok`，4 轮全部成功。
  - 主动路径（每轮 40 KB）：第 6 轮真实上下文为 168,968 token，超过阈值 160,000。第 7 轮在本地收到信号，usage 记录为 `reason=context_compact_signal observedContextTokens=169097 signalThresholdTokens=160000`，没有打到上游。CLI 自动压缩（pre 261,587 降到 post 11,865）后，第 7 至 9 轮继续成功，全程没有发生上游超限。
  - usage 字段保持原样：各轮上报的 `cache_read_input_tokens` 仍然是整形后的值（如 219,583、251,340），信号判断用的是真实值（147k、169k）。

## 兼容性与风险

- 记录只保存在进程内存中，最多 2 万个会话，TTL 6 小时。多实例部署时，只有处理过上一轮请求的实例才会发出主动信号，其余情况由被动信号兜底。
- 会话标识使用 Kiro `conversationId`，Claude Code 会从 `metadata.user_id` 中的 session 派生，同一会话保持稳定。
- 误触发的代价只是提前压缩一次；`triggerRatio` 可以调整，也可以直接关闭。
- 压缩请求本身接近上限时，仍然走现有的 payload guard 和 too-long 处理。
