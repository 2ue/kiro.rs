# P22 连续 assistant 各带原生 reasoning 时本地转换前置拒绝（400）

Status: fallback-fixed-in-3a1306d (v0.0.177) / all-request pre-conversion shaping in working tree (2026-09-29, not committed)
Severity: High
Area: request
Discovered: 2026-09-28 usage 页面错误样本

## 问题与影响

usage 页面错误元数据（当前最新版本）：

```json
{
  "localBodyPrepareCategory": "unsupported_content",
  "localBodyPrepareDiagnostic": "consecutive assistant messages contain multiple native reasoning blocks and cannot be merged losslessly",
  "localBodyPrepareKind": "conversion_error",
  "observedCount": 2,
  "observedCountIsExact": false,
  "reason": "local_body_prepare",
  "sampled": true,
  "stage": "handler_preflight"
}
```

结论：这个错误**不是上游 Kiro 返回的，也不是 Anthropic 官方返回的**。kiro.rs 在把客户端的 Claude Code 协议请求转换成 Kiro `conversationState` 时，在本地 preflight 阶段主动拒绝了请求，请求根本没有发到 Kiro。

| 字段 | 含义 | 来源 |
| --- | --- | --- |
| `stage: handler_preflight` | 在 handler 调用上游之前 | `src/anthropic/handlers.rs:6114` |
| `reason: local_body_prepare` | `RequestRejectionReason::LocalBodyPrepare` | `src/anthropic/request_admission.rs:172` |
| `localBodyPrepareKind: conversion_error` | `LocalBodyPrepareError::conversion` | `src/anthropic/handlers/local_body_pipeline.rs:28-37` |
| `localBodyPrepareCategory: unsupported_content` | `ConversionError::UnsupportedContent` | `src/anthropic/handlers/local_body_pipeline.rs:51` 起 |
| `localBodyPrepareDiagnostic` | 转换器内部固定文案 | `src/anthropic/converter/history.rs:485` |
| `sampled` / `observedCountIsExact=false` | 拒绝记录是采样，不是精确计数 | `src/anthropic/usage.rs:608` |

用户可见现象：Claude Code 收到 `API Error: 400 consecutive assistant messages contain multiple native reasoning blocks ...`，当前 turn 停止。这是确定性错误：同一段历史每次都会被原样发送，所以重试和"继续"都会再次命中同样的 400，会话无法继续。

影响面：本地凭据的 Kiro 转换路径，且请求开启了 thinking。外部池路径不受影响，原因见下文。

## 官方协议对照

项目定位：kiro.rs 基于 Kiro 对外提供 Anthropic 协议（Claude Code 协议）接口，任何兼容这个协议的客户端都可以使用，例如 Claude Code CLI。客户端发出的是 Claude Code 协议请求。代理把请求从 Claude Code 协议转换成 Kiro 协议，调度到最终上游 Kiro。上游响应再从 Kiro 协议转换回 Claude Code 协议，返回给客户端。所以这是两种协议之间的双向转换，上游始终是 Kiro。Anthropic 官方 API 只作为协议规范参照，并不是上游。

thinking 与签名是这套双向转换的一部分：

- 响应方向（Kiro → Claude Code）：Kiro 原生 `reasoningContentEvent` 里的 text、signature 和 redactedContent，会转换成 Claude Code 协议的 `thinking` block（带 `signature_delta`）和 `redacted_thinking` block。
- 请求方向（Claude Code → Kiro）：客户端回放历史时，`thinking.signature` 和 `redacted_thinking.data` 会原样还原成 Kiro 的 `assistantResponseMessage.reasoningContent`。

签名始终是 Kiro 原生签名，代理只负责转换和透传。这保证了 Claude Code CLI 回放历史时 Kiro 能够校验通过，CLI 可以正常使用。

本错误就来自两侧协议的结构差异：

