# P08 流式 ContentLengthExceededException 被转成 SSE error，非流式却返回正常 max_tokens

Status: fixed-in-619ac56 + 5358be6 (not released)
Severity: Medium
Area: stream
Discovered: 2026-09-28 协议互转审计
Verified-against: a4227c1 (2026-09-29)

> 2026-09-29 代码核对（HEAD a4227c1）：3a1306d 未改动 `stream.rs` 的 `Event::Exception` / `ContextUsage` 分支和非流式事件循环，主问题与子问题均仍存在。所引 `stream.rs`、`handlers.rs`、`tool_use.rs` 行号与 HEAD 一致（仅把既有测试起始行从 `:5684` 更正为函数所在的 `:5685`）。3a1306d 在 `usage.rs` 的改动只影响请求拒绝（pre-usage rejection）记录，不影响流错误的用量记录。

## 问题与影响

上游在 HTTP 200 的事件流中途发出 `:message-type=exception`、`:exception-type=ContentLengthExceededException` 时：

- 流式：代理先 `set_stop_reason("max_tokens")`，紧接着 `record_stream_error("api_error", message)`。
  `generate_final_events` 看到 `stream_error` 后关闭已打开的块，只发一个 SSE `error` 事件后 `return`，
  **没有 `message_delta`、没有 `message_stop`**，`max_tokens` 这个 stop_reason 从未发给下游。
  error message 在有 `error_id` 时被替换为通用的 `The request could not be completed. Please retry shortly. ... error ID: …`。
- 非流式：同一事件只把 `stop_reason` 设为 `max_tokens`，返回 200 和已生成的内容。

同一上游行为，两种模式结论相反。对 Claude Code（主循环使用流式）的影响：

- 已经输出的部分文本/工具调用被当成失败请求，Claude Code 显示 API Error 并按错误重试（经验观察：对 `api_error` 会做有限次重试）；
- 重试是完整重发，上游再次在同一位置截断，用户看到反复失败；
- 如果按官方语义收到 `stop_reason: max_tokens`，Claude Code 会保留已生成内容并按"输出达到上限"处理（例如提示或自动续写），会话能继续。

相关子问题（同一 issue 记录）：`contextUsageEvent.contextUsagePercentage >= 100` 会把 stop_reason 强制设为
`model_context_window_exceeded`，且该值优先级高于 `tool_use`，见"源码链与根因 · 子问题"。

## 官方协议对照

> 方向约定：kiro.rs 基于 Kiro 对外提供 Claude Code（Anthropic）协议接口。客户端（如 Claude Code CLI）发出 Claude Code 协议请求，经转换后变为 Kiro 协议，调度到最终上游 Kiro；Kiro 的响应再转换回 Claude Code 协议返回给客户端。下文引用的"Anthropic 文档/官方"一律指 **Claude Code 协议规范**，即接口侧必须满足的行为；"Kiro 侧"指上游 Kiro 协议的要求。Anthropic 官方 API 不是本项目的上游。thinking 签名均为 Kiro 原生签名，在转换中原样透传。

已验证事实（Anthropic Messages API 文档）：

- 输出被 `max_tokens` 截断：正常 `message_delta`，`delta.stop_reason = "max_tokens"`，随后 `message_stop`；不是错误。
- `model_context_window_exceeded`：官方 stop_reason 之一，表示生成过程中达到模型上下文窗口上限；Sonnet 4.5 及更新模型默认可返回，旧模型需要 beta header。它同样是正常结束，不是错误。
- 流式 `error` 事件用于请求中途的服务端错误（如 `overloaded_error`），客户端会把整个请求视为失败。
- `stop_reason = "tool_use"` 表示 assistant 以工具调用结束，客户端据此执行工具并回传 `tool_result`。

Kiro 侧（经验推断，来自代码中的既有处理与社区 Kiro 客户端实现）：

- `ContentLengthExceededException` 在 Kiro/CodeWhisperer 事件流中表示**生成内容长度超限**（输出被截断），社区实现普遍映射为 `max_tokens`；
  本仓库非流式与流式都在注释/代码里按 `max_tokens` 理解（`src/anthropic/stream.rs:2533-2536`，`src/anthropic/handlers.rs:10875-10877`）。
  未找到 AWS 官方文档说明该异常的精确语义。
- `contextUsageEvent` 报告的是本轮上下文占用百分比（本仓库用它反推 input_tokens，`src/anthropic/stream.rs:2436-2447`）。
  百分比 >= 100 是否意味着"本轮生成因窗口满而停止"，还是只是"输入 + 输出占满窗口"，无官方说明。

