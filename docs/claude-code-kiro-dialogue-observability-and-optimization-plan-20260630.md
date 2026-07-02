# Claude Code + Kiro 对话断裂问题可观测性与优化方案

日期：2026-06-30

适用范围：当前 `kiro.rs` 对 Claude Code CLI 的兼容链路、Claude Code 本地 transcript 分析、后续排查“用户最新输入未被理会、thinking 不展示、长等待后一坨输出”的优化工作。

本文目的：把前面对当前会话的事实分析整理成一份可实施的改进文档。后续实现者可以按本文补日志、补 usage 字段、补本地 transcript 分析工具、补验证用例，而不需要重新阅读完整对话。

---

## 0. 结论先行

当前问题不是单点 bug，而是三类体验叠加：

1. **最新用户输入没有被处理**。
   - 典型例子：用户输入“要记录目的，以及实现后如何配置（各项参数如何配置）”后，assistant 继续旧任务，没有按新增要求修改文档。
   - 本地 transcript 显示这条输入是 `queue-operation`，不是正常 `user` message。这意味着它大概率没有进入后续模型请求。

2. **thinking 展示预期不一致**。
   - 当前代码可以识别 `ultrathink` 并注入 thinking 意图。
   - 但这不保证上游一定返回可见 `thinking_delta`，也不保证 Claude Code CLI 一定展示 thinking 正文。

3. **长等待期间缺少用户可见反馈**。
   - 某些轮次大量使用 Agent/tool，等待数分钟后才集中输出。
   - Kiro 底层可以发 ping 或流式事件，但这些不一定会变成 CLI 里用户能理解的进度提示。

后续优化的核心不是“让模型一定更聪明”，而是先把链路变透明：

- 每轮请求到底有没有收到最新用户话。
- Kiro 有没有把最后用户消息转丢或变形。
- thinking 是否被请求、是否返回、是否转成下游事件。
- 长等待具体卡在调度、上游首包、thinking、可见文本、工具结果、下游 flush 的哪一步。
- 用户中途输入是否被 Claude Code CLI 记录成 queue 但没有消费。

---

## 1. 已确认的本地事实

### 1.1 当前 session transcript

当前会话 transcript：

```text
/Users/yuanfeijie/.claude/projects/-Users-yuanfeijie-Desktop-procode-kiro-rs/77cfa36b-9ed8-4165-a416-4e61015605dd.jsonl
```

关键事实：

- session 共 409 行。
- 用户正常 turn 可以在 transcript 中看到 `type=user` 和 `message.role=user`。
- 中途追加的“要记录目的，以及实现后如何配置（各项参数如何配置）”出现在 transcript line 390，但类型是 `queue-operation`，不是正常 user message。
- 全局历史也记录了这条输入：

```text
/Users/yuanfeijie/.claude/history.jsonl:4404
```

### 1.2 queue 输入未被消费的具体表现

相关时间线：

| transcript line | 时间 | 类型 | 内容摘要 |
| --- | --- | --- | --- |
| 390 | 08:19:34 | `queue-operation` | 要记录目的，以及实现后如何配置（各项参数如何配置） |
| 391 | 08:19:16 | assistant text | 继续写 implementation/config/verification |
| 392 | 08:21:14 | assistant tool_use | 继续执行旧的 `Edit` |
| 399 | 08:21:33 | assistant text | 检查文档占位符 |
| 402 | 08:21:44 | assistant text | 读回全文核对 |
| 408 | 08:22:05 | assistant final | 总结文档已写入，但没有回应新增要求 |

判断：

- 这不是“assistant 表达不清楚”，而是新增用户要求没有被处理。
- 由于该输入不是正常 user message，不能直接归因到 Kiro converter 丢失用户文本。
- 更可能是 Claude Code CLI 在 assistant 仍运行时把用户输入记录为队列操作，但没有把它变成后续模型请求。

### 1.3 thinking 的事实边界

当前 session 中出现过 assistant `thinking` block：

- line 37
- line 229
- line 361

但 `ultrathink` 那一轮：

- 用户输入：line 94
- assistant 后续：line 96 到 line 137
- 该区间没有 thinking content block

判断：

- 链路不是完全不支持 thinking。
- 但“用户文本里有 ultrathink”不等于“本轮一定有可见 thinking block”。