- Claude Code 协议侧（接口需要接受的请求形状）：连续的同角色消息合法，官方会把它们合并成一个 turn；一个 assistant turn 里可以有多个 thinking block。对于非最后一个 assistant turn，历史 thinking 可以省略。
- Kiro 协议侧（上游要求）：`AssistantMessage.reasoning_content` 是 `Option<ReasoningContent>`，一个 assistant history 项只能放一个 reasoning union 值，可以是 reasoningText 加 signature，也可以是 redactedContent。参见 `docs/analysis/official-claude-code-kiro-protocol-audit-20260928.md`「多块 thinking」一节。

所以，当客户端请求里的多条相邻 assistant 消息各带一个原生 reasoning 时，它们要合并成一个 Kiro history 项，而这个项只能放一个 reasoning 值。转换器为了避免把两个 reasoning 硬拼成一个导致上游校验失败，选择了 fail-closed，直接返回 400。

## 源码链与根因

1. 调用链：
   - `handlers.rs:6706` 调用 `local_body_pipeline::prepare`
   - 内部调用 `convert_request_with_resolved_model`
   - 失败时在 `local_body_pipeline.rs:165` 返回 `LocalBodyPrepareError::conversion`
   - `handlers.rs:6717` 记录采样拒绝
   - `handlers.rs:5691` 的 `conversion_error_response` 返回 400
2. `converter/history.rs:132-171` 把相邻的 assistant 消息累积起来，交给 `merge_assistant_messages_with_known_tools` 合并。`history.rs:481-489` 处理第二条带 reasoning 的消息时，`set_native_reasoning_content`（`history.rs:409-421`）报错，文案改写为本错误。
3. 能进入 `reasoning_content` 的只有两类：带非空 signature 的 `thinking`（`history.rs:293-299`），以及 `redacted_thinking`。不带 signature 的 thinking 会降级为文本，不会触发本错误。

**根因：本地路径的"丢弃历史 thinking"发生在转换之后。**

- `payloadShaping.discardHistoricalThinking` 默认为 `true`（`src/model/config.rs:2470`）。它的作用本来就是在发送前去掉旧 assistant 的 thinking，只保留当前工具续写那一条 assistant。受保护的 assistant 由 `payload_guard.rs:1709` 的 `active_anthropic_tool_turn_assistant_index` 确定。
- 外部池路径在转换前（Claude Code 协议请求层）先执行这一步（`payload_guard.rs:2080` 的 `apply_anthropic_payload_safety_shaping`，调用方为 `payload_guard_runtime.rs:66` 和 `external_pool/body_pipeline.rs:605`），所以不会出现这个问题。
- 本地路径只在 Kiro 层执行这一步（`payload_guard.rs:2173` 的 `apply_payload_shaping`），而且**只在请求超限的分支里执行**（`payload_guard.rs:581`：`size_limit_enabled && final_weight > max_weight`）。每次都会执行的 `apply_payload_safety_shaping`（`payload_guard.rs:2063`）只负责丢弃超限的历史图片。所以默认 `on_too_long` 首发时，本地路径根本不会丢弃历史 thinking，所有历史签名 reasoning 都会进入转换器。转换器在对这些 reasoning 做无损合并校验时直接失败。

> 2026-09-29 更正：初版文档写的是"丢弃发生在转换之后"，这个说法不准确。实际情况是本地首发完全不丢弃，和外部池路径的无条件丢弃不一致，`discardHistoricalThinking=true` 在本地首发时并没有生效。

触发条件：最后一条 user 之前存在两条或更多相邻的 assistant 消息（中间没有 user），且其中至少两条各带一个签名 thinking 或 redacted_thinking。相邻 assistant 在 Claude Code 中是如何产生的（例如流中断后重试），目前没有抓包确认。这不影响修复，因为修复针对的是转换顺序。

## 复现

### 最小复现（单测）

