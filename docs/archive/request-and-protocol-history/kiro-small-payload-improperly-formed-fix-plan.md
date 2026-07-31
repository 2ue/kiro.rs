# Kiro 小 payload `Improperly formed request` 根因分析与修复方案

## 1. 文档目的

本文档用于独立说明一次现网 `Kiro rejected the converted request as improperly formed` 错误的背景、证据、原因分析和修复方案。

本文档不依赖历史会话上下文。即使读者没有看过排查过程、没有访问当时的服务和日志，也应能理解：

1. 现网服务使用的版本是什么。
2. 报错原文是什么。
3. 哪个错误是主因，哪个错误是外层包装。
4. 为什么该错误不是 payload 过大、账号不可用、模型不可用或网络问题。
5. 最新代码是否已经能修复该错误。
6. 哪些现网错误可以通过 `kiro.rs` 代理修复，哪些只能缓解。
7. 需要如何修改代码和配置才能修复这类小 payload malformed 请求。

本文档只包含脱敏后的技术信息，不记录账号标签、邮箱、token、password、cookie、refresh token 或其他凭据内容。

## 2. 环境与版本

### 2.1 现网服务

服务目录：

```text
/root/docker-compose/kiro-rs-2ue-59137
```

服务容器：

```text
kiro-rs-2ue-59137-app
```

compose 中的镜像配置为：

```text
ghcr.io/2ue/kiro-rs:latest
```

但容器内实际运行版本为：

```text
kiro-rs 0.0.33
```

镜像 OCI 标签显示：

```text
org.opencontainers.image.version = 0.0.33
org.opencontainers.image.revision = 1c357eabbd341d6cca93ac655d87c365a0576a8a
```

因此，不能只根据 compose 中的 `latest` 判断现网已经运行最新版本。该服务当时实际运行的是 `0.0.33`。

### 2.2 本地仓库

本地仓库：

```text
/root/code/kiro_rs_2ue
```

分析最新版时，本地仓库状态为：

```text
main
v0.0.35
HEAD 5bed5c5
```

`v0.0.33..v0.0.35` 的关键差异：

- `src/anthropic/handlers.rs` 有较多更新，新增 too-long/context-full/malformed 分类与 retry 判断。
- `src/model/config.rs` 新增 `PayloadGuardMode`。
- admin usage dashboard 和 usage 查询增强。
- `src/anthropic/converter.rs` 未变化。
- `src/anthropic/payload_guard.rs` 未变化。

这点很关键：最新版没有改变 Anthropic -> Kiro 的核心转换形态，也没有改变 payload guard 的协议修复规则。因此，对小 payload malformed 请求，单纯升级到 `v0.0.35` 不等于已经修复。

## 3. 报错原文

下游看到的错误：

```json
{
  "type": "error",
  "error": {
    "type": "invalid_request_error",
    "message": "Kiro rejected the converted request as improperly formed. Check model mapping, tool_use/tool_result pairing, tool schema, multimodal sources, and payload size."
  },
  "request_id": "req_01bGG677KX44sBJfcoW6ycQt"
}
```

随后还出现两条 SSE 风格的外层错误：

```text
data: {"type":"error","error":{"type":"upstream_error","message":"Upstream request failed"}}
data: {"type":"error","error":{"type":"upstream_error","message":"Upstream request failed"}}
```

直接上游错误为：

```text
400 Bad Request {"message":"Improperly formed request.","reason":null}
```

## 4. 错误链路

### 4.1 主错误

主错误来自 Kiro 上游。当前服务已经完成 Anthropic 请求解析、模型映射、凭据调度、Anthropic -> Kiro 请求转换，并向 Kiro 上游发出请求。Kiro 上游返回：

```json
{
  "message": "Improperly formed request.",
  "reason": null
}
```

`kiro.rs` 在 `src/anthropic/handlers.rs` 中识别该上游错误，并映射为下游看到的：

```text
Kiro rejected the converted request as improperly formed...
```

对应代码位置：

