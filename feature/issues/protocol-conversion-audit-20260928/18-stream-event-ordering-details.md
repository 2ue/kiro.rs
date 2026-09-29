# P18 流式事件顺序细节：多个块同时打开、`content_block_stop` 顺序不确定、纯工具调用多出空 text 块、reasoning 非增量、帧 prelude CRC 校验滞后

Status: open / documented / not-fixed
Severity: Low
Area: stream
Discovered: 2026-09-28 协议互转审计
Verified-against: a4227c1 (2026-09-29)

## 问题与影响

`SseStateManager` 和 `StreamContext` 保证了"每个块都是 start → delta → stop、message_start 只发一次、message_delta 在所有 stop 之后"。但在下面几个细节上，和官方流的形态不同：

1. **多个块可以同时打开**：`handle_content_block_start` 在 tool_use 开始时只会关闭 **text** 块。如果上一个 tool_use 还没收到 `stop=true`，下一个 tool_use 就开始了，那么两个 tool_use 块会同时处于打开状态。另外，文本输出发现当前 text 块已关闭时，会新开一个 text 块，**不检查是否还有 tool_use 块打开**。所以 tool_use 期间插进来的文本，也会和 tool_use 同时打开。
2. **`content_block_stop` 顺序不确定**：流结束时，`close_open_blocks` 遍历 `HashMap<i32, BlockState>`；未完成工具的 flush 遍历 `HashMap<String, String>`（`tool_input_buffers`）。HashMap 的迭代顺序每个进程、每个实例都随机，所以同时打开的多个块，它们 stop（以及延迟下发的 `input_json_delta`）的顺序不固定。同一个上游回放两次，得到的 SSE 字节序列可能不同。
3. **纯工具调用多出空 text 块**：非 thinking 模式下，`generate_initial_events` 总是先打开 index 0 的 `text` 块。如果模型只调用工具、没有输出文本，下游收到的是 `content_block_start(text, "")` → `content_block_stop(0)` → `tool_use(index 1)`，最终 content 为 `[{"type":"text","text":""}, {"type":"tool_use",…}]`。
4. **原生 reasoning 不增量下发**：Kiro 的 `reasoningContentEvent` 是累计快照。代理把快照缓存起来，直到 reasoning 结束（遇到文本、工具或流结束）才一次性发出一个完整的 `thinking_delta`，中间没有增量。
5. **EventStream 帧的 prelude CRC 校验在完整帧到齐之后才做**：`parse_frame` 先检查 `total_length` 是否在 16 B..16 MiB 范围内，然后等缓冲区攒够 `total_length` 字节，才校验 prelude CRC。如果 prelude 损坏，而损坏后的长度仍在范围内（最大 16 MiB），解码器会一直等后续字节，直到凑满或 EOF，然后才发现 CRC 错误，再进入逐字节恢复。

影响：

- #1、#2：Anthropic SDK 按 `index` 累积内容，对交错和乱序都比较宽容，最终 message 一般是正确的（推断）。但是：
  - Claude Code 如果在某个 tool_use 块的 `content_block_stop` 时就开始执行这个工具（流式工具执行），stop 顺序会影响工具的执行顺序（推断）；
  - 严格按"上一个块 stop 之后才 start 下一个"实现的第三方客户端和中转，可能会报错或丢失 delta；
  - SSE 字节序列不确定，回放测试和差异比对很难稳定。
- #3：Claude Code 会把这个空 text 块写进会话历史（推断）。这个会话以后如果切到官方 API，或者经过外部池透传给严格的 Anthropic 兼容上游，就可能触发 `text content blocks must be non-empty` 类 400（推断，官方对请求中空 text block 的校验来自经验）。
- #4：长推理时，用户在 CLI 里看不到 thinking 的进度，首个可见输出的延迟等于整个推理时长。这是有意的取舍，见下文。
- #5：正常的 TLS/HTTP 传输下 prelude 基本不会损坏，影响主要在理论层面：一旦发生，恢复会延迟一段时间，并且最多缓存 16 MiB。

