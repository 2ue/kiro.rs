# P10 `stop_sequences` / `temperature` / `top_p` / `top_k` / `context_management` 和 `anthropic-beta` 在本地 Kiro 路径被静默丢弃

Status: open / documented / not-fixed
Severity: Medium
Area: request
Discovered: 2026-09-28 协议互转审计

## 问题与影响

`MessagesRequest` 只建模了这些字段：`model`、`max_tokens`、`messages`、`stream`、`system`、`tools`、`tool_choice`、`thinking`、`output_config`、`metadata`（`src/anthropic/types.rs:282-308`）。结构体没有 `#[serde(deny_unknown_fields)]`（在 `src/anthropic/` 下 grep 不到），所以其他顶层字段在反序列化时会被**静默丢弃**，不会报错也不会发告警：

| 字段 | 官方语义 | 当前本地 Kiro 路径的行为 |
| --- | --- | --- |
| `stop_sequences` | 生成到任意一个序列时停止，响应里 `stop_reason="stop_sequence"`，`stop_sequence` 填命中的序列 | 丢弃。响应里 `stop_sequence` 恒为 `null`（`src/anthropic/handlers.rs:11162`、`src/anthropic/stream.rs:1499`、`src/anthropic/stream.rs:2338`） |
| `temperature` / `top_p` / `top_k` | 采样参数 | 丢弃 |
| `context_management` | beta `context-management-2025-06-27`，服务端清理旧的 tool_use/tool_result（`clear_tool_uses_20250919`）和 thinking（`clear_thinking_*`） | 丢弃，历史原样发给 Kiro |
| `service_tier`、`container`、`mcp_servers` 等 | 各自的官方语义 | 丢弃 |

请求头：本地 Kiro 路径**不读取** `anthropic-beta` / `anthropic-version`。在 `src/anthropic/` 下 grep 这两个名字没有任何命中。只有外部池会转发或模拟它们：

- `anthropic-version` 缺失时默认补 `2023-06-01`（`src/external_pool.rs:11290-11295`）；
- ClaudeCodeMimic profile 固定发送一组 beta（`src/external_pool.rs:11371-11377`）；
- Passthrough profile 的白名单包含 `anthropic-beta` / `anthropic-version`（`src/external_pool.rs:11989-12016`）；
- 客户端没有声明 `context-management-2025-06-27` beta 时，外部池会从 body 里删掉 `context_management`，防止上游 400（`src/external_pool.rs:12029-12051`）。

`context-1m-2025-08-07`：本地路径只能通过模型名后缀 `[1m]` 或上游目录的 `maxInputTokens` 表达 1M 上下文。`[1m]` 在解析时被剥掉（`src/anthropic/model_capabilities.rs:1176-1179`），`context_window_tokens` 优先取上游目录，缺失时退回 `get_context_window_size`（`src/anthropic/handlers.rs:4872-4882`、`src/anthropic/converter/model.rs:89-115`）。beta 头本身对本地路径没有任何作用。

影响：

1. **`stop_sequences` 语义丢失（最实际的问题）。** 依赖停止序列截断输出的脚本或 SDK 会拿到超出预期的长文本，`stop_reason` 是 `end_turn` 而不是 `stop_sequence`，客户端逻辑可能出错，还会多付 output token。
2. **采样参数被忽略。** 客户端设 `temperature: 0` 期望输出稳定，实际拿到的是上游默认采样，而且没有任何信号告诉它。
3. **`context_management` 丢失。** Claude Code 开启这个 beta 后依赖服务端清理旧 tool 结果来控制上下文。代理丢弃后，长会话更早触发 payload guard 的裁剪或上游 "输入过长"（和 P03 相互影响）。现有测试说明 Claude Code 确实会发这个字段：`raw_request_sanitization_preserves_unmodeled_fields` 使用的就是 `clear_tool_uses_20250919`（`src/anthropic/transcript_sanitizer.rs:1686-1716`）。
4. **没有披露。** `x-kiro-rs-warnings` 头只统计 converter 内部的改写（`src/anthropic/converter.rs:215-275`），不包括被忽略的字段。它默认也是关闭的（`expose_proxy_warnings` 默认 `false`，`src/model/config.rs:4720-4722`）。

## 官方协议对照

> 方向约定：kiro.rs 基于 Kiro 对外提供 Claude Code（Anthropic）协议接口。客户端（如 Claude Code CLI）发出 Claude Code 协议请求，经转换后变为 Kiro 协议，调度到最终上游 Kiro；Kiro 的响应再转换回 Claude Code 协议返回给客户端。下文引用的"Anthropic 文档/官方"一律指 **Claude Code 协议规范**，即接口侧必须满足的行为；"Kiro 侧"指上游 Kiro 协议的要求。Anthropic 官方 API 不是本项目的上游。thinking 签名均为 Kiro 原生签名，在转换中原样透传。