```text
src/anthropic/handlers.rs
map_provider_error(...)
is_upstream_improperly_formed_error(...)
```

### 4.2 外层 `Upstream request failed`

仓库中未找到固定字符串：

```text
Upstream request failed
```

因此这两条：

```text
data: {"type":"error","error":{"type":"upstream_error","message":"Upstream request failed"}}
```

更可能是外层调用方、网关、SSE 客户端或另一个代理层对同一次失败的二次包装，不是本仓库当前代码直接生成的主错误。

排查时应以 Kiro 返回的 `400 Bad Request {"message":"Improperly formed request.","reason":null}` 作为主因。

## 5. 该请求的脱敏 usage 证据

请求 ID：

```text
req_01bGG677KX44sBJfcoW6ycQt
```

创建时间：

```text
2026-06-07 01:02:21 UTC
```

接口：

```text
/ha/v1/messages
```

请求模式：

```text
stream = true
```

下游模型：

```text
claude-opus-4-7
```

上游模型映射：

```text
claude-opus-4-7 -> claude-opus-4.7
```

输入 token：

```text
totalInputTokens = 26142
```

payload breakdown：

```text
totalBytes                  = 96819
historyBytes                = 33101
historyEntries              = 14
currentMessageBytes         = 63499
currentContentBytes         = 0
currentToolCount            = 79
currentToolsBytes           = 62918
largestToolBytes            = 5945
currentToolResultCount      = 1
currentToolResultsBytes     = 445
historyToolUseCount         = 6
historyToolResultCount      = 5
currentImageCount           = 0
currentImagesBytes          = 2
historyImagesBytes          = 14
```

payload guard report：

```text
enabled                         = true
maxBytes                        = 563200
originalBytes                   = 96819
finalBytes                      = 96819
stillOversized                  = false
trimmedHistoryEntries           = 0
alignedLeadingEntries           = 0
removedEmptyToolUses            = 0
removedOrphanToolUses           = 0
removedOrphanToolResults        = 0
textifiedOrphanToolResults      = 0
compressedToolDefinitions       = 0
compressedToolDefinitionBytes   = 0
truncatedCurrentToolResults     = 0
truncatedCurrentDocuments       = 0
truncatedCurrentUserContent     = 0
droppedCurrentImages            = 0
```

这些字段说明：该请求没有被 payload guard 裁剪、压缩、移除 orphan tool 或截断内容。请求体原样发送给 Kiro，上游返回 malformed。

## 6. 同一 conversation 的连续失败

同一个 conversation 在约 2 分钟内连续出现 5 次 `Improperly formed request`。

脱敏后的关键特征：

```text
request_id                       time UTC                  totalBytes  currentToolCount
req_01XEu6SDx22w1CbPstsWtCjU     2026-06-07 01:00:25       88812       79
req_01KeCoNCnshCxzQhZu6ZHKzE     2026-06-07 01:00:45       90422       79
req_01F2Vgq66YmsNoSevn2V3Lhw     2026-06-07 01:01:13       91957       79
req_01KYqbLjtGMCug6YYu3T6Ab8     2026-06-07 01:01:56       95252       79
req_01bGG677KX44sBJfcoW6ycQt     2026-06-07 01:02:21       96819       79
```

共同点：

- 全部是 `/ha/v1/messages`。
- 全部是 stream 请求。
- 全部使用 `claude-opus-4-7 -> claude-opus-4.7`。
- 全部 current tool count 为 `79`。
- payload 都在 `88KB` 到 `96KB` 之间，远低于现网 guard 阈值 `563200` bytes。
- 都是上游 `400 Bad Request {"message":"Improperly formed request.","reason":null}`。

该模式更像“稳定的请求形态不被 Kiro 接受”，不是偶发网络问题，也不是某一次请求刚好超限。

## 7. 排除项

### 7.1 不是 payload 过大

该请求最终 body 为 `96819` bytes，现网 guard 阈值为 `563200` bytes。

对比：

```text
96819 < 563200
```

payload guard 报告中：

```text
stillOversized = false
originalBytes  = finalBytes
```

