# Claude Code Base URL + API Key 直通分析

Role: Implementation-driving protocol and body-boundary analysis

Status: Analyzed; no production behavior changed

As of: 2026-08-29

Related: [Account Runtime Upstream Accounts](../README.md), [Final target plan](final-target-plan.md)

## 结论

如果账号的契约已经限定为“能够接收 Anthropic Messages/Claude Code 协议的 `base_url + api_key` 上游”，外部账号接入不需要当前 `Normalized` 路径那样复杂的 body 重建。

推荐的默认链路是：

```text
Claude Code/Anthropic request
  -> 入口做有限 JSON/协议事实检查
  -> 选择账号
  -> base_url 规范化并拼接 /v1/messages
  -> 删除入站认证，写入账号 api_key/auth scheme
  -> body 原始字节直通
  -> SSE/JSON 原样转发并捕获 usage
  -> 仅在 usage projection 开启时定点改写 usage 字段
  -> 记录账号尝试、usage、费用、延迟和失败恢复
```

当前系统中最重的 `Normalized` body pipeline、typed payload overlay、完整 JSON 重新序列化、thinking 自动规范化以及 payload guard retry，不应成为兼容 Claude Code 账号的默认行为。它们最多作为显式的兼容策略保留，不能混入账号调度和基础直通契约。

## 参考实现：`../sub2api`

本分析只参考 `../sub2api` 中 Anthropic API Key passthrough 相关实现，不把其 OAuth、Bedrock、Vertex、其他平台转换逻辑带入目标系统。

主要参考文件：

- `../sub2api/backend/internal/service/gateway_anthropic_passthrough.go`
- `../sub2api/backend/internal/service/gateway_forward.go`
- `../sub2api/backend/internal/service/gateway_request.go`
- `../sub2api/backend/internal/service/gateway_count_tokens.go`
- `../sub2api/backend/internal/service/gateway_scheduling.go`
- `../sub2api/backend/internal/service/account.go`
- `../sub2api/backend/internal/service/gateway_anthropic_apikey_passthrough_test.go`
- `../sub2api/backend/internal/service/gateway_context_management_test.go`

### 请求链路

`sub2api` 的 API Key passthrough 主要做四件事：

1. 选择账号并读取账号的 `base_url` 与 API key。
2. 将 `base_url` 规范化为 Messages endpoint：末尾路径是 `/v1` 时追加 `/messages`，否则追加 `/v1/messages`；`count_tokens` 使用对应的 `/messages/count_tokens`。
3. 过滤入站 header，删除客户端认证和 Cookie，按账号配置写入 `x-api-key` 或 `Authorization: Bearer`。
4. 发送请求并尽量透传响应；流式路径按 SSE 事件边界读取，同时从 `message_start.message.usage`、`message_delta.usage` 等位置捕获 usage。

这条链路没有把 Claude Messages body 转成另一种平台协议，也没有依赖账号 OAuth、平台事件流、区域、profile 或供应商专用 envelope。

### body 处理

`sub2api` 的“透传”不是绝对零处理，但处理范围很窄：

- `StripEmptyTextBlocks`：发现空 text block 时才删除，避免部分上游返回 400。
- `FilterWebSearchHistoryBlocks`：针对 web-search 历史 block 的兼容性过滤；这是第三方上游兼容策略，不是 Claude Code API Key 直通的必要步骤。
- `sanitizeAnthropicBodyForBetaTokens`：仅当 body 含 `context_management` 且最终 `anthropic-beta` 不含 `context-management-2025-06-27` 时删除该字段；这是 body 与 header 能力对称保护。
- 模型映射：只改顶层 `model`，不重建 `messages`、`tools`、`thinking` 或未知字段。

只有发生上述定点变更时才重新生成 body。没有变更时，body 保持原始内容和字段语义。

### header 处理

其 passthrough header 策略是白名单而不是全量复制：

