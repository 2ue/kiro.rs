# Production usage error evidence and optimization analysis

日期: 2026-06-30

范围: 远端生产服务 `152.53.194.142` 的 `usage_records` 错误记录只读分析。本文固化已查询到的证据，供后续代码修复、调度优化和验证设计使用。

重要约束:

- 远端是现网服务，只能只读分析。
- 不重启服务，不改数据库，不清理日志，不跑压测，不执行迁移。
- 远端部署版本不一定等于当前本地 `main`，所以每个结论都区分“现网证据”和“当前代码是否仍可能存在”。
- 后续任何修复都必须能验证。不能设计验证闭环的项不进入实施清单。
- 本文不记录服务器密码、账号 token、完整凭证、外部池密钥等敏感信息。

## 1. 已确认的远端环境证据

远端服务形态:

- Docker 部署。
- 主服务容器: `kiro-rs-app`
- 镜像: `ghcr.io/2ue/kiro-rs:latest`
- 端口映射: `0.0.0.0:40182->8990/tcp`
- 应用进程:

```text
./kiro-rs -c /app/config/config.json --credentials /app/config/credentials.json
```

数据库容器:

- Postgres 容器: `kiro-rs-postgres`
- Postgres 镜像: `postgres:18-alpine`
- Redis 容器: `kiro-rs-redis`
- Redis 镜像: `redis:7-alpine`

数据库:

- database: `kiro_db_5c0aca87`
- user: `kiro_u_b8e7b551`

同机还存在其他服务:

- `kiro-go-app`
- `codex2api-app`
- `sub2api-app`
- `caddy`

这些服务不是本次分析对象，不能触碰。

## 2. `usage_records` 表结构和索引证据

`usage_records` 字段:

```text
id:text
created_at:timestamptz
endpoint:text
stream:boolean
model:text
conversation_id:text
credential_id:bigint
credential_label:text
status:text
usage_source:text
total_input_tokens:int
compat_input_tokens:int
billable_input_tokens:int
output_tokens:int
cache_read_input_tokens:int
cache_creation_input_tokens:int
cache_creation_5m_input_tokens:int
cache_creation_1h_input_tokens:int
estimated_cost_usd:double
pricing_available:boolean
pricing_model:text
duration_ms:bigint
simulated:boolean
sticky_bound:boolean
fallback_from_sticky:boolean
error_type:text
error_message:text
error_detail:text
data:jsonb
deleted_at:timestamptz
updated_at:timestamptz
```

`usage_records` 索引:

```text
idx_usage_records_created_at(created_at DESC) WHERE deleted_at IS NULL
idx_usage_records_status_created(status, created_at DESC) WHERE deleted_at IS NULL
idx_usage_records_credential_created(credential_id, created_at DESC) WHERE deleted_at IS NULL
idx_usage_records_model_created(model, created_at DESC) WHERE deleted_at IS NULL
idx_usage_records_conversation(conversation_id) WHERE deleted_at IS NULL
```

表规模估算:

```text
usage_records ~80,382 rows
usage_cache_read_rollup_time_buckets ~752,734 rows
usage_duration_rollup_time_buckets ~913,440 rows
usage_rollup_totals ~247,524 rows
usage_cache_read_totals ~306,498 rows
```

分析原则:

- 可以查 `usage_records` 的最近错误，因为有 `idx_usage_records_status_created(status, created_at DESC)`。
- 不应该随意查 rollup 大表，因为规模明显更大且不是本次错误根因分析必需。

## 3. 近期错误总量证据

查询窗口:

```text
last 2h: 0
last 6h: 29
last 24h: 133
```

最近 6 小时按 `error_type`:

```text
api_error: 28
bad_request: 1
```

解释:

- 最近 2 小时没有错误，说明不是持续高频爆发。
- 24 小时内 133 条足够做模式分析。
- `error_type` 过粗，大部分都落到 `api_error`，必须基于 `error_message/error_detail/data` 再分类。

## 4. 24 小时错误分类证据

所有 133 条均可归入以下类别:

```text
thinking_signature_invalid  | 40 | 2026-06-29 15:32:41+08 to 20:48:59+08
invalid_tool_use_format     | 37 | 2026-06-29 15:24:40+08 to 23:34:59+08
image_too_large             | 29 | 2026-06-29 16:16:11+08 to 2026-06-30 00:34:42+08
tool_schema_top_level_union | 21 | 2026-06-29 18:34:51+08 to 20:39:17+08
local_capacity_unavailable  | 5  | 2026-06-29 15:58:24+08 to 17:34:00+08
invalid_tool_use_id         | 1  | 2026-06-29 16:26:30+08
```

价值判断:

- 高价值修复候选: `tool_schema_top_level_union`, `image_too_large`, `thinking_signature_invalid`, `invalid_tool_use_id`
- 需要更多诊断再定性: `invalid_tool_use_format`
- 低频且当前代码已有较多调度优化，优先做回归验证: `local_capacity_unavailable`

## 5. 端点分布证据

24 小时内按端点和错误类别:

```text
thinking_signature_invalid | /cc/v1/messages | 33
api_error                  | /cc/v1/messages | 24
image_too_large            | /na/v1/messages | 22
invalid_tool_use_format    | /ha/v1/messages | 22
invalid_tool_use_format    | /cc/v1/messages | 15
thinking_signature_invalid | /ha/v1/messages | 6
image_too_large            | /cc/v1/messages | 4
image_too_large            | /ha/v1/messages | 3
api_error                  | /ha/v1/messages | 2
api_error                  | /v1/messages    | 1
thinking_signature_invalid | /v1/messages    | 1
```

