# Anthropic / Claude Code 兼容性检测失败复盘与修复路线图

**日期**: 2026-05-17  
**项目**: `kiro-rs`  
**文档目标**: 即使读者没有任何历史会话上下文，也能理解问题背景、当前代码状态、失败原因、已修复内容、剩余缺陷、不可修复边界和后续实施方案。  
**文档性质**: 分析与计划文档。本文件不代表代码已经完成对应修复。

## 1. 一句话结论

当前上游 Kiro 账号返回的模型可以是真实 Opus 4.7 / Claude Code 模型，但对外暴露的不是直连响应原貌，而是本项目把 AWS Kiro / CodeWhisperer event-stream 转换成 Anthropic-like SSE/JSON 后的合成信封。黑盒检测主要检查“通道特征”和“协议信封”，所以即使模型权重是真的，也可能在 LLM 指纹、结构完整性、行为验证、签名、多模态等项目上失败。

后续修复方向不是“证明模型是真的”，而是把代理层的外部特征尽量收敛到 Anthropic / Claude Code 客户端期望的形态，通过最大化的来补齐特征来实现在claude code cli等工具中正常使用。

## 2. 读者需要知道的背景

### 2.1 这个项目在做什么

`kiro-rs` 是把请求转换成 Kiro / CodeWhisperer / AWS 通道请求，然后把上游 event-stream 转换回 Anthropic 风格响应的服务。

简化链路：

```text
Claude Code CLI / Anthropic-compatible client
  -> kiro-rs /v1/messages 或 /cc/v1/messages
  -> Anthropic request 转 Kiro request
  -> AWS Kiro / CodeWhisperer event-stream upstream
  -> Kiro event-stream 转 Anthropic-like SSE / JSON
  -> client
```

这条链路有两个重要后果：

1. 模型输出内容来自上游模型，但响应外壳大多由 `kiro-rs` 转换。
2. 检测器如果看的是 Anthropic 官方通道特征，而不是只看自然语言答案，就会看到痕迹。

### 2.2 用户遇到的问题

用户用项目提供的接口检测 `opus 4.7` 模型时，黑盒检测显示多项失败：

- LLM 指纹验证失败
- 流结构完整性失败
- 行为验证失败
- 签名校验失败
- 多模态能力失败或受损

同时用户在 Claude Code CLI 真实使用中还观察到：

- 日志里多数是 cache creation，cache read 较少。
- 中间过程输出少，经常只看到读取文件、执行命令等工具事件，最终才输出完整结论。
- 希望模拟高缓存命中，但不能每次都是精确的 95%。
- 希望支持真实 Claude Code CLI 工具调用、think/plan/长会话、流式输出和缓存模拟。

### 2.3 检测器大概检查什么

用户提供的检测基准包含五项：

1. **LLM 指纹验证**：检查模型是否像真实 Claude，包括响应 ID、model 字段、headers、固定 prompt 行为、字段形态等。
2. **流结构完整性**：检查 SSE 事件序列是否符合 Anthropic Messages streaming 规范。
3. **非流结构完整性**：检查普通 JSON 响应结构是否符合 Anthropic Messages API。
4. **签名校验**：解析 thinking signature / Protobuf 签名，识别来源通道。
5. **多模态能力**：测试图片、文档、PDF、URL、File source 等能力。

### 2.4 官方行为基线

本分析按 Anthropic 当前公开文档作为外部基线：

- Streaming Messages: https://docs.anthropic.com/en/api/messages-streaming
- Extended thinking: https://docs.anthropic.com/en/docs/build-with-claude/extended-thinking
- Prompt caching: https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching
- Vision: https://docs.anthropic.com/en/docs/build-with-claude/vision
- PDF support: https://docs.anthropic.com/en/docs/build-with-claude/pdf-support

关键基线：