## 源码链与根因

### 主问题：ContentLengthExceededException

流式（`src/anthropic/stream.rs:2529-2541`）：

```rust
Event::Exception { exception_type, message } => {
    // 处理 ContentLengthExceededException
    if exception_type == "ContentLengthExceededException" {
        self.state_manager.set_stop_reason("max_tokens");
    }
    tracing::warn!("收到异常事件: {} - {}", exception_type, message);
    self.record_stream_error("api_error", message.clone());   // 对 ContentLength 也记录了错误
    Vec::new()
}
```

最终事件（`src/anthropic/stream.rs:3806-3811`）：

```rust
if let Some((error_type, raw_message)) = self.stream_error.take() {
    events.extend(self.state_manager.close_open_blocks());
    let message = self.public_stream_error_message(&error_type, raw_message);
    events.push(Self::create_error_event(error_type, message));
    return events;                       // 不会生成 message_delta / message_stop
}
```

`public_stream_error_message`（`src/anthropic/stream.rs:2591-2608`）：上游 message 能解析出 JSON message 时透出；
否则在有 `stream_error_id` 时替换为 `PUBLIC_PROCESSING_FAILED_MESSAGE`。流式 handler 总会设置 error id
（`src/anthropic/handlers.rs:7389`）。

handler 侧（`src/anthropic/handlers.rs:9485-9545`）：`had_stream_error_before_finalization` 为真时走
`generate_final_events()`，随后 `stream_error_detail()` 非空 → `report_upstream_stream_failure`，用量记录为流错误。

非流式（`src/anthropic/handlers.rs:10871-10894`）：

```rust
Event::Exception { exception_type, message } => {
    if exception_type == "ContentLengthExceededException" {
        stop_reason = "max_tokens".to_string();
    } else {
        // 其他异常 -> 502 api_error
    }
}
```

非流式首输出前的错误探测也显式排除了该异常（`src/anthropic/handlers.rs:8763-8771`，
`non_stream_status_error_before_first_output`），说明作者意图是"不是错误"。流式分支漏了 `else`。

根因：流式 `Event::Exception` 分支把 `record_stream_error` 放在 if 之外，对 `ContentLengthExceededException` 也生效；
而 `stream_error` 在最终事件生成中优先级最高，覆盖了已设置的 stop_reason。

### 子问题：contextUsagePercentage >= 100 覆盖 tool_use

流式（`src/anthropic/stream.rs:2448-2452`）：

```rust
if percentage.is_finite() && percentage >= 100.0 {
    self.state_manager.set_stop_reason("model_context_window_exceeded");
}
```

`get_stop_reason`（`src/anthropic/stream.rs:1361-1369`）先返回显式 `stop_reason`，再看 `has_tool_use`，所以本轮若已经产生 tool_use，
最终仍报 `model_context_window_exceeded`。`maybe_set_max_tokens_stop_reason`（`:1324-1331`）也因为已有显式值而跳过。

非流式（`src/anthropic/handlers.rs:10765-10768` 设置，`:10997-11000` 只在 `stop_reason == "end_turn"` 时改为 `tool_use`），同样丢失 `tool_use`。

审计给的行号 `~stream.rs:3832-3840` 不准确：最终事件里没有对 `model_context_window_exceeded` 的专门处理，
相关位置是 `src/anthropic/stream.rs:2448-2452`（设置）、`:1361-1369`（优先级）以及 `:2001`（`stop_reason_source` 标记为
`local_context_window_exceeded`）。

影响（推断）：Claude Code 是否执行工具以 content 中的 tool_use 块为准还是以 stop_reason 为准，未对其源码验证。
如果以 stop_reason 为准，工具不会被执行，会话停住；即使以块为准，`model_context_window_exceeded` 也会让客户端提示上下文满，与"工具调用成功发出"矛盾。
另外 `>= 100.0` 是浮点边界，上游若把 99.995 四舍五入为 100 上报，也会触发。

现有测试 `test_context_usage_100_percent_reports_context_window_exceeded`（`src/anthropic/stream.rs:5685` 起）只覆盖纯文本场景。

## 复现

### 最小复现（单测）

放入 `src/anthropic/stream.rs` 的 `mod tests`：