判断:

- `thinking_signature_invalid` 主要集中在 `/cc/v1/messages`。
- `image_too_large` 主要集中在 `/na/v1/messages`，但 `/cc` 和 `/ha` 也有，说明不是单一路径独有。
- `invalid_tool_use_format` 分布在 `/cc` 和 `/ha`，更像通用 Claude Code/Kiro payload 兼容问题。

## 6. 模型分布证据

24 小时内按模型和错误类别:

```text
thinking_signature_invalid | claude-opus-4-8             | 22
api_error                  | claude-opus-4-6             | 21
invalid_tool_use_format    | claude-opus-4-8             | 18
invalid_tool_use_format    | claude-sonnet-4-6           | 12
thinking_signature_invalid | claude-sonnet-4-6           | 11
image_too_large            | claude-opus-4-5-20251101    | 8
thinking_signature_invalid | claude-opus-4-6             | 6
image_too_large            | claude-opus-4-6             | 6
image_too_large            | claude-opus-4-8             | 5
api_error                  | claude-opus-4-8             | 5
image_too_large            | claude-sonnet-4-6           | 4
invalid_tool_use_format    | claude-haiku-4-5-20251001   | 4
image_too_large            | claude-opus-4-7             | 3
image_too_large            | claude-sonnet-4-5-20250929  | 3
invalid_tool_use_format    | claude-opus-4-7             | 3
api_error                  | claude-haiku-4-5-20251001   | 1
thinking_signature_invalid | claude-opus-4-7             | 1
```

判断:

- 错误不是单一模型造成。
- `thinking_signature_invalid` 和 `invalid_tool_use_format` 在 opus/sonnet 上都出现，优先看协议/payload 转换。
- `image_too_large` 与模型关系不强，优先看多模态历史消息裁剪。

## 7. 典型错误样本证据

### 7.1 `image_too_large`

典型样本:

```text
400 Bad Request {"message":"Bedrock error message: The model returned the following errors: messages.50.content.1.image.source.base64: image exceeds 5 MB maximum: 5452500 bytes > 5242880 bytes","reason":"IMAGE_SIZE_EXCEEDED"}
```

另一个变体:

```text
messages.N.content.1.image.source.bytes: image exceeds 5 MB maximum: 8265836 bytes > 5242880 bytes
```

关键特征:

- 报错指向 `messages.50`, `messages.86` 等历史消息位置。
- 不是只发生在当前用户最新图片上。
- 上游限制是单张图片最大 5 MB。

初步结论:

- 当前 payload guard 的图片处理主要面向 current message，不足以处理历史消息里超 5 MB 的图片。
- 如果历史图片继续透传，上游会直接 400，无法靠重试账号解决。

### 7.2 `invalid_tool_use_format`

典型样本:

```text
400 Bad Request {"message":"Invalid tool use format.","reason":"REQUEST_BODY_INVALID"}
```

最近样本特征:

```text
routeKind=local_credential
routeSubtype=local_error_no_fallback
endpoint=/cc/v1/messages 或 /ha/v1/messages
```

判断:

- 这是 Kiro 上游返回的 400。
- 这不是本地 Claude Code CLI 的 `Invalid tool parameters`。本地 CLI 的 ask-user-question 参数问题已经在当前本地提交 `451602a fix: repair Claude Code ask user question tool input` 中修过，但它不等价于所有 Kiro 上游 `Invalid tool use format`。
- 当前主线已有 `tool_format_debug` 异步写盘诊断能力，但现网版本不一定包含，且远端样本中没有直接看到可用于定位的完整诊断引用。

### 7.3 `tool_schema_top_level_union`

典型样本:

```text
400 Bad Request {"message":"Bedrock error message: The model returned the following errors: tools.66.custom.input_schema: input_schema does not support oneOf, allOf, or anyOf at the top level","reason":"TOOL_SCHEMA_INVALID"}
```

关键特征:

- 上游明确说明 tool `input_schema` 顶层不支持 `oneOf`, `allOf`, `anyOf`。
- 这是明确的协议兼容问题，不是账号、代理或调度问题。

初步结论:

- 当前本地代码仍需要重点检查和修复。当前 `normalize_json_schema` 会规范化这些关键字，但不会删除或扁平化 root 层组合 schema。

### 7.4 `thinking_signature_invalid`

典型样本:

```text
bad_request: {"error":{"type":"<nil>","message":"上游 API 400: {... \"Invalid `signature` in `thinking` block\", \"reason\":\"THINKING_SIGNATURE_INVALID\"} ..."}}
```

已观察到的路由样本:

```text
routeKind=external_pool
routeSubtype=external_fallback_preflight
fallbackReason=local_capacity_exhausted 等
```

关键特征:

- 最新抽样中，这类错误至少有一部分来自外部池 fallback/preflight 路径。
- 错误不是 Kiro 本地账号返回，而是外部池链路里上游拒绝了 Anthropic thinking block 的 signature。

初步结论:

- 当前本地代码已有历史 thinking 清理逻辑，但需要确认外部池所有转发路径都必定经过同样清理。
- 现有代码里存在“external pool payload guard failed; forwarding original request body”的降级逻辑，这个分支可能把未清理 body 转发给外部池。

### 7.5 `local_capacity_unavailable`

典型样本:

```text
本地账号调度容量暂不可用（可用: 10/47, 临时可调度: 0, global_credential_max_concurrent_requests=10, effective_credential_max_concurrent_requests=50, retry_after_secs=1）
```

关键特征:

- 24 小时内只有 5 条。
- 远端版本可能不含当前主线中已做的本地调度、外部池 fallback、容量快照和 public error 归一化修复。

初步结论:

- 不优先按生产证据直接改调度。
- 应该保留为调度回归测试项，特别是本地容量满、外部池 fallback、外部池失败后 local rescue 是否会死循环。

### 7.6 `invalid_tool_use_id`

典型样本:

```text
messages.13.content.1.tool_use.id: String should match pattern '^[a-zA-Z0-9_-]+$'
```

关键特征:

- 24 小时只有 1 条。
- 上游明确要求 `tool_use.id` 只能包含字母、数字、下划线、短横线。

初步结论:

- 这是可低风险修复项。历史 assistant `tool_use.id` 进入 Kiro history 前应做合法化或丢弃处理。

## 8. 远端运行配置证据

远端 runtime config 最新版本:

```text
version=42
updated_at=2026-06-27 23:09:30+08
```

`payloadShaping` 关键配置:

```json
{
  "enabled": true,
  "webFetchTrimEnabled": true,
  "webFetchBodyMaxChars": 12000,
  "currentImagesMaxBytes": 180000,
  "truncateCurrentImages": false,
  "compressToolDefinitions": true,
  "currentDocumentMaxChars": 80000,
  "toolDescriptionMaxChars": 4000,
  "truncateCurrentDocuments": false,
  "currentToolResultMaxChars": 80000,
  "discardHistoricalThinking": true,
  "fitCurrentPayloadToBudget": true,
  "currentUserContentMaxChars": 120000,
  "toolDefinitionsBudgetBytes": 20000,
  "toolSchemaAnnotationMaxChars": 1000,
  "historicalToolResultMaxChars": 8000,
  "historicalToolResultHeadLines": 80,
  "historicalToolResultTailLines": 40,
  "truncateHistoricalToolResults": true
}
```

`externalPools` 关键配置:

```json
{
  "externalPoolsEnabled": true,
  "localPoolPreflightEnabled": true,
  "fallbackOnLocalCapacityExhausted": true,
  "fallbackOnNoAvailableCredentials": true,
  "fallbackOnLocalTransientExhausted": true,
  "externalPoolLocalRescueEnabled": true,
  "externalPoolLocalRescueOnTimeout": true,
  "externalPoolLocalRescueOnCapacity": true,
  "externalPoolLocalRescueOnRateLimit": true,
  "externalPoolGlobalMaxConcurrentRequests": 300,
  "externalPoolRetryMaxAttempts": 3,
  "externalPoolCapacityMode": "wait",
  "externalPoolAutoDisableEnabled": false,
  "externalPoolAutoDisableFailureThreshold": 1,
  "externalPoolAutoDisableDurationSecs": 0
}
```

配置判断:

- `discardHistoricalThinking=true`，理论上历史 thinking 应该被清理。
- `fitCurrentPayloadToBudget=true`，当前消息超预算时会触发当前内容裁剪。
- `truncateCurrentImages=false`，但当前代码在 `fitCurrentPayloadToBudget=true` 且 body 超预算时仍会 drop current images。
- 错误样本中的图片多为 history messages，所以只处理 current images 不够。
- 外部池开启且 fallback 开启，`thinking_signature_invalid` 出现在 external fallback 路径时，需要确认外部转发 body 是否使用了清理后的 body。

## 9. 当前本地代码对应证据

当前本地提交:

```text
451602a fix: repair Claude Code ask user question tool input
```

### 9.1 Tool schema root union

代码位置:

- `src/anthropic/converter.rs:35` `normalize_json_schema`
- `src/anthropic/converter.rs:40` 对 root 调 `normalize_schema_object(&mut obj, true)`
- `src/anthropic/converter.rs:41-50` 强制 root `type=object` 和 `properties={}`
- `src/anthropic/converter.rs:123-125` 对 `oneOf/anyOf/allOf` 仅调用 `normalize_schema_array_keyword`
- `src/anthropic/converter.rs:414-424` `normalize_schema_array_keyword` 会保留非空组合关键字

代码事实:

```text
当前代码会把 root schema 规范成 object，但不会删除或扁平化 root oneOf/anyOf/allOf。
```

与现网错误关系:

```text
现网上游明确拒绝 input_schema 顶层 oneOf/allOf/anyOf。
当前代码仍可能保留这些顶层关键字。
因此这是当前主线仍值得修的高价值问题。
```

### 9.2 Historical images

代码位置:

- `src/model/config.rs:114-118` 只有 `truncate_current_images/current_images_max_bytes`
- `src/model/config.rs:1267-1282` payload shaping 默认只配置 current image 限制
- `src/anthropic/payload_guard.rs:145-146` report 只有 `dropped_current_images/dropped_current_image_bytes`
- `src/anthropic/payload_guard.rs:2765-2786` `drop_anthropic_current_images_to_budget`

代码事实:

```text
当前 payload guard 有 current image drop 统计和处理，但没有明确的 historical image drop 统计。
现网样本指向 messages.50/messages.86 等历史消息位置。
```

与现网错误关系:

```text
这不是纯配置问题。即使 currentImagesMaxBytes 很小，如果历史图片不处理，仍会触发上游单图 5 MB 限制。
```

### 9.3 Historical thinking cleanup and external pool

代码位置:

- `src/anthropic/payload_guard.rs:1479-1483` Anthropic history thinking cleanup 开关
- `src/anthropic/payload_guard.rs:1570-1600` 移除 history 中 `thinking/redacted_thinking/signature`
- `src/anthropic/handlers.rs:926-947` external pool guard 失败时转发原始 body
- `src/anthropic/handlers.rs:6305-6307` thinking/model 等处理后 refresh external fallback payload
- `src/external_pool.rs:3048-3064` 只处理顶层 `thinking` 字段的 `adaptive/disabled` budget，不处理 message content 中 signature

代码事实:

```text
当前代码有历史 thinking 清理，但 external pool guard 失败分支会 fallback 到 original request body。
外部池 prepare 阶段没有二次清理 message content thinking signature。
```

与现网错误关系:

```text
thinking_signature_invalid 的抽样路由是 external_pool/external_fallback_preflight。
如果 external guard 成功，理论上历史 signature 应被移除。
如果 external guard 失败或某条外部池路径绕过 guard，signature 可能继续透传。
```

### 9.4 Tool format diagnostics

代码位置:

- `src/model/config.rs:123-153` `ToolFormatDebugConfig`
- `src/model/config.rs:2499-2536` 默认开启，异步写 `logs/tool-format-debug`
- `src/anthropic/tool_format_debug.rs:177-181` 启动 mpsc writer
- `src/anthropic/tool_format_debug.rs:192-289` `record()` 非阻塞采样，队列满丢弃，不阻塞主请求
- `src/anthropic/tool_format_debug.rs:317-344` 异步 append jsonl
- `src/anthropic/handlers.rs:3218-3245` 仅在 upstream tool use format error 时记录诊断

代码事实:

```text
当前主线已经有非阻塞、限流、异步写盘的 tool format debug。
这能帮助定位 Invalid tool use format，但现网版本未必包含，且旧 usage 中未看到完整 debug 样本。
```

与现网错误关系:

```text
现网 invalid_tool_use_format 还不能仅凭 usage 字段定位具体是哪类 tool 结构问题。
如果当前主线已部署，应检查 tool-format-debug jsonl 是否产生，并在 usage payloadGuardReport 中能否看到 debug ref。
```

### 9.5 `tool_use.id` legality

代码位置:

- `src/anthropic/converter.rs:2439-2459` 转换 assistant `tool_use` 时只 trim 和判空，没有看到字符合法化

代码事实:

```text
当前代码跳过空 id，但不保证 id 匹配 ^[a-zA-Z0-9_-]+$。
```

与现网错误关系:

```text
现网已有 1 条明确 tool_use.id pattern 错误。
这是低频但修复成本低、可验证性强的问题。
```

### 9.6 外部池 public error masking

代码位置:

- `src/external_pool.rs:541-608` `ExternalPoolFinalError::public_error/into_response` 对下游返回归一化 public message
- `src/external_pool.rs:2252-2285` `record_external_failure` 保存原始 error message/detail 到 usage
- `src/external_pool.rs:2428-2431` usage 同时记录 requested model/upstream model/external outbound model
- `src/external_pool.rs:3278-3297` public error 从 final error 归一化

代码事实:

```text
当前主线已经区分 usage 内部错误和对下游 public error。
外部池原始错误应保存在 usage 中用于排查，但对下游不应原样暴露。
```

需要验证:

```text
外部池欠费、429、5xx、HTML 成功页、error envelope with 200、网络超时等路径，是否全部返回统一 public error，并保留内部 usage detail。
```

## 10. 可实施修复候选和验证要求

### 候选 A: root `oneOf/anyOf/allOf` 工具 schema 兼容

优先级: 高

证据:

- 现网 21 条 `tool_schema_top_level_union`。
- 上游明确拒绝 top-level `oneOf/allOf/anyOf`。
- 当前代码仍保留 root 组合关键字。

建议修复:

1. 在 `normalize_json_schema` root 级别增加专用处理。
2. root `allOf`:
   - 如果子项是 object schema，合并 `properties`。
   - `required` 可以取并集，因为 `allOf` 语义是同时满足。
   - 不可合并字段丢弃或降级为 annotation，不保留 root `allOf`。
3. root `oneOf/anyOf`:
   - 如果子项是 object schema，合并所有 `properties`。
   - `required` 不宜取并集，否则会过度约束。建议取空或取所有分支 required 的交集。
   - 不保留 root `oneOf/anyOf`。
4. 如果没有可合并 object schema，降级为空 object schema:

```json
{"type":"object","properties":{}}
```

验证要求:

- 单测:
  - root `oneOf` 输入，输出不包含 root `oneOf/anyOf/allOf`，仍是 object。
  - root `anyOf` 输入，输出不包含 root 组合关键字。
  - root `allOf` 输入，properties 合并，required 取并集。
  - nested property 里的 `oneOf/anyOf/allOf` 是否保留需要按 Kiro 实测决定，至少不能破坏已有测试。
- 协议集成测试:
  - 构造带 root `oneOf` 的 tool input_schema，通过本地 `/cc/v1/messages` 走转换，确认发给 Kiro/mock 的 schema 不含顶层组合关键字。
- 真实验证:
  - Claude Code CLI 场景触发 MCP 工具 schema 中 root union，确认不再出现 `TOOL_SCHEMA_INVALID`。

风险:

- schema 被放宽，模型可能生成更宽的 input。
- 这是可接受风险，因为当前行为是整次请求 400。要用诊断记录保留 schema 改写计数，便于后续观察。