所以该请求不是当前 guard 认为的过大请求。

### 7.2 不是 `CONTENT_LENGTH_EXCEEDS_THRESHOLD`

Kiro 返回的 reason 为：

```text
null
```

不是：

```text
CONTENT_LENGTH_EXCEEDS_THRESHOLD
```

错误 message 也是：

```text
Improperly formed request.
```

不是：

```text
Input is too long.
```

因此它不能归类为普通 too-long 错误。

### 7.3 不是模型整体不可用

同一上游模型 `claude-opus-4.7` 在同一时间窗口内有大量成功请求。

如果模型整体不可用，通常会看到：

```text
Invalid model
MODEL_NOT_FOUND
unsupported model
```

或所有同模型请求失败。现有证据不支持该判断。

### 7.4 不是凭据整体不可用

同一已调度凭据在同一时间窗口内存在大量成功请求。

如果凭据整体不可用，通常会看到认证、额度、禁用、401、403、402、凭据不可调度等错误。当前错误是 Kiro 对请求体返回 400，因此不应优先归因于凭据。

### 7.5 不是网络问题

网络问题通常表现为连接失败、超时、read error、stream idle timeout 等。

本请求收到明确 HTTP 响应：

```text
400 Bad Request
```

并带有 Kiro 上游 JSON 错误体。因此不是网络层无响应。

### 7.6 不应通过切换账号重试解决

`Improperly formed request` 表示请求体形态不合法。对同一个坏 body 切换账号重试，通常仍会被 Kiro 拒绝，还会污染更多凭据的错误统计。

如果要重试，必须改变请求体形态后重试，而不是原样重发或换账号重发。

## 8. 根因判断

### 8.1 直接原因

直接原因是：Kiro 上游认为转换后的 Kiro request body 格式不合法，返回：

```text
400 Bad Request {"message":"Improperly formed request.","reason":null}
```

`kiro.rs` 只是把该上游 400 映射成 Anthropic 兼容错误返回给下游。

### 8.2 深层原因的最高概率方向

最高概率方向是：该请求的 Anthropic -> Kiro 转换结果中，某些工具定义、工具 schema、工具结果、消息历史或 current message 组合触发了 Kiro 的严格校验。

具体表现为：

1. 当前消息正文为空：

   ```text
   currentContentBytes = 0
   ```

2. 当前消息只携带一个 tool result：

   ```text
   currentToolResultCount = 1
   ```

3. 当前请求携带大量工具定义：

   ```text
   currentToolCount  = 79
   currentToolsBytes = 62918
   ```

4. 历史中工具调用与结果数量为：

   ```text
   historyToolUseCount   = 6
   historyToolResultCount = 5
   ```

   当前 `tool_result` 很可能用于补齐最后一个未完成的工具调用。

5. payload guard 没有发现 orphan tool 或 oversized，也没有进行修复：

   ```text
   removedOrphanToolUses       = 0
   removedOrphanToolResults    = 0
   compressedToolDefinitions   = 0
   ```

这说明请求在代理现有规则下看起来是“可发送”的，但 Kiro 上游仍拒绝。问题很可能在 Kiro 更严格或未公开的校验规则中。

### 8.3 为什么不能精确指出具体字段

当前 usage 记录没有保存完整 Kiro request body，也没有保存完整工具 schema、映射后工具名列表或 schema 特征摘要。

Kiro 上游返回：

```json
{
  "message": "Improperly formed request.",
  "reason": null
}
```

没有字段级错误原因。

因此目前只能基于 payload breakdown 和代码逻辑给出高概率判断，不能断言“一定是第 N 个工具 schema”或“一定是某个具体字段”。

要做到无歧义定位，需要新增 malformed 专用诊断，见本文后续修复方案。

## 9. 最新版 `v0.0.35` 是否能自动修复

结论：不能自动修复该请求。

### 9.1 最新版新增内容

`v0.0.35` 新增 `PayloadGuardMode`：

```text
preemptive
on_too_long
```

含义：