定级 Low：目前没有证据表明真实 Claude Code 因此失败。这些主要是协议保真度和确定性方面的问题。

## 官方协议对照

> 方向约定：kiro.rs 基于 Kiro 对外提供 Claude Code（Anthropic）协议接口。客户端（如 Claude Code CLI）发出 Claude Code 协议请求，经转换后变为 Kiro 协议，调度到最终上游 Kiro；Kiro 的响应再转换回 Claude Code 协议返回给客户端。下文引用的"Anthropic 文档/官方"一律指 **Claude Code 协议规范**，即接口侧必须满足的行为；"Kiro 侧"指上游 Kiro 协议的要求。Anthropic 官方 API 不是本项目的上游。thinking 签名均为 Kiro 原生签名，在转换中原样透传。

已验证事实（Anthropic Streaming Messages 文档）：

- 事件流：先 `message_start`；每个内容块依次是一个 `content_block_start`、若干 `content_block_delta`、一个 `content_block_stop`；然后 `message_delta`，最后 `message_stop`，中间可以穿插 `ping`。
- `index` 对应最终 `content` 数组的位置，从 0 开始递增。
- thinking 块通过多个 `thinking_delta` 增量下发，最后是 `signature_delta`，然后 `content_block_stop`。

观察到的官方行为（多次抓包，属经验，不是文档保证）：

- 块严格串行：下一个块的 `content_block_start` 总是在上一个块的 `content_block_stop` 之后。
- 模型直接调用工具时，`content` 可以从 `tool_use`（index 0）开始，不会插入空 text 块。
- 官方请求校验会拒绝空字符串的 text block（经验）。

AWS EventStream 编码（公开规范）：prelude = `total_length(4) + headers_length(4) + prelude_crc(4)`。prelude CRC 就是用来在读取 payload 之前确认长度字段可信的，所以可以在只收到 12 字节时就校验。

## 源码链与根因

状态机（`src/anthropic/stream.rs`）：

- `src/anthropic/stream.rs:1249`：`active_blocks: HashMap<i32, BlockState>`。
- `src/anthropic/stream.rs:1341-1358` `close_open_blocks`：`for (index, block) in self.active_blocks.iter_mut()`，按 HashMap 顺序发出 stop。在 `generate_final_events_with_usage`（第 1484-1488 行）中调用。
- `src/anthropic/stream.rs:1382-1423` `handle_content_block_start`：第 1391-1406 行只在 `block_type == "tool_use"` 时关闭 `block_type == "text"` 的块，其他打开的块（上一个 tool_use、thinking）不处理。关闭 text 块时也在遍历 HashMap。
- `src/anthropic/stream.rs:1547`：`tool_input_buffers: HashMap<String, String>`。
- `src/anthropic/stream.rs:3627-3689` `flush_incomplete_tool_input_buffers`：第 3632 行 `std::mem::take(&mut self.tool_input_buffers)` 之后遍历 HashMap，逐个发出延迟的 `input_json_delta` 和 `content_block_stop`，顺序不确定。
- `src/anthropic/stream.rs:3436-3625` `process_tool_use`：每个 `tool_use_id` 第一次出现时分配新的 index（第 3513-3520 行）；只有 `tool_use.stop` 为 true 时才发 stop（第 3581-3622 行）。上游如果先发工具 A 的片段、再发工具 B 的片段，而 A 的 stop 缺失或晚到，就会出现同时打开的情况。上游缺少 stop 的真实情况见 [企业版 EventStream usage-only 工具 EOF](../enterprise-eventstream-usage-only-tool-eof.md)。
- `src/anthropic/stream.rs:3175-3243` `emit_text_delta_raw`：第 3190-3195 行发现 `text_block_index` 已关闭时，丢弃这个索引；第 3198-3221 行新开一个 text 块，不检查是否有打开的 tool_use。

空 text 块：

