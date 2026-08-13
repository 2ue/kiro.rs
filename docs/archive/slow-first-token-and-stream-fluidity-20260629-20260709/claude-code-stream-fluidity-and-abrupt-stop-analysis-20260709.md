# Claude Code 流式卡顿、不流畅、突然结束问题分析

日期：2026-07-09  
范围：先完成资料学习、对比项目和本地代码分析；随后在本地代码中落地低风险修复，不触碰现网服务。

## 目标问题

用户反馈的现实问题不是单纯“首字慢”，而是 Claude Code CLI / VSCode Claude Code 插件使用时的交互体验：

- 状态点或红点长时间停住，没有可见内容输出。
- 输出不是持续增量，而是一块一块或一大段突然出来。
- 有时只输出一句后就结束，或者执行到中途突兀停止，但没有明确错误。
- 同一上游经过不同系统时，用户主观体验差异明显，所以不能只用“上游慢”解释。

本分析要回答的是：协议上什么情况会天然无可见输出，当前 `kiro.rs` 哪些处理会额外放大这种体验，对比 `sub2api` / `kiro-go` 有哪些具体差异，下一步应该如何做事实验证。

## 资料与证据来源

官方资料：

- Anthropic Messages streaming 文档：`https://docs.anthropic.com/en/api/messages-streaming`
- Anthropic fine-grained tool streaming 文档：`https://docs.anthropic.com/en/docs/agents-and-tools/tool-use/fine-grained-tool-streaming`
- Claude Code CLI reference：`https://docs.claude.com/en/docs/claude-code/cli-reference`
- MCP transports：`https://modelcontextprotocol.io/specification/2025-06-18/basic/transports`
- MCP progress：`https://modelcontextprotocol.io/specification/2025-06-18/basic/utilities/progress`

本地代码：

- 当前项目：`/Users/yuanfeijie/Desktop/procode/kiro.rs`
- 对比项目：`../sub2api`，已 `git pull --ff-only` 到 `6f43986c`
- 对比项目：`../kiro-go`，已确认 up to date
- 当前本机 Claude Code CLI：`2.1.197 (Claude Code)`
- 当前 VSCode 扩展：`anthropic.claude-code-2.1.145-darwin-arm64`，扩展内置 `resources/native-binary/claude`

## 协议事实

Anthropic Messages streaming 的标准事件顺序是：

1. `message_start`
2. 多个 content block，每个 block 包含 `content_block_start`、一个或多个 `content_block_delta`、`content_block_stop`
3. 一个或多个 `message_delta`
4. `message_stop`

关键点：

- `message_start` 只表示 assistant message 开始，`content` 为空，不代表用户能看到正文。
- `content_block_start` 只表示 block 打开，也不一定有可见正文。
- `ping` 只是保活事件，可以出现任意多个，不等于模型输出了文字。
- 真正能让用户看到“有内容在走”的核心是 `content_block_delta`：
  - `text_delta`：文本输出。
  - `thinking_delta`：可见 thinking 输出。
  - `input_json_delta`：工具参数流式输出。
  - `signature_delta`：thinking 签名，不是正文。
- `message_delta.usage` 是累计口径，且通常接近尾部出现；不能用它判断中间是否流畅。
- 流中允许出现 `error` event，客户端应该能按标准错误事件显示。

工具调用相关的官方事实更关键：

- 标准工具流下，当前模型可能一次只在完成一个完整 key/value 后再发 `input_json_delta`。
- 因此使用 tools 时，模型在生成某个大参数值期间可能天然存在一段没有 delta 的等待。
- `eager_input_streaming=true` 可以让工具输入跳过服务端 JSON buffering，更早返回大参数的碎片，但客户端要能处理 partial 或 invalid JSON。

Thinking 相关的官方事实：

- extended thinking 开启且 display 正常时，会通过 `thinking_delta` 流式返回 thinking。
- 如果 `display: "omitted"`，不会发送 `thinking_delta`；通常是 thinking block 打开，收到一个 `signature_delta`，然后关闭。
- 这种情况下连接和协议都正常，但 UI 很可能表现为“有状态，没有可见正文”。

MCP / Claude Desktop / Remote Control 相关边界：

- MCP 是 JSON-RPC 协议，stdio 传输用换行分隔，Streamable HTTP 可用 SSE 发送多个 server messages。
- MCP 的 progress 是独立的 `notifications/progress`，依赖请求 metadata 里的 `progressToken`，且接收方可以选择不发 progress。
- 所以 Claude Desktop / MCP 的工具执行进度不等同于 `/v1/messages` 的 SSE 文本流。网关能改善模型输出流和本系统代理的保活，但不能保证所有本地工具执行阶段都在 UI 上持续显示进度。

## Claude Code 客户端事实

本机 `claude --help` 显示：