### 候选 B: 历史图片超过 5 MB 的预处理

优先级: 高

证据:

- 现网 29 条 `image_too_large`。
- 样本指向 `messages.N.content.*.image`，多为历史消息，不是 current image。
- 当前配置和代码主要处理 current images。

建议修复:

1. 在 payload shaping 增加历史图片处理能力:
   - 新统计字段: `dropped_historical_images`, `dropped_historical_image_bytes`
   - 新配置可选字段: `historicalImagesMaxBytes`，默认可设为 `5 * 1024 * 1024` 或略低于上游阈值。
2. 对历史消息中的 image block:
   - 如果单图超过阈值，替换为 text block:

```json
{"type":"text","text":"[Historical image omitted because it exceeded the image size limit.]"}
```

3. 对 current message:
   - 即使 body 没超过总 `max_bytes`，也应该单独检查单图是否超过上游单图限制。
   - current image 是否 drop 需要更谨慎，可以优先返回明确 `invalid_request_error`，提示用户压缩图片；历史 image 更适合自动替换。
4. usage payload guard report 记录删除数量和字节数。

验证要求:

- 单测:
  - history 中 5.4 MB base64 image 被替换为 text。
  - history 中 4.9 MB image 保留。
  - current 5.4 MB image 按设计返回错误或被处理，不能透传给上游后再 400。
- 集成测试:
  - 构造 `messages.50.content.1.image.source.base64` 超 5 MB 的请求，确保 guard 后 body 不含该 oversized image。
- 真实验证:
  - Claude Code CLI 长会话中包含历史图片，继续对话不应出现 `IMAGE_SIZE_EXCEEDED`。

风险:

- 删除历史图片会降低图像上下文准确性。
- 但现网证据显示这些请求当前直接失败。替换成占位文本比 400 更可用。

### 候选 C: 外部池路径强制清理 thinking signature

优先级: 高

证据:

- 现网 40 条 `thinking_signature_invalid`。
- 抽样路由显示 external pool fallback/preflight。
- 当前代码有 history thinking 清理，但 external guard 失败会转发 original body。

建议修复:

1. 不允许 external pool 在 guard 失败时无条件转发 original body。
2. 增加轻量兜底清理函数:
   - 删除 message content 中 `thinking` block 的 `signature`。
   - 删除 `redacted_thinking`。
   - 如果 `discardHistoricalThinking=true`，删除历史 assistant thinking blocks。
3. external direct/fallback/local rescue 共用同一个 external payload preparation 入口。
4. 如果 guard 失败但轻量清理也失败，返回归一化 public error，并把原始错误记录 usage，不要向外部池转发风险 body。

验证要求:

- 单测:
  - 历史 assistant thinking block 带 signature，external payload body 不含 `signature`。
  - guard 人为失败时，也不能转发含 `signature` 的 original body。
- 集成测试:
  - external pool mock 返回会检查 body，确认所有 external 路径均不含 signature。
- 真实验证:
  - Claude Code CLI thinking/ultrathink 长会话，触发 fallback 到 external pool，不应再出现 `THINKING_SIGNATURE_INVALID`。

风险:

- 如果外部池本身支持 Anthropic signed thinking，删除 signature 可能降低官方 thinking 连续性。
- 但现网证据中的外部池上游不接受该 signature。建议按 pool compat profile 做配置，默认对 Kiro/代理池清理。

### 候选 D: `tool_use.id` 合法化

优先级: 中

证据:

- 现网 1 条 `tool_use.id` pattern 错误。
- 当前 converter 只 trim 和判空。

建议修复:

1. 增加 `sanitize_tool_use_id_for_kiro_history`。
2. 规则:
   - 保留 `[a-zA-Z0-9_-]`
   - 其他字符替换为 `_`
   - 空值仍跳过
   - 如果替换后为空，跳过或生成稳定 hash id
3. 必须同步处理 tool_result 的引用关系，避免 assistant `tool_use.id` 被改了但 user `tool_result.tool_use_id` 没改。

验证要求:

- 单测:
  - `toolu:abc/123` 转成合法 id。
  - 对应 tool_result id 同步映射。
  - 空 id 仍按原逻辑跳过。
- 集成测试:
  - 构造 history assistant tool_use id 含非法字符，上游 mock 不返回 pattern 错误。

风险:

- 如果只改 tool_use 不改 tool_result，会引入新的孤儿工具结果。这个修复必须和 tool id map 一起做。

### 候选 E: `Invalid tool use format` 的诊断闭环

优先级: 中到高，取决于当前主线真实复现结果

证据:

- 现网 37 条。
- 远端 usage 中错误信息过粗，无法仅凭现有字段定位具体 payload 结构。
- 当前主线已有异步 tool format debug，但需要验证生产部署后是否能形成定位闭环。

建议修复或验证:

1. 先不盲改转换逻辑。
2. 确认当前主线在 upstream 返回 `Invalid tool use format` 时:
   - usage `payloadGuardReport.tool_format_debug_ref` 有采样信息。
   - `logs/tool-format-debug/tool-format-YYYY-MM-DD.jsonl` 有对应 request_id 或 fingerprint。
   - 队列满时不阻塞请求，只记录 dropped 计数。
3. 如果诊断显示具体原因，再做精准修复:
   - duplicate tool_use ids
   - orphan tool_result
   - non-object tool_use input
   - tool_result 和 last assistant 不匹配
   - cache point 插入后导致格式异常
   - tool choice 与 tools 列表不一致

验证要求:

- 单测:
  - debug recorder 采样限流和 channel full 不阻塞。
  - max record bytes 生效。
- 集成测试:
  - 构造 mock Kiro 返回 `Invalid tool use format`，确认 usage 有诊断引用。
- 真实验证:
  - Claude Code CLI 多轮工具调用、agent、MCP 场景运行 20 轮，确认若再出现错误，有可追踪诊断。

风险:

- 如果在错误发生时同步写大 payload，会引入性能风险。
- 当前主线的 mpsc `try_send` 设计方向正确，后续不能改成同步阻塞写数据库。

### 候选 F: 本地容量不可用和 fallback 死循环验证

优先级: 中

证据:

- 现网 5 条，低频。
- 当前本地代码已经有多处调度和 fallback 相关修复，远端版本可能落后。

建议:

1. 不因这 5 条直接重写调度。
2. 把它列入压测和异常恢复验证:
   - 本地账号全满
   - 外部池可用
   - 外部池失败后 local rescue
   - local rescue 不能再次 fallback 到 external pool 形成死循环
   - external pool global concurrency 满
   - Redis 容量快照过期/缓存命中混合场景

验证要求:

- 调度单测:
  - local capacity full -> external fallback 只发生一次。
  - external fallback fail -> local rescue 禁止再次进入 external fallback。
- 压测:
  - 突发并发、突发大量 400、突发大量 429、外部池恢复。
  - 观察内存、队列长度、Redis 慢日志、PG usage 异步写入延迟。

风险:

- 调度改动最容易引入放大效应。没有复现或明确指标前不应该大改。

## 11. 哪些现网错误不应该切账号重试

不应该切账号重试的错误:

- `Invalid tool use format`
- `TOOL_SCHEMA_INVALID`
- `THINKING_SIGNATURE_INVALID`
- `IMAGE_SIZE_EXCEEDED`
- `CONTENT_LENGTH_EXCEEDS_THRESHOLD`
- `tool_use.id` pattern invalid

原因:

- 这些都是请求体或协议兼容问题。
- 换账号不会改变 payload。
- 切账号会增加上游压力，可能放大并发槽占用。

应该允许 fallback/rescue 的错误:

- local capacity full
- local rate limit
- token refresh transient error
- external pool timeout
- external pool 429
- external pool 5xx
- external pool network error

但需要防死循环:

- local -> external -> local rescue 后，必须标记这次请求已经经过 external fallback。
- rescue local path 必须调用 provider-local entrypoint，不允许再次进入 normal routing。

## 12. 后续如果需要补充远端证据，应该只补这些

原则:

- 不再无目标拉日志。
- 只补能决定修复方向的最小数据。

可补查项:

1. `invalid_tool_use_format` 对应的 `payloadGuardReport.tool_format_debug_ref`
   - 目的: 判断当前主线部署后是否已有诊断闭环。
   - 不需要拉全量日志，只查指定 request_id 的 data 字段或 jsonl 中对应 fingerprint。
2. `thinking_signature_invalid` 的 external attempts
   - 目的: 判断是否集中在某个外部池或某种 fallback reason。
   - 只查 5 条样本的 `externalAttempts/fallbackReason/routeSubtype`。
3. `image_too_large` 的 message index 分布
   - 目的: 判断 history image 还是 current image。
   - 只抽样错误文本，不需要拉原始请求 body。
4. 发布新版本后同样窗口复查错误计数
   - 目的: 验证修复是否生效。
   - 查询方式仍用 `status='error' and created_at >= now() - interval '24 hours'`。

不建议补查:

- 不查完整 credentials。
- 不查完整请求 body。
- 不查 rollup 大表。
- 不拉全量 docker logs，除非需要定位某个 request_id 且 usage 中信息不足。

## 13. 建议实施顺序

第一批，协议确定性问题:

1. root tool schema `oneOf/anyOf/allOf` 扁平化或降级。
2. historical image >5 MB 处理。
3. external pool thinking signature 清理兜底。
4. `tool_use.id` 合法化并同步 tool_result 映射。

第二批，诊断闭环:

1. 验证 `Invalid tool use format` 是否能生成 tool-format debug。
2. 如果不能，修 usage debug ref 挂载逻辑。
3. 用真实 Claude Code CLI 工具/MCP/agent 场景复现并收敛具体格式问题。

第三批，调度压力回归:

1. local capacity full -> external fallback。
2. external fail -> local rescue。
3. 并发队列满、Redis 慢、PG usage 写入慢时不阻塞下游响应。

## 14. 验证矩阵

每个修复完成后都必须覆盖:

```text
cargo test <相关单测>
cargo test anthropic::<payload/schema/tool/thinking 相关测试>
cargo test external_pool::<相关测试>
本地 mock upstream 集成测试
真实本地 9022 调用测试
Claude Code CLI 交互式测试
异常场景测试
并发/突发/恢复测试
usage 记录检查
public error 检查
```

真实 Claude Code CLI 场景至少覆盖:

- 普通多轮对话。
- 长会话。
- tool use。
- MCP 工具。
- agent 派发。
- think/ultrathink。
- 图片输入。
- 大文档和小文档。
- web fetch/web search 如果本地账号支持。
- 模型 alias。
- 外部池 fallback。
- 本地容量满。
- 外部池 400/429/5xx/timeout。

性能和稳定性观察:

- 请求首字延迟。
- 流式输出是否顿挫。
- 内存是否持续上涨。
- Redis 慢日志是否增加。
- PG usage 异步写入是否堆积。
- 调度队列是否堆积。
- 错误时是否出现重试风暴。