- `preemptive`：保持原行为，发送上游前按 `payloadGuardMaxBytes` 做裁剪和整形。
- `on_too_long`：首次请求只做协议修复，不按大小裁剪；只有上游返回 too-long/context-full 类错误后，才按 `payloadGuardMaxBytes` 裁剪并重试一次。

默认值：

```text
payloadGuardMode = preemptive
```

### 9.2 最新版 malformed retry 条件

最新版中，是否在上游错误后触发 payload guard retry 的逻辑为：

```text
too-long/context-full 错误
OR
improperly formed 且 attempted_body_bytes > retry_max_bytes
```

该逻辑的含义是：如果 malformed 其实是“过大请求被 Kiro 泛化成 malformed”，并且 body 超过 retry 阈值，则把它当作 size 类问题处理。

### 9.3 为什么该请求不会触发最新版 retry

该请求：

```text
attempted_body_bytes = 96819
```

现网阈值：

```text
payloadGuardMaxBytes = 563200
```

最新版默认阈值：

```text
450 KiB = 460800 bytes
```

无论使用现网阈值还是最新版默认阈值：

```text
96819 < 460800
96819 < 563200
```

所以它不会被最新版当作 oversized malformed 重试。

最新版测试中也明确验证：约 `100000` bytes 的 `Improperly formed request` 不触发 too-long retry。

### 9.4 关键结论

`v0.0.35` 对 too-long 类错误有改进，但没有针对“小 payload malformed request”的修复。

要修复该请求，需要新增专门的 small malformed 修复路径，而不是仅升级版本。

## 10. 最近 24 小时现网错误分类

以下为 `2026-06-07 03:02 UTC` 左右统计的最近 24 小时快照。随着时间推移，数量会变化，但分类结论不依赖具体数量。

```text
success                    66657
all_credentials_cooling     1712
upstream_429                 354
too_long_or_context_full     103
improperly_formed             50
stream_idle_timeout           50
stream_read_error              9
stream_error_other             7
credentials_unavailable        4
api_error_other                1
```

### 10.1 可以由代理重点修复的类型

可以由 `kiro.rs` 代理重点修复：

```text
too_long_or_context_full
improperly_formed 中由转换形态、工具 schema、tool_result 配对、空 content 等导致的部分
```

其中，`too_long_or_context_full` 已经有 payload guard 方向；`improperly_formed` 需要新增小 payload malformed 修复链路。

### 10.2 只能缓解，不能根治的类型

只能由代理缓解，不能彻底消除：

```text
all_credentials_cooling
upstream_429
stream_idle_timeout
stream_read_error
stream_error_other
```

这些错误主要受上游限流、账号池容量、并发、网络和流式稳定性影响。代理可以优化调度、退避、重试和 fallback，但不能保证上游永远不限流或不中断。

### 10.3 需要配置或账号处理的类型

需要配置或账号处理：

```text
credentials_unavailable
```

这类错误通常不是协议转换问题。

## 11. 针对该请求的修复方案总览

针对 `req_01bGG677KX44sBJfcoW6ycQt` 这类“小 payload malformed”，推荐按以下顺序落地：

1. 新增 malformed 专用脱敏诊断。
2. 修复 current user 只有 `tool_result` 且 `content=""` 的情况。
3. 新增 small malformed 的一次 Kiro-safe retry。
4. 在 retry body 中启用 strict schema sanitizer。
5. 增加工具名重复和 schema 复杂度预检。
6. 单独调整 too-long 类错误的 payload guard 阈值。

这些方案分别说明如下。

## 12. 方案 A：新增 malformed 专用脱敏诊断

### 12.1 目的

当前最大问题是：Kiro 返回 `reason:null`，而 usage 记录没有保存足够的结构信息。

因此需要在发生 `Improperly formed request` 时，记录一个脱敏的 `malformedRequestDebug` 报告。该报告不保存用户正文、不保存工具参数值、不保存账号信息，但保存足够的结构特征。

### 12.2 建议记录字段

建议在 `usage_records.data` 中新增：