- `--output-format stream-json` 是实时流式输出格式。
- `--include-partial-messages` 表示包含 partial message chunks，但只在 `--print` 和 `--output-format=stream-json` 下生效。
- `--remote-control`、`--ide`、`--include-hook-events` 等属于 Claude Code 自身运行面，不等同于上游 Messages SSE。

Cursor 旧版 Claude Code SDK 类型中有：

- `includePartialMessages?: boolean`：`/Users/yuanfeijie/.cursor/extensions/anthropic.claude-code-2.0.1-universal/resources/claude-code/sdk.d.ts:240`
- `SDKPartialAssistantMessage`：`type: 'stream_event'; event: RawMessageStreamEvent`，见 `sdk.d.ts:351`

SDK 运行时会以 `--output-format stream-json --input-format stream-json` 启动 Claude Code，并在 `includePartialMessages` 为 true 时追加 `--include-partial-messages`，见 `sdk.mjs:6391` 到 `sdk.mjs:6450`。

结论：

- Claude Code 有能力把底层 partial stream event 暴露出来。
- 但交互式 TUI / VSCode WebView 是否把 `ping`、空 delta、thinking signature、tool input delta 显示成“用户感觉有输出”，是客户端 UI 决策。
- 用户截图里的红点或状态点不能直接等同于上游错误；它也可能代表 busy、pending、permission mode、插件状态或当前 block 状态。需要用同一请求的 stream-json 时间线验证。

## 当前 kiro.rs 流式路径

当前 `kiro.rs` 有两条主要流式链路：

1. 本地 Kiro 凭证路径：`src/anthropic/handlers.rs` + `src/anthropic/stream.rs`
2. 外部池路径：`src/external_pool.rs`

### 本地 Kiro 凭证路径

入口会创建 `StreamContext`，然后立即生成初始 SSE 事件：

- `src/anthropic/handlers.rs:5153` 到 `src/anthropic/handlers.rs:5168`
- `src/anthropic/stream.rs:1586` 到 `src/anthropic/stream.rs:1612`

当前行为：

- 一开始就发 `message_start`。
- thinking 未启用时，还会立即创建一个空 text block。
- thinking 启用时，只发 `message_start`，等待后续真实内容决定 block 顺序。

这符合 Anthropic event flow，但对交互体验有副作用：客户端很早进入 assistant busy 状态，而后续如果长时间没有 `content_block_delta`，用户会看到“已经开始了，但没有内容”。

上游读取与解码：

- `src/anthropic/handlers.rs:5467` 使用 `response.bytes_stream()` 读上游。
- `src/anthropic/handlers.rs:5540` 先 feed 到 AWS eventstream decoder。
- `src/anthropic/handlers.rs:5552` 到 `src/anthropic/handlers.rs:5584` 只有解出完整 frame，才能转换为 Anthropic SSE event。
- `src/kiro/parser/decoder.rs:134` 到 `src/kiro/parser/decoder.rs:190` 表明 decoder 在数据不足时返回 `Ok(None)`，需要等更多字节。

这意味着：

- 上游 TCP chunk 到了，不代表 `kiro.rs` 能立刻产出下游 SSE。
- 如果上游 chunk 边界不等于 AWS frame 边界，`chunks_before_first_output` 会增加，`stream_gap_to_first_output_ms` 会变大。
- 这种等待是 binary eventstream 解帧要求带来的，但是否过大需要看真实日志。

保活：

- `PING_INTERVAL_SECS = 5`，见 `src/anthropic/handlers.rs:5178` 到 `src/anthropic/handlers.rs:5180`
- ping 内容是 `event: ping`，见 `src/anthropic/handlers.rs:5406` 到 `src/anthropic/handlers.rs:5408`
- ping 分支见 `src/anthropic/handlers.rs:5747` 到 `src/anthropic/handlers.rs:5750`

这能保持连接活着，但不能保证 Claude Code UI 把它当作可见进度。

SSE 响应头：

- `src/anthropic/envelope.rs:133` 到 `src/anthropic/envelope.rs:141` 设置了 `Content-Type: text/event-stream`、`Cache-Control: no-cache`、`Connection: keep-alive`、request id。
- 当前没有 `X-Accel-Buffering: no`。

如果部署链路上有 Nginx 或兼容代理，缺这个头会增加被代理缓冲的风险。它不是唯一原因，但它是和 `sub2api` 的明确差异。

### 本地 Kiro 路径的文本缓冲风险

`src/anthropic/stream.rs` 为了识别上游泄漏出的字面 `<invoke>` 工具调用，有一个统一明文出口：

- `create_text_delta_events()` 把文本追加到 `invoke_sniff_buffer`，见 `src/anthropic/stream.rs:2043` 到 `src/anthropic/stream.rs:2049`
- `MAX_INVOKE_HOLD_BYTES = 262_144`，见 `src/anthropic/stream.rs:2052`
- 如果检测到疑似真实 `<invoke>` 开始但没有闭合，remainder 小于上限时会继续持有，不下发，见 `src/anthropic/stream.rs:2089` 到 `src/anthropic/stream.rs:2110`