## 15. 当前结论

基于 24 小时 133 条错误，最值得转化为代码优化的不是调度容量，而是请求体协议兼容:

1. 工具 schema root union 被 Kiro/Bedrock 明确拒绝，当前主线代码仍有保留 root union 的风险。
2. 历史图片超过 5 MB 会直接 400，当前 current-image-only 的裁剪思路不够。
3. 外部池 fallback 路径出现 thinking signature invalid，当前 external guard failure 转发 original body 是风险点。
4. `tool_use.id` 非法字符是低频但明确的问题，应修。
5. `Invalid tool use format` 需要依赖当前主线的异步 debug 机制形成证据闭环，不能凭现网旧 usage 盲修。

后续实施时，每个修复都必须带单测、集成测试和真实调用验证。没有验证闭环的猜测项不应该进入代码。

## 16. 已实施修复记录

本节记录 2026-06-30 基于上述生产证据落地的代码修复。修复原则是只处理能从 usage 错误和当前代码事实共同确认的问题，不对证据不足的转换语义做猜测性改动。

### 16.1 工具 schema 顶层组合关键字

对应错误: `tool_schema_top_level_union`

证据结论:

- 生产错误明确指向 tool `input_schema` 顶层 `oneOf/allOf/anyOf`。
- 当前 `normalize_json_schema` 会递归清理 schema，但之前仍可能把 root 级组合关键字留在最终 tool schema 顶层。

代码改动:

- `src/anthropic/converter.rs`
- `normalize_json_schema` 在常规 schema 归一化后新增 root combinator 扁平化。
- root `allOf` 合并 object 分支的 `properties`，`required` 取并集。
- root `oneOf/anyOf` 合并 object 分支的 `properties`，`required` 取所有分支交集，避免过度约束。
- 最终不保留 root `oneOf/anyOf/allOf`。

验证:

- `test_normalize_json_schema_flattens_root_union_combinators`
- `test_normalize_json_schema_flattens_root_all_of_required_union`
- `cargo test anthropic::converter::tests`
- `cargo test --locked`
- `cargo test --locked --no-default-features`

风险边界:

- 该修复会放宽 `oneOf/anyOf` 的输入约束。生产风险小于请求直接 400，因为模型仍能看到合并后的字段结构。
- nested property 里的组合关键字保持原有清理逻辑，不做额外展开，避免破坏复杂子 schema。

### 16.2 历史图片超过上游 5 MB 单图限制

对应错误: `image_too_large`

证据结论:

- 生产样本多指向 `messages.N.content.*.image.source.base64`，属于历史消息中的图片。
- 当前已有 current image 裁剪思路，但历史图片即使总 body 未超过 payload guard 阈值，也可能因为单图超过上游限制被拒绝。

代码改动:

- `src/anthropic/payload_guard.rs`
- 新增历史图片安全处理阈值 `UPSTREAM_IMAGE_SOURCE_MAX_BYTES = 5 * 1024 * 1024`。
- Kiro 请求历史中的 oversized image 会从 `conversation_state.history` 移除，并向历史用户文本追加简短占位说明。
- Anthropic external forwarding 请求历史中的 oversized image block 会替换为 text block，占位说明保留被移除 source 字节数和阈值。
- `PayloadGuardReport` 增加:
  - `dropped_historical_images`
  - `dropped_historical_image_bytes`
- 该安全处理现在在 payload shaping 开启时总会执行，不再只在总 body 超限时执行。

验证:

- `kiro_guard_drops_oversized_historical_images_even_when_body_fits`
- `anthropic_guard_drops_oversized_historical_images_even_when_body_fits`
- `cargo test anthropic::payload_guard::tests`
- `cargo test --locked`
- `cargo test --locked --no-default-features`

风险边界:

- 只自动处理历史图片，不删除当前用户本轮图片，避免把用户刚提交的核心输入静默丢掉。
- 对历史图片使用占位文本，比继续透传导致 400 更可用；usage report 会记录删除数量和字节数，便于后续审计。

### 16.3 外部池路径历史 thinking signature 清理

对应错误: `thinking_signature_invalid`

证据结论:

- 生产抽样显示该错误集中出现在 external pool fallback/preflight 路径。
- 代码已有 `discard_historical_thinking` 能力，但之前只有在 body 超过 payload guard 阈值触发 shaping 时才会执行。
- 小请求、正常大小请求仍可能携带历史 signed thinking 进入外部池，被外部上游拒绝。

代码改动:

- `src/anthropic/payload_guard.rs`
- `guard_anthropic_messages_request` 在 shaping 开启时，总是先执行安全 shaping:
  - 按配置删除历史 assistant `thinking/redacted_thinking/signature` block。
  - 删除历史 oversized image。
- `src/anthropic/handlers.rs`
- external pool guard 失败兜底不再标注为“转发原始 body”，而是尽量对 cloned payload 执行安全清理后再序列化转发。

验证:

- `anthropic_guard_discards_historical_thinking_even_when_body_fits`
- `cargo test anthropic::payload_guard::tests`
- `cargo test anthropic::handlers::tests`
- `cargo test external_pool::tests`
- `cargo test --locked`
- `cargo test --locked --no-default-features`

风险边界:

- 默认 `discard_historical_thinking=true`，与当前系统的 Kiro/代理池兼容策略一致。
- 只处理历史 thinking，不处理当前响应流中的 thinking 输出。
- 外部池如果未来明确支持 signed thinking，可以再按 pool compat profile 做更细粒度配置；当前生产证据支持默认清理。