- 保留 `anthropic-version`、`anthropic-beta`、`User-Agent`、`content-type`、`accept`、Claude Code/Anthropic SDK 的 `x-stainless-*`、`x-claude-code-session-id`、`x-client-request-id` 等必要头。
- 删除 `Authorization`、`x-api-key`、`x-goog-api-key`、`Cookie` 等入站认证残留。
- 缺省补 `content-type: application/json` 和 `anthropic-version: 2023-06-01`。
- 最后应用账号级 header override。

`?beta=true` 是 `sub2api` 对其默认 Anthropic 直连实现的具体约定，不应无条件复制到所有自定义兼容上游。目标系统应以账号的 endpoint/header capability 为准，不能因为参考实现带有该查询参数就改变所有上游的 URL 契约。

### 响应和 usage

`sub2api` 的响应处理重点不在重写内容，而在可靠读取和 usage 记录：

- 流式响应按 SSE 事件边界读取，尽量原样写回客户端。
- 从 `message_start`、`message_delta` 捕获输入、输出、缓存创建和缓存读取 usage。
- 非流式响应读取 JSON 顶层 `usage`，成功响应不是 JSON 时触发 failover，而不是把它伪装成成功结果。
- 客户端断开后仍可继续消费上游，以完成 usage 记录。
- 账号重试、跨账号 failover、冷却和错误分类与 body 透传是分开的。

## 当前 Rust 外部账号链路盘点

### 已经接近目标的部分

当前 `RawPassthrough` 路径已经具备直通所需的基础形状：

- `src/anthropic/request_facts.rs` 只对顶层 `model`、`stream`、`max_tokens`、`thinking`、`output_config` 做有限 probe，不反序列化完整 `messages` 图。
- `src/external_pool/body_pipeline.rs:394-447` 的 raw 路径可直接返回 `effective_raw_body`，只有配置了模型策略时才定点改写顶层 `model`。
- `src/external_pool.rs:9427-9444` 已按 `base_url` 是否包含 `/v1` 拼出 `/v1/messages` 或 `/messages`。
- `src/external_pool.rs:9485-9531` 已过滤 hop-by-hop 与入站认证 header，并按账号 auth type 写入 Bearer 或 `x-api-key`。
- `src/external_pool.rs:6055` 之后的执行路径已经把 lease、attempt budget、SSE、usage capture、响应超时和客户端断开处理放在调度执行层，而不是 body parser 内。

这些属于通用账号网关能力，应保留并继续从旧模块命名中抽离。

### 当前仍然过重的部分

`Normalized` 路径位于 `src/external_pool/body_pipeline.rs:20-519`，主要开销和语义风险包括：

1. 依赖完整 `MessagesRequest` typed payload。
2. 运行 payload guard、图片/历史 shaping 和 fallback sanitize。
3. 对原始 JSON 与 typed JSON 做 overlay 合并。
4. 重新序列化完整 body，再次 probe。
5. 默认执行 `normalize_external_pool_thinking_value`。
6. 可能触发 `src/external_pool/retry_pipeline.rs:20-59` 的 payload-too-large normalized retry。

这套逻辑的价值是修复非标准或超限上游的兼容性，而不是把一个已经兼容 Claude Code 的账号请求发送出去。对于 Claude Code 原生 body，它会增加 CPU、内存、延迟和语义变化风险，并可能影响未知字段、tool/thinking signature、`context_management`、prompt-cache metadata 以及 failover 时的 body 一致性。

### 入口仍会做的处理需要分层

`src/anthropic/handlers/request_entry.rs:34-106` 当前在选择账号前会做多项操作：

- raw JSON probe 和大小/嵌套/重复 key 检查：这是通用安全和协议 admission，可保留。
- `missing_max_tokens`：这是本地产品策略，会改变 body；应明确标为入口策略，不要伪装成账号协议转换。
- `validate_raw_reasoning_protocol_with_probe`：如果目标协议要求严格的 Anthropic thinking 结构，这是通用协议校验；如果目标是完全交给兼容上游校验，应降为可配置 admission，而不是账号 body rewrite。
- transcript/history sanitization：仅在检测到污染并且产品选择宽松兼容时才改写；纯 raw 账号应优先拒绝或保留原文，不能静默清理后仍声称是 byte-preserving passthrough。