这是当前代码中最具体的“一大坨输出”候选来源之一。

它不是无界内存风险，因为有 256KiB 上限；但 256KiB 对交互流畅度已经非常大。如果模型普通文本里出现类似工具协议前缀，或者上游把 tool leak 分多段吐出，文本可能被本地嗅探逻辑滞留到闭合或超过 256KiB 后才放出。

### 本地 Kiro 路径的突然结束风险

`StreamContext::generate_final_events()` 里当前顺序值得警惕：

- 先处理 `stream_error`：`src/anthropic/stream.rs:2639` 到 `src/anthropic/stream.rs:2643`
- 然后才 flush `invoke_sniff_buffer`：`src/anthropic/stream.rs:2658` 到 `src/anthropic/stream.rs:2661`

因此存在一个代码级高风险候选：

- 如果普通文本正滞留在 `invoke_sniff_buffer`
- 同时上游发生 stream error 或读取异常
- 当前逻辑会先关闭 block 并发送 error，然后 return
- 滞留文本没有机会 flush

这可以解释“输出一句后突兀结束 / 中间少了一段 / 结束时没有把已到达但未放出的文本吐出来”的一类体验。是否已经在线上发生，需要用 request id 对照 stream error 和 buffered text 指标验证。

另一个相关点：

- decoder feed 溢出只 warn 并记录首输出前错误，见 `src/anthropic/handlers.rs:5540` 到 `src/anthropic/handlers.rs:5546`
- decode_iter 解析失败也主要 warn 和记录指标，见 `src/anthropic/handlers.rs:5600` 到 `src/anthropic/handlers.rs:5609`

读取响应流失败会转成标准 SSE error，见 `src/anthropic/handlers.rs:5640` 到 `src/anthropic/handlers.rs:5654`。但解帧错误是否应该更早转换为明确下游 error，需要独立验证，避免客户端表现为“突然没了但不知道为什么”。

### 外部池路径

外部池流式请求路径：

- `src/external_pool.rs:1598` 到 `src/external_pool.rs:1600` 判断响应是否 stream。
- `src/external_pool.rs:1613` 使用 `response.bytes_stream()`。
- `src/external_pool.rs:1638` 到 `src/external_pool.rs:1778` 在 `unfold` 中读 chunk、追加 buffer、drain SSE event。
- `src/external_pool.rs:4103` 到 `src/external_pool.rs:4122` 只有读到完整 SSE event delimiter 后才输出。
- delimiter 是 `\n\n` 或 `\r\n\r\n`，见 `src/external_pool.rs:4304` 到 `src/external_pool.rs:4318`。

当前外部池的“透传”不是原始字节级 raw passthrough，而是 SSE event 级 passthrough：

- `src/model/config.rs:2255` 到 `src/model/config.rs:2268` 当前 `ExternalPoolStreamResponseMode` 只有 `event_passthrough`。
- `src/external_pool.rs:4080` 到 `src/external_pool.rs:4100` 会先屏蔽上游 error event，再按 usage 配置决定是否改写 usage，否则返回原 event。

这条路径的含义：

- 普通 text/thinking/tool 事件如果已经是完整 SSE event，基本按 event 原样下发。
- 它不会等整段 assistant 完成，但会等一个完整 SSE event 结束。
- 如果上游本身迟迟不发 `\n\n`，或者上游工具参数使用标准 buffered 模式，`kiro.rs` 也不会有可见 delta。
- 如果目标是保留错误屏蔽和 usage 整形，纯 raw byte passthrough 不能直接替代现有逻辑；更合理的是保持 event boundary flush，并只在 usage/error 事件做轻处理。

## 对比 kiro-go

`../kiro-go` 当前流式实现的几个差异：

- SSE header 设置了 `Content-Type`、`Cache-Control`、`Connection`，见 `../kiro-go/proxy/handler.go:847` 到 `../kiro-go/proxy/handler.go:850`。没有看到 `X-Accel-Buffering: no`。
- `ensureMessageStart()` 是延迟调用的，不是一开始就发，见 `../kiro-go/proxy/handler.go:869` 到 `../kiro-go/proxy/handler.go:887`。
- text/thinking delta 会在 `sendText()` 中立即发 SSE，见 `../kiro-go/proxy/handler.go:966` 到 `../kiro-go/proxy/handler.go:1046`。
- 工具调用前会强制 flush 文本：`processClaudeText("", false, true)`，见 `../kiro-go/proxy/handler.go:1164` 到 `../kiro-go/proxy/handler.go:1167`。
- 每个 SSE event 后 `flusher.Flush()`，见 `../kiro-go/proxy/handler.go:1290` 到 `../kiro-go/proxy/handler.go:1293`。
- AWS binary eventstream 解析处注释明确直接读，避免 streaming response 被 bufio 增加延迟，见 `../kiro-go/proxy/kiro.go:416` 到 `../kiro-go/proxy/kiro.go:450`。