### 1.4 当前 Kiro 代码的相关事实

当前工作区代码中，`/cc/v1/messages` 会做以下处理：

- `override_thinking_from_model_name(&mut payload)`：模型名带 `-thinking` 时注入 thinking 配置。
- `apply_thinking_trigger_mode(&mut payload, &runtime_config)`：根据配置和最新自然用户文本决定是否注入 thinking。
- `user_text_has_claude_code_visible_thinking_signal()` 会匹配 `ultrathink`。
- `convert_request_with_resolved_model()` 将 Anthropic 请求转换为 Kiro 请求。
- converter 会把最后一条 user message 当成 current message，并保留 user text。

关键代码位置：

```text
src/anthropic/handlers.rs
  override_thinking_from_model_name
  apply_thinking_trigger_mode
  latest_natural_user_text
  user_text_has_claude_code_visible_thinking_signal
  should_force_visible_thinking

src/anthropic/converter.rs
  convert_request_with_model_id
  process_message_content
  validate_tool_pairing

src/anthropic/stream.rs
  process_assistant_response
  process_reasoning_content
  process_content_with_thinking

src/external_pool.rs
  ExternalLatencyTraceState
  external stream first output detection
```

注意：这些是当前工作区代码事实。若要证明现网当时完全一致，需要结合现网部署版本或 request_id 级日志。

---

## 2. 问题拆分与可优化性

### 2.1 最新用户输入没有被处理

问题表现：

- 用户在 assistant 工作中途追加要求。
- CLI 界面显示了用户输入。
- assistant 继续旧任务，没有把新增要求纳入后续动作。

当前证据：

- transcript 中该输入是 `queue-operation`，不是正常 user turn。
- 后续没有 assistant 文本回应这条新增要求。
- 后续没有专门编辑文档补“目的”和“各项参数如何配置”。

能否在 Kiro 中直接修：

- **不能直接修全部问题**。
- 如果 Claude Code CLI 根本没有把这条输入发给 Kiro，Kiro 不可能处理。

Kiro 能做的优化：

- 记录每次收到的请求摘要，证明“这轮 Kiro 到底有没有收到这条用户输入”。
- 记录转换后 current message 摘要，证明“Kiro 有没有转丢或变形”。

需要额外做的本地工具：

- transcript queue 检测脚本，专门发现“用户输入被记录为 queue-operation，但没有后续正常 user turn”的情况。

### 2.2 thinking 不展示

问题表现：

- 用户使用 `ultrathink` 或看到模型像在思考。
- CLI 里没有 thinking block 正文。

当前证据：

- 当前 session 曾有 thinking block。
- `ultrathink` 那轮没有 thinking block。
- 当前代码会识别 `ultrathink`，但可见 thinking 取决于上游是否返回 reasoning 或 `<thinking>` 内容。

能否优化：

- 可以补记录，清楚区分“没触发”“触发但没返回”“返回了但没转出”“转出了但 CLI 没展示”。
- 不建议为了“看起来有 thinking”伪造 thinking 内容。

### 2.3 长等待后集中输出

问题表现：

- 用户输入后等很久。
- 中间没有稳定的、用户能理解的进度。
- 最后一次性输出一大段。

当前证据：

- `ultrathink` UI 分析那轮派了 9 个 Agent。
- 最慢 Agent 结果数分钟后才返回。
- 这类等待不一定是 Kiro stream 卡住，也可能是 Claude Code 本地工具/Agent 执行时间长。

能否优化：

- Kiro 可以补 request latency trace，定位服务端和上游阶段。
- Kiro 不能直接改变 Claude Code CLI 对本地 Agent 进度的展示方式。
- 可以通过 transcript 分析脚本计算 Agent/tool 等待时间。

---

## 3. P0 优化项

### 3.1 请求入口安全摘要日志

目的：证明 Kiro 是否收到最新用户消息。

位置：

```text
src/anthropic/handlers.rs
  post_messages
  post_messages_cc
  post_messages_ha
  post_messages_na / related handlers if present
```

建议记录字段：