- 流式事件顺序应是 `message_start -> content_block_start/delta/stop -> message_delta -> message_stop`，可穿插 `ping` 和 `error`。
- extended thinking 流式响应中，`thinking_delta` 表示思考内容；`signature_delta` 应在对应 thinking block 的 `content_block_stop` 前出现，用于完整性校验。
- prompt cache usage 中，`cache_creation_input_tokens` 表示本次写入缓存的 tokens，`cache_read_input_tokens` 表示本次从缓存读取的 tokens，`input_tokens` 是未从缓存读也未用于创建缓存的输入 tokens。
- 图片和 PDF/document 支持 base64、URL、Files API 等 source 形态，但具体模型和接口能力会有限制。

## 3. 当前代码状态摘要

本文件基于 2026-05-17 当前工作区未提交代码分析。当前工作区已有用户修改，主要涉及：

- `Cargo.toml` / `Cargo.lock`
- `src/anthropic/converter.rs`
- `src/anthropic/handlers.rs`
- `src/anthropic/middleware.rs`
- `src/anthropic/router.rs`
- `src/main.rs`
- `src/model/config.rs`
- README / config / docs / docker compose 相关文件

已执行过的基础验证：

```text
cargo check
结果: 通过

cargo test
结果: 253 passed

cargo fmt --check
结果: 失败
原因: src/anthropic/converter.rs 多处测试调用行过长，需要 rustfmt 处理。
```

## 4. 当前已经改善的内容

### 4.1 thinking 后缀覆盖

位置：`src/anthropic/handlers.rs:1608`

当前 `override_thinking_from_model_name` 的策略是：

- 模型名包含 `thinking` 且调用方未传 `thinking` 时，才注入默认 thinking。
- 调用方已显式传 `thinking` 时保留原值。
- `output_config.effort` 也只在调用方未设置时填充。
- 最终目标要使其达到官方的think特征，因为必须要保证在claude code cli中正常使用，可以强制覆盖

### 4.2 远程 URL 多模态 source 已开始 materialize

位置：`src/anthropic/handlers.rs:295`

当前 `materialize_remote_multimodal_sources` 会处理 `source.type=url` 的 image/document：

- 下载远程 URL。
- 推断 media type。
- 替换为 base64 source。
- 传入后续 converter。

影响：

- 远程 URL 图片/文档不再一律在 converter 层失败。
- 但它仍不是 Anthropic 原生 URL source 透传，而是代理侧下载后转换。

### 4.3 增加了基础 SSRF 防护

位置：`src/anthropic/handlers.rs:522`

新增 `ensure_safe_remote_url` 和 `is_blocked_ip`：

- 拒绝 localhost、metadata、私有 IP、回环、链路本地、文档地址等。
- redirect 时也会重新检查目标 URL。

剩余风险：

- 当前主要检查 host 字符串或 literal IP。
- 如果域名 DNS 解析到内网地址，仍需增加解析后 IP 校验。
- 当前使用 `response.bytes()` 读完整 body 后再检查大小，大文件仍会先占用内存。

### 4.4 PDF 从“一律失败”变成“尝试文本抽取”

位置：`src/anthropic/converter.rs:641`

新增依赖：

- `pdf-extract = "0.7"`

当前处理策略：

- base64 PDF 解码。
- 优先用 `pdf_extract::extract_text_from_mem` 提取文本。
- 失败或空文本时用简易 fallback。
- 最终转为 `<document media_type="application/pdf">...</document>` 文本。

影响：

- 简单文本 PDF 可以工作。
- 扫描件、图片型 PDF、复杂表格、加密 PDF 仍可能失败。
- 这不是官方 PDF 视觉理解能力。

### 4.5 代理隐式改写开始可观测

位置：`src/anthropic/converter.rs:171`

新增 `ProxyWarnings`，可统计：

- `prefill_dropped`
- `orphan_tool_results`
- `orphan_tool_uses`
- `duplicate_tool_results`

响应头开关：