```json
{
  "malformedRequestDebug": {
    "version": 1,
    "upstreamMessage": "Improperly formed request.",
    "bodyBytes": 96819,
    "model": "claude-opus-4-7",
    "upstreamModel": "claude-opus-4.7",
    "stream": true,
    "endpoint": "/ha/v1/messages",
    "current": {
      "contentEmpty": true,
      "toolResultCount": 1,
      "imageCount": 0,
      "toolCount": 79,
      "toolsBytes": 62918
    },
    "history": {
      "entries": 14,
      "toolUseCount": 6,
      "toolResultCount": 5,
      "alternationValid": true
    },
    "tools": {
      "count": 79,
      "mappedNameDuplicateCount": 0,
      "largestToolBytes": 5945,
      "schemaMaxDepth": 0,
      "schemaFeatureCounts": {
        "$ref": 0,
        "$defs": 0,
        "anyOf": 0,
        "oneOf": 0,
        "allOf": 0,
        "patternProperties": 0
      }
    },
    "riskFlags": [
      "current_content_empty_with_tool_result",
      "large_tool_set"
    ]
  }
}
```

上面数值中的 schema 统计只是示例。实际实现应根据请求真实 schema 计算。

### 12.3 禁止记录的内容

不得记录：

- 凭据 token。
- refresh token。
- cookie。
- 账号邮箱。
- 账号标签。
- 用户正文原文。
- tool_result 原文。
- 文件内容。
- 图片 base64。
- 完整工具参数值。

如需定位工具 schema，可记录 schema 的 hash、大小、深度和关键字统计，不记录完整 schema 原文。

### 12.4 价值

该方案本身不修复请求，但它能把后续 malformed 从“猜测”变成“可定位”。

没有这一步，后续只能根据 payload breakdown 推断，无法确认到底是空 content、工具 schema、工具名、历史顺序还是 Kiro 模型校验差异。

## 13. 方案 B：修复 current user `tool_result` only 且 `content=""`

### 13.1 背景

该请求中：

```text
currentContentBytes     = 0
currentToolResultCount  = 1
```

当前代码对 assistant 只有 tool_use 的情况已有补空格逻辑：

```text
当 assistant content 为空且存在 tool_use 时，content = " "
```

但 current user 只有 tool_result 时，没有同等占位逻辑。

### 13.2 建议改动

在构建 current user message 时，增加：

```text
如果 current content 为空，并且 validated_tool_results 非空：
    current content = " "
```

该逻辑应放在：

```text
src/anthropic/converter.rs
convert_request_with_model_id(...)
```

大致位置：

1. 已完成 `process_message_content`。
2. 已完成 `validate_tool_pairing`。
3. 已完成 `append_orphan_tool_result_texts`。
4. 创建 `UserInputMessage::new(content, model_id)` 之前。

### 13.3 为什么低风险

空格不会改变用户语义，只是避免 Kiro 对空字符串 content 的潜在拒绝。

该项目已经在 assistant tool_use-only 场景使用空格占位，因此这种做法与现有兼容策略一致。

### 13.4 局限

日志中存在 current content 为空且有 tool_result 的成功请求，因此该问题不是唯一根因。

但对 `req_01bGG...`，该风险特征明确存在。该修复成本低、风险低，建议优先落地。

## 14. 方案 C：新增 small malformed 的 Kiro-safe retry

### 14.1 背景

现有 retry 逻辑主要面向 too-long：

```text
too-long/context-full -> payload guard retry
large improperly formed -> 按疑似 too-long retry
small improperly formed -> 不 retry
```

该请求属于：

```text
small improperly formed
```

因此需要新增独立的 small malformed retry。

### 14.2 触发条件

建议触发条件：

```text
上游错误匹配 Improperly formed request
AND 尚未做过 malformed retry
AND provider 调用在产生任何下游 stream chunk 前失败
AND payload guard enabled
AND 请求带有至少一个 malformed risk flag
```

risk flag 可包括：