```json
{
  "event": "anthropic_request_summary",
  "request_id": "req_xxx",
  "endpoint": "/cc/v1/messages",
  "model": "claude-opus-4-8",
  "stream": true,
  "message_count": 42,
  "last_user_index": 41,
  "last_user_content_type": "string|array|object|empty",
  "last_user_text_len": 128,
  "last_user_text_hash": "sha256:...",
  "last_user_preview": "可选，默认最多 64 字，可配置关闭",
  "tool_result_count_in_last_user": 0,
  "assistant_tool_use_count_in_history": 9,
  "metadata_session_id_hash": "sha256:..."
}
```

安全要求：

- 默认不要记录完整 user text。
- preview 必须有限长，建议可配置关闭。
- 不记录 API key、Authorization、完整 tool_result、完整 request body。

价值：

- 如果用户说“我刚说的话没被理会”，可以用 hash/preview 证明 Kiro 是否收到。
- 如果 Kiro 没收到，问题转向 Claude Code CLI 队列或请求构造。

### 3.2 转换后 current message 摘要

目的：证明 Kiro converter 有没有转丢、改乱最后用户消息。

位置：

```text
src/anthropic/converter.rs
  convert_request_with_model_id
```

建议在 conversion result 中增加 diagnostic summary，或在 handler 转换成功后记录：

```json
{
  "event": "kiro_conversion_summary",
  "request_id": "req_xxx",
  "endpoint": "/cc/v1/messages",
  "conversation_id_hash": "sha256:...",
  "current_message_text_len": 128,
  "current_message_text_hash": "sha256:...",
  "current_tool_result_count": 2,
  "history_entries": 30,
  "tool_count": 120,
  "warnings": {
    "orphan_tool_results": 0,
    "orphan_tool_results_textified": 0,
    "duplicate_tool_results": 0,
    "duplicate_tool_results_textified": 0,
    "orphan_tool_uses": 0,
    "empty_content_placeholders": 0,
    "tool_result_content_placeholders": 0
  }
}
```

判断方式：

- 入口摘要有最新文本，转换后摘要没有：Kiro 转换问题。
- 入口和转换后都有：不是 Kiro 丢消息，转向模型遵循、上下文淹没或工具结果干扰。
- 入口没有：CLI 没发或请求没到 Kiro。

### 3.3 transcript queue-operation 分析脚本

目的：发现用户输入被 CLI 记录了，但没有进入正常 user turn。

建议新增脚本：

```text
scripts/analyze_claude_transcript_queue.js
```

输入：

```text
--transcript /Users/.../77cfa36b-...jsonl
--history /Users/yuanfeijie/.claude/history.jsonl
```

检测规则：

1. 找到 `type=queue-operation` 且 content 非空。
2. 判断后续是否出现相同文本或 hash 对应的正常 `type=user` + `message.role=user`。
3. 判断后续 assistant final 是否覆盖该文本的关键词。
4. 输出未消费队列输入。

输出示例：

```text
[UNCONSUMED_QUEUE_INPUT]
time=2026-06-30T08:19:34.540Z
line=390
text=要记录目的，以及实现后如何配置（各项参数如何配置）
next_assistant_action=Edit old document body
final_answer_covered=false
classification=queued_input_not_consumed
```

价值：

- 直接解释“我明明输入了，但它完全没理会”。
- 这个问题不需要现网日志，靠本地 transcript 就能定性。

### 3.4 thinking trace

目的：证明 thinking 是没请求、没返回、没转出，还是 CLI 没展示。

位置：

```text
src/anthropic/handlers.rs
src/anthropic/stream.rs
src/anthropic/usage.rs
```

建议字段：

```json
{
  "thinking_trace": {
    "requested": true,
    "trigger_source": "latest_user_ultrathink|model_suffix|explicit_enabled|explicit_adaptive|always|none",
    "thinking_type": "adaptive",
    "effort": "high",
    "force_visible_thinking": true,
    "extract_xml_thinking": true,
    "received_native_reasoning": false,
    "received_xml_thinking": false,
    "visible_thinking_emitted": false,
    "first_thinking_delta_ms": null,
    "thinking_tokens": 0
  }
}
```

实现要点：

- `apply_thinking_trigger_mode` 需要返回或写入 trigger source，而不是只改 payload。
- `process_reasoning_content` 看到 reasoning 时标记 `received_native_reasoning=true`。
- `process_content_with_thinking` 识别 `<thinking>` 或 `<think>` 时标记 `received_xml_thinking=true`。
- 发送 `thinking_delta` 时标记 `visible_thinking_emitted=true`。