- 配置项：`exposeProxyWarnings`
- 默认：`false`
- 开启后通过 `x-kiro-rs-warnings` 输出。

影响：

- 调试时可以知道代理做了哪些兜底改写。
- strict 检测时应保持关闭，否则自定义 header 会暴露代理特征。

### 4.6 prompt cache 模拟不再是精确固定比例

位置：

- `src/anthropic/prompt_cache.rs:17`
- `src/anthropic/prompt_cache.rs:454`

当前策略：

- `promptCacheSimulationMode` 默认 `disabled`。
- 本地模拟模式只保留 `local-prompt-cache`。
- `promptCacheTargetReadRatio` 是目标中心值，默认 `0.85`。
- 有 `TARGET_READ_RATIO_SPREAD = 0.03`，会按 prompt fingerprint 做确定性浮动。
- 没有 stable conversation id 时不模拟。
- 上游 metadata usage 优先于本地模拟 usage。

影响：

- 比每次精确 95% 更自然。
- 但它仍是 usage 层模拟，不是真实 Anthropic prompt cache。

## 5. 当前仍然失败或高风险的点

### 5.1 LLM 指纹验证仍高风险

#### 当前问题

`message.id`、`request id`、headers 需要保证符合特征。

证据：

- 流式 `message.id`：`src/anthropic/stream.rs:594`
- 非流式 `message.id`：`src/anthropic/handlers.rs:1581`
- WebSearch `message.id`：`src/anthropic/websearch.rs:244`
- internal request id：`src/anthropic/handlers.rs:288`

当前形态主要是：

```text
msg_<uuid hex>
req_<uuid hex>
```

而 Anthropic 官方示例中的 `msg_` 看起来不是 UUID hex 形态。检测器如果检查 ID 字符集、长度、前缀、字段顺序，很容易识别代理。

#### 需要修改

1. 抽出统一 ID 生成器，例如 `AnthropicIds`。
2. stream、non-stream、websearch、error 全部使用同一 ID 策略。
3. 替换 UUID hex 形态。
4. 给 response 统一补 `request-id` 类 header。
5. 保持 `x-kiro-rs-warnings` 只在 debug profile 输出。

### 5.2 流结构完整性仍未完全收口

#### 当前已有改善

原生 reasoning signature 已经按 `signature_delta` 在 thinking block stop 前发送。

证据：

- `src/anthropic/stream.rs:1127`
- `src/anthropic/stream.rs:1157`

#### 当前问题

1. SSE event 仍分散用 `serde_json::json!` 拼装，字段顺序和缺省行为不可控。
2. XML thinking 提取路径会生成无签名 thinking block。
3. XML thinking 结束时会额外发空 `thinking_delta`。
4. WebSearch 路径独立合成 SSE，不复用主状态机。
5. ping 固定 25 秒，不能按 profile 调整。
6. `message_start.usage` 可能先估算或模拟，最终再在 `message_delta` 修正；实用但不一定完全匹配官方节奏。

#### 需要修改

1. 新建统一 SSE builder。
2. 用固定结构体或有序序列化替代散落 `json!`。
3. strict profile 下禁用无签名 thinking block。
4. WebSearch 复用统一 envelope / event builder。
5. 为以下场景建立 golden tests：
   - 纯文本流
   - tool_use/input_json_delta
   - native thinking + signature_delta
   - redacted_thinking
   - error event
   - web_search
   - client dropped / upstream timeout

### 5.3 非流 JSON 结构仍需集中管理

#### 当前问题

非流响应仍在 `handle_non_stream_request` 里手工 `json!` 拼装。

证据：

- `src/anthropic/handlers.rs:1579`

风险：

- 字段顺序、空字段、错误结构、usage 结构和流式路径容易漂移。
- XML thinking 提取仍会生成无 signature 的 thinking block。
- error response 没有统一 Anthropic envelope/header builder。

#### 需要修改

