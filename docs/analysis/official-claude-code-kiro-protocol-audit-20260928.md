# 官方 Claude Code / Anthropic 与 Kiro 协议互操作审计

日期：2026-09-28（Asia/Shanghai）  
工作模式：先 `audit` 只读分析；随后按用户要求实施 reasoning fallback 修复、
本地真实验证并准备发版  
目标：解释当前协议错误的根因，判断当前转换是否符合官方 Anthropic/Claude Code
预期，区分必须修复、可以优化、暂不应修改和证据不足的事项。

## 结论摘要

### 当前主错误的确定根因

远端当前出现的错误：

```text
API Error: 400 reasoning was requested, but both native reasoning fields
and compatible thinking prompt controls are unavailable
```

根因已经由远端配置、源码因果链和真实请求共同证明：

1. Claude Code/Anthropic 请求确实携带了 reasoning 意图，表现为
   `thinking.type=enabled/adaptive` 或 `output_config.effort`。
2. 远端 Kiro 模型能力发现状态是不完整的：`4/5 cohorts observed`，
   因此当前模型的 `KiroReasoningCapabilityState` 为 `Unknown`。
3. `Unknown` 状态不会生成 Kiro 的 `additionalModelRequestFields` 原生推理字段。
4. 远端又显式关闭了 `promptSteering.enabled`，所以兼容的 thinking prompt
   fallback 也不会注入。
5. 两条可以承载 reasoning 的线路同时不可用，转换器在发往 Kiro 之前返回
   `ConversionError::UnsupportedContent`，因此请求被本地以 HTTP 400 拒绝。

这不是：

- Kiro 已经收到请求后返回的 reasoning 错误；
- 账号无效或 token 失效；
- 普通模型请求不可用；
- `model=unknown` 本身造成的错误；
- 之前被误提到的 `503 No available accounts`。

普通请求在同一远端服务上成功，进一步证明错误发生在 reasoning 转换能力路径，
而不是通用网络或账号调度路径。

### 当前现场与发布状态

- 远端：`152.53.243.159:59137`，用户已切回 `ghcr.io/2ue/kiro-rs:0.0.174`。
- `v0.0.175` 已成功发布，GitHub Actions run `36419453699` 的 quality、
  Linux amd64、Linux arm64 和 manifest jobs 全部成功。
- `v0.0.175` 的发布提交为
  `9c27f881abc0b5100c7d0b39166c166406f1db26`。
- 发布成功不等于 reasoning 400 已解决；该问题跨 `v0.0.174`/`v0.0.175`
  的配置和能力发现状态仍需单独处理。
- 本文后续修复阶段已在当前工作树实现 explicit reasoning fallback 修复：
  `promptSteering.enabled=false` 不再阻断客户端显式 reasoning 的专用
  thinking 兼容传输；普通运营方 prompt 注入仍受总开关控制。

### 本次实际做了什么

- 读取官方 Anthropic Messages、thinking、streaming、errors 和 tool-use 文档。
- 核对 Claude Code -> Anthropic -> Kiro 的请求、历史、事件、错误和 usage
  转换源码。
- 对远端运行版本做只读配置和真实 API 验证。
- 在指定本地实例 `127.0.0.1:19023` 使用真实 Claude Code CLI 验证普通、
  thinking 和工具请求。
- 复核签名失败、签名重试后 too-long、HTTP 200 流内错误和 `model=unknown`
  的证据。
- 审计阶段没有修改 Rust 业务代码、默认配置、数据库、Redis、容器、进程或
  远端账号。随后修复阶段修改了
  `src/anthropic/converter.rs`、`src/anthropic/handlers/local_body_pipeline.rs`
  和 `src/model/config.rs`，并补充回归测试。

## 证据边界和版本口径

### 远端现场

远端只读证据来自 `152.53.243.159:59137`。当前现场版本按用户最后说明为
`v0.0.174`。此前生产记录中还存在 `v0.0.173` 的历史请求，尤其是
`THINKING_SIGNATURE_INVALID -> CONTENT_LENGTH_EXCEEDS_THRESHOLD` 记录；不能
把这些历史记录直接当成 `v0.0.174` 或 `v0.0.175` 的当前现场事实。

### 本地验证实例

按照项目测试实例约束，使用：