- 转换器层：已有单测 `consecutive_assistant_native_reasoning_cannot_be_merged_lossily`（`src/anthropic/converter.rs:5760`）。2026-09-28 在当前工作树执行 `cargo test --bin kiro-rs consecutive_assistant_native_reasoning`，结果 1 passed，确认转换器遇到这种形状一定报错。这个测试描述的是转换器自身的边界，修复后保持不变。
- handler 层：当前工作树的 `src/anthropic/handlers/tests.rs` 已有 `local_preconversion_shaping_handles_consecutive_assistant_reasoning_for_five_rounds`。它发送 `user → assistant(thinking sig-1 + text) → assistant(thinking sig-2 + text) → user`，断言 200、上游 body 不含 `reasoningContent`、两段可见回答都保留、usage 记录为 Success，共 5 轮。

### 端到端复现

```bash
curl -sS http://127.0.0.1:19023/cc/v1/messages \
  -H 'content-type: application/json' -H 'x-api-key: <key>' \
  -H 'anthropic-version: 2023-06-01' \
  -d '{"model":"claude-sonnet-4-5","max_tokens":512,
       "thinking":{"type":"enabled","budget_tokens":1024},
       "messages":[
         {"role":"user","content":"q1"},
         {"role":"assistant","content":[{"type":"thinking","thinking":"t1","signature":"sig-1"},{"type":"text","text":"a1"}]},
         {"role":"assistant","content":[{"type":"thinking","thinking":"t2","signature":"sig-2"},{"type":"text","text":"a2"}]},
         {"role":"user","content":"q2"}]}'
```

- 修复前：HTTP 400，文案与本错误一致；usage 页面出现 `stage=handler_preflight`、`reason=local_body_prepare` 的记录；上游没有调用。
- 修复后：HTTP 200；上游 history 中两条 assistant 被合并成一项，旧 reasoning 被丢弃。

## 修复方案

### 候选方案

- A1：无条件前置 shaping。每个本地请求在转换前，都先在 Claude Code 协议请求层执行历史 thinking 丢弃。
  - 缺点是范围过大：单个历史签名 thinking 本来可以原样发给 Kiro，现在会被提前删除。这会破坏既有的"Kiro 签名校验失败后同账号 stripped-retry"路径。
  - 工作树里最早的版本用的就是这个方案，已被替换。
- A2（推荐，已在工作树实现）：失败触发的 fallback。
  - 先用原始 payload 转换。只有当转换失败，且错误属于已知的历史 reasoning 结构冲突时，才对副本执行一次历史 thinking 丢弃，然后重新转换。
  - 正常请求的行为和转换结果与修复前完全一致。
- B：修改转换器的合并规则，冲突时保留一个 reasoning。这会改变转换器的无损语义，范围更大，不在本问题中处理，见 [P23](23-native-reasoning-multi-block-conversion-gaps.md)。

### 推荐方案

> 2026-09-29 方案升级：v0.0.177 已发布下面的 A2 fallback 方案。复核后，改为**所有请求在转换前处理历史 thinking**（第 3 版），并保留 A2 作为配置关闭时的兜底。详细分析、取舍和验收见 [00 修复计划：2026-09-29 方案复核](00-current-protocol-fix-plan.md)。下面的 A2 描述保留，作为 v0.0.177 的记录。

采用 A2，只改 `src/anthropic/handlers/local_body_pipeline.rs` 的 `prepare_with_plan`。以下内容按工作树当前 diff 核对，由并行会话实现，尚未提交：

1. `should_retry_local_conversion_after_reasoning_shaping(error)` 只匹配两条 `UnsupportedContent` 文案：
   - 本错误 `consecutive assistant messages contain multiple native reasoning blocks and cannot be merged losslessly`
   - `assistant history contains multiple or mixed native reasoning blocks; ...`