```rust
#[test]
fn test_content_length_exceeded_exception_ends_with_max_tokens_not_error() {
    let mut ctx = StreamContext::new_with_thinking("test-model", 1, false, HashMap::new());
    let _ = ctx.generate_initial_events();
    let mut all_events = Vec::new();
    all_events.extend(ctx.process_assistant_response("partial answer"));
    all_events.extend(ctx.process_kiro_event(&Event::Exception {
        exception_type: "ContentLengthExceededException".to_string(),
        message: r#"{"message":"Content length exceeded"}"#.to_string(),
    }));
    all_events.extend(ctx.generate_final_events());

    assert!(
        all_events.iter().all(|e| e.event != "error"),
        "ContentLengthExceededException 不应产生 SSE error"
    );
    let delta = all_events.iter().find(|e| e.event == "message_delta").expect("message_delta");
    assert_eq!(delta.data["delta"]["stop_reason"], "max_tokens");
    assert!(all_events.iter().any(|e| e.event == "message_stop"));
    assert_eq!(collect_text_content(&all_events), "partial answer");
}

#[test]
fn test_context_usage_100_percent_does_not_override_tool_use() {
    use crate::kiro::model::events::{ContextUsageEvent, ToolUseEvent};
    let mut ctx = StreamContext::new_with_thinking("test-model", 1, false, HashMap::new());
    let _ = ctx.generate_initial_events();
    let mut all_events = Vec::new();
    all_events.extend(ctx.process_kiro_event(&Event::ToolUse(ToolUseEvent {
        name: "Read".to_string(),
        tool_use_id: "toolu_p08".to_string(),
        input: r#"{"file_path":"/tmp/a"}"#.to_string(),
        stop: true,
    })));
    all_events.extend(ctx.process_kiro_event(&Event::ContextUsage(ContextUsageEvent {
        context_usage_percentage: 100.0,
    })));
    all_events.extend(ctx.generate_final_events());
    let delta = all_events.iter().find(|e| e.event == "message_delta").unwrap();
    assert_eq!(delta.data["delta"]["stop_reason"], "tool_use");
}
```

`ToolUseEvent` 字段（`name` / `tool_use_id` / `input` / `stop`）已对照 `src/kiro/model/events/tool_use.rs:18-29`。当前实现下第一个测试得到 `error` 事件且无 `message_delta`，第二个得到 `model_context_window_exceeded`。

### 端到端复现

fake upstream（用 `src/anthropic/handlers/tests.rs` 的 `eventstream_test_frame` 与 `eventstream_test_control_frame`）：

```rust
let mut body = Vec::new();
body.extend(eventstream_test_frame("assistantResponseEvent", json!({"content": "partial answer"})));
body.extend(eventstream_test_control_frame(
    "exception",
    ":exception-type",
    "ContentLengthExceededException",
    json!({"message": "Content length exceeded"}),
));
```

分别以 `"stream": true` 与 `"stream": false` 请求同一 fake upstream：

- 当前：stream 返回 `message_start … content_block_* … event: error`；non-stream 返回 200，`stop_reason = "max_tokens"`；
- 期望：两者都以 `max_tokens` 正常结束。

真实 Claude Code：让模型一次写超长文件（例如"把 5000 行代码一次性写进一个文件"），在上游触发输出截断时观察到 API Error 并自动重试，而不是"输出达到上限"。

## 修复方案

### 候选方案

A. 流式 `Event::Exception` 对 `ContentLengthExceededException` 只设 `max_tokens`，不记 `stream_error`，与非流式对齐。

B. 保留 error 但在 error 之前补 `message_delta(max_tokens)`。违反 SSE 状态机（`message_delta` 后不应再有 error），不采用。

C. 子问题：`contextUsagePercentage >= 100` 不再设置显式 stop_reason，只作为观测；或只在"没有 tool_use 且没有其他显式原因"时于最终阶段应用。

### 推荐方案

1. `src/anthropic/stream.rs:2529-2541` 改为：

```rust
Event::Exception { exception_type, message } => {
    if exception_type == "ContentLengthExceededException" {
        tracing::warn!(message = %message, "上游输出长度超限，按 max_tokens 结束");
        self.state_manager.set_stop_reason("max_tokens");
        self.upstream_content_length_exceeded = true;   // 观测字段，写入 usage 诊断
    } else {
        tracing::warn!("收到异常事件: {} - {}", exception_type, message);
        self.record_stream_error("api_error", message.clone());
    }
    Vec::new()
}
```

   同时确认 `upstream_terminal_failure_detail`（`src/anthropic/stream.rs:1868` 起）不会因为缺少 `messageStatus` 把这种结束判成失败；
   必要时把 `upstream_content_length_exceeded` 视为可信结束信号（`has_trusted_upstream_completion_signal`）。