- **Anthropic（已验证事实，官方文档）：** 上面这些字段都是 Messages API 的正式参数或 beta 参数。`stop_sequence` 的响应字段只在 `stop_reason="stop_sequence"` 时非空。
- **Anthropic 对未知字段（经验推断）：** 官方 API 对不认识的顶层字段通常返回 `400 invalid_request_error: Extra inputs are not permitted`，也就是说官方不会静默忽略。代理的静默丢弃比官方更宽松，这是兼容性上的取舍，但至少应该披露。
- **Kiro（本仓库已知的 wire 模型）：** `KiroRequest` 只有 `conversationState`、`profileArn`、`additionalModelRequestFields`（`src/kiro/model/requests/kiro.rs:31-50`）。`additionalModelRequestFields` 目前只建模了 `thinking`、`output_config`、`reasoning`（`src/kiro/model/requests/kiro.rs:58-65`）。本仓库和 sub2api 同级仓库都**没有** `inferenceConfig`、`stopSequences`、`temperature` 的使用或实测记录（在 `docs/`、`feature/`、`../sub2api-kiro/backend/internal/pkg/kiro/*.go` 下 grep 均没有命中）。Kiro 是否接受 `additionalModelRequestFields.temperature` 这类透传**尚未验证**，不能假设可用。

## 源码链与根因

1. HTTP body → `MessagesRequest` 反序列化（`src/anthropic/types.rs:282-308`），未建模的字段在这一步就没了。
2. 本地 Kiro 转换入口 `convert_request_with_resolved_model`（`src/anthropic/converter.rs:405-415`，调用点在 `src/anthropic/handlers/local_body_pipeline.rs:166`）只读取已建模的字段。
3. `build_additional_model_request_fields` 只根据 thinking / output_config 生成 native reasoning 字段（`src/anthropic/converter.rs:601-607`）。
4. 响应构造时 `stop_sequence` 固定写 `null`（`src/anthropic/handlers.rs:11155-11164`、`src/anthropic/stream.rs:1490-1502`）。

根因：请求模型按 "Kiro 能用什么" 来建模，而不是按 "Anthropic 协议有什么" 来建模。协议里有、Kiro 没有的能力，没有经过显式的 "转发 / 模拟 / 披露 / 拒绝" 决策，被默认成了 "丢弃"。

审计原述核对：成立。补充和修正几点：
- `external_pool.rs` 里相关代码的准确位置是 `11290-11295`、`11371-11377`、`11989-12016`、`12029-12051`。
- 原述的 `converter/model.rs ~88-115` 实际指的是 `get_context_window_size`，函数主体在 `src/anthropic/converter/model.rs:93-115`。
- 另外，`transcript_sanitizer` 的 raw 路径**会保留**未建模字段（`src/anthropic/transcript_sanitizer.rs:157`），但它只服务于外部池 raw body，不影响本地 Kiro 转换。

## 复现

### 最小复现（单测）

放进 `src/anthropic/types.rs` 的 `mod tests`（`src/anthropic/types.rs:142`）：

```rust
#[test]
fn audit_p10_unmodeled_protocol_fields_are_silently_dropped() {
    let raw = serde_json::json!({
        "model": "claude-sonnet-4-6",
        "max_tokens": 128,
        "messages": [{"role": "user", "content": "hi"}],
        "stop_sequences": ["END"],
        "temperature": 0.0,
        "top_p": 0.5,
        "top_k": 5,
        "context_management": {"edits": [{"type": "clear_tool_uses_20250919"}]}
    });
    // 现状：反序列化成功，没有任何错误
    let req: MessagesRequest = serde_json::from_value(raw).expect("accepted silently");
    let round_trip = serde_json::to_value(&req).unwrap();
    for field in ["stop_sequences", "temperature", "top_p", "top_k", "context_management"] {
        assert!(round_trip.get(field).is_none(), "{field} 现状被丢弃");
    }
}
```

修复后，这个测试改为断言字段被建模，并且转换结果或警告里有对应的记录。

### 端到端复现

1. `stop_sequences`：向测试实例 `/v1/messages` 发送 `{"stop_sequences":["world"],"messages":[{"role":"user","content":"Reply exactly: hello world again"}]}`。现状：响应文本里包含 `world again`，`stop_reason="end_turn"`，`stop_sequence=null`。官方应在 `hello ` 之后停止，`stop_reason="stop_sequence"`。
2. `temperature`：同一个 prompt 在 `temperature: 0` 下跑 5 次，比较输出是否一致（只作为观察，不作为强断言）。
3. `context_management`：Claude Code 开启 context-management beta 的长会话中，对比请求里的 tool_result 总字节数和 Kiro 请求体里的对应字节数，二者相等说明没有清理。