因此，“入口有限检查”和“上游 body 变换”必须在代码结构上分开。

### 响应层仍有不必要的兼容扫描

当前 `src/external_pool.rs:10736-10865` 的非流式 usage 处理会：

- 扫描多个潜在 usage 路径；
- 识别 OpenAI 风格 `prompt_tokens/completion_tokens`；
- 在缺失 usage 时估算输出并注入新的顶层 usage；
- 运行 response content sanitization；
- 将 usage projection 与响应 body 重序列化结合。

对于严格 Anthropic-compatible 账号，默认只需读取标准顶层 `usage`，并在 usage projection 开启时定点替换 usage 对象。多协议 usage 兼容、缺失 usage 估算和 response content sanitization 应属于显式 `protocol_profile` 或诊断策略，不能成为所有账号的默认响应处理。

当前流式路径在 `src/external_pool.rs:6150-6510` 还包含：SSE 缓冲、终端事件处理、错误事件隐私化、transcript sanitizer、keepalive、idle timeout、lease loss、usage projection 和 debug 采样。这里需要保留的是流式可靠性、usage 捕获和错误隐私；不应默认重写非 usage 事件。

## 通用能力与旧专用适配边界

| 能力 | 结论 | 目标位置/策略 |
| --- | --- | --- |
| `base_url` 规范化、`/v1/messages` 拼接 | 通用 | `upstream account HTTP` |
| API key/Bearer 注入、入站认证清理 | 通用 | `upstream account HTTP` |
| hop-by-hop header 过滤 | 通用 | `upstream account HTTP` |
| 代理绑定、TLS、超时、lease | 通用 | 账号执行与调度 |
| 优先级、模型支持、RPM、并发、sticky、队列 | 通用 | 账号调度 |
| 401/403/429/5xx 分类、冷却、failover | 通用 | attempt/retry engine |
| SSE 边界、terminal、客户端断开、idle timeout | 通用 | canonical stream executor |
| raw usage 捕获、usage record、pricing、projection | 通用 | usage pipeline |
| 顶层 `model` 映射 | 可选通用策略 | 账号显式配置，默认关闭 |
| `context_management` 与 beta header 对称清理 | 可选协议安全 | 仅在 capability 不匹配时定点删除 |
| 空 text block 清理 | 可选兼容策略 | 仅上游明确需要时启用 |
| web-search 历史 block 清理 | 第三方兼容策略 | 不进入严格 Claude Code 默认路径 |
| 完整 typed overlay/JSON 重建 | 默认不需要 | 仅显式 normalized compatibility profile |
| thinking 自动降级/转文本 | 默认不需要 | 仅明确非标准上游 profile |
| 多协议 usage 候选扫描 | 默认不需要 | 兼容 profile 或诊断模式 |
| response content/tool transcript 改写 | 默认不需要 | 只在检测到具体协议错误时使用 |
| 旧专用 envelope、事件流、凭证刷新、profile 注入 | 删除 | 不迁移到账号层 |

## 推荐的最终账号请求契约

### 账号字段

账号最小字段应是：

```text
id
name
enabled
base_url
api_key secret reference
auth scheme: x-api-key | bearer
supported models
priority
max concurrent requests
rpm
proxy
route policy
retry/cooldown policy
usage projection policy
```

不需要账号携带旧平台的 OAuth、区域、profile、事件流、订阅、配额同步或专用 body envelope 字段。

### 请求 body 默认规则

1. 入口只读取调度所需的有限事实，并执行通用 JSON 安全限制。
2. 账号选择完成后，默认发送同一份 `effective_raw_body`。
3. 默认不完整反序列化、不 overlay、不重新序列化。
4. 默认不修改 `messages`、`system`、`tools`、`thinking`、`metadata`、未知字段和 cache control。
5. 模型映射若启用，只允许改顶层 `model`，并将 outbound model 记录到 attempt trace。
6. body/header 能力不对称时，只允许删除明确不被支持的字段；不能凭模型名猜测能力。
7. payload guard 不属于默认账号 body 处理；如果启用，应在请求上下文中产生新 attempt/body 版本并完整记录。