2. 若截断发生在 tool_use 输入 JSON 未完成时，现有 `flush_incomplete_tool_input_buffers` 会处理不完整工具输入；
   这种情况下 stop_reason 应保持 `max_tokens`（官方语义：截断的 tool_use 伴随 `max_tokens`），需补测试确认 `has_tool_use` 不会把它改成 `tool_use`。
   当前 `get_stop_reason` 显式值优先，满足要求。
3. 子问题采用 C 的第二种：把 `model_context_window_exceeded` 从"事件到达时立即设置"改为"最终阶段兜底"：

```rust
// 事件处理：只记录
self.context_usage_reached_full = percentage.is_finite() && percentage >= 100.0;

// generate_final_events 中 maybe_set_max_tokens_stop_reason 之前：
if self.context_usage_reached_full
    && !self.state_manager.has_explicit_stop_reason()
    && !self.state_manager.has_tool_use()
{
    self.state_manager.set_stop_reason("model_context_window_exceeded");
}
```

   非流式 `src/anthropic/handlers.rs:10765-10768` 与 `:10997-11000` 同步：`has_tool_use` 时保持 `tool_use`。
4. 不改非流式 ContentLengthExceeded 行为（已符合预期）。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)，只使用 `127.0.0.1:19023` 指定测试实例和隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，Cargo 通过 `feature/tests/run-cargo-scoped.sh` 运行，不得影响本机正在运行的 Claude Code CLI 与服务。

单测：

- `test_content_length_exceeded_exception_ends_with_max_tokens_not_error`（上面草稿）；
- 截断发生在 thinking 中、tool_use 输入中各一条，断言无 error 事件、`stop_reason = max_tokens`、块都正确关闭；
- 其他异常类型（如 `InternalServerException`）仍产生 error 事件（回归）；
- `test_context_usage_100_percent_does_not_override_tool_use`；
- 已有 `test_context_usage_100_percent_reports_context_window_exceeded` 保持通过（纯文本场景仍报 `model_context_window_exceeded`）；
- 非流式对应用例：`contextUsagePercentage = 100` + tool_use → `stop_reason = tool_use`。

handler 级（`src/anthropic/handlers/tests.rs`）：

- 用上面的 fake upstream 序列分别跑 stream / non-stream，断言两者 stop_reason 都是 `max_tokens`，usage 记录状态为成功（不是 `StreamError`），completion 报告 success。

端到端验收：真实 Claude Code 触发输出截断时不再出现 API Error 重试循环。

## 兼容性与风险

- 用量统计：这类请求从"流错误"变为"成功"，错误率指标会下降；需要在 usage 诊断中保留 `upstream_content_length_exceeded` 以便区分。
- 如果 `ContentLengthExceededException` 在某些场景其实表示**输入**超限且发生在任何输出之前，按 max_tokens 返回空内容会误导客户端。
  建议加保护：若此时没有任何有意义输出（`has_meaningful_upstream_response()` 为假），按 [P01](01-context-window-error-message-not-official.md) 的官方 `prompt is too long` 语义处理
  （流式此时通常尚未提交下游，可走重试/错误映射）。这一分支的上游真实行为未验证。
- 子问题修改会改变 `stop_reason_source` 统计中 `local_context_window_exceeded` 的数量。
- 回滚：两处改动都局部，可单独回退。
- 未验证项：Kiro `ContentLengthExceededException` 的官方语义；Claude Code 对 `model_context_window_exceeded` + tool_use 组合的处理方式。

## 修复结果与验证（2026-09-29）

- 修复（`619ac56`）：流式收到 `ContentLengthExceededException` 时，如果之前已有输出，以 `stop_reason=max_tokens` 正常结束，发出 message_delta 和 message_stop，行为与非流式一致；如果之前没有任何输出，仍然按错误处理。
- 子问题修复（`5358be6`）：contextUsage 达到 100% 时，如果本轮已经输出了 tool_use，stop_reason 保持 `tool_use`（流式和非流式都是），避免 Claude Code 不执行已返回的工具调用。
- 单测：`content_length_exceeded_after_output_ends_with_max_tokens_for_five_rounds`、`content_length_exceeded_without_output_is_still_an_error`、`context_window_full_does_not_hide_emitted_tool_use`。
- 真实上游：没有找到能稳定触发该异常的输入，这一项只由单测覆盖。
- 验证环境：真实上游验证使用 `127.0.0.1:19023` 指定测试实例和隔离的 CLI `HOME`/`CLAUDE_CONFIG_DIR`，未改动本机正在运行的 Claude Code CLI 环境。证据见 `tmp/thinking-budget-local/fix-evidence-20260929/`。
