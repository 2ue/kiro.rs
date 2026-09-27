# 渠道账号领域模型与 Provider 契约

## 1. 统一术语

| 术语 | 定义 |
| --- | --- |
| Channel | 一组共享 endpoint、协议、认证方式、路由和调度默认值的上游接入，例如一个 Kiro 集群或一个第三方 Claude Code Base URL |
| Account | Channel 下可被调度的凭据/密钥单元，例如一个 Kiro refresh token 或一组 Base URL+API key |
| Provider type | 实现协议和生命周期的类型：`kiro`、`claude_code_upstream`、未来 `openai_compatible` 等 |
| Protocol profile | 入站/出站 wire contract，例如 `anthropic_messages`、`claude_code_messages`、`openai_chat_completions` |
| Target | 调度器实际尝试的对象，包含 channel、account、provider type 和 route context |

“外部池”在 UI 和 API 中改称“Claude Code upstream channel”；数据库可以在兼容期保留 `external_pool` 的旧列/视图，但新领域接口不再把它当成特殊业务概念。

## 2. 领域模型

```json
{
  "channel": {
    "id": "ch_01...",
    "name": "third-party-main",
    "providerType": "claude_code_upstream",
    "enabled": true,
    "baseUrl": "https://example.invalid/anthropic",
    "protocolProfile": "claude_code_messages",
    "defaultConfigRef": "cfg_channel_...",
    "routePolicyRef": "route_...",
    "capabilities": ["anthropic_passthrough", "streaming", "usage_projection"]
  },
  "account": {
    "id": "acct_01...",
    "channelId": "ch_01...",
    "providerType": "claude_code_upstream",
    "label": "vendor-a-key-1",
    "secretRef": "secret://accounts/acct_01/api-key",
    "enabled": true,
    "priority": 10,
    "supportedModels": ["claude-sonnet-4-5"],
    "limits": {"maxConcurrent": 4, "rpm": 0},
    "providerState": {}
  }
}
```

通用字段只包括 id、label、enabled、priority、supportedModels、proxyRef、route rules、并发/RPM、warmup、cooldown 和审计信息。`providerState` 是版本化 JSON，只有 Provider 能解释：Kiro 放 region、auth method、refresh token metadata、machine id source、endpoint、quota cohort；Claude Code upstream 放 header profile、TLS profile、model mapping、stream mode、usage projection 和 key fingerprint。

敏感值不直接进入普通 JSON：`secretRef` 指向受保护的 secret store（第一阶段可由 PostgreSQL 加密列实现），API key/refresh token 永不出现在 list/get response、日志或 usage record。

## 3. Kiro Provider 边界

Kiro Provider 私有模块必须拥有：

- KiroCredentials、access/refresh token、region/authRegion/apiRegion、Kiro endpoint、machine id、proxy 和 token refresh。
- `KiroEndpoint` 注册表、CLI/IDE endpoint 差异、Kiro request body 和 EventStream parser。
- `MultiTokenManager` 语义：账号选择、RPM、并发 in-flight、sticky session、cooldown、Redis degraded、refresh wave 和 Kiro quota/cohort。
- Kiro 特有 model capability discovery、native reasoning、tool schema/name mapping、thinking、cache point、MCP 和 Kiro 错误分类。

Kiro Provider 对 generic kernel 暴露的是：

```text
CandidateMetrics:
  priority, load, cooldown_until, rpm_remaining,
  concurrent_remaining, model_compatible, sticky_match,
  provider_quota_dimensions[]

ProviderResult:
  response stream, protocol usage, completion,
  provider_error_class, replay_safety, account attribution
```

generic kernel 不得读取 `profile_arn`、`conversation_state`、CRC、Kiro header 或 Kiro quota 字段。

## 4. Claude Code upstream Provider 边界

该 Provider 的最小责任是：

- 使用 channel base URL 与 account API key 构造 HTTP 请求。
- 按 channel/account 的 auth type、header profile、wire profile、TLS profile、path preservation 和 beta query 设置 outbound headers/URL。
- 在没有入口重写的情况下保留 raw body；必须重写时使用明确的 normalized body，并记录原因。
- 对 Anthropic JSON/SSE 做有限解析，以识别首语义输出、终止事件、error envelope 和 raw usage；不创建 Kiro conversation state。
- 执行外部 provider 自己的 model mapping、supported model 检查、池/账号并发 lease、同渠道 retry、跨渠道 retry 和 usage projection。

禁止该 Provider 依赖：

- Kiro `KiroProvider`、`KiroEndpoint`、`KiroCredentials`、`MultiTokenManager`、Kiro parser/protocol。
- `body_conversion` 中 tool choice steering、Kiro tool name mapping、Kiro prompt steering、Kiro cache point 或 native reasoning 字段。
- Kiro region/token refresh/machine id 或 Kiro quota API。

如果第三方上游本身要求 Claude Code 特定 header，应在 `ClaudeCodeWireProfile` 中声明并测试；这仍属于 Anthropic/Claude Code passthrough，不等于 Kiro 协议转换。

## 5. 未来 OpenAI 兼容扩展

OpenAI Provider 预留三类接口：

1. `RequestAdapter`：Anthropic messages ↔ OpenAI chat/responses 的可选转换，或直接接受 OpenAI 入站 profile。
2. `StreamDecoder`：将 provider event 转为 generic `StreamEvent`，由 transport 层输出目标协议。
3. `UsageExtractor`：把 prompt/completion/cached/reasoning token 归一为 `UsageFacts`。

不要在当前 Kiro/Claude Code 实现中提前加入 OpenAI 字段。只需让 route policy 的 `acceptedProtocols`、Provider registry 和 usage contract 可扩展。

