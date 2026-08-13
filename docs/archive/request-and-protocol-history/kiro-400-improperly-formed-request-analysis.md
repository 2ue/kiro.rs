# Kiro 上游 400 Improperly Formed Request 分析

## 背景

线上日志示例：

```text
api_error: 非流式 API 请求失败（凭据 #17 rajah87@icloud.com）: 400 Bad Request {"message":"Improperly formed request.","reason":null}
```

该日志来自线上服务，不是本地服务日志。日志中的“非流式 API 请求失败”是当前服务调用 Kiro 上游后记录的错误，不是下游客户端直接返回的错误文本。

## 结论

这类错误的直接含义是：当前服务已经完成凭据调度，并使用某个凭据把转换后的请求发送给 Kiro 上游；Kiro 上游返回了 `400 Bad Request`，认为最终请求体格式不合法。

因此它不是以下问题：

- 不是网络连接问题。
- 不是调度器没有选到账号的问题。
- 不是账号额度、credits、并发槽位导致的问题。
- 不是 429 限流或 402 支付/额度状态问题。
- 通常也不是凭据本身坏了，因为同一个凭据跑普通文本请求可能完全正常。

更准确的归因是：这是一个请求内容或协议转换问题。根因可能在下游客户端，也可能在当前服务的 Anthropic-to-Kiro 转换层。

单凭这一条错误日志，不能 100% 判断是下游客户端问题还是当前服务问题。它只能证明 Kiro 上游拒绝了当前服务发过去的最终请求体。

## 当前代码链路

错误文本来源于 `src/kiro/provider.rs` 中的非流式 API 调用路径：

```rust
"非流式 API 请求失败（{}）: {} {}"
```

也就是当前服务已经向 Kiro 上游发起 HTTP 请求，并收到了非 2xx 响应。

调度重试路径里对 `400 Bad Request` 的处理逻辑是：

```rust
// 400 Bad Request - 请求问题，重试/切换凭据无意义
```

这个判断方向是合理的。对于请求体格式不合法的问题，换凭据重试通常没有意义，继续打其他账号只会制造更多失败记录。

最终错误会在 Anthropic handler 中记录为 usage failure：

```rust
record_failure(UsageRecordStatus::Error, "api_error", message)
```

然后通过 `map_provider_error` 映射给下游。当前没有专门识别 `"Improperly formed request"`，所以它会落到通用 `api_error` 分支。

## 为什么不是简单的“客户端问题”

下游客户端传入的是 Anthropic 兼容格式。这个格式可能对 Anthropic 官方是合法的，但 Kiro 上游不一定支持同样的结构。

当前服务的职责是把 Anthropic 请求转换为 Kiro 请求。因此：

- 如果下游传入的是明显非法 JSON、非法 messages 或非法 tools，那么更偏向下游客户端问题。
- 如果下游传入的是 Anthropic 合法请求，但当前服务转换后变成 Kiro 不接受的结构，那么是当前服务兼容层问题。
- 如果 Kiro 上游近期改变了支持格式或校验规则，那么可能是上游兼容变化导致当前服务需要适配。

所以这类错误更适合定义为“请求兼容性问题”，而不是直接定性为“客户端错”。

## 和 Invalid Model 的区别

模型不存在或模型不被 Kiro 支持时，常见错误类似：

```json
{"message":"Invalid model. Please select a different model to continue.","reason":"INVALID_MODEL_ID"}
```

这种可以明确归因到模型 ID 不合法，应该通过模型映射、模型同步、模型能力表解决。

而当前错误是：

```json
{"message":"Improperly formed request.","reason":null}
```

它没有给出具体字段，也没有明确 reason。它更像是 Kiro 上游对请求 JSON 结构、消息历史、工具调用、content block、schema 或上下文状态不满意。

## 高概率触发场景

### 1. Tools / MCP 工具 schema 不兼容

代码中已经有相关注释：

```rust
/// Claude Code / MCP 工具定义偶尔会出现 `required: null`、`properties: null` 等，
/// 导致上游返回 400 "Improperly formed request"。
```

常见风险：

- `input_schema.required` 是 `null`，而不是数组。
- `input_schema.properties` 是 `null`，而不是 object。
- `input_schema.type` 缺失或不是字符串。
- `additionalProperties` 是 Kiro 不接受的类型。
- schema 中嵌套字段仍存在未规范化的异常结构。

当前服务已有 `normalize_json_schema`，但如果只处理了顶层 schema，没有递归处理嵌套 schema，仍可能遗漏复杂 MCP 工具定义。

### 2. tool_use / tool_result 配对问题

Kiro API 要求每个工具调用关系保持一致。风险包括：