1. 抽出 `AnthropicMessageResponse` / `AnthropicErrorResponse` 结构。
2. 统一 stream/non-stream 的 usage 构造逻辑。
3. strict profile 下关闭非原生 thinking 提取。
4. 错误响应统一加 request id 和标准 JSON shape。

### 5.4 行为验证仍会被 prompt 改写污染

#### 当前问题

行为验证失败不一定是模型不真，而是实际送到 Kiro 的 prompt 已不同于客户端原始输入。

关键改写：

- 系统消息追加 `SYSTEM_CHUNKED_POLICY`：`src/anthropic/converter.rs:90`、`src/anthropic/converter.rs:1052`
- thinking XML 前缀注入：`src/anthropic/converter.rs:996`
- system 转为 user + assistant 配对：`src/anthropic/converter.rs:1031`
- prefill assistant 丢弃：`src/anthropic/converter.rs:324`
- tool_use/tool_result 过滤和去重：`src/anthropic/converter.rs:810`
- WebSearch 被本地特殊路由接管：`src/anthropic/handlers.rs:943`

这些处理对 Claude Code CLI 的可用性可能有价值，但会改变固定 prompt 的输出分布。

#### 需要修改

引入兼容 profile：

```text
compatProfile = "claude-code" | "anthropic-strict"
```

建议语义：

- `claude-code`: 保留工具兜底、chunk policy、必要的 history 修复，优先真实 CLI 可用。
- `anthropic-strict`: 尽量减少 prompt 改写，优先检测结构、行为指纹和外部协议形态。

至少需要 profile 化的行为：

- `SYSTEM_CHUNKED_POLICY`
- thinking XML 前缀
- system user/assistant 配对文本
- prefill 丢弃策略
- orphan tool_use/tool_result 策略
- WebSearch 特殊合成路径

### 5.5 签名校验存在结构性边界

#### 当前事实

Anthropic extended thinking 的 signature 用于 thinking block 完整性验证。官方流式文档说明 `signature_delta` 会在 thinking block stop 前发送。

当前项目可以做的是：

- 如果 Kiro 上游事件里有 `reasoning.signature`，就按 Anthropic SSE 形态透传为 `signature_delta`。
- 非流 native thinking 可以在 content block 上带 `signature`。
- 模拟 Anthropic 官方私钥签名。
- 补齐 XML 提取出来的 `<thinking>` 文本补真实 signature。

#### 需要修改

1. 明确签名策略：补齐官方策略的签名，因为模型是从aws平台出来，必然为真。
2. strict profile 下无 signature 的 thinking 输出为 thinking block。
3. 对 history 中 thinking signature 丢失输出 warning 或记录。

### 5.6 多模态能力仍不等价官方

#### 当前已有能力

- base64 image
- data URL image
- URL image/document materialize
- 简单 PDF 文本抽取

#### 当前缺口

1. `source.type=file` 未实现：
   - image file：`src/anthropic/converter.rs:540`
   - document file：`src/anthropic/converter.rs:589`
2. document/PDF 最终被降级为文本包裹：
   - `src/anthropic/converter.rs:634`
3. PDF 不是视觉理解，仅文本抽取。
4. URL 下载未做流式限流。
5. DNS 解析后 IP 未校验。

#### 需要修改

1. 实现 Files API adapter，至少支持常见 `file_id` 检测样本。
2. PDF 支持分层：
   - `reject`: 明确不支持，返回官方风格错误。
   - `text-extract`: 当前能力，声明只提取文本。
   - `page-image`: 将 PDF 页转图片，走视觉模型。
3. 远程 URL 下载改为流式读取并设置硬限制。
4. SSRF 防护增加 DNS resolved IP 检查。
5. 增加多模态测试矩阵。

### 5.7 prompt cache 模拟需要明确边界

#### 当前正确方向

本地 cache 模拟已经不是“凭空固定 95%”：