`kiro-go` 的取向更偏“等到真实输出再开始 message，并尽快 flush 每个输出事件”。这可能让用户主观上更少看到“状态开始但没字”的阶段。

## 对比 sub2api

`../sub2api` 当前有几个和流畅度强相关的实现：

- Anthropic APIKey passthrough 设置 `X-Accel-Buffering: no`，见 `../sub2api/backend/internal/service/gateway_anthropic_passthrough.go:377` 到 `../sub2api/backend/internal/service/gateway_anthropic_passthrough.go:390`。
- 通用 stream 响应也设置 `X-Accel-Buffering: no`，见 `../sub2api/backend/internal/service/gateway_upstream_response.go:650` 到 `../sub2api/backend/internal/service/gateway_upstream_response.go:654`。
- passthrough 路径用 scanner 逐行读上游，写到下游后在 SSE 空行边界 flush，见 `gateway_anthropic_passthrough.go:406` 到 `gateway_anthropic_passthrough.go:442`、`gateway_anthropic_passthrough.go:541` 到 `gateway_anthropic_passthrough.go:552`。
- passthrough 路径有 ping keepalive，见 `gateway_anthropic_passthrough.go:574` 到 `gateway_anthropic_passthrough.go:591`。
- 通用 stream 路径有标准 Anthropic SSE error event helper，见 `gateway_upstream_response.go:753` 到 `gateway_upstream_response.go:779`。
- 通用 stream 路径对 Claude Code 版本做特殊 keepalive：`shouldUseClaudeCodeNoopDeltaKeepalive()`，见 `gateway_upstream_response.go:42` 到 `gateway_upstream_response.go:48`。
- 版本门槛是 `claudeCodeNoopDeltaKeepaliveMinVersion = "2.1.193"`，见 `gateway_service.go:60`。当前本机 `2.1.197` 命中。
- 它会根据当前打开 block 类型构造空 `content_block_delta` keepalive，见 `gateway_upstream_response.go:50` 到 `gateway_upstream_response.go:82`。
- keepalive 分支在有活动 block 时优先发空 delta，否则发 ping，见 `gateway_upstream_response.go:1071` 到 `gateway_upstream_response.go:1082`。

这不是“输出真实文字”，但对 Claude Code UI 可能比 `ping` 更像“当前 content block 仍然在活动中”。用户截图里的“点停住但连接未断”场景，需要重点验证这类差异。

注意：`sub2api` 的 Anthropic passthrough 路径目前主要是 event boundary flush + ping；特殊 noop delta keepalive 在通用 stream 路径里。实际用户接入的是哪条路径，需要用请求路由日志确认。

## 症状映射

### 症状 1：状态点停很久，没有内容

可能来源按优先级：

1. `kiro.rs` 本地 Kiro 路径立即发 `message_start`，但后续长时间没有可见 `content_block_delta`。这会让 UI 进入 busy 状态，但用户看不到文字。
2. 官方标准工具流在生成完整 key/value 前可能不发 `input_json_delta`。工具调用多或工具参数大时，这是协议层天然等待。
3. thinking `display: omitted` 时不会有 `thinking_delta`，只会有签名和 block 开闭。用户看到状态但没有正文，符合协议。
4. 当前 `kiro.rs` 只发 `ping` 保活，Claude Code UI 不一定把 ping 当作可见进度。`sub2api` 对 Claude Code 2.1.193+ 的 noop delta keepalive 是明确差异。
5. 缺 `X-Accel-Buffering: no` 时，中间代理可能缓冲 SSE event，尤其在 Nginx 或类似反代路径下。

### 症状 2：输出一块一块、一大坨出现

可能来源按优先级：

1. `invoke_sniff_buffer` 对疑似 `<invoke>` 的明文最大可持有 256KiB，这是当前代码中最明确的本地攒输出机制。
2. 外部池 event 级透传必须等完整 SSE event delimiter。一般 event 很小，这不是问题；但如果上游把很大的 data event 一次性写完，或者迟迟不发空行，kiro.rs 只能等完整 event。
3. 官方标准工具流可能在完整参数 value 形成后才集中发多个 `input_json_delta`，这会表现为工具参数阶段一坨输出。
4. 代理缓冲或客户端 UI 批量渲染也会把已到达的事件集中展示。需要 raw SSE timeline 和 Claude Code stream-json timeline 对照。

### 症状 3：输出一句后突然结束或中途突兀停止

高风险代码候选：

1. `stream_error` 分支早于 `invoke_sniff_buffer` flush。上游错误发生时，已到达但滞留在 invoke sniff buffer 的文本可能被丢弃。
2. 上游读取错误会转标准 SSE error，但客户端 UI 未必明显展示；需要确认 stream-json 中是否出现 `event:error`。
3. decoder 层 parse/feed 错误主要 warn 和指标记录，是否应该对客户端明确 error 需要验证。
4. 如果客户端主动断开或 Claude Code 自身 max turns / permission / tool 执行逻辑结束，也可能表现为突兀停止，但这不属于 `/v1/messages` SSE 网关本身。