2. 命中后，`local_reasoning_fallback_shaping_config` 强制设置 `enabled=true`、`discard_historical_thinking=true`。这是代码层面的协议兼容兜底，不依赖运行配置。即使运营方关闭了 payload shaping，这类结构冲突也不会退化成本地 400。
3. 对 payload 副本调用 `sanitize_anthropic_messages_for_external_forwarding`，即 `apply_anthropic_payload_safety_shaping`。当前工具续写的 assistant 由 `active_anthropic_tool_turn_assistant_index` 保护，它的签名 thinking 会原样保留。然后重新调用 `convert_request_with_resolved_model`。
4. 重转仍然失败，或者 shaping 没有产生任何修改时，返回原来的转换错误，诊断文案不变。

效果：

- 本错误对应的请求形状（相邻的旧 assistant 各带一个 reasoning）可以到达 Kiro，上游 body 中不包含旧的 thinking 和签名。
- 正常请求、单个历史签名 thinking，以及签名失败后的 stripped-retry 路径都不受影响。
- 转换器、Kiro 层 shaping、签名转换与透传逻辑都没有改动。

不在本问题范围内：受保护的工具续写 assistant 内部仍有多个 reasoning 的情况、签名与无签名混用、流式多段 reasoning 的签名输出，这些都记录在 [P23](23-native-reasoning-multi-block-conversion-gaps.md)。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)，只使用 `127.0.0.1:19023` 指定测试实例和隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，Cargo 通过 `feature/tests/run-cargo-scoped.sh` 运行，不得影响本机正在运行的 Claude Code CLI 与服务。

1. 单测（工作树已有，根据并行会话进度记录已通过，本会话未独立复跑）：
   - `local_preconversion_shaping_handles_consecutive_assistant_reasoning_for_five_rounds`：本错误形状返回 200，上游 body 不含旧的私有 thinking 和签名，两段可见回答都保留。
   - `local_reasoning_fallback_ignores_payload_shaping_disabled_for_five_rounds`：`payloadShaping.enabled=false` 且 `discardHistoricalThinking=false` 时，仍然不会变成本地 400。
   - `handler_thinking_signature_retry_accepts_json_labeled_eventstream_success_for_five_rounds`：单个历史签名 thinking 仍原样发给 Kiro；Kiro 返回签名错误后，同账号 stripped retry 成功。
2. 边界回归：`consecutive_assistant_native_reasoning_cannot_be_merged_lossily`、`multiple_or_mixed_native_reasoning_blocks_are_rejected_for_five_rounds`、`one_native_reasoning_survives_consecutive_assistant_merge`、`anthropic_guard_discards_historical_thinking_even_when_body_fits` 全部通过。转换器自身的边界不变；本会话在 2026-09-28 复跑了第一个，结果通过。
3. 端到端：
   - 用上面的 curl 请求返回 200。
   - 真实 Claude Code CLI 使用隔离的 `HOME` 和 `CLAUDE_CONFIG_DIR`，开启 thinking，完成一段带工具调用的多轮会话，工具续写和签名回放都正常，没有 400。
4. 生产观察：升级后，usage 页面中该 `localBodyPrepareDiagnostic` 的采样记录归零，并且没有新增上游 thinking 或 signature 类的 400。

## 兼容性与风险

- 行为变化只发生在"原本会返回本地 400"的请求上。其他请求不做额外处理，转换结果与修复前逐字节一致。
- fallback 的 shaping 同时会处理超限的历史图片（`drop_oversized_anthropic_history_images`）。这一步只在 fallback 分支执行，Kiro 层本来就有等价处理。
- fallback 强制丢弃历史 thinking，不受配置影响。这是有意设计的：旧 turn 的 thinking 在 Claude Code 协议语义里本来就可以省略，而 Kiro 的单 union 结构又无法承载多个 reasoning。
- 残余边界：如果冲突发生在受保护的当前工具续写 assistant 内部，fallback 之后仍然会失败，并返回原诊断。见 [P23](23-native-reasoning-multi-block-conversion-gaps.md)。
- 性能：只有命中错误的请求会多一次 clone、shaping 和转换。
- 回滚：撤销 `local_body_pipeline.rs` 中 fallback 分支的改动即可。