- 需要 stable conversation id。
- 同 credential / conversation / model scope 下才会读。
- 有目标比例浮动。
- 上游 metadata usage 优先。

#### 剩余风险

1. 它仍是 usage 模拟，不是真 Anthropic prompt cache。
2. 检测器如果测缓存命中延迟、cache breakpoint、精确复用时序，仍可能识别。
3. 用户容易误解 `promptCacheTargetReadRatio=0.95` 是每次都 95%。

#### 需要修改

1. 文档明确 `promptCacheTargetReadRatio` 是目标中心，不是保证比例。
2. Admin usage 明确标注 `UpstreamMetadata` 与 `LocalPromptCache`。
3. 测试覆盖：
   - 首轮 creation
   - 同 session 二轮 read
   - 不同 session 不 read
   - 不同 credential 不串读
   - 增长会话仍能读旧前缀
   - 目标比例在范围内波动

## 6. 修复目标分层

后续不要把所有目标混在一个模式里。建议分成三类。

### 6.1 Claude Code 可用性目标

目标：

- Claude Code CLI 可以正常跑真实任务。
- 工具调用稳定。
- think / plan / 长会话不报错。
- 流式输出尽可能及时。
- 缓存 usage 对 Claude Code 观感合理。

可以接受：

- 为了 Kiro 上游兼容，对 prompt 做少量必要改写。
- 输出 `x-kiro-rs-warnings` 供调试，但默认关闭。

### 6.2 Anthropic strict shape 目标

目标：

- 尽量通过黑盒结构检测。
- 减少 ID/header/SSE/JSON 形态差异。
- 减少 prompt 改写造成的行为偏移。
- 不输出无法校验的 thinking block，按官方补齐输出，我希望正常在claude code cli工具中使用。

### 6.3 Debug / observability 目标

目标：

- 让开发者知道代理做了哪些兜底动作。

特征：

- 可开启 `x-kiro-rs-warnings`。
- 可记录转换前/转换后摘要。
- 可暴露 usage source。

## 7. 推荐实施路线

### P0: 先修工程质量和文档一致性

任务：

1. 运行 `cargo fmt`，修复当前 `cargo fmt --check` 失败。
2. 更新 `/cc/v1` 过期注释。
3. 补充 `exposeProxyWarnings` 到 README 和 `config.example.json`，或决定移除。
4. 给 SSRF、PDF、warnings header 新增单元测试。

验收：

```text
cargo fmt --check
cargo check
cargo test
```

全部通过。

### P1: 建统一 Anthropic envelope 层

任务：

1. 新增 `anthropic/envelope.rs` 或等价模块。
2. 集中管理：
   - message id
   - request id
   - standard headers
   - error response
   - stream response headers
   - non-stream response body
3. 替换：
   - `stream.rs` 的 `msg_` 生成
   - `handlers.rs` 的 `msg_` / `req_` 生成
   - `websearch.rs` 的 `msg_` 生成
4. 保持 debug headers 只在 debug profile 输出。

验收：

- stream/non-stream/websearch ID 形态一致。
- 所有响应都有统一 request id。
- strict profile 下无自定义 debug header。

### P2: 建统一 SSE event builder 和 golden tests

任务：

1. 新增统一 event builder。
2. 普通文本、tool_use、thinking、redacted、web_search 都走统一 builder。
3. 删除或收敛散落的 `json!` event 拼装。
4. golden tests 固定关键事件序列。

验收：

- `message_start -> content_block_start -> content_block_delta -> content_block_stop -> message_delta -> message_stop` 主路径稳定。
- error 路径只发 error，不发正常 stop。
- native thinking 一定在 stop 前发 `signature_delta`。
- 无 signature thinking 在 strict profile 下不会作为 thinking block。

### P3: 引入 compat profile

建议配置：

```json
{
  "compatProfile": "claude-code"
}
```

可选值：

```text
claude-code
anthropic-strict
debug
```

任务：