### 症状 4：红点或状态点不闪

目前不能直接判定为上游错误。

原因：

- VSCode 扩展和 Claude Code TUI 有自己的状态模型。
- 红点可能来自 permission mode、busy/pending/error、当前 block 状态或插件 UI 状态。
- 只有把同一时刻的 UI 表现、Claude Code `--output-format stream-json --include-partial-messages`、服务器 raw SSE/usage trace 对齐，才能确认它对应哪类事件。

## 当前 observability 能支撑的分类

当前 `kiro.rs` 已经有一些关键字段：

- `first_token_latency_ms`
- `upstream_header_ms`
- `first_upstream_chunk_ms`
- `first_thinking_delta_ms`
- `first_visible_text_delta_ms`
- `stream_gap_to_first_output_ms`
- `events_before_first_output`
- `chunks_before_first_output`
- `terminal_reason`

相关代码：

- 本地 Kiro latency trace：`src/anthropic/handlers.rs:1673` 到 `src/anthropic/handlers.rs:1930`
- 慢流日志字段：`src/anthropic/handlers.rs:2941` 到 `src/anthropic/handlers.rs:2985`
- 外部池 first output 判断：`src/external_pool.rs:2829` 到 `src/external_pool.rs:2855`

这些字段能把慢体验分成几类：

- 上游 header 慢：请求发出后上游不开始响应。
- 首个上游 chunk 慢：上游或网络没吐第一批字节。
- chunk 到了但没有 output：本地解帧、协议事件、thinking/tool/usage/ping、buffering 造成没有可见 delta。
- visible text 慢但 thinking 早：模型在 thinking，用户是否能看到取决于 thinking display 和客户端 UI。
- terminal_reason 异常：上游错误、idle timeout、client drop、stream error。

不足：

- 当前没有直接记录 `invoke_sniff_buffer` 持有时长/最大持有字节，这会让“一坨输出”类问题难以闭环。
- 也没有记录 ping keepalive 与 noop delta keepalive 的客户端差异，因为目前 `kiro.rs` 没有 noop delta keepalive。
- 外部池 event boundary 等待时间没有独立字段，只能从 chunk 与 first output 间接推断。

## 初步结论

不能把所有卡顿都归因于上游慢。当前代码和对比项目已经给出几个足够具体的本地候选原因：

1. **本地 Kiro 路径提前发送 `message_start`**  
   这符合协议，但会让 Claude Code UI 很早进入“正在响应”状态；如果后续还在 thinking/tool/key-value buffering 或上游未出可见 delta，用户就会感觉卡住。

2. **当前缺 `X-Accel-Buffering: no`**  
   `sub2api` 两条 stream 路径都加了该头，`kiro.rs` SSE builder 当前没有。在线上反代存在缓冲时，这会直接影响“持续流出”的体验。

3. **`kiro.rs` 只有 ping keepalive，没有 Claude Code 2.1.193+ 的 active-block 空 delta keepalive**  
   `sub2api` 已经针对 Claude Code 新版本做了这个兼容。当前用户本机 CLI 是 2.1.197，正处在该兼容范围内。这是非常值得验证的体验差异。

4. **`invoke_sniff_buffer` 最多可滞留 256KiB 明文**  
   这是当前项目里最明确的本地“攒一大坨再输出”的机制。它可能是为了解决工具协议泄漏，但交互流畅度代价偏高。

5. **stream error 先于 invoke buffer flush 是真实代码风险**  
   这个顺序可能导致已到达但未下发文本在错误路径丢失，从而表现为“中途突然结束”。

6. **外部池是 event 级透传，不是 byte 级 raw passthrough**  
   这本身不一定错，因为还要做 error mask 和 usage 整形。但它意味着完整 SSE event delimiter 之前不会下发，且当前只有一个 `event_passthrough` 模式。

7. **工具流和 thinking omitted 有协议层天然等待**  
   这不能当成所有问题的借口，但定位时必须区分：如果 upstream 本身没有可见 delta，网关最多能保活或发空 delta，不能凭空产生真实文字。

## 下一步事实验证方案

本轮没有修改代码，也没有跑真实 CLI 压测。下一步如果进入验证，建议按下面顺序做。

### 1. 单请求时间线验证

对同一个请求同时收集：

- 客户端 Claude Code `--output-format stream-json --include-partial-messages` 时间线。
- 服务器 usage trace：`first_upstream_chunk_ms`、`first_visible_text_delta_ms`、`stream_gap_to_first_output_ms`、`events_before_first_output`、`chunks_before_first_output`。
- raw SSE 时间线：每个 event 名称、data 长度、到达时间。

目标是把“用户看到没动”的时间段精确分类为：

- 没收到上游字节。
- 收到上游字节但没有完整 frame/event。
- 有 event 但只是 ping/message_start/signature/usage。
- 有 text/tool/thinking delta 但客户端没有展示。