- `src/anthropic/stream.rs:2369-2389` `generate_initial_events_*`：`thinking_enabled` 时直接返回；否则第 2374-2388 行无条件 `next_block_index()` 并 `handle_content_block_start(text, "")`。
- `src/anthropic/stream.rs:3442`：`drop_pending_trivial_text_before_tool_use` 只清理暂存的 trivial 文本（`2210-2219`），不会撤回已经发出的 index 0 的 start。
- 对照非流式：`src/anthropic/handlers.rs:10995-11074` 按实际的 `text_content` 组装 content，文本为空时不会生成 text 块。stream 和 non-stream 不对称。

原生 reasoning：

- `src/anthropic/stream.rs:2661-2737` `process_reasoning_content`：第 2737-2738 行（2026-09-29 核对时由 2735-2736 更正）`native_reasoning_content.clear(); push_str(text)`，每个事件用最新的累计快照覆盖，不下发任何事件。
- `src/anthropic/stream.rs:3373-3433` `close_native_reasoning_block`：拿到完整内容后，先整体经过 sanitizer，签名块如果被污染就整体丢弃；然后一次性发出 start、单个 `thinking_delta`、`signature_delta`、stop。
- 缓冲上限：`src/anthropic/stream.rs:85` `MAX_BUFFERED_ATOMIC_THINKING_BYTES = 1 MiB`。
- **这是有意的设计**：[Thinking 与签名内容安全](../thinking-and-signed-content-safety.md) 规定"signed/redacted：完整原子缓冲后才决定是否下发；绝不局部改写或把净化后的正文与原 signature 重新组合"，并把"首 thinking 延迟"列为必要成本。本文不推翻这个决策。

帧解析：

- `src/kiro/parser/frame.rs:100-141` `parse_frame`：第 111-124 行检查长度范围，第 129-131 行 `buffer.len() < total_length` 时返回 `Ok(None)` 继续等待，第 134-141 行才校验 prelude CRC。
- `src/kiro/parser/decoder.rs:247-265` `try_recover`：prelude 错误时跳过 1 字节。

根因：

1. 状态机用 HashMap 保存块状态，只保证每个块自身的合法性，不保证块之间串行和顺序确定。
2. "tool_use 开始时关闭 text"是针对最常见的交错情况做的局部修补，没有推广成"任何新块开始前关闭所有打开的块"。
3. 为了让首个事件尽早发出，非 thinking 模式预先打开了 text 块，没有延迟到第一个真实文本出现时。
4. prelude CRC 的校验时机沿用了"整帧校验"的写法。

## 复现

### 最小复现（单测）

放进 `src/anthropic/stream.rs` 的 `mod tests`（沿用 `StreamContext::new_with_thinking("test-model", 1, false, HashMap::new())` 和 `ToolUseEvent` 的写法）：

```rust
fn assert_blocks_are_sequential(events: &[SseEvent]) {
    let mut open: Option<i64> = None;
    for event in events {
        match event.event.as_str() {
            "content_block_start" => {
                let index = event.data["index"].as_i64().unwrap();
                assert!(open.is_none(), "block {index} started while {open:?} still open");
                open = Some(index);
            }
            "content_block_stop" => {
                let index = event.data["index"].as_i64().unwrap();
                assert_eq!(open, Some(index), "stop for non-current block {index}");
                open = None;
            }
            _ => {}
        }
    }
}

#[test]
fn interleaved_tool_uses_are_emitted_sequentially() {
    for _round in 0..20 {
        let mut ctx = StreamContext::new_with_thinking("test-model", 1, false, HashMap::new());
        let mut events = ctx.generate_initial_events();
        for (id, input) in [("toolu_a", r#"{"p":"a"#), ("toolu_b", r#"{"p":"b"#)] {
            events.extend(ctx.process_kiro_event(&Event::ToolUse(ToolUseEvent {
                name: "Read".to_string(),
                tool_use_id: id.to_string(),
                input: input.to_string(),
                stop: false,
            })));
        }
        events.extend(ctx.generate_final_events());
        // 当前失败：toolu_b start 时 toolu_a 仍打开；stop 顺序随 HashMap 变化
        assert_blocks_are_sequential(&events);
    }
}

#[test]
fn pure_tool_call_stream_has_no_empty_text_block() {
    let mut ctx = StreamContext::new_with_thinking("test-model", 1, false, HashMap::new());
    let mut events = ctx.generate_initial_events();
    events.extend(ctx.process_kiro_event(&Event::ToolUse(ToolUseEvent {
        name: "Read".to_string(),
        tool_use_id: "toolu_1".to_string(),
        input: r#"{"file_path":"/tmp/a"}"#.to_string(),
        stop: true,
    })));
    events.extend(ctx.generate_final_events());
    let first_start = events
        .iter()
        .find(|e| e.event == "content_block_start")
        .expect("has a block");
    // 当前失败：首个块是 text ""（index 0），tool_use 在 index 1
    assert_eq!(first_start.data["content_block"]["type"], "tool_use");
    assert_eq!(first_start.data["index"], 0);
}
```