```text
current_content_empty_with_tool_result
large_tool_set
tool_schema_complex
tool_name_duplicate
history_order_suspicious
tool_result_not_for_last_assistant
```

对本次请求，至少命中：

```text
current_content_empty_with_tool_result
large_tool_set
```

### 14.3 retry body 改写顺序

建议 retry body 只做一次，且按固定顺序改写：

1. 执行现有 payload guard 协议修复。
2. 给 current user 空 content 补 `" "`。
3. 对工具 schema 使用 strict Kiro-safe sanitizer。
4. 压缩工具 description。
5. 检查并修复映射后工具名重复。
6. 重新计算 payload breakdown。
7. 发送同一个上游模型，不切换凭据。

### 14.4 为什么不切换凭据

malformed 是请求形态问题。原样换账号没有意义。

如果 retry body 已经改变，可以使用同一个已调度凭据重试一次。这样可以验证“修复后的请求形态”是否被 Kiro 接受，同时避免污染其他账号。

### 14.5 为什么只重试一次

避免 retry loop。

如果 Kiro-safe retry 仍然返回 malformed，应把失败记录为：

```text
improperly_formed_after_safe_retry
```

并保留 `malformedRetryReport`，用于后续定位。

### 14.6 stream 请求的注意事项

只有在上游还没有产生任何 stream chunk 前失败时，才允许 retry。

如果下游已经收到部分流式内容，则不能透明重试，否则会造成重复输出或协议错乱。

本次请求属于上游直接 400，未产生有效 stream 内容，因此符合 retry 安全条件。

## 15. 方案 D：strict Kiro-safe schema sanitizer

### 15.1 背景

现有 `normalize_json_schema` 已经会清洗一部分 MCP/OpenAPI/Zod schema，但 Kiro 对工具 schema 的真实接受范围可能更窄。

该请求包含：

```text
currentToolCount  = 79
currentToolsBytes = 62918
largestToolBytes  = 5945
```

大量工具定义会放大单个脏 schema 导致整次请求失败的概率。

### 15.2 strict sanitizer 目标

strict sanitizer 不是为了保持最完整 schema，而是为了让 Kiro 接受请求。

建议目标：

```text
把工具 input_schema 降级到 Kiro 最稳定接受的 JSON Schema 子集。
```

### 15.3 建议规则

root：

```text
必须为 object
必须有 type = "object"
必须有 properties object
```

required：

```text
required 必须是字符串数组
只保留存在于 properties 的字段
空 required 可删除或置为空数组
```

复杂关键字：

```text
移除或降级 $ref
移除或降级 $defs
移除 dependentSchemas
移除 patternProperties
移除 unevaluatedProperties
移除 oneOf/anyOf/allOf，或只保留可安全合并的 object properties
```

深度：

```text
限制 schema 最大递归深度
超过深度后降级为宽松 object/string
```

enum：

```text
只保留 string/number/boolean/null 等简单 scalar enum
过大 enum 可截断或移除
```

异常 schema：

```text
无法安全规范化时，降级为：
{"type":"object","properties":{}}
```

### 15.4 使用时机

不建议默认首发就使用最严格 sanitizer，因为这可能削弱工具参数提示质量。

推荐使用时机：

```text
仅在 small malformed retry 中启用 strict sanitizer
```

这样正常请求保持原有能力，只有 Kiro 已经拒绝时才降级。

## 16. 方案 E：工具名重复与映射预检

### 16.1 背景

Kiro 通常要求工具名有效且唯一。当前代码有工具名 sanitize/shorten/hash 逻辑，但仍应显式检查映射后的工具名是否重复，尤其是大小写不敏感重复。

### 16.2 建议检查

在转换工具后检查：

```text
mapped_name lower-case 是否重复
mapped_name 是否为空
mapped_name 是否符合 Kiro 命名规则
mapped_name 长度是否超过限制
历史 tool_use name 是否能映射到当前 tools
```

### 16.3 处理方式

如果发现重复：

```text
对重复项追加稳定 hash 后缀
同步更新 tool_name_map
同步更新历史 tool_use 中的 name
```

