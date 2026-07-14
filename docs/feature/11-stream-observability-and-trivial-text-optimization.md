# 流式观测与 trivial 文本块优化实施计划

> 本文记录 2026-07-14 本轮实施范围。目标是把近期 Claude Code CLI 会话分析得到的问题拆开处理，
> 避免把 `end_turn` 完成性盲区、语言串台、孤立 `.`、调度 400 混成一个高风险大改。

## 1. 已实现能力核对

以下能力当前代码已经具备，本轮不重复实现：

- 已解析 `assistantResponseEvent.messageStatus`。
- usage `latencyTrace` 已记录：
  - `upstreamMessageStatus`
  - `sawUpstreamCompleted`
  - `stopReasonSource`
  - `suspectedIntentPreambleEndTurn`
- 首次输出前的上游诊断已记录：
  - `upstreamEventTypesBeforeFirstOutput`
  - `upstreamFramesBeforeFirstOutput`
  - `upstreamEventsBeforeFirstOutput`
  - parse/decode 错误计数等。
- 首个下游事件前的流式换号重试已经有开关和边界：
  - `kiroUpstreamStreamRetryEnabled`
  - `kiroUpstreamStreamRetryMaxAttempts`
  - idle/read/status 三类子开关。

## 2. 本轮实施项

### 2.1 EOF 前上游事件摘要

问题：

- 真实 `/cc`/Claude Code CLI 验证显示，成功流经常是 `sawUpstreamCompleted=false`。
- 如果 Kiro 当前协议本身经常不发送 `messageStatus=COMPLETED`，单靠该字段无法判定是否静默截断。

实现：

- 在 `StreamContext` 维护一个小的上游事件 ring buffer，仅保存最近事件类型，不保存正文和 payload。
- usage `latencyTrace` 追加：
  - `upstreamEofWithoutCompleted`
  - `lastUpstreamEventType`
  - `lastUpstreamEvents`
  - `sawUpstreamAssistantResponse`
  - `sawUpstreamToolUse`
  - `sawUpstreamMetadata`
  - `lastAssistantContentChars`

风险控制：

- 只记录短字符串和计数，内存固定上限。
- 不改变下游 SSE。
- 不改变 success/error 判定。

### 2.2 intent preamble 风险等级

问题：

- `suspectedIntentPreambleEndTurn=true` 只有一个 bool，不便于后续区分“只是短回答”与“明显说要执行但 end_turn”。

实现：

- 保留原 bool 字段兼容旧 UI。
- 追加 `intentPreambleRisk`，取值：
  - `none`
  - `low`
  - `medium`
  - `high`
- 风险等级只对新增字段做更细分诊断：
  - 短回答但没有“我会/我先/I will/let me + 检查/读取/修改/执行”等行动意图时，只记为 `low`。
  - 明显像执行前说明、但最后仍是本地推断 `end_turn`，才记为 `medium/high`。

本轮只做轻量规则，不引入正则大库或复杂 NLP。

### 2.3 tool_use 前 trivial 文本块过滤

问题：

- 最近 Claude Code CLI 会话显示，孤立 `.` 或空白文本块多发生在 `stop_reason=tool_use` 且后面紧跟工具调用。
- 当前 `process_assistant_response` 只过滤完全空字符串，`"."`、`" "`、`"\n"` 都会原样转成 text block。

直接按 delta 丢弃风险很高，因为正常文本可能被拆成：

```text
"3"
"."
"14"
```

实现：

- 对“工具可用、尚未输出可见正文、收到的 assistant 片段是空白或单个标点”的情况，先缓冲，不立即下发。
- 如果下一事件是 `toolUseEvent`，丢弃该缓冲 trivial 文本并计数。
- 如果下一事件是普通 assistant 文本或流结束，则先把缓冲内容刷出，避免误吞合法单字符回答。
- usage `latencyTrace` 追加：
  - `filteredTrivialTextBlocks`
  - `filteredTrivialTextChars`

风险控制：

- 只延迟极短 trivial 片段。
- 不按任意 chunk 过滤正常文本。
- end_turn 场景会在 final 前刷出，不吞最终回答。

## 3. 本轮不实施但保留待办

### 3.1 语言串台策略

日文/英文/葡语串台是模型输出内容，不是代理协议错误。本轮不做硬过滤。

后续如果要做，只建议做可配置 system 软约束：

```text
除非用户明确要求其他语言，否则所有面向用户的自然语言解释必须使用简体中文。
代码、命令、路径、日志、错误原文、协议字段不翻译。
```

不建议代理层自动删除或改写外语输出，避免误伤代码、日志、翻译任务和多语言测试。

### 3.2 模型不可用 400 的调度能力识别

真实 Claude CLI 工具场景曾复现后续轮 400：

```text
Invalid model ID. Please select a different model to continue.
```

这属于账号模型能力 / sticky 调度问题。本轮不混改调度器，后续单独处理：

- 记录 `credential -> unsupported_models`。
- sticky 绑定改为 capability-aware。
- 仅对 `INVALID_MODEL_ID` / model unavailable 做能力避让，不把 request body invalid 当可换号重试。

### 3.3 thinking-only 空格 fallback 配置化

当前 `stream.rs` 在 thinking-only 且无 text/tool_use 时会补一个 `" "` text block。

本轮先不改该兼容行为，只保留为独立待办。原因是该逻辑可能服务于 Claude Code/Anthropic content
数组兼容，改动需要 thinking 真实 CLI 回归。

## 4. 验证要求

本轮完成后必须执行：

- `cargo fmt --check`
- `git diff --check`
- 相关 Rust 单测：
  - `message_status`
  - `intent_preamble`
  - trivial text filter 新增测试
  - stream success usage/stop reason 测试
- `cargo build --release`
- 临时端口真实服务验证，不触碰 `9022`：
  - C1 直接 `/cc/v1/messages` stream。
  - C2 真实 `claude --print --output-format=stream-json` 简单回答。
  - C2 真实 `claude` 工具写文件场景。
  - 查询 usage detail，确认新增 `latencyTrace` 字段落库。