放进 `src/kiro/parser/frame.rs` 的 `mod tests`：

```rust
#[test]
fn corrupted_prelude_is_rejected_before_waiting_for_full_frame() {
    let mut buffer = vec![0u8; PRELUDE_SIZE];
    buffer[0..4].copy_from_slice(&(8 * 1024 * 1024u32).to_be_bytes()); // 合法范围内的伪长度
    buffer[4..8].copy_from_slice(&0u32.to_be_bytes());
    let crc = crc32(&buffer[..8]) ^ 0xdead_beef; // 故意错误
    buffer[8..12].copy_from_slice(&crc.to_be_bytes());
    // 当前失败：返回 Ok(None)，继续等待 8 MiB
    assert!(matches!(
        parse_frame(&buffer),
        Err(ParseError::PreludeCrcMismatch { .. })
    ));
}
```

### 端到端复现

1. 用 handler 测试中的 fake Kiro upstream（`eventstream_test_frame` 一类工具，见 `src/anthropic/handlers/tests.rs:49`）按这个顺序发帧：`toolUseEvent(A, 片段)`、`toolUseEvent(B, 片段)`、`toolUseEvent(B, stop)`、EOF（A 没有 stop）。用 `curl -N` 接收 `/v1/messages` 的流，记录 `content_block_start` 和 `content_block_stop` 的 index 顺序。重启服务后重复 5 次，比较不同进程间的 stop 顺序。
2. 纯工具调用：fake upstream 只发一个完整的 `toolUseEvent`。确认 SSE 中先出现 `{"index":0,"content_block":{"type":"text","text":""}}`。用官方 SDK 的 `stream.get_final_message()` 检查 content 的第一项是空 text。
3. 真实 Claude Code：在隔离环境中执行"读取三个文件"类任务，在会话 JSONL 中找 `type:"text","text":""` 的 assistant 块，确认它被写入了历史。

```bash
curl -sN http://127.0.0.1:19023/v1/messages -H 'x-api-key: <key>' -H 'content-type: application/json' \
  -d '{"model":"claude-sonnet-4-6","max_tokens":512,"stream":true,
       "tools":[{"name":"Read","description":"read","input_schema":{"type":"object","properties":{"file_path":{"type":"string"}}}}],
       "tool_choice":{"type":"any"},
       "messages":[{"role":"user","content":"read /etc/hosts"}]}' \
  | grep -E 'content_block_(start|stop)'
```

## 修复方案

### 候选方案

A. 只把 HashMap 换成 `BTreeMap`：stop 顺序变为确定的升序，但仍允许多个块同时打开。

B. 严格串行状态机（推荐）：任何新块开始前，先关闭当前打开的块；文本延迟开块；帧解析提前校验 prelude。

C. 在输出层重排：先缓存整条流，最后按 index 排序输出。这会破坏流式特性，不可取。

### 推荐方案

采用 B：