如果发现无法修复：

```text
记录 malformed risk flag
在 strict retry 中降级或跳过问题工具
```

## 17. 方案 F：too-long 类错误的独立配置修复

### 17.1 背景

最近 24 小时有 `103` 条：

```text
too_long_or_context_full
```

这些请求的 final body 大多在：

```text
441KB - 563KB
```

现网配置：

```text
payloadGuardEnabled = true
payloadGuardMaxBytes = 563200
payloadGuardTrimHistory = true
payloadShaping.fitCurrentPayloadToBudget = true
payloadShaping.compressToolDefinitions = true
```

其中 `payloadGuardMaxBytes = 563200` 对 Kiro 实际限制偏乐观。很多请求虽然低于该阈值，仍然被 Kiro 返回 too-long。

### 17.2 修复方向

该类错误应单独处理：

```text
降低 payloadGuardMaxBytes
启用或评估 payloadGuardMode = on_too_long
保留 preemptive 模式用于高风险入口
增强当前内容、tool_result、documents、images 和 tools 的 budget 分配
```

### 17.3 与本次 malformed 的关系

降低 `payloadGuardMaxBytes` 不能修复 `req_01bGG...`。

原因：

```text
req_01bGG body = 96819 bytes
```

它远低于任何合理的 too-long 阈值。该请求需要 small malformed 修复链路，而不是 size guard。

## 18. 不推荐方案

### 18.1 不推荐原样重试

原样重试同一个 request body 通常仍会 400。

### 18.2 不推荐切换账号重试

malformed 是请求体问题，换账号通常无效，还会污染多个凭据的错误统计。

### 18.3 不推荐默认删除所有工具

直接删除所有工具可能让请求通过，但会破坏 agent 功能。工具降级应作为 retry fallback，并尽量保留工具名和基础参数结构。

### 18.4 不推荐把所有 malformed 都当 too-long

最近 24 小时 `50` 条 malformed 全部低于 guard 阈值，说明它们不是当前 guard 视角下的 oversized 请求。

如果把所有 malformed 都裁剪历史，可能无法修复根因，还会损失上下文。

## 19. 代码落点建议

### 19.1 `src/anthropic/converter.rs`

建议修改：

```text
convert_request_with_model_id(...)
convert_tools(...)
normalize_json_schema(...)
```

新增或调整：

```text
current tool_result-only content placeholder
strict schema sanitizer
tool name duplicate preflight
malformed risk flag collection
```

### 19.2 `src/anthropic/payload_guard.rs`

建议修改：

```text
PayloadGuardReport
breakdown_kiro_request(...)
repair_current_orphan_tool_results(...)
```

新增：

```text
MalformedRequestDebug
MalformedRiskFlags
strict retry transformation report
```

### 19.3 `src/anthropic/handlers.rs`

建议修改：

```text
stream request error branch
non-stream request error branch
should_retry_payload_guard_after_error(...)
```

新增：

```text
should_retry_malformed_with_safe_body(...)
PayloadMalformedRetryRequest
malformed safe retry once
malformed retry usage recording
```

注意：small malformed retry 不应和 too-long retry 混在一个条件里。它们的触发原因、修复动作和统计字段不同。

### 19.4 `src/anthropic/usage.rs` 与 storage

建议新增 usage data 字段：

```text
malformedRequestDebug
malformedRetryReport
```

要求：

```text
脱敏
结构化
可按字段查询
不保存账号标签或用户正文
```

### 19.5 配置

建议新增配置：

```json
{
  "malformedRetryEnabled": true,
  "malformedRetryStrictSchema": true,
  "malformedRetryCompressTools": true,
  "malformedRetryMaxAttempts": 1,
  "malformedDiagnosticsEnabled": true
}
```

生产默认建议：

```text
malformedDiagnosticsEnabled = true
malformedRetryEnabled = true
malformedRetryMaxAttempts = 1
```

strict schema 和 compress tools 可先在 debug 或灰度开启，再逐步扩大。

## 20. 验收标准