1. 在 config 中增加 profile。
2. 将以下行为按 profile 控制：
   - `SYSTEM_CHUNKED_POLICY`
   - thinking XML 前缀
   - non-stream XML thinking extraction
   - `x-kiro-rs-warnings`
   - WebSearch 合成路径
   - orphan tool cleanup 行为
3. README 明确每个 profile 的目标和取舍。

验收：

- Claude Code profile 通过真实 CLI 回归。
- Anthropic strict profile 的 detector-style fixtures 更接近官方响应。

### P4: 多模态能力分层补齐

任务：

1. 实现或明确拒绝 Files API。
2. PDF 支持分层：
   - text extraction
   - page image conversion
   - reject
3. URL 下载流式限流。
4. DNS resolved IP SSRF 校验。
5. 多模态 fixtures 覆盖：
   - base64 image
   - URL image
   - base64 text document
   - URL PDF
   - file image/document
   - oversized source
   - redirect to private IP

验收：

- 不支持项返回明确官方风格错误。
- 支持项不静默降级。

### P5: prompt cache 模拟语义稳定化

任务：

1. 文档强调目标比例是范围中心。
2. usage record 明确 source。
3. 建 detector-style cache 测试。
4. 避免跨 session / credential / model 串读。

验收：

- 首轮 creation、二轮 read 行为稳定。
- 不同 scope 不串读。
- 高缓存不是每次精确固定值。

## 8. 测试计划

### 8.1 单元测试

必须覆盖：

- ID 生成格式和唯一性。
- response headers。
- SSE event 顺序。
- native signature 顺序。
- strict profile 下无签名 thinking 降级。
- WebSearch 事件结构。
- 多模态 URL materialization。
- SSRF blocked hosts / blocked IP / redirect blocked。
- PDF text extraction success/failure。
- prompt cache scope isolation。

### 8.2 本地 API fixture 测试

用 curl 或脚本覆盖：

- `/v1/messages` 非流。
- `/v1/messages` 流式。
- `/cc/v1/messages` 流式。
- tool_use / tool_result。
- thinking enabled/adaptive。
- image base64。
- image URL。
- PDF/document。
- cache_control 多轮请求。

### 8.3 真实 Claude Code CLI 回归

固定测试目录，使用最大权限但限制在测试目录内：

- 项目分析任务。
- 深入分析任务。
- plan 模式。
- think 模式。
- Read / Grep / Glob / Bash / Edit / Write。
- 长会话 resume。
- 多轮工具调用。
- 错误恢复。

验收关注：

- 是否正常输出中间过程。
- 是否出现工具调用 JSON/SSE 解析错误。
- 是否出现 session id 冲突。
- 是否 cache read 随会话增长出现。
- 后端 usage records 是否合理。

### 8.4 detector-style 测试

模拟用户的五项检测：

1. LLM 指纹：ID、headers、model、字段形态。
2. 流结构：SSE 事件序列和 thinking signature 位置。
3. 非流结构：JSON shape、usage、stop_reason。
4. 签名：只验证透传位置，不承诺 Anthropic 官方私钥来源。
5. 多模态：图片、PDF、URL、file source。

## 9. 其他改造

### 9.1 Anthropic 官方签名模拟

解析 Anthropic 官方私钥签出的 protobuf signature，并验证通道来源为 Anthropic direct API，优先使用这个，如果没有或者不对，那么 Kiro/AWS 通道请补齐代理来模拟官方签名，因为我的模型就是aws出来的，就是真的。

### 9.2 headers补齐

项目可以提供类似Anthropic organization、Cloudflare `cf-ray`、官方 rate limit 计数、官方 request id的兼容形态，但不能声称它们是官方真实来源。



## 10 最终目的

在claude code cli等工具以及兼容cc协议的工具中正常使用任何功能，比如工具调用，长会话等等
并且保证符合官方特征来使其调用不出错，保证用户使用稳定性