- 历史 assistant 消息中存在孤立 `tool_use`。
- 当前 user 消息中存在找不到对应 `tool_use_id` 的 `tool_result`。
- 同一个 `tool_use_id` 被重复返回 `tool_result`。
- 多轮 agent 会话中工具调用历史被下游裁剪，导致配对关系断裂。
- 当前服务为了修复孤立 tool_use 做了过滤，但某些复杂顺序仍未覆盖。

代码中也有相关注释：

```rust
/// Kiro API 要求每个 tool_use 必须有对应的 tool_result，否则返回 400 Bad Request。
```

因此如果该错误集中出现在 Claude Code CLI、MCP、agent、多工具调用、长会话场景，应优先排查工具调用历史。

### 3. Content block 类型或组合不兼容

Anthropic 请求可能包含：

- text
- image
- document
- tool_use
- tool_result
- thinking / redacted thinking

Kiro 上游不一定支持所有组合。尤其是以下组合需要重点观察：

- image + tools
- document + tools
- thinking + tools
- 多个 image 或 document
- 空 text 但只有 tool_result
- system message 为数组结构
- assistant message 只有 tool_use 没有文本

这些请求可能在 Anthropic 语义上成立，但转换成 Kiro 请求后触发上游校验失败。

### 4. 长会话历史或被裁剪历史

长会话容易出现：

- messages 顺序复杂。
- tool_use 和 tool_result 跨多轮配对。
- 下游客户端裁剪历史时只保留了一半工具调用关系。
- 当前服务转换时把某些 assistant/user 内容合并或过滤，导致结构变化。

如果同一个会话持续复现，但新开会话正常，优先怀疑历史结构问题。

### 5. 非流式路径特有差异

日志明确包含“非流式 API 请求失败”。如果同类请求在流式路径成功、非流式路径失败，需要对比：

- 非流式和流式调用是否使用完全一致的转换结果。
- 非流式是否传入了不同的 request body。
- 下游客户端是否在非流式时发送了不同结构。
- usage / cache / compatibility profile 是否对两个路径有不同处理。

## 如何排查线上单条记录

对这类记录，排查时不要先看账号，而是先看请求结构摘要。

建议收集以下字段：

- request id
- 下游原始 model
- 最终 upstream model
- model resolution source
- endpoint，例如 `/v1/messages`、`/cc/v1/messages`、`/ha/v1/messages`、`/na/v1/messages`
- stream 是否为 false
- credential id 和 email
- message count
- tool count
- tool_result count
- image count
- document count
- 是否带 thinking
- 是否有 model mapping
- 是否有 tool name mapping
- 是否有 converter warnings
- attempt chain
- Kiro 上游原始错误 body

判断方式：

| 现象 | 更可能的根因 |
| --- | --- |
| 普通文本正常，带 MCP/tools 后失败 | tools schema 或 tool_use/tool_result 转换问题 |
| 同一个长会话稳定失败，新会话正常 | 历史消息结构或工具调用配对问题 |
| 同一个请求换账号仍然 400 | 请求结构问题，不是凭据问题 |
| 某个模型稳定失败，其他模型正常 | 模型映射或模型能力不匹配 |
| 只有非流式失败，流式正常 | 非流式路径或下游非流式请求结构差异 |
| 所有请求都失败 | 当前服务转换层、模型能力同步或 Kiro 上游接口变化 |
| 只在某个客户端版本失败 | 下游客户端请求格式变化或兼容差异 |

## 当前日志的不足

当前记录只包含：

- 错误类型：`api_error`
- 凭据：`#17 rajah87@icloud.com`
- 上游状态码：`400`
- 上游错误 body：`Improperly formed request`

这些信息不足以判断请求到底哪里 malformed。

最大的问题是：日志里过度暴露了“哪个凭据失败”，但没有足够暴露“请求结构为什么可能失败”。

后续应该把观测重点从“账号失败”补充到“请求结构摘要失败”。

## 建议优化

## 2026-05-29 已实施优化

本分析中的 P0/P1 优化已结合当前项目完成，不是单独实验代码。

已实施内容：

- 在 `src/anthropic/handlers.rs` 中识别 Kiro 上游返回的 `Improperly formed request`，并向下游映射为 `400 invalid_request_error`，不再落入通用 `api_error`。
- 在 `src/anthropic/payload_guard.rs` 新增最终 Kiro payload guard，在发送上游前按真实 JSON body 字节数检查。
- 默认 payload 上限为 `450 * 1024` bytes。
- payload 超限时裁剪最旧 history，仍超限则前置返回下游 400，不继续消耗账号打上游。
- 裁剪/修复会记录 original/final bytes、裁剪历史条数、tool 修复计数、endpoint、requested model、upstream model、conversation id。
- 在 `src/anthropic/converter.rs` 中增强孤立 tool_result 处理：兼容模式下从 Kiro toolResults 中移除，但将可读内容文本化追加到当前 user content，减少上下文损失。
- 递归 schema 清洗和 Kiro-safe tool name 映射已在 converter 中保留并有单测覆盖。
- 新旧 UI 都已在运行配置里增加 payload guard 开关、最大字节数和是否裁剪旧历史配置。