1. **串行化**：
   - `active_blocks` 改为 `BTreeMap`，或者记录"当前打开的块"。`handle_content_block_start` 在开始任何新块之前，先按 index 升序关闭所有已打开的块，不再只关闭 text 块。
   - tool_use 被提前关闭后，如果同一个 `tool_use_id` 又收到后续片段，它的 `input_json_delta` 无法再发出。实现上要先缓存、再在关闭时补发：对未 stop 的 tool_use，在新块开始前，用 `tool_input_buffers` 中已有的内容发出剩余 delta 并关闭，与 `flush_incomplete_tool_input_buffers` 的逻辑合并，并记录一条 warning 观测。
   - `flush_incomplete_tool_input_buffers` 按 block index 升序处理（先收集，再排序）。
   - `emit_text_delta_raw` 新开 text 块时，同样先关闭已打开的 tool_use 块。
2. **延迟开 text 块**：去掉 `generate_initial_events` 中预先打开的 text 块，让 `emit_text_delta_raw` 在第一个真实文本出现时再开块（它已经具备按需开块的能力）。需要同时检查依赖 `text_block_index == Some(0)` 的逻辑，以及 keepalive（`active_open_block_for_keepalive`，第 1286-1292 行）在"还没有任何块"时的行为，保证 `ping` 仍然正常发送。
3. **reasoning**：保持原子缓冲这一既定决策。可以增加一个可选的观测字段，记录"首个 thinking 事件的延迟"；如果将来要支持增量 thinking，只对**未签名**且 sanitizer 能确认前缀安全的内容做增量下发，单独立项。
4. **帧解析**：`parse_frame` 读完 12 字节 prelude 后立刻校验 prelude CRC，再判断长度和等待完整帧。message CRC 的校验时机保持不变。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)，只使用 `127.0.0.1:19023` 指定测试实例和隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，Cargo 通过 `feature/tests/run-cargo-scoped.sh` 运行，不得影响本机正在运行的 Claude Code CLI 与服务。

- 串行性：上面的 `assert_blocks_are_sequential` 作为通用断言，加到 stream 测试矩阵中所有涉及多块的用例（text→tool、tool→tool、tool→text、thinking→text→tool、stop 缺失、EOF flush），每个用例 20 轮，全部通过。
- 确定性：同一组上游事件在 20 个新建的 `StreamContext` 上回放，SSE 事件序列逐字节相同。
- 空 text：纯工具调用时，首个块是 index 0 的 tool_use；"文本+工具"时 text 仍在 index 0；thinking 模式行为不变。non-stream 与 stream 的 content 结构一致。
- 回归：[流式观测与 trivial 文本优化](../11-stream-observability-and-trivial-text-optimization.md) 的 trivial 文本过滤、[Thinking 与签名内容安全](../thinking-and-signed-content-safety.md) 的原子缓冲和 1 MiB 边界测试全部通过；keepalive 在长时间没有块的情况下仍然正常发送。
- 帧：prelude 损坏在 12 字节时就报错；合法的大帧（例如 1 MiB）解析不受影响；解码器恢复路径的测试（`src/kiro/parser/decoder.rs`）全部通过。
- 真实 Claude Code：多工具并行调用、只调用工具不输出文本两类任务各 5 次，会话 JSONL 中没有空 text 块，工具执行顺序与 index 顺序一致。

## 兼容性与风险

- 提前关闭未 stop 的 tool_use 会改变"上游晚到片段"的处理方式：以前能拼接到同一个块里，现在要在关闭时用已有的缓冲一次补齐。如果上游确实会交错发送两个工具的片段，第一个工具的后续片段会丢失。需要先用生产 trace 确认 Kiro 会不会交错发送；如果会，就改成"缓存非当前块的片段，等当前块结束后再输出"的方案，代价是增加延迟。
- 去掉预开的 text 块后，首个 SSE 块事件会推迟到模型的首个输出。`message_start` 仍然立即发送，客户端的"已连接"判断不受影响；如果有中转依赖"message_start 之后立刻出现 content_block_start"，需要回归验证。
- `BTreeMap` 比 `HashMap` 多一点开销，但活跃块数量很少，可以忽略。
- 帧解析改动只影响异常路径，风险很低。