判断方式：

- `requested=false`：未触发 thinking。
- `requested=true` 但 `visible_thinking_emitted=false`：上游没有给可见 thinking，或转换未识别。
- `visible_thinking_emitted=true` 但 CLI 没显示：偏 CLI 展示问题。

### 3.5 慢请求结构化诊断日志

目的：把“卡很久”拆成具体阶段。

当前已有一部分字段：

```text
payload_guard_ms
upstream_header_ms
first_upstream_chunk_ms
first_output_delta_ms
first_thinking_delta_ms
first_visible_text_delta_ms
stream_gap_to_first_output_ms
chunks_before_first_output
events_before_first_output
```

建议补齐：

```text
server_received_ms
dispatch_wait_ms
credential_acquire_ms
upstream_request_sent_ms
first_downstream_flush_ms
final_message_stop_ms
ping_count_before_visible_output
terminal_reason
```

慢请求触发条件：

```text
first_visible_text_delta_ms > 10s
or stream_gap_to_first_output_ms > 10s
or total_response_ms > 60s
or thinking_requested=true and first_thinking_delta_ms is null
or events_before_first_output > 20
```

输出示例：

```json
{
  "event": "slow_interaction_diagnostic",
  "request_id": "req_xxx",
  "endpoint": "/cc/v1/messages",
  "conversation_id_hash": "sha256:...",
  "last_user_text_hash": "sha256:...",
  "message_count": 42,
  "tool_result_count": 9,
  "thinking_requested": true,
  "thinking_emitted": false,
  "upstream_header_ms": 1200,
  "first_upstream_chunk_ms": 1500,
  "first_visible_text_delta_ms": 28700,
  "events_before_first_output": 14,
  "terminal_reason": "completed"
}
```

安全要求：

- 只记摘要和数字。
- 不记完整 body。

---

## 4. P1 优化项

### 4.1 外部池补 thinking 与可见输出时间

当前外部池 latency trace 中 `first_thinking_delta_ms` 为 `None`。这会导致外部池路径无法判断：

- 有没有 thinking。
- 是 thinking 先出来但用户看不到，还是根本没有 thinking。
- 是先 tool_use 还是先 text。

建议补字段：

```text
first_thinking_delta_ms
first_visible_text_delta_ms
first_tool_use_ms
events_before_first_visible_text
```

位置：

```text
src/external_pool.rs
  ExternalLatencyTraceState
  ExternalStreamUsageGuard
  external_sse_data_has_first_output
```

实现方式：

- 解析 external SSE data。
- 识别 `content_block_delta.delta.type=thinking_delta`。
- 识别 `text_delta` 且 text 非空。
- 识别 `content_block_start.content_block.type=tool_use`。

### 4.2 tool_result / Agent 结果摘要

目的：判断上下文是否被大量工具/Agent 结果淹没。

建议字段：

```json
{
  "tool_context_summary": {
    "current_tool_result_count": 9,
    "total_tool_result_chars": 180000,
    "largest_tool_result_chars": 64000,
    "tool_result_content_types": ["text", "text"],
    "orphan_tool_results": 0,
    "duplicate_tool_results": 0,
    "current_user_text_len": 80,
    "history_estimated_chars": 240000
  }
}
```

价值：

- 如果最新用户指令只有几十字，而前面堆了几十万字 tool_result，就可以解释模型为什么容易跑偏。
- 也能发现某些 tool_result 空内容、重复内容、对象内容不易读的问题。

### 4.3 CLI transcript 时间线分析脚本

建议新增：

```text
scripts/analyze_claude_transcript_timeline.js
```

功能：

- 列出每个 human prompt。
- 计算到第一条 assistant 文本的时间。
- 计算到最终 end_turn 的时间。
- 统计 tool_use 数量、Agent 数量。
- 计算每个 Agent/tool 的运行时间。
- 标出 long silent gap。
- 标出 queue-operation 未消费。
- 标出 thinking block 是否出现。

输出示例：

```text
line 94 user ultrathink
  first_assistant_text: 27.0s
  final_answer: 797.0s
  agent_count: 9
  slowest_agent: 372.2s
  thinking_block: no
  queue_input_during_turn: none
```

价值：