仍建议后续继续补充的内容：

- 将 payload guard report / converter warnings 持久化进 usage detail，而不仅是日志和可选响应头。
- 在 usage record 中增加脱敏请求结构摘要字段，方便不看完整请求体也能定位 tool/schema/content block 类问题。

### 1. 将该类错误映射为 invalid_request_error

此前 `Improperly formed request` 会落入通用 `api_error`。当前已在 `map_provider_error` 中增加识别：

- `Improperly formed request`
- `Bad Request`
- `bad_request`

对于 Kiro 明确返回 400 且语义是请求格式错误的情况，下游响应更适合是：

```json
{
  "error": {
    "type": "invalid_request_error",
    "message": "Upstream rejected the request as improperly formed. Check messages, tools, tool results, and content blocks."
  }
}
```

这样调用方能知道这是请求结构问题，而不是服务暂时异常。

### 2. 保存脱敏请求结构摘要

不要直接保存完整请求体，因为里面可能包含用户代码、prompt、隐私内容、token。

建议保存脱敏摘要：

```json
{
  "stream": false,
  "endpoint": "/v1/messages",
  "requested_model": "claude-sonnet-4-20250514",
  "upstream_model": "claude-sonnet-4.6",
  "message_count": 18,
  "system_kind": "array",
  "tool_count": 14,
  "tool_result_count": 2,
  "tool_use_count": 2,
  "image_count": 0,
  "document_count": 0,
  "thinking_blocks": 0,
  "has_model_mapping": true,
  "converter_warnings": {
    "orphan_tool_uses": 0,
    "orphan_tool_results": 0,
    "duplicate_tool_results": 0,
    "tool_name_mappings": 3
  }
}
```

这样既能排查问题，又不会泄露正文内容。

### 3. 记录转换前后模型

对于模型映射后的请求，usage detail 和 call trace 里应明确记录：

- 下游请求模型
- 上游实际模型
- 映射来源
- 映射说明

这样能区分是模型不支持某种能力，还是请求结构本身错误。

### 4. 增强 converter warnings

当前 converter 已有 warnings 机制，但应确保这些 warnings 能持久化到 usage detail 中，而不仅是日志。

建议覆盖：

- 孤立 tool_use 数量。
- 孤立 tool_result 数量。
- 重复 tool_result 数量。
- 被移除的 tool_use 数量。
- 被跳过的 content block 数量。
- 被规范化的 schema 字段数量。
- 被缩短的 tool name 数量。

### 5. 增强 schema 规范化

如果线上错误集中在 MCP/tools 场景，需要检查 `normalize_json_schema` 是否递归处理嵌套 schema。

建议覆盖：

- `properties.*`
- `items`
- `anyOf`
- `oneOf`
- `allOf`
- `$defs`
- `definitions`
- nested `required`
- nested `additionalProperties`

只规范化顶层 schema 可能不够。

### 6. 对 400 不做账号轮换

当前 400 不继续切换凭据是合理的，应保留。

原因：

- 400 是请求体问题，换账号没有意义。
- 继续切换会污染多个账号的失败统计。
- 大并发下会放大上游无效请求量。
- 可能误伤调度健康评分。

但记录时应明确标记为 `bad_request`，避免误判为账号失败。

## 推荐排查流程

1. 找到 usage record 的 request id。
2. 查看是否非流式、是否带 tools/MCP、是否长会话。
3. 查看 requested model 和 upstream model。
4. 查看是否有 model mapping。
5. 查看该凭据普通文本请求是否正常。
6. 如果普通文本正常，排除凭据问题。
7. 如果同会话复现，新会话正常，优先排查 history/tool pairing。
8. 如果只在 tools 场景复现，优先排查 schema 和 tool_result。
9. 如果所有模型或所有请求都复现，排查当前服务转换层或 Kiro 上游接口变化。
10. 如需进一步定位，在管理员调试模式下保存脱敏结构摘要，不直接保存完整 prompt。

## 最终判断口径

这类错误应向用户或运维侧解释为：

> Kiro 上游拒绝了当前服务发出的最终请求体，错误为 `Improperly formed request`。这不是账号、额度、代理或调度问题，而是请求结构或协议转换兼容性问题。需要结合该请求的模型映射、messages、tools、tool_result、content block 和非流式路径进一步定位。当前不能仅凭该日志断定是下游客户端错误，也不能排除当前服务转换层需要增强兼容。