### 2. 构造可控 fake upstream

用本地临时端口，不占用 9022，不打生产服务。构造这些 case：

- 正常 text delta 每 200ms 一个。
- 只发 `message_start` 后 20s 再发 text。
- 打开 thinking block 但 `display: omitted`，只发 signature。
- 标准 tool input，大 value 在 20s 后一次性发多个 `input_json_delta`。
- 伪 `<invoke>` 文本分片，确认 `invoke_sniff_buffer` 是否攒到明显延迟。
- 上游中途 stream error，确认 buffer 是否丢文本，客户端是否看到标准 error。
- 反代缓冲开/关，对比 `X-Accel-Buffering: no` 效果。

### 3. 对比 sub2api / kiro-go 的体验变量

只改变一个变量做 A/B：

- `message_start` 是否延迟到首个真实 content block 前。
- ping keepalive vs active-block 空 `content_block_delta` keepalive。
- `X-Accel-Buffering: no` 有无。
- invoke sniff buffer 上限和 flush 策略。
- event boundary flush 是否及时。

### 4. 现网日志只读分类

对最近慢请求做只读分析，不增加主路径压力：

- 按外部池 / 本地 Kiro 路径分开。
- 按 `stream_gap_to_first_output_ms` 大于 10s 分类。
- 统计 `events_before_first_output` 的事件类型分布。
- 重点找：chunk 很早到但 visible text 很晚的请求。
- 重点找：`terminal_reason=stream_error` 且输出很短的请求。

## 后续可选改进方向

这些是分析后的候选方向，不代表本轮已经实施：

1. SSE builder 增加 `X-Accel-Buffering: no`。
2. 评估本地 Kiro 路径是否延迟 `message_start`，或至少避免在长时间无 delta 时只靠 `message_start` 让客户端进入空忙状态。
3. 针对 Claude Code 2.1.193+ 增加 active content block 空 delta keepalive，保留 ping 作为无活动 block 时的保活。
4. 重构 `invoke_sniff_buffer`：降低最大持有量，或者可以先 flush 安全部分，只保留短 tail 做协议嗅探。
5. 在 stream error 前先 flush 已确认安全的 buffered text / invoke buffer，再发送标准 error event。
6. 明确 decoder fatal error 的下游行为：达到不可恢复条件时发标准 SSE error，而不是只 warn。
7. 外部池继续保持 event 级处理，但保证普通 event 不做额外解析；只对 error 和 usage event 做必要处理，并记录 event boundary 等待指标。

## 本轮边界

- 初始分析阶段没有修改代码；后续已按下方“落地修复”实施代码变更。
- 没有运行现网请求。
- 没有对用户真实 Claude Code 会话做侵入式抓取。
- 已完成官方资料、本地 CLI/扩展、本项目、`sub2api`、`kiro-go` 的只读对比分析。

## 2026-07-09 落地修复

本次修复只处理有代码证据支持、且不会改变模型真实输出语义的网关侧问题。

### 1. SSE 响应禁用反代缓冲

改动文件：

- `src/anthropic/envelope.rs`
- `src/external_pool.rs`

事实依据：

- 当前项目本地 Kiro SSE builder 只设置了 `Content-Type: text/event-stream`、`Cache-Control: no-cache`、`Connection: keep-alive`，缺少 `X-Accel-Buffering: no`。
- `sub2api` 的流式响应路径设置了 `X-Accel-Buffering: no`。
- 如果部署链路中存在 Nginx 或兼容反向代理，缺少该头会增加 SSE event 被代理缓冲后成块下发的风险。

实现：

- 本地 Kiro SSE builder 固定增加 `x-accel-buffering: no`。
- 外部池 stream 分支在转发响应头后增加 `x-accel-buffering: no`。
- 非流式外部池响应不额外增加该头。

边界：

- 这个修复不能让上游没产生的 token 变快；它只减少网关/反代把已经产生的 SSE 延迟到后面一块输出的概率。

### 2. Claude Code 新版本活动 block 空 delta 保活

改动文件：

- `src/anthropic/handlers.rs`
- `src/anthropic/stream.rs`

事实依据：

- 当前本机 Claude Code CLI 是 `2.1.197 (Claude Code)`。
- `sub2api` 对 `claude-cli/2.1.193+` 使用 active-block 空 `content_block_delta` 保活；本项目此前只发 `event: ping`。
- Claude Code UI 不一定把 `ping` 视为当前内容块仍在活动；空 delta 不是可见正文，但更贴近“当前 content block 还活着”的协议形态。

实现：

- 仅当入站 `User-Agent` 匹配 `claude-cli/x.y.z` 且版本 `>= 2.1.193` 时启用。
- 有打开的 `text` block 时，保活事件为：
  - `content_block_delta`
  - `delta.type=text_delta`
  - `text=""`
- 有打开的 `thinking` block 时，保活事件为：
  - `content_block_delta`
  - `delta.type=thinking_delta`
  - `thinking=""`