### 16.4 `tool_use.id` 非法字符

对应错误: `invalid_tool_use_id`

证据结论:

- 生产有 1 条明确 pattern 错误: `tool_use.id` 必须满足 `^[a-zA-Z0-9_-]+$`。
- 当前 converter 之前只 trim 和判空，没有字符合法化。

代码改动:

- `src/anthropic/converter.rs`
- 新增 `sanitize_tool_use_id`:
  - 合法 `[a-zA-Z0-9_-]` 原样保留。
  - 非法字符替换为 `_`。
  - 追加原始 id 的 4 字节 SHA256 前缀，降低不同非法 id 清洗后碰撞的风险。
  - 空 id 仍按原逻辑跳过。
- assistant `tool_use.id` 和 user `tool_result.tool_use_id` 使用同一规则独立清洗，保证引用关系稳定一致。

验证:

- `test_tool_use_ids_are_sanitized_consistently`
- `cargo test anthropic::converter::tests`
- `cargo test --locked`
- `cargo test --locked --no-default-features`

风险边界:

- 不改变空 id 的跳过语义。
- 不引入跨请求状态 map，避免内存增长；同一原始 id 通过纯函数得到同一清洗结果。

## 17. 暂不实施的项

### 17.1 `Invalid tool use format` 不做语义改写

对应错误: `invalid_tool_use_format`

本次没有直接修改 tool-use/tool-result 的配对语义，原因:

- 生产 usage 里只有上游粗错误，不能确认是 duplicate id、orphan result、非 object input、cache point 插入、tool choice，还是某类 MCP schema 导致。
- 当前主线已经有 `tool_format_debug` 异步诊断能力，且全量测试覆盖了 recorder 限流和不阻塞路径。
- 盲改转换语义会直接影响 Claude Code CLI 的 agent/tools/MCP 场景，风险高于收益。

当前建议:

- 先用已存在的异步 debug 记录收集真实失败 payload 摘要。
- 收到具体 fingerprint 后再做精准修复。

已验证相关测试:

- `anthropic::tool_format_debug::tests::recorder_writes_sampled_jsonl_and_rate_limits_same_fingerprint`
- `anthropic::handlers::tests::request_body_invalid_tool_format_is_bad_request_diagnostic_error`
- `cargo test --locked`
- `cargo test --locked --no-default-features`

### 17.2 `local_capacity_unavailable` 本次不做调度改动

对应错误: `local_capacity_unavailable`

本次没有继续改调度，原因:

- 生产样本只有 5 条，且远端版本不一定是当前最新。
- 当前代码已有本地容量 fail-fast、外部池 fallback、external payload guard retry、Redis hot op guard、并发模拟等大量测试。
- 本次改动集中在请求体协议兼容，避免把调度行为变化和 payload 修复混在同一个风险面里。

已验证相关测试:

- `cargo test external_pool::tests`
- `kiro::token_manager::manager::tests::test_scheduler_handles_500_daily_credentials_1000_rpm_simulation`
- `kiro::token_manager::manager::tests::test_sonnet_high_concurrency_dispatch_respects_limits_and_spreads_load`
- `cargo test --locked`
- `cargo test --locked --no-default-features`

## 18. 本次本地验证记录

本地 Rust 链接器注意事项:

- 本机 `/Users/yuanfeijie/.volta/bin/cc` 会干扰 Rust 链接。
- 测试使用了系统 clang:

```bash
CLANG="$(xcrun --find clang)"
CLANGXX="$(xcrun --find clang++)"
SDK="$(xcrun --show-sdk-path)"
CC="$CLANG" CXX="$CLANGXX" SDKROOT="$SDK" \
CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER="$CLANG" \
cargo test <filter>
```

已执行并通过:

```text
cargo test anthropic::converter::tests
80 passed

cargo test anthropic::payload_guard::tests
41 passed

cargo test anthropic::handlers::tests
57 passed

cargo test external_pool::tests
65 passed

cargo test --locked --no-default-features
783 main tests passed, 11 kiro_loadtest tests passed

cargo test --locked
783 main tests passed, 11 kiro_loadtest tests passed

git diff --check
passed
```

验证结论:

- 本次修复覆盖 4 类生产中有明确结构证据的问题:
  - `tool_schema_top_level_union`
  - `image_too_large`
  - `thinking_signature_invalid`
  - `invalid_tool_use_id`
- 没有修改外部池调度循环、账号选择、fallback 判定、usage 上报整流核心逻辑。
- `invalid_tool_use_format` 保持诊断优先，不做缺证据的语义修复。

## 19. 后续线上观察点

上线后建议只观察聚合指标，不对生产做压测:

```sql
-- 按错误类型统计是否下降
select
  error_type,
  count(*)
from usage_records
where status = 'error'
  and created_at >= now() - interval '24 hours'
group by error_type
order by count(*) desc;
```

重点预期:

- `tool_schema_top_level_union` 应明显下降或消失。
- `image_too_large` 中历史图片相关错误应明显下降；当前本轮图片过大仍可能返回明确错误。
- external fallback 路径的 `thinking_signature_invalid` 应明显下降。
- `invalid_tool_use_id` 应消失，除非上游还有长度等未暴露限制。
- `invalid_tool_use_format` 如果仍出现，应检查本机 `logs/tool-format-debug/*.jsonl` 的 request id/fingerprint，再做下一轮精准修复。