### 响应和 usage 默认规则

1. 成功的 Anthropic JSON/SSE 响应尽量原样返回。
2. 流式响应按 SSE 边界转发，保持事件顺序；usage capture 可以旁路进行。
3. 非流式响应只解析标准 `usage`；若启用 projection，只定点改写 usage 字段。
4. 不为了“补齐统计”而默认重写 content、tool、thinking 或未知响应字段。
5. 上游没有 usage 时，运营记录可以标记为 estimated/unknown；不能把估算冒充为上游原始 usage。
6. 下游已经收到语义事件后，禁止把同一请求拼接到其他账号的响应；failover 只能发生在语义输出提交前。

## 对当前代码的处理建议

本轮不改代码，只确定后续重构边界：

1. 将 `external_pool_url`、`forward_headers` 和 raw body preparation 提取到中性 `upstream account HTTP` 边界；不再让它们依赖旧模块名。
2. 将账号默认 body policy 改为 raw compatible forwarding。若最终只允许 Claude-compatible 账号，`Normalized` 不再参与默认账号选择，之后可删除账号专用 normalized path。
3. 将 payload guard、图片 materialization、历史裁剪、thinking 修复和第三方 web-search 清理改为显式兼容能力，不能由“账号存在”自动触发。
4. 将响应处理拆成“canonical passthrough + usage observer + usage projection”。usage projection 只负责 usage，不负责通用 content 重写。
5. 将 `count_tokens` 视为同一协议的另一个 endpoint：复用 URL/header/auth 组装，body 默认直通；上游不支持时返回可识别的 404，让客户端本地估算。
6. 保留调度、lease、sticky、代理、超时、错误分类、冷却、同账号重试、跨账号 failover、客户端断开后的 usage drain、统计和 Admin 能力。
7. 删除所有旧专用认证、endpoint、事件流、envelope、profile/body 注入和模型/配额同步依赖，而不是把它们包装成新的 provider 变体。

## 后续验证合同

实现时至少应补齐以下测试，且测试断言要区分 body 字节、协议语义和 usage 投影：

- `base_url` 为根路径、带 `/v1`、带路径前缀、带尾斜杠时的 endpoint 结果；
- 入站 `Authorization`、`x-api-key`、Cookie 被清理，账号 key 正确注入；
- 无 body 变换时复杂 messages/tools/thinking/context metadata 字节保持不变；
- 仅模型映射时只有顶层 `model` 改变；
- beta capability 不匹配时只删除对应不支持字段；
- stream/non-stream 原始响应透传；
- `message_start`、`message_delta` 和非流式 `usage` 捕获；
- usage projection 只改变 usage 字段，且 raw/effective/reported 三层记录完整；
- 400 不做无依据 body 降级重试；401/403/429/5xx 按策略 failover；
- 首个语义事件前失败可以切换账号，首个语义事件后不得拼接第二账号；
- 客户端断开、上游 idle、SSE malformed、lease 丢失时仍能释放资源并记录最终 usage/错误；
- 多账号正常、异常、冷却、恢复和调度公平性矩阵；
- 源码扫描确认账号运行时不再依赖旧专用认证、事件流、envelope、profile 或产品术语。

## 最终判断

对“Claude Code 协议 `base_url + api_key` 账号调度系统”而言，外部账号接入的核心不是复杂 body 转换，而是可靠的账号执行层：认证替换、URL 规范化、请求/响应边界、调度与 failover、usage 观测和 usage projection。

当前系统的复杂 body 处理应被视为历史兼容能力，默认路径应收敛到 raw compatible forwarding。这样既能保留统计、调度、代理、重试、流式可靠性和 usage 整形，也能避免网关在不知情的情况下改写 Claude Code 请求语义。