- 有打开的 `tool_use` block 时，保活事件为：
  - `content_block_delta`
  - `delta.type=input_json_delta`
  - `partial_json=""`
- 没有活动 block、客户端版本不命中、或 block 类型不支持时，继续发原来的 `event: ping`。

边界：

- 空 delta 不计入 usage，不参与首字判断，不改变最终 message_delta/message_stop。
- 它不能替代真实工具进度；标准工具流仍可能在完整 key/value 形成前没有真实 `input_json_delta`。

### 3. stream error 前 flush 本地嗅探缓冲

改动文件：

- `src/anthropic/stream.rs`

事实依据：

- 旧顺序是先处理 `stream_error`，然后才在正常收尾路径 flush `invoke_sniff_buffer`。
- 如果上游读流失败或返回 stream error 时，已有文本正在 `<invoke>` 嗅探缓冲里，旧逻辑会关闭 block 并发 error 后直接 return，缓冲内容没有机会下发。
- 这能解释一类“已经输出一部分，随后突然结束或少一截内容”的网关侧风险。

实现：

- 当 `stream_error` 已记录时，进入 error 分支前先：
  - `drain_invoke_sniff_buffer(true)`
  - `emit_queued_leaked_tool_uses()`
- 然后关闭打开的 content block，发送标准 SSE `error` event。
- 仍然不发送正常 `message_delta` 或 `message_stop`，保持错误流语义。

边界：

- 只 flush 已进入本地缓冲的数据；上游未发送的数据不可能恢复。
- 如果缓冲内容本身是未闭合的字面协议片段，错误收尾时会按普通文本下发，优先避免静默吞字。

## 本地新增测试

新增或覆盖的测试点：

- `anthropic::envelope::tests::sse_builder_disables_proxy_buffering`
- `external_pool::tests::stream_response_headers_disable_proxy_buffering`
- `anthropic::handlers::tests::claude_code_noop_delta_keepalive_is_version_gated`
- `anthropic::stream::tests::claude_code_noop_keepalive_matches_open_block_type`
- `anthropic::stream::tests::stream_error_flushes_held_invoke_sniff_text_before_error`

已通过的针对性命令：

- `cargo test claude_code_noop -- --nocapture`
- `cargo test buffering -- --nocapture`
- `cargo test stream_error_flushes_held_invoke_sniff_text_before_error -- --nocapture`
- `cargo fmt --check`
- `git diff --check`

后续已完成：

- 全量 `cargo test`
- `cargo build --release`
- 真实 Claude Code CLI 流式验证

## 本地真实验证证据

验证环境：

- 本地 release 二进制：`./target/release/kiro-rs`
- 临时服务端口：`127.0.0.1:19095`
- 启动参数：`KIRO_RS_HOST=127.0.0.1 KIRO_RS_PORT=19095 ./target/release/kiro-rs -c config.json --credentials credentials.json`
- Claude Code CLI：`2.1.197 (Claude Code)`
- Claude 配置隔离：
  - `HOME=/tmp/kiro-claude-home-19095`
  - `CLAUDE_CONFIG_DIR=/tmp/kiro-claude-config-19095`
- `ccman` 已在隔离 HOME 下切换到 `http://127.0.0.1:19095/cc`
- 测试模型固定为 `claude-sonnet-4-5`，服务端日志显示按 alias 解析为 `claude-sonnet-4.5`
- 未使用 `auto` 模型。
- 测试结束后已停止临时服务，`lsof -nP -iTCP:19095 -sTCP:LISTEN` 无监听残留。

注意：

- `ccman` 写入的隔离配置使用 `ANTHROPIC_AUTH_TOKEN`，Claude Code 2.1.197 在本次 `--bare --print` 验证里没有把它识别为登录态，首次尝试返回 `Not logged in · Please run /login`，服务端也没有收到请求。
- 为完成真实协议验证，后续命令在进程环境中显式注入 `ANTHROPIC_BASE_URL=http://127.0.0.1:19095/cc` 和 `ANTHROPIC_API_KEY`。API key 只来自本地开发库 runtime config 的客户端 key，没有写入源码，也没有输出到日志或文档。

### Direct SSE 检查

直接请求 `/cc/v1/messages`，使用 `claude-sonnet-4-5`、`stream=true`。

结果：

- HTTP 状态：`200 OK`
- 响应头包含：
  - `content-type: text/event-stream`
  - `x-accel-buffering: no`
  - `request-id`
  - `anthropic-request-id`
- SSE 事件统计：
  - 总事件数：39
  - 起始事件：`message_start`、`content_block_start`
  - 存在 `content_block_delta`
  - 存在 `message_stop`
  - 不存在 `error`

结论：

- 本地 Kiro SSE builder 的禁缓冲头已在真实 HTTP 响应中生效。
- 基础流式事件顺序满足 Anthropic-compatible 客户端预期。