- 监听地址：`127.0.0.1:19023`
- 配置：`tmp/thinking-budget-local/config.json`
- 独立 PostgreSQL：`kiro_thinking_budget_20260901`
- Redis：`127.0.0.1:26379/0`
- Claude Code：`2.1.280`
- 本地候选二进制 SHA-256：
  `0bcbe543417999db779ccc73f0f9b566115817d4807785e0ffa9ebcf056803a2`

该实例是项目已有实例。修复验证阶段按项目测试约束原地替换为候选二进制，
使用同一端口、配置、PostgreSQL 和 Redis。

### Kiro “官方协议”的证据级别

没有找到公开、稳定、可引用的 Kiro 内部 REST/EventStream schema。本文把证据
分成三层：

1. Anthropic 官方公开协议：可作为硬性兼容要求。
2. 当前仓库的 typed model、parser、converter 和真实 Kiro 运行观测：可作为
   当前实现事实。
3. 社区实现、逆向结构或 `sub2api-kiro`：只能作为设计参考，不能称为
   Kiro 官方规范。

因此，`conversationState`、`reasoningContent`、
`additionalModelRequestFields`、`reasoningContentEvent` 等字段在本文中被称为
“当前 Kiro 运行时协议/实现约定”，不称为公开官方规范。

## 官方 Anthropic 协议逐项对照

官方参考：