- 解释用户体感时不再靠主观判断。
- 可以直接生成一张“断感原因表”。

### 4.4 admin/usage 查询补充

如果 usage record 已存 latency_trace，可以在 admin 或 API 增加查询字段：

```text
first_visible_text_delta_ms
first_thinking_delta_ms
events_before_first_output
slow_interaction_reason
thinking_requested
thinking_emitted
last_user_text_hash
tool_result_count
```

这不是第一优先级，但长期会让线上问题排查更省时间。

---

## 5. 不建议做的优化

### 5.1 不要为了进度感伪造 assistant 文本

不要在模型输出里插入：

```text
正在处理...
请稍等...
我还在分析...
```

原因：

- 会污染 Claude Code 协议。
- 可能被 CLI 当作真实 assistant 内容。
- 可能影响模型后续上下文。
- 不能解决真实“用户输入未被消费”的问题。

正确做法：

- 记录真实阶段耗时。
- 在外部 UI 或本地诊断工具里展示进度。
- 不污染模型输出。

### 5.2 不要长期打开完整 request_body trace

当前代码存在完整 request body 的 trace 日志点。

这只适合本地临时验证，不适合生产长期打开。

风险：

- 泄露用户输入。
- 泄露工具结果。
- 泄露文件内容。
- 可能包含敏感上下文。

正确做法：

- 加脱敏摘要日志。
- 用 hash 和长度定位问题。
- 需要全文时只在本地隔离环境短期开启。

### 5.3 不要把 thinking 显示问题和用户输入未消费混在一起

这两类问题是独立的。

- thinking 不显示：模型可能仍然看到了用户问题。
- 用户输入未消费：即使 thinking 正常，也不会处理那条用户输入。

后续日志和文档都应分开记录。

---

## 6. 建议实现顺序

### 第一阶段：先把事实链打通

1. 新增请求入口安全摘要日志。
2. 新增转换后 current message 摘要。
3. 新增 transcript queue-operation 分析脚本。
4. 新增 thinking_trace。
5. 新增慢请求结构化诊断日志。

目标：

- 能回答“这条用户输入到底有没有进入 Kiro”。
- 能回答“thinking 到底有没有触发/返回/转出”。
- 能回答“长等待卡在哪一步”。

### 第二阶段：补齐外部池和工具结果分析

1. 外部池补 first thinking / first visible text。
2. 增加 tool_result / Agent 结果摘要。
3. 增加 transcript timeline 分析脚本。

目标：

- 能解释外部池路径的 thinking 和长等待。
- 能解释大工具结果是否淹没最新用户指令。
- 能自动复盘整个会话的断感来源。

### 第三阶段：做验证和查询能力

1. 增加 Claude Code CLI 交互队列复现验证。
2. 增加 direct SSE / stream-json thinking 验证。
3. 增加慢首字/慢可见文本 fake upstream 验证。
4. 可选：在 admin/usage 页面展示 slow interaction trace。

目标：

- 后续修复不靠猜。
- 每次协议改动都能验证“不会再让用户输入被吞、thinking 被误报、长等待无证据”。

---

## 7. 推荐新增的数据结构草案

### 7.1 RequestMessageDigest

```rust
struct RequestMessageDigest {
    endpoint: String,
    request_id: String,
    model: String,
    stream: bool,
    message_count: usize,
    last_user_index: Option<usize>,
    last_user_content_type: String,
    last_user_text_len: usize,
    last_user_text_hash: Option<String>,
    last_user_preview: Option<String>,
    tool_result_count_in_last_user: usize,
    assistant_tool_use_count_in_history: usize,
    metadata_session_id_hash: Option<String>,
}
```

### 7.2 ConversionMessageDigest

```rust
struct ConversionMessageDigest {
    request_id: String,
    endpoint: String,
    conversation_id_hash: String,
    current_message_text_len: usize,
    current_message_text_hash: Option<String>,
    current_tool_result_count: usize,
    history_entries: usize,
    tool_count: usize,
    warnings: ProxyWarnings,
}
```

### 7.3 ThinkingTrace