### 20.1 单元测试

必须增加测试：

1. current user 只有 `tool_result` 且 content 为空时，转换后 content 为 `" "`。
2. current user 有正常文本时，不替换文本。
3. orphan tool_result 仍按现有逻辑移除或转文本。
4. strict schema sanitizer 能处理：
   - `required: null`
   - `properties: null`
   - `$ref`
   - `$defs`
   - `anyOf/oneOf/allOf`
   - 过深嵌套
5. 映射后工具名重复能被检测或修复。
6. small malformed retry 只触发一次。
7. too-long retry 和 malformed retry 的统计字段互不混淆。

### 20.2 集成测试

建议构造一个脱敏 fixture，模拟本次请求形态：

```text
stream = true
model = claude-opus-4-7
current content = ""
current tool_result count = 1
history tool_use count = 6
history tool_result count = 5
tool count = 79
tools bytes roughly = 60KB
```

测试目标：

```text
首发 body 保持现有形态
上游模拟返回 malformed
代理构造 safe retry body
safe retry body 中 content 不为空
safe retry body 中 schema 已 strict sanitize
safe retry 只执行一次
usage 记录包含 malformedRetryReport
```

### 20.3 生产观测指标

上线后应观察：

```text
improperly_formed 总量
improperly_formed_under_guard_limit 总量
malformed_safe_retry_success 数量
malformed_safe_retry_failed 数量
tool_call_error 是否上升
平均延迟是否上升
上游 429 是否上升
```

成功标准：

```text
under-guard malformed 明显下降
safe retry 成功率可观
没有明显增加 429、stream_error 或工具调用失败
```

## 21. 风险与权衡

### 21.1 schema 降级风险

strict schema sanitizer 可能降低模型对工具参数的理解精度。

缓解：

```text
仅在 malformed retry 中启用
保留工具名和 description
尽量保留 properties 的基础类型
记录 sanitizer report
```

### 21.2 retry 延迟风险

malformed safe retry 会增加一次上游调用延迟。

缓解：

```text
只在明确 malformed 且命中 risk flag 时 retry
只 retry 一次
记录 retry duration
```

### 21.3 流式重复输出风险

如果在已向下游输出部分 stream 后 retry，会造成协议错乱。

缓解：

```text
只有 provider 在返回 Response 前失败时 retry
已产生任何 chunk 后禁止 retry
```

### 21.4 误判风险

不是所有 malformed 都能通过 schema 或 content 修复。

缓解：

```text
记录 malformedRequestDebug
记录 malformedRetryReport
失败后不继续循环
保留原始错误映射
```

## 22. 结论

`req_01bGG677KX44sBJfcoW6ycQt` 的主因是 Kiro 上游拒绝转换后的请求体：

```text
400 Bad Request {"message":"Improperly formed request.","reason":null}
```

它不是 payload 过大、不是模型整体不可用、不是凭据整体不可用、不是网络问题，也不应通过原样换账号重试解决。

该请求的关键风险形态是：

```text
小 payload，约 96KB
current content 为空
current message 只有 tool_result
当前携带 79 个工具定义
工具定义约 62.9KB
历史中存在多轮 tool_use/tool_result
payload guard 没有做任何修复
```

最新版 `v0.0.35` 不能自动修复该请求，因为其 malformed retry 只覆盖“大于 retry 阈值的 malformed”，而该请求远低于阈值。

推荐修复路径：

1. 新增 malformed 脱敏诊断。
2. 给 current tool_result-only 空 content 补 `" "`。
3. 新增 small malformed 的一次 Kiro-safe retry。
4. 在 retry 中启用 strict schema sanitizer 和工具定义压缩。
5. 增加工具名重复、schema 复杂度、history 顺序等预检。
6. 对 too-long/context-full 类错误单独调低 payload guard 阈值。

其中，第 2、3、4 项最可能直接修复本次错误形态；第 1 项用于消除后续定位歧义；第 6 项用于处理另一类现网 too-long 错误，不能替代本次 malformed 修复。