- [Messages API](https://platform.claude.com/docs/en/api/messages)
- [Streaming Messages](https://platform.claude.com/docs/en/api/messages-streaming)
- [Errors](https://platform.claude.com/docs/en/api/errors)
- [Thinking](https://platform.claude.com/docs/en/build-with-claude/thinking)
- [Extended thinking](https://platform.claude.com/docs/en/build-with-claude/extended-thinking)
- [Preserved thinking](https://platform.claude.com/docs/en/build-with-claude/preserved-thinking)
- [Tool use overview](https://platform.claude.com/docs/en/agents-and-tools/tool-use/overview)

### 请求字段

官方 Messages 请求至少需要由网关保留并正确解释：

- `model`
- `messages`
- `max_tokens`
- `stream`
- `system`
- `tools`
- `tool_choice`
- `thinking`
- `output_config.effort`

当前入口先解析 `MessagesRequest`，再进行模型解析和本地转换。没有证据表明
普通 `model` 字段在进入转换前会丢失；“页面显示 `model=unknown`”来自入口拒绝
usage factory 的固定默认值，属于可观测性缺陷，不是请求没有 model。

### Thinking / reasoning

官方允许：

- `thinking.type=enabled`
- `thinking.type=adaptive`
- `thinking.type=disabled`
- `thinking.budget_tokens`
- `output_config.effort`

官方的新模型优先使用 `adaptive`；`output_config.effort` 是兼容的 effort
控制。当前代码能识别这些请求并把它们归类为
`requested_native_reasoning(req)`，但能否生成 Kiro 原生字段取决于能力发现
结果。

### Signed thinking 和 redacted thinking

官方要求：

- `thinking` block 的 `signature` 是不透明值，不能解析、重算、修改或替换。
- `redacted_thinking.data` 是不透明/加密值，不能当普通文本清洗。
- 多轮继续时，合法的 thinking/redacted thinking 必须原样保留。
- 工具循环中必须保留 thinking、redacted thinking、tool_use、tool_result
  的顺序和配对。
- 删除、重排、过滤、重建或改变前置 `system`、`tools`、messages，均可能
  使后续 signature 失效。

当前 Kiro history model 的 `AssistantMessage` 只有一个
`reasoning_content: Option<ReasoningContent>`，而不是 Anthropic content block
数组。这使单个 assistant 消息中的多个或交错 signed thinking 存在结构性限制，
当前 converter 对此采取保守拒绝。

### Streaming SSE

官方顺序为：

```text
message_start
content_block_start
content_block_delta...
content_block_stop
message_delta
message_stop
```

thinking stream 还要求：

- `thinking_delta` 出现在 thinking block 内；
- `signature_delta` 在 thinking block 关闭前发送；
- 最终 `message_delta.usage` 是权威累计 usage；
- HTTP 200 后仍可能出现 SSE `error` event。

当前 `SseStateManager` 明确维护 block start/delta/stop、单个
`message_start`、单个 `message_delta` 和最终 `message_stop`。真实本地 direct
stream 已验证顺序，native reasoning 的 signature 也有针对
`signature_delta < content_block_stop` 的测试。

### Errors

官方语义大致为：

- 400：`invalid_request_error`，请求字段/内容不合法；
- 429：速率限制；
- 529：服务过载；
- 每个响应有 `request-id` header；
- 错误 body 包含 `error.type` 和 `error.message`；
- 流已经以 HTTP 200 开始后，错误通过 SSE `error` event 表达。

当前实现把“本地无法转换 reasoning”映射为 HTTP 400。对于
`AuthoritativeAbsent/Invalid`（确实不支持）可以接受；但对于 discovery
`Unknown`（能力发现暂时不完整），把它当成客户端请求格式错误并不理想，建议
后续区分“请求不支持”和“网关能力尚未就绪”的公共状态。

### Tools

官方 tool loop 要求：

```text
assistant: tool_use
user: tool_result
assistant: next response
```

`tool_use` 与 `tool_result` 必须保留 id、顺序和输入/结果语义。当前本地真实
Claude Code 验证产生了 1 个 `tool_use` 和 1 个 `tool_result`，最终文本为
`tool-ok`。工具 schema 名称和 property key 的映射存在本地确定性映射和
collision 检测，但多轮历史与 signed thinking 交错仍需更完整的真实上游矩阵。

## 当前实现的因果链

### Claude Code 请求进入本地转换

主要入口：

- `src/anthropic/handlers/request_entry.rs`
- `src/anthropic/handlers/local_body_pipeline.rs`
- `src/anthropic/handlers.rs`
- `src/anthropic/converter.rs`
- `src/anthropic/converter/model.rs`
- `src/anthropic/converter/history.rs`

本地 pipeline 先做 JSON 解析、模型 resolution，再把
`KiroReasoningCapabilityState` 传给 converter。转换失败会返回
`invalid_request_error`，同时将有限诊断写入 `localBodyPrepareDiagnostic`。

### reasoning 400 的源码因果链

`src/anthropic/converter/model.rs` 的
`build_additional_model_request_fields()` 仅在能力状态为
`Supported` 或有合法 legacy fallback 时生成原生字段；`Unknown`、
`AuthoritativeAbsent` 和 `AuthoritativeInvalid` 都返回 `None`。

`src/anthropic/converter.rs` 随后执行：

```text
additional_model_request_fields == None
AND requested_native_reasoning(req)
AND !inject_thinking_prefix()
    => UnsupportedContent(
         "reasoning was requested, but both native reasoning fields and
          compatible thinking prompt controls are unavailable"
       )
```

修复前，`inject_thinking_prefix()` 只有下列条件全部满足才为真：

- `promptSteering.enabled`
- `thinkingPromptControls`
- `promptSteering.thinking.enabled`
- 当前 compat profile 允许该类注入

修复后，显式客户端 reasoning 请求会设置
`explicit_reasoning_request=true`，仅重新允许专用 thinking 兼容传输：

```text
(promptSteering.enabled OR explicit_reasoning_request)
AND thinkingPromptControls
AND promptSteering.thinking.enabled
AND 当前 compat profile 允许该类注入
```

这不会恢复 language/task/custom prompt、tool-choice prompt、chunked-write prompt
或自动 thinking trigger；也不会在 capability `Unknown` 时伪造 Kiro 原生
`additionalModelRequestFields`。

### 远端配置和能力状态

远端只读快照：

```text
compatProfile = claude-code
bodyConversion.nativeReasoningFields = true
bodyConversion.thinkingPromptControls = true
promptSteering.enabled = false
promptSteering.thinking.enabled = true
thinkingTriggerMode = real_request
modelResolutionMode = compatible
payloadGuardEnabled = true
payloadGuardMode = on_too_long
payloadGuardMaxBytes = 460800
payloadGuardKiroMaxWeight = 1300000
payloadGuardTrimHistory = true
```

能力同步状态：

```json
{
  "model_count": 20,
  "reasoning_fields": {},
  "reasoning_cohort_keys": [],
  "reasoning_cohort_complete": false,
  "reasoning_contract_version": 1,
  "reasoning_invalid_models": [],
  "last_error": "native reasoning capability discovery is incomplete (4/5 cohorts observed)"
}
```

源码会在 capability cohort contract 不匹配或不完整时返回 `Unknown`。因此：

```text
capability discovery incomplete
  -> state = Unknown
  -> no additionalModelRequestFields
promptSteering.enabled = false
  -> no compatible thinking prefix
  -> local conversion error
  -> HTTP 400 before upstream dispatch
```

这就是当前主错误的根因，而不是“第一次上游签名失败”。

### 为什么普通请求能成功

普通请求没有 reasoning 意图，不会触发上述 fail-closed 条件，因此可以生成普通
Kiro `conversationState` 并进入上游。远端对照请求：

- thinking 请求：HTTP 400，request id
  `req_01qRzjRznciijGKDL6GwP8aY`
- 普通请求：HTTP 200，request id
  `req_01TXEwmmoLw8p3n69T5M2XC7`

普通响应返回 `protocol-pong`、非零 usage、`model=claude-opus-5`、
`stop_reason=end_turn`。这排除了“该模型整体不可用”的解释。

## 真实验证矩阵

### C1：远端 direct `/cc/v1/messages`

请求端点：

```text
POST http://152.53.243.159:59137/cc/v1/messages
```

thinking 请求包含：

```json
{
  "model": "claude-opus-5",
  "max_tokens": 1024,
  "stream": false,
  "thinking": {
    "type": "enabled",
    "budget_tokens": 512
  }
}
```

结果：

| 用例 | HTTP | 结果 |
| --- | ---: | --- |
| thinking | 400 | `invalid_request_error`，reasoning fields/prompt controls 均不可用 |
| 同模型普通请求 | 200 | 返回 `protocol-pong`，usage 非零 |

400 的响应 header 同时包含 `request-id` 和 `anthropic-request-id`，body 内有
`request_id`。它发生在 upstream dispatch 之前。

### C1：本地 direct 协议

已验证：

- 普通 non-stream：HTTP 200；返回 `protocol-pong`，usage 非零。
- 显式 `thinking.type=enabled` non-stream：HTTP 200；同时有 `thinking` 与
  `text` block，最终文本 `protocol-think-pong`，`thinking_tokens=93`。
- 该本地实例的 capability 状态为 `Unknown`，且配置中
  `promptSteering.enabled=false`、`bodyConversion.thinkingPromptControls=true`；
  因此该用例直接覆盖远端本地 400 的失败条件。

### C2：真实 Claude Code CLI

CLI 版本：`2.1.280`。通过真实 `claude` binary、隔离 HOME/config 和
`ANTHROPIC_BASE_URL` 连接 `127.0.0.1:19023`。

| 用例 | 结果 | 关键证据 |
| --- | --- | --- |
| 普通请求 | 通过，exit 0 | 最终文本 `cli-pong`，usage 非零 |
| `--effort high` | 通过，exit 0 | 最终文本 `cli-think-pong`，不再出现 reasoning 400 |
| `sonnet-thinking --effort high` | 通过，exit 0 | 1 个 thinking block，thinking tokens 129，最终文本 `cli-sonnet-thinking-pong` |
| 工具请求 | 通过，exit 0 | 1 个 `tool_use`、1 个 `tool_result`，工具 `Bash`，最终文本 `tool-ok` |

这证明候选代码在本地 capability `Unknown`、operator prompt master 关闭的
条件下，真实 Claude Code 的普通、显式 effort、可见 thinking 和 tool 基础
路径均可完成，不再触发本地 converter 400。

### C3：交互式和长会话

本次已完成的证据足以覆盖 non-interactive CLI 和 direct protocol，但没有把
完整 interactive 多轮、MCP、agent 并行和长历史 signed-thinking 作为已通过项。
这些属于下一轮 C3/C4 证据，不应被普通 C2 通过结果替代。

### C4：签名/体积定向回归

使用项目 scoped wrapper：

```bash
feature/tests/run-cargo-scoped.sh protocol-audit -- \
  cargo test --bin kiro-rs thinking_signature_retry -- --nocapture
```

结果：

```text
11 passed
0 failed
2192 filtered out
```

覆盖：

- signature retry 成功；
- same credential 和一次性 retry 限制；
- retry transport failure / transient second response；
- JSON-labeled EventStream / JSON error envelope；
- signature retry 后 too-long 进入统一 payload guard；
- retry body 删除历史 native reasoning；
- tool continuation 保留；
- payload guard trim 后再次 signature retry 保留实际裁剪历史。

## 签名失败与第二次 too-long 的根因

该问题与当前 reasoning 400 不同，不能混成一个错误。

### 已确认的历史链路

目标历史请求：

- `req_01hJ2YhjMEyjrrVEQFTZpqri`
- endpoint：`/ha/v1/messages`
- requested model：`claude-opus-4-8`
- upstream model：`claude-opus-4.8`
- credential：`#1428`
- input tokens：约 `851129`
- body：约 `5.76 MB`
- history entries：`1996`

上游 attempt：

```text
Attempt 1:
  HTTP 400
  reason=THINKING_SIGNATURE_INVALID
  messages.1727.content.0: Invalid signature in thinking block

Attempt 2:
  同 credential
  先移除历史 reasoningContent
  HTTP 400
  reason=CONTENT_LENGTH_EXCEEDS_THRESHOLD
  message=Input is too long.
```

这不是“签名第二次自动修复成功”。真实语义是：

```text
第一份 body 含有上游拒绝的 signed thinking
  -> signature retry builder 删除历史 reasoningContent
  -> 第二份 body 不再触发同一个 signature 校验
  -> Kiro 继续执行下一层 body size 校验
  -> 暴露 Input is too long
```

### 为什么旧版本没有触发裁剪

历史 `v0.0.173` 的 `payloadGuardMode=on_too_long` 初始 pass 使用
`max_bytes=0`、不裁剪；只有识别到 too-long 后才进入完整 payload guard。

因为第一次错误是 signature invalid，不是 too-long，所以流程先进入特殊的
signature retry。签名 retry 的派生 body 再返回 too-long 时，旧代码没有回到
通用 handler classifier，最终 reason 被包在 `thinking_signature_retry` 场景里。

因此根因不是“signature retry 应该自己裁剪”，而是：

> 所有即将发往 Kiro 的原始或派生 body 都应经过同一套 outbound admission；
> 所有 attempt 的 too-long 都应进入同一套 classifier/payload guard 状态机。

### 当前源码状态

当前源码已加入：

- `PayloadTooLongRetryRequest`
- `PayloadTooLongRetryBase`
- `ThinkingSignatureRetryWithoutHistoryReasoning`
- signature retry 后重新构建去历史 reasoning 的 body，再进入统一 guard；
- 记录 `removed_history_reasoning_blocks`；
- 避免对旧的含坏签名 body 再次 guard。

这部分已有 11 项定向回归通过。它说明当前源码路径已覆盖该状态机；不能把它
写成远端 `v0.0.174` 已经部署并验证修复。

完整历史 production 明细（包括 28 条
`THINKING_SIGNATURE_INVALID -> CONTENT_LENGTH_EXCEEDS_THRESHOLD` 记录）保留在：

- [生产签名错误审计](production-thinking-signature-error-20260928.md)
- [签名重试后 too-long 根因跟进](production-thinking-signature-followup-root-cause-20260928.md)

## 多块 thinking、redacted thinking 和历史重放风险

### 已确认的结构限制

当前 Kiro `AssistantMessage` 的 `reasoning_content` 是单一 union：

```rust
Option<ReasoningContent>
```

union 只能容纳一个 `ReasoningText` 或一个 `RedactedContent`。当前
`set_native_reasoning_content()` 在同一 assistant history 出现第二个或混合
native reasoning block 时直接拒绝：

```text
assistant history contains multiple or mixed native reasoning blocks;
Kiro accepts one reasoningContent union value per assistant message
```

这是保守 fail-closed，避免把多个合法 Anthropic block 合并成一个无法验证的
伪造 block；但它不等于完整无损互转。

### 必须保留但当前尚未完全证明的场景

- 多个连续 `thinking`/`redacted_thinking` block；
- thinking -> tool_use -> thinking -> tool_use 交错；
- 最新 tool continuation 的 signed thinking；
- 同一会话切换模型后的历史签名；
- `display=omitted`、空 thinking 与 signature；
- Kiro 是否接受数组/位置化 reasoning。

这些场景不能通过“把内容拼成一个字符串”“重新编码 signature”或“无条件
strip-all”解决。需要真实 Kiro 上游脱敏抓包矩阵后再选择：

- Kiro 支持位置化多 block：扩展内部模型；
- Kiro 只支持单 union：对不可无损表示的请求明确 fail-closed。

### `discardHistoricalThinking` 的边界

生产配置存在 `payloadShaping.discardHistoricalThinking=true`，但单独存在该配置
不能证明目标请求已经删除 thinking。历史目标记录中
`removedHistoryThinkingBlocks=0`。因此：

- 配置是独立的协议风险；
- 不能把配置存在直接写成当前 signature 400 的根因；
- 必须通过长历史、工具续写和 signed/redacted block 矩阵验证其保护边界。

## HTTP 400、HTTP 200、SSE error 与 Claude Code 行为

### 本地转换前 400

远端 reasoning 400 在响应头之前产生，真实 Claude Code CLI 收到：

```text
API Error: 400 reasoning was requested, but both native reasoning fields
and compatible thinking prompt controls are unavailable
```

CLI exit code 为 `1`，当前 turn 停止。因为请求协议在进入上游前就不可转换，
不能把它伪装成 `tool_result` error 让模型继续；这和“工具执行失败”不是同一层。

### HTTP 200 后的真实错误

流式响应一旦发送给客户端，HTTP 状态不能再改成 4xx/5xx。当前实现：

1. 保持 HTTP 200；
2. 记录 `stream_error`；
3. 关闭已经打开的 content block；
4. 发送 Anthropic SSE `event: error`；
5. 不伪造正常 `message_delta/message_stop`。

`req_01tA8uNUqeCYgg94MNMbNaX6` 的历史记录正是这种情况：第二次上游调用收到
200 headers 和大量 reasoning event，随后 body read/decode 失败，最终 public
status 仍为 200。这里的 200 只表示下游响应头已提交，不表示请求成功。

### 工具错误与请求错误的区别

此前 WebSearch/fake-protocol 证据显示：

- HTTP 200 + 合法 `tool_result` error：Claude Code 能继续下一步；
- HTTP 502 或请求协议层 400：CLI 进入错误/retry 语义，通常结束当前请求。

因此稳定设计应遵循：

- WebSearch 查询执行失败、且协议仍允许下一轮决策：表达为 `tool_result`
  错误；
- reasoning conversion、无效签名、body 超限：在服务端能安全重建时内部修复；
  无法安全重建时 fail-closed；
- 不能把协议错误包装成工具结果来“保证不中断”，否则会破坏 Claude Code
  对请求状态的判断。

### `error` event 与 CLI 是否继续

服务端可以保证协议状态真实、不中断已提交的字节流并不伪造成功，但不能保证
所有 Claude Code 版本收到 SSE error 后自动继续。当前已验证的边界是：

- pre-dispatch HTTP 400：CLI 当前 turn 停止；
- post-commit HTTP 200 + SSE error：当前流失败，客户端不得把它当成功；
- tool_result error：CLI 有机会继续；
- HTTP 502：CLI 进入错误/retry 路径。

## `model=unknown`、内容缺失和采样记录

这属于附带可观测性问题，不是 reasoning 400 的根因。

### `model=unknown` 的直接原因

`src/anthropic/usage.rs` 的
`sampled_request_rejection_usage_record_with_metadata()` 固定写入：

```rust
model: "unknown".to_string()
```

`record_pre_usage_rejection_with_metadata()` 只传 reason、stage、status、request
id、endpoint 和 extra metadata。即使 local body pipeline 已解析
`payload.model`，sampled rejection factory 也没有接收它，因此 UI/usage row
会显示 `model=unknown`。

这说明：

- 请求通常已经带了 model；
- model 在请求解析或 model resolution 阶段未必丢失；
- 丢失发生在 usage serialization/projection；
- 不应把 `model=unknown` 当作模型没传或模型不可用。

### 为什么内容或诊断可能为空

当前 conversion error metadata 其实会保存 bounded 的：

- `localBodyPrepareKind=conversion_error`
- `localBodyPrepareCategory=unsupported_content`
- `localBodyPrepareDiagnostic=...`

如果页面只显示通用错误摘要、或 sampled rejection 记录未映射这些字段，就会看到
“内容不知道是什么”。因此应继续检查 usage query/UI projection；不能据此推断
本地转换没有产生诊断。

### 采样不是精确计数

错误记录带有：

```json
{
  "sampled": true,
  "observedCountIsExact": false
}
```

单条 usage row 不是所有拒绝请求的精确全集。它适合保留诊断样本，不适合直接
作为错误率分母或“只发生过一次”的证明。

## 必须修复、可以优化、暂不修改、证据不足

### P0：必须修复或先建立明确策略

1. **Reasoning capability discovery 的 `Unknown` 策略**
   - 修复前会让真实 Claude Code 默认 reasoning 直接 400。
   - 当前修复已使显式客户端 reasoning 在已启用
     `thinkingPromptControls` 且 `promptSteering.thinking.enabled=true` 时走
     专用兼容传输，不再被 `promptSteering.enabled=false` 阻断。
   - `Supported`：生成原生 Kiro fields。
   - `AuthoritativeAbsent/Invalid`：明确说明该上游模型不支持。
   - `Unknown`：不能永久等价于“不支持”。后续仍应把 capability discovery
     长期不完整的问题单独修复，并继续区分 capability-not-ready 与
     unsupported-capability。

2. **统一 outbound body admission**
   - 原始请求、signature retry body、cache-point retry body、payload guard
     body 必须共享同一套 byte/weight admission 和 too-long classifier。
   - 不要让特殊 retry 分支绕过 guard。

3. **signed/redacted thinking 的无损保护**
   - 不解析、不重算、不改写 signature/redacted data。
   - 明确多块/交错/工具续写无法表达时的 fail-closed 行为。
   - 给 reasoning block 增加模型来源和保护边界，避免无条件 strip-all。

4. **错误阶段和 terminal reason 分离**
   - `priorAttemptFailures[]` 保存第一次 signature invalid；
   - `terminalFailure` 保存第二次 too-long 或 stream read error；
   - UI 不应把 `reason=thinking_signature_retry` 当成最终上游 reason。

### P1：应优化

1. 为 body read error 记录 content encoding、HTTP version、已读字节、最后完整
   frame、decoder pending bytes、是否 commit、region/代理脱敏标识。
2. 把 `original_body_bytes` 与 `diagnostic_fragment_bytes` 分开命名。
3. 将 `stream_error` 与账号/模型/region 的短期质量指标关联，但不要直接永久
   禁用仍有大量成功请求的账号。
4. sampled request rejection 记录 requested model、resolved model 或安全
   fingerprint；不写入完整请求正文、token 或 signature。
5. 为 C3 interactive、长会话、MCP、agent 和真实 Kiro 多块 reasoning 建立
   可重复低并发矩阵。

### 暂不应修改

- 不因单个 `THINKING_SIGNATURE_INVALID` 就永久禁用账号。
- 不因 `model=unknown` 直接改模型路由或模型 alias。
- 不把所有 400/stream error 转成 HTTP 200 或 tool_result。
- 不未经真实 Kiro 证据扩展 `reasoningContent` 为猜测性的数组协议。
- 不把 `payloadShaping.discardHistoricalThinking=true` 直接认定为本次请求
  已删块。
- 不以 `v0.0.175` 发布成功替代现场验证。

### 证据不足，后续必须补齐

- Kiro 对多个 reasoning block、数组、交错 tool loop 的真实接受形状。
- `THINKING_SIGNATURE_INVALID` 的具体产生机制：跨模型、重排、历史压缩、
  上游过期还是更早请求污染。
- `error decoding response body` 的具体传输层断点。
- 完整 Claude Code interactive 行为在 post-commit SSE error 下的版本差异。
- `model=unknown` 在 UI 查询层的最终字段映射。

## 建议的稳定状态机

建议后续实现按以下语义设计，当前仅作为审计结论，不代表本次已修改：

```text
parse Anthropic request
  -> resolve requested/resolved model
  -> classify reasoning intent
  -> read capability state
       Supported:
         build native Kiro reasoning fields
       AuthoritativeAbsent/Invalid:
         return explicit unsupported-capability error
       Unknown:
         use only a real-upstream-verified fallback;
         otherwise return capability-not-ready error
  -> build conversation/history without touching opaque signed blocks
  -> run one shared outbound byte/weight admission
  -> dispatch Kiro
       signature invalid before downstream commit:
         derive a documented compatibility body;
         re-run shared admission;
         preserve terminal upstream reason
       too-long before downstream commit:
         run shared payload guard at most once;
         if still oversized, fail clearly
       stream/read error after commit:
         HTTP 200 + SSE error, no replay and no fake message_stop
  -> record requested model, resolved model, attempt list and terminal reason
```

这套状态机的核心不是“为了不中断而吞错”，而是把可以安全修复的协议错误留在
服务端边界内，把不能安全重放的错误如实交给 Claude Code。

## 文件、源码和证据索引

### 计划与本报告

- [协议审计计划](../plantree/plans/rust-runtime-scheduler-stabilization/topics/official-claude-code-kiro-protocol-audit-20260928.md)
- [Rust runtime scheduler stabilization](../plantree/plans/rust-runtime-scheduler-stabilization/README.md)
- 本报告：`docs/analysis/official-claude-code-kiro-protocol-audit-20260928.md`

### 生产签名/流错误

- [生产 Thinking Signature 错误审计](production-thinking-signature-error-20260928.md)
- [签名重试后 too-long 根因](production-thinking-signature-followup-root-cause-20260928.md)
- [Thinking signature 协议安全计划](../plantree/plans/rust-runtime-scheduler-stabilization/topics/thinking-signature-protocol-safety.md)
- [Thinking signature 后 too-long 修复计划](../plantree/plans/rust-runtime-scheduler-stabilization/topics/thinking-signature-too-long-remediation-20260928.md)

### 关键源码

- `src/anthropic/converter.rs`：`explicit_reasoning_request`、thinking prompt
  fallback 条件、reasoning 两条路径均不可用时的拒绝。
- `src/anthropic/handlers/local_body_pipeline.rs`：handler 构造
  `ConverterOptions` 的真实路径。
- `src/model/config.rs`：`promptSteering.enabled` 与显式客户端 reasoning 的
  语义注释。
- `src/anthropic/converter/model.rs:217-264`：能力状态到 Kiro native fields。
- `src/anthropic/model_capabilities.rs:591-610`：cohort 不匹配返回 `Unknown`。
- `src/anthropic/model_capabilities.rs:687-850`：不完整 discovery 的状态更新。
- `src/anthropic/handlers/local_body_pipeline.rs:131-165`：转换错误入口。
- `src/anthropic/handlers.rs:6631-6658`：reasoning capability 注入和入口拒绝。
- `src/anthropic/converter/history.rs:371-420`：混合/多 native reasoning 保守拒绝。
- `src/kiro/model/requests/conversation.rs:343-424`：单值 reasoning union。
- `src/anthropic/handlers.rs:1144-1231`：signature retry 后统一 payload guard。
- `src/anthropic/handlers.rs:5516-5588`：too-long 与 signature retry 状态判断。
- `src/anthropic/handlers.rs:6860-6869`：删除历史 reasoning 的兼容 retry body。
- `src/anthropic/stream.rs:1235-1516`：SSE 状态机和 final usage。
- `src/anthropic/stream.rs:2545-2619`：stream error -> SSE error event。
- `src/anthropic/handlers.rs:8717-8746`：流失败记录和终态输出。
- `src/anthropic/usage.rs:519-632`：sampled rejection 固定 `model=unknown`。

## 本次最终状态

- 已定位当前 reasoning 400 的确定根因。
- 已证明普通请求、thinking 请求、工具请求在本地能力可用实例上可由真实
  Claude Code CLI 正常完成。
- 已区分 pre-dispatch 400、post-commit HTTP 200 + SSE error、tool_result error
  和 HTTP 502 的客户端语义。
- 已解释第一次 signature、第二次 too-long 为什么是同一客户端请求中的两个
  不同上游 attempt。
- 已核对当前源码的 signature -> too-long 修复回归。
- 已记录 `model=unknown` 和 rejection sampling 的真实实现原因。
- 已实施 reasoning fallback 修复，新增现网失败条件的 converter 回归测试；
  `converter` 回归 154 项通过，`local_body_pipeline` 定向测试 2 项通过。
- 已使用候选 release 二进制完成本地真实 direct API 和 Claude Code CLI 验证。
- 未修改远端配置、数据库、Redis、容器或账号；远端修复需随新版本发布和部署后
  再做现场验证。
- 后续仍需补齐 capability discovery 长期稳定、真实 Kiro 多块 signed-thinking
  矩阵和统一 outbound admission 的验收证据。