### Claude Code CLI 复杂任务 1

命令形态：

- `claude --bare --print --verbose --output-format stream-json --include-partial-messages`
- `--model claude-sonnet-4-5`
- 工具允许：`Read,Grep,Glob,LS`
- 任务：读取并分析 `src/anthropic/stream.rs` 和 `src/anthropic/handlers.rs` 的流式实现。

结果：

- 退出状态：0
- 输出 JSONL 字节数：约 532 KiB
- JSONL 行数：448
- `stream_event` 数量：367
- 事件计数：
  - `message_start`: 3
  - `content_block_start`: 10
  - `content_block_delta`: 338
  - `content_block_stop`: 10
  - `message_delta`: 3
  - `message_stop`: 3
- delta 计数：
  - 非空 `text_delta`: 180
  - `thinking_delta`: 64
  - `input_json_delta`: 89
- 工具相关内容出现：是
- `error` event：无
- 最终 usage 非零：
  - `input_tokens`: 97
  - `cache_creation_input_tokens`: 1579
  - `cache_read_input_tokens`: 141447
  - `output_tokens`: 1371

结论：

- 真实 Claude Code CLI 能通过本地服务完成复杂工具任务。
- partial stream event 能持续暴露到底层 JSONL，不是只在最终聚合输出。
- thinking、工具输入、文本输出三类 delta 都被 Claude Code CLI 正常接收。

### Claude Code CLI 复杂任务 2

命令形态：

- 同样使用 `--bare --print --verbose --output-format stream-json --include-partial-messages`
- `--model claude-sonnet-4-5`
- 工具允许：`Read,Grep,Glob,LS`
- 任务：读取 `stream.rs`、`handlers.rs`、`external_pool.rs` 和本文档，对照实现和分析输出结构化报告。

结果：

- 退出状态：0
- JSONL 行数：982
- `stream_event` 数量：827
- 事件计数：
  - `message_start`: 5
  - `content_block_start`: 16
  - `content_block_delta`: 780
  - `content_block_stop`: 16
  - `message_delta`: 5
  - `message_stop`: 5
- delta 计数：
  - 非空 `text_delta`: 360
  - `thinking_delta`: 124
  - `input_json_delta`: 286
  - 空 delta：17
- `error` event：无
- 结果：
  - `is_error`: false
  - `num_turns`: 13
  - `duration_ms`: 88068
  - `duration_api_ms`: 92288
- 最终 usage 非零：
  - `input_tokens`: 92
  - `cache_creation_input_tokens`: 2793
  - `cache_read_input_tokens`: 343253
  - `output_tokens`: 3340
- 文本 delta 间隔统计：
  - 样本数：359
  - p50：52ms
  - p90：109ms
  - p99：165ms
  - max：33589ms

结论：

- 13 轮工具/模型交互完成，未出现突然结束、无声失败或 malformed tool 协议错误。
- `emptyDeltaCount=17` 说明 Claude Code 2.1.197 命中了新增的 active-block 空 delta 保活路径。
- 常规文本 delta 很密集；一次 33.6s 的最大间隔仍然存在，结合这次任务包含多轮工具调用和模型思考，这类 gap 更像工具执行、模型 planning 或上游无真实可见 delta 阶段，不能用网关已经生成但未 flush 的证据解释。

### 服务端日志证据

临时服务日志显示：

- 多次 `/cc/v1/messages` 命中本地路径。
- requested model 均为 `claude-sonnet-4-5`。
- upstream model 均为 `claude-sonnet-4.5`。
- 多个 request id 的 Kiro API 凭据调用链路结果为 `success`。
- 长上下文工具回合触发 payload guard，但 `still_oversized=false`，且最终 CLI 成功完成。

这说明本轮验证不是只跑了 trivial prompt，而是覆盖了：

- Claude Code CLI 真实调用。
- 工具输入流。
- thinking 流。
- 文本 partial message。
- 多轮 `/cc/v1/messages`。
- 长上下文 payload guard 参与后的继续流式输出。

## 更新后的结论

本次修复能降低网关自身造成的几类卡顿/突兀结束风险：

1. 已经生成的 SSE 不应被常见反代缓冲。
2. Claude Code 2.1.193+ 在活动 content block 长时间无真实正文时，会收到协议形态更贴近当前 block 的空 delta 保活，而不是只有 ping。
3. 上游 stream error 到来时，已进入本地 `<invoke>` 嗅探缓冲的内容会在 error 前 flush，不再因为错误收尾顺序被静默跳过。

仍需保留的现实边界：

1. 工具执行、标准工具参数 buffering、thinking omitted、上游 planning 阶段，仍可能天然没有可见文本。
2. 空 delta 不是模型正文，只能改善客户端“连接/当前 block 是否仍活动”的感知，不能伪造真实输出。
3. 外部池仍是 SSE event 级透传，不是 byte 级 raw passthrough；如果上游迟迟不发完整 SSE delimiter，本系统无法提前发出该 event。