## 修复方案

### 候选方案

逐个字段决策：

| 字段 | 决策 | 做法 |
| --- | --- | --- |
| `stop_sequences` | **代理侧模拟** | 流式：维护 `max(len(seq))-1` 字节的尾部缓冲，只作用于 text 块（不作用于 thinking 和 tool_use input）。命中后截断文本、关闭 text 块，发送 `stop_reason="stop_sequence"` 和 `stop_sequence=<seq>`，然后停止向下游转发并取消上游读取。非流式：在最终 text 上查找最早的命中位置并截断。usage 的 output_tokens 仍按上游计量（已经消耗了）。 |
| `temperature` / `top_p` / `top_k` | **先披露，再实测** | 默认忽略，但写入 warnings（`ignored-field=temperature` 等）和 usage/debug 记录。用真实账号实测 `additionalModelRequestFields` 下的透传；确认上游接受并且生效后，再按模型能力开放。 |
| `context_management` | **代理侧模拟（可选开关）** | `clear_tool_uses_*`：按 `trigger`/`keep`/`clear_at_least` 语义，把较早的 tool_result 内容替换成中性占位，保持 tool_use/tool_result 配对。`clear_thinking_*`：从历史里去掉 thinking 块。响应里按官方格式返回 `context_management.applied_edits`。 |
| `anthropic-beta` | **解析并映射** | `context-1m-2025-08-07` → 等价于 `[1m]` 的解析意图；`context-management-2025-06-27` → 开启上一行的模拟；其余 beta 记录下来但忽略。 |
| 其他未知顶层字段 | **披露** | 使用 `#[serde(flatten)] extra: serde_json::Map` 收集，写入 warnings 和日志，不改变转发行为。不建议直接 `deny_unknown_fields`，会破坏现有客户端。 |

### 推荐方案

分三步做，每步单独上线：

1. **可见性（低风险，先做）：** `MessagesRequest` 增加 `stop_sequences`、`temperature`、`top_p`、`top_k`、`context_management` 的显式字段，以及 `#[serde(flatten)] extra`。`ProxyWarnings` 增加 `ignored_fields` 计数和名单，编码进 `x-kiro-rs-warnings`。usage 记录增加 `ignored_request_fields`，方便运营统计真实使用频率。
2. **`stop_sequences` 模拟：** 放在 `StreamContext` 的 text 输出前（和 transcript sanitizer 同一层，在 sanitizer 之后执行匹配，保证匹配的是用户最终可见的文本）。非流式在 `append_non_stream_reasoning_and_text` 之后截断。匹配要考虑 UTF-8 边界。
3. **`context_management` 模拟：** 放在 payload guard 之前的历史预处理阶段，受 beta 头和开关双重控制。**注意** [transcript 泄漏专题](../protocol-transcript-and-tool-history-leak.md) 的结论：命令式占位（如 `Tool results provided.`）会被模型复述。清理后的占位必须用中性、非命令式的文本，并且加入 sanitizer 的已知 scaffold 名单。

`temperature` 等透传放到实测之后再做，本期不做。

## 测试与验收

- 单测：`audit_p10_unmodeled_protocol_fields_are_silently_dropped` 改写为 "字段被建模 + 出现在 warnings" 的断言。
- `stop_sequences`：
  - 流式：序列跨 chunk 边界（例如 `wor` / `ld`）、多个序列取最早命中、序列出现在 thinking 里不触发、序列出现在 tool_use input 里不触发、中文和 emoji 的 UTF-8 边界。每种 5 轮。
  - 非流式：同样的用例；`stop_sequence` 字段值正确。
  - 命中后上游连接被取消，usage 状态为 success，`stop_reason="stop_sequence"`。
- `context_management`：清理后 tool_use/tool_result 仍然严格配对（strict profile 不报错）；`applied_edits` 格式和官方一致；占位文本没有被 sanitizer 当成泄漏；5 轮真实 Claude Code 长会话不增加 400。
- warnings：打开 `expose_proxy_warnings` 时，头里包含 `ignored-field=temperature`；关闭时不输出。

## 兼容性与风险

- `stop_sequences` 模拟会让一部分以前 "输出更长" 的请求变短，这正是官方行为。依赖旧行为的客户端很少，但需要写进变更说明。
- 流式尾部缓冲会让 text 输出最多延迟 `max(len(seq))-1` 字节，对体验影响可以忽略。只在请求携带 `stop_sequences` 时启用。
- `context_management` 模拟改变了发给上游的历史，会影响 prompt cache 命中和模型行为，必须默认关闭，灰度开启。
- `temperature` 透传如果上游不认识这个字段，可能直接 400，所以必须先实测。
- 回滚：每一步都有独立开关，关掉就回到现状。