```rust
struct ThinkingTrace {
    requested: bool,
    trigger_source: ThinkingTriggerSource,
    thinking_type: Option<String>,
    effort: Option<String>,
    force_visible_thinking: bool,
    extract_xml_thinking: bool,
    received_native_reasoning: bool,
    received_xml_thinking: bool,
    visible_thinking_emitted: bool,
    first_thinking_delta_ms: Option<u64>,
    thinking_tokens: Option<i32>,
}
```

### 7.4 SlowInteractionDiagnostic

```rust
struct SlowInteractionDiagnostic {
    request_id: String,
    endpoint: String,
    conversation_id_hash: Option<String>,
    last_user_text_hash: Option<String>,
    message_count: usize,
    tool_result_count: usize,
    thinking_requested: bool,
    thinking_emitted: bool,
    upstream_header_ms: Option<u64>,
    first_upstream_chunk_ms: Option<u64>,
    first_thinking_delta_ms: Option<u64>,
    first_visible_text_delta_ms: Option<u64>,
    first_downstream_flush_ms: Option<u64>,
    stream_gap_to_first_output_ms: Option<u64>,
    events_before_first_output: Option<u32>,
    ping_count_before_visible_output: Option<u32>,
    terminal_reason: Option<String>,
}
```

---

## 8. 验证方案

### 8.1 用户输入未消费复现

步骤：

1. 启动隔离的本地 Kiro 服务。
2. 使用真实 Claude Code CLI 连接本地服务。
3. 发起一个会执行较久工具/Agent 的任务。
4. 在 assistant 仍执行时输入一条新增要求，例如：

```text
要记录目的，以及实现后如何配置（各项参数如何配置）
```

5. 检查：
   - transcript 是否出现 `queue-operation`。
   - 后续是否出现正常 user message。
   - Kiro 请求摘要里是否有这条输入 hash。
   - assistant final 是否覆盖新增要求。

通过标准：

- 如果 queue 输入没有进入请求，脚本必须能标红。
- 如果进入请求，Kiro 摘要必须能证明入口和转换后均保留。

### 8.2 thinking 验证

覆盖三类请求：

```text
普通模型 + ultrathink
显式 sonnet-thinking / opus-thinking 模型
显式 thinking.enabled
```

检查：

- thinking_trace.requested
- trigger_source
- first_thinking_delta_ms
- visible_thinking_emitted
- transcript 中是否有 thinking block

通过标准：

- 不能只因为 prompt 里有 ultrathink 就宣称 thinking 已输出。
- 必须有 stream/transcript/usage 证据。

### 8.3 长等待验证

用 fake upstream 或受控上游制造：

1. 慢响应头。
2. 响应头快但首个 chunk 慢。
3. 首 chunk 快但只有 thinking，无 visible text。
4. 首 chunk 快但 Kiro parser 缓冲。
5. 正常流但 tool/Agent 长时间运行。

检查：

- latency_trace 是否能区分这五种情况。
- slow_interaction_diagnostic 是否给出明确原因。

---

## 9. 后续拆任务建议

建议拆成以下任务：

1. `request-digest-logging`
   - 增加入口请求摘要和转换后摘要。
   - 默认脱敏。

2. `transcript-queue-analyzer`
   - 新增本地脚本分析 queue-operation 未消费。

3. `thinking-trace`
   - 记录 thinking 请求来源、是否返回、是否转出。

4. `slow-interaction-diagnostics`
   - 完善 latency trace，增加慢请求结构化日志。

5. `external-pool-visible-output-trace`
   - 外部池补 thinking/text/tool_use 首次时间。

6. `tool-result-context-summary`
   - 记录工具结果数量、体积、配对修复摘要。

7. `claude-cli-interactive-regression`
   - 建立真实 Claude Code CLI 交互验证用例。

---

## 10. 最终判断

后续优化应优先解决“证据不足”的问题。

对于当前用户反馈，最关键的不是先改模型、改 max_tokens 或改缓存，而是让每轮交互能回答下面四个问题：

1. 用户最新输入有没有作为正常 user message 进入请求。
2. Kiro 有没有保留最后用户文本和 tool_result。
3. thinking 是否被请求、是否真的返回、是否转给 CLI。
4. 长等待到底发生在调度、上游、thinking、可见文本、工具执行还是 CLI 展示。

只要这四个问题能用日志和 transcript 工具直接回答，后续再定位“断感强”“像没理会”“thinking 不展示”“停很久后一坨输出”就不会靠猜。
