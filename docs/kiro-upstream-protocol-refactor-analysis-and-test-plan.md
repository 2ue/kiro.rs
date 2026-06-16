# Kiro 上游凭据协议优化分析与回归测试方案

日期：2026-06-16  
范围：当前项目 `kiro.rs`，对照 `kiro-account-manager` 与 `freedom-kirors`。  
状态：已固化分析与测试方案；不在生产代码中写“改造前/改造后”分支代码。

## 1. 结论

当前 `kiro.rs` 已经吸收了 KAM / freedom 的一部分生产经验：ExternalIdp `TokenType`、Enterprise `ListAvailableProfiles` 自愈、profileArn fallback、API Key 不伪造 profileArn、风控错误识别、assistant-prefill bad request 分类等。

但当前仍有一个核心结构性问题：`src/kiro/protocol.rs::resolve_profile_arn()` 同时服务于流式请求体、MCP/header、模型列表 query、用量接口 query。KAM 和 freedom 的 release 记录都证明，`profileArn` 在不同上游调用中的语义不是同一个字段：

- 流式 `generateAssistantResponse` / `SendMessageStreaming` body 需要按账号类型补 `profileArn`，BuilderId/free 可使用官方占位 ARN。
- header / MCP / `ListAvailableModels` / usage / preferences 只能安全发送真实 profile ARN 或 Social 共享 ARN，BuilderId 占位 ARN 和 Enterprise fallback ARN 都可能触发 403/400。
- Enterprise/IdC 真实 ARN 应通过 `ListAvailableProfiles` 自愈并持久化；fallback 只能作为请求兜底，不应被当作真实 ARN 持久化。

因此最优改造不是继续在一个 resolver 里补条件，也不是盲目照搬 KAM 某个 release 的单点策略，而是把 profileArn 协议语义按调用面拆开：请求体、header/query、持久化写入、真实 profile 自愈分别有独立语义。

## 1.1 “更符合”协议的判断标准

这里的“更符合”不是代码是否更简洁，而是代理发出去的请求是否更像 Kiro 官方客户端在同一账号类型、同一端点上的真实行为。判断标准如下：

| 维度 | 更符合的做法 | 不符合或风险做法 |
| --- | --- | --- |
| 调用面 | `generateAssistantResponse` body、MCP header、`ListAvailableModels` query/header、usage query 分开处理 | 用一个 `profileArn` resolver 同时喂所有上游接口 |
| 账号类型 | API Key 不伪造 profile；Social 使用固定 Social ARN；Enterprise/IdC 先解析真实 ARN；BuilderId/free 只在流式 body 保留占位兜底 | 把 BuilderId 占位 ARN 或 Enterprise fallback 当成所有接口都能接受的真实 profile |
| 持久化 | 只有 `ListAvailableProfiles` 返回的真实 ARN 才写回凭据；fallback 仅限本次请求兜底 | 403/无 profile 时把 fallback ARN 写入数据库，后续阻断真实 ARN 自愈 |
| endpoint | API Key 应靠 CLI endpoint 或明确的 API_KEY token type 协议，不带 profileArn | API Key 继续走 IDE endpoint 并混入 OAuth profile 逻辑 |
| 错误语义 | `assistant-prefill`、tool result mismatch、profileArn required/invalid token 被识别为协议/请求问题，不做无意义换号风暴 | 上游包成 500 就按服务端瞬态错误盲目重试 |
| 模型输入 | 不改用户 messages/system/tool schema，不注入额外 agentic prompt | 用 prompt 改写掩盖协议问题，可能改变模型行为和用户感知智商 |

按这个标准，KAM 的价值是提供大量上游行为样本和踩坑记录；freedom 的价值是把这些样本收敛成 endpoint/profile/error 分层；当前 `kiro.rs` 应保留自身 PgSQL/Redis 调度骨架，吸收 freedom 的协议分层，而不是反向替换为 freedom 的简化运行态。

## 2. 三项目具体对比

### 2.1 kiro-account-manager

KAM 是上游接口行为的实战记录库。它的价值主要来自 release 演进，而不是某一版当前代码可直接照搬。尤其是 v1.7.3、v1.7.4、v1.7.5 对 BuilderId 占位 ARN 在非流式/流式接口上的结论有反复：这不是矛盾可以忽略，而是证明 Kiro 上游不同入口的容忍度会变化，必须按调用面建模。

关键经验：

- `Kiro-account-manager/src/main/kiroAuthSync.ts::resolveProfileArnForWrite` 统一了 token 文件写入策略：真实 ARN 优先，Social 使用固定 ARN，Enterprise 使用 region-aware fallback，BuilderId 写官方占位 ARN。
- `Kiro-account-manager/src/main/proxy/kiroApi.ts::fetchEnterpriseProfileArn` 通过 `codewhisperer.{region}.amazonaws.com/ListAvailableProfiles` 获取 Enterprise 真实 profileArn，并通过 callback 持久化。
- `callKiroApiStream` 支持 CodeWhisperer、AmazonQ、AmazonQCLI 三类端点，并在 CLI 模式下禁用 fallback。
- release 记录明确踩过 BuilderId 占位 ARN 在 REST / model list 类调用中造成 403 的坑，也踩过 streaming 缺 `profileArn` 造成 400 的坑；这两类问题不能用同一个规则同时解决。
- release 记录还修复过 CodeWhisperer model id 跨 family 误匹配，说明模型别名解析不能把 opus/sonnet/haiku 静默混用。

不可直接照搬点：

- KAM 当前代码和 release 文字存在阶段性矛盾，例如 v1.7.3 强调 BuilderId 占位 ARN 在 REST 类接口会 403，v1.7.4 又为若干非流式接口恢复占位 ARN，v1.7.5 又引入 Enterprise fallback 和 profile 自愈。这说明它是上游行为探索过程，不是最终架构答案。
- KAM 的 `AGENTIC_SYSTEM_PROMPT` 属于模型行为层改写，不应作为协议修复引入当前项目，否则可能改变模型推理和输出风格，造成用户感知的“降智”。

### 2.2 freedom-kirors

freedom 是更接近当前 Rust 服务形态的生产参考。它的核心优点是把 KAM 的经验收敛成清晰的协议分层。

关键实现：

- `src/kiro/model/credentials.rs::effective_profile_arn()`：只返回真实 ARN / Social ARN，跳过 BuilderId 占位 ARN。
- `src/kiro/model/credentials.rs::streaming_profile_arn()`：只用于流式 body。API Key 返回 `None`，BuilderId/free 可使用官方占位 ARN，Enterprise/IdC 依赖真实 ARN 自愈。
- `src/kiro/model/credentials.rs::effective_streaming_api_region()`：如果真实 profileArn 带 region，则用 profileArn region 调 streaming endpoint，避免 EU profile 打到 US endpoint。
- `src/kiro/provider.rs::ensure_profile_arn()`：对缺失或占位 profileArn 的 OAuth 凭据按需调用 `ListAvailableProfiles`；成功写回，确定无 profile 才标记 attempted；瞬态错误不标记，避免一次网络抖动永久跳过自愈。
- `src/kiro/token_manager.rs::normalize_api_key_endpoints_to_cli()`：API Key 自动迁移到 `cli` endpoint，避免 IDE endpoint 下模型测试或实际请求 403。
- `src/kiro/endpoint/cli.rs`：实现 CLI endpoint 的 content-type、`x-amz-target`、origin 转换和不支持字段删除。
- provider 错误分类中把 `TOOL_USE_RESULT_MISMATCH` / `Expected toolResult blocks` 这类客户端消息协议错误从 5xx 重试流里剥离，避免重试风暴；524 gateway timeout 快速失败。

当前项目应学习 freedom 的部分：

- profileArn resolver 按调用类型拆分。
- API Key endpoint 默认迁移到 CLI。
- `ListAvailableProfiles` fallback 不伪装成真实 ARN，不轻易持久化。
- client-validation 5xx 与 524 特判。

### 2.3 当前 kiro.rs

当前项目已有优势：

- PgSQL / Redis 运行态、凭据调度、in-flight lease、session sticky、模型维度 cooldown、风险控制比 KAM/freedom 更完整。
- `src/kiro/provider.rs::detect_risk_control_error()` 已覆盖 `TEMPORARILY_SUSPENDED`、`suspicious activity + temporary limits`、账号暂停、423 locked。
- `src/kiro/provider.rs::classify_bad_request_reason()` 已识别 assistant-prefill final message 和 profileArn bad request。
- `src/kiro/provider.rs::ensure_profile_arn_for_context()` 已对 Enterprise 缺失真实 ARN 做 `ListAvailableProfiles` 自愈。
- `src/kiro/endpoint` 已有 endpoint trait，具备扩展 CLI endpoint 的结构基础。

当前项目主要缺口与已处理状态：

- 已处理：`resolve_profile_arn()` 现在只代表 header/query/usage/MCP/model-list 的真实 identity selector；`endpoint/ide.rs::transform_api_body` 改用 `resolve_streaming_profile_arn()`。
- 已处理：`token_manager.rs::get_usage_limits` 仍调用 `resolve_profile_arn()`，但该函数语义已收窄，不再返回 BuilderId 占位或 Enterprise fallback。
- 已处理：`fetch_enterprise_profile_arn_for_context` 在 403 时不再返回 fallback，因此不会把 fallback 持久化。
- 已处理：旧数据中已持久化的 Enterprise fallback 不再被当成真实 ARN；自愈逻辑会继续尝试 `ListAvailableProfiles` 获取真实 ARN。
- 只有 IDE endpoint 被注册，API Key 还没有 freedom 那种 CLI endpoint 迁移路径。
- 对 `TOOL_USE_RESULT_MISMATCH` / `Expected toolResult blocks` 和 524 的分类还不如 freedom 细。

## 3. 目标协议设计

实际改造时应把生产路径改成单一路径的最终行为，不在生产代码里保留“legacy/new”双分支。下面的 resolver 名称用于描述目标职责，落地时可以是函数、方法或端点内部策略，但必须只服务真实生产行为。

### 3.1 改造前问题模型

改造前的核心问题是一个 resolver 承担多种协议语义：它既要保证流式 body 不缺 `profileArn`，又被用于 header/query/model-list/usage。这个模型短期简单，但会把占位/fallback 泄漏到不该携带它们的接口。本次生产代码不保留 legacy/new 双分支；改造前行为只通过本地 baseline 日志保存。

### 3.2 effective resolver

当前实现沿用函数名 `resolve_profile_arn(credentials, config)`，但语义已收窄为 effective resolver，用于 header / MCP / `ListAvailableModels` / usage / preferences。

规则：

- API Key：`None`。
- 真实 profileArn：返回。
- Social：返回固定 Social ARN。
- BuilderId 占位 ARN：跳过。
- Enterprise fallback ARN：跳过。
- Enterprise/IdC 缺真实 ARN：跳过，等待自愈或使用不带 profileArn 的兼容路径。

### 3.3 streaming resolver

`resolve_streaming_profile_arn(credentials, config)` 用于 `generateAssistantResponse` / `SendMessageStreaming` request body。

规则：

- API Key：`None`。
- 已有 profileArn：原样返回，包括真实 ARN、Social ARN、BuilderId 占位 ARN。
- Enterprise/IdC 缺真实 ARN：返回 region-aware fallback 作为请求体兜底，但不得持久化为真实 ARN。
- Social：返回固定 Social ARN。
- BuilderId/free：返回官方 BuilderId 占位 ARN。

### 3.4 token file/write resolver

后续需要单独增加 `resolve_profile_arn_for_write()`，对齐 KAM 的 token 文件需求。它可以写 BuilderId 占位 ARN，因为 IDE/CLI 本地逻辑依赖字段存在；但这不能复用到 header/query。

### 3.5 profile self-healing

Enterprise/IdC 自愈流程目标：

1. API Key 跳过。
2. 已有真实 ARN 直接使用。
3. 缺失或占位 ARN 时调用 `ListAvailableProfiles`。
4. 只有真实 ARN 才写回凭据并持久化，BuilderId 占位 ARN 和 Enterprise fallback ARN 都不能作为“已有真实 profile”跳过解析。
5. 上游确定无 profile 时标记 attempted，本进程不重复查。
6. 网络/5xx/timeout 不标记 attempted，下次继续尝试。
7. fallback 只进入本次 streaming body，不写入持久层。

## 4. 分阶段实施方案

### 阶段 A：分析与本地测试方案固化

- 写入本分析文档。
- 写入本地测试 runbook：先采集改造前基线，再实施真实修改，再用同一组本地/Claude Code CLI 场景复测并对比。
- 不在生产模块中加入只为比较而存在的 branch/helper/test-only 代码。
- 回归测试只断言改造后的最终行为；改造前行为通过基线日志、curl 响应、Claude Code CLI debug 输出和服务日志保存。

### 阶段 B：profileArn 调用点迁移

- `endpoint/ide.rs::transform_api_body` 改用 `resolve_streaming_profile_arn()`。
- `endpoint/ide.rs::decorate_mcp` / `decorate_models` 改用 `resolve_effective_profile_arn()`。
- `endpoint/ide.rs::models_url` 改用 `resolve_effective_profile_arn()`。
- `token_manager.rs::get_usage_limits` 继续调用 `resolve_profile_arn()`，但该函数已是 effective resolver，不再返回 fallback/placeholder。
- 更新对应测试，删除“models_url 使用 BuilderId 占位”的旧断言，改成“models_url 跳过 BuilderId 占位”。

### 阶段 C：Enterprise 自愈修正

- 已落地：403 fallback 不进入 `update_credential_profile_arn`。
- 已落地：旧持久化 fallback 不再被视为真实 ARN，仍会触发真实 ARN 自愈。
- 后续增强：返回值可进一步区分 `RealArn` / `NoProfile` / `TransientError` / `FallbackOnly`，用于更细日志和 attempted cache。
- 增加 attempted cache，瞬态错误不标记。
- 真实 ARN region 优先，用于 streaming endpoint region。

### 阶段 D：CLI endpoint 与 API Key

- 新增 `src/kiro/endpoint/cli.rs`。
- 注册 `CliEndpoint`。
- API Key 凭据启动时自动迁移到 `cli`，或至少在 admin/import/test credential 路径中默认 endpoint 为 `cli`。
- 增加 API Key model test 回归。

### 阶段 E：错误分类补强

- endpoint trait 增加或复用 client validation classifier。
- `TOOL_USE_RESULT_MISMATCH` / `Expected toolResult blocks` 直接返回 400，不重试、不切账号。
- 524 gateway timeout 快速失败，并按 account limiter 记 soft error。
- 增加 5xx-body 客户端错误回归测试，防止 503 风暴。

### 阶段 F：真实交互测试

在本地 free 凭据只能 Sonnet 的约束下测试：

- ccman 切换本地服务到当前服务。
- Claude Code CLI 使用 Sonnet 多轮交互。
- 长会话，包含 MCP、agent、tools/search、工具结果、重复 tool_use 防护。
- 检查 assistant-prefill final message 不再出现。
- 检查重复输出、XML/tool leak、工具调用错位不会出现。
- 检查模型列表/模型测试不会因 BuilderId 占位或 Enterprise fallback 误打 header/query。

## 5. 回归测试设计

回归测试分两类：代码级最终行为测试和本地真实前后对比测试。

代码级测试只写“改造后应该怎样”，不在生产代码或单元测试中同时实现 legacy/new 双策略。改造前行为通过本地基线采集保存，详见 [kiro-protocol-local-before-after-test-runbook.md](kiro-protocol-local-before-after-test-runbook.md)。

### 5.1 代码级最终行为测试

- `ide_models_url_skips_builder_placeholder_after_migration`。
- `ide_mcp_header_skips_builder_placeholder_after_migration`。
- `ide_streaming_body_keeps_builder_placeholder_after_migration`。
- `usage_limits_skips_builder_placeholder_after_migration`。
- `enterprise_list_available_profiles_403_does_not_persist_fallback`。
- `enterprise_real_profile_region_overrides_streaming_region`。
- `api_key_endpoint_defaults_to_cli`。
- `client_validation_5xx_is_bad_request_without_retry`。
- `gateway_524_fast_fails_without_retry_storm`。

### 5.2 本地真实前后对比测试

- 改造前启动当前服务，采集 `/cc/v1/models`、`/cc/v1/messages`、`/v1/messages`、MCP/search/tool/agent/long-session 的响应和服务日志。
- 实施真实协议修改，不保留 branch 代码。
- 使用完全相同的 prompt、session、模型、Claude Code CLI 参数和 MCP 配置复测。
- 对比错误类型、HTTP 状态、Kiro 上游 request id、服务端 attempt chain、Claude Code CLI debug 日志、重复输出、工具调用完整性和最终回答质量。

## 6. 风险评估

不会导致 Kiro 上游模型理解错误的改动：

- 拆分 profileArn resolver。
- 调整 header/query/body 的 profileArn 使用。
- 增加 CLI endpoint。
- 增加错误分类和调度策略。

可能导致模型“降智”或行为变化的改动：

- 修改用户 messages、system prompt、tool schema、thinking/reasoning 字段。
- 引入 KAM 那种强制 agentic 系统提示。
- 模型 ID 静默降级或跨 family fallback。

因此当前方案明确限制在上游协议层，不引入 prompt 注入，不改变用户消息语义。

## 7. 验收标准

- 单元测试通过。
- endpoint 迁移后，BuilderId/free：streaming body 有占位 ARN，models/MCP/usage/header 无占位 ARN。
- Enterprise/IdC：优先真实 ARN；fallback 不持久化；真实 ARN region 能影响 streaming endpoint。
- API Key：不带 profileArn，默认走 CLI endpoint。
- Claude Code CLI 真实交互测试中无 assistant-prefill final message、无重复输出、无工具 XML 泄漏、无 tool result mismatch 重试风暴。

## 8. 本轮实施结果

本轮已经实施并验证的范围：

- `src/kiro/protocol.rs` 将 `resolve_profile_arn()` 收窄为 header/query/MCP/model-list/usage 的真实 identity selector，不再返回 BuilderId 占位 ARN 或 Enterprise fallback ARN。
- 新增 `resolve_streaming_profile_arn()` 专供 `generateAssistantResponse` 请求体使用，保留 BuilderId/free body 级占位 ARN和 Enterprise/IdC request-body fallback。
- `src/kiro/endpoint/ide.rs::transform_api_body()` 已切到 streaming resolver；`models_url()`、`decorate_mcp()`、`decorate_models()` 继续使用收窄后的 header/query resolver。
- `src/kiro/provider.rs` 的 Enterprise 自愈逻辑只把 `ListAvailableProfiles` 返回的真实 ARN 写回；403 不再返回 fallback 给持久化路径；旧持久化 fallback 不再被当成真实 ARN 跳过自愈。

本轮没有实施的范围：

- 没有引入 KAM 的 agentic system prompt 或任何 prompt 注入，避免改变模型行为。
- 没有在生产代码里保留 legacy/new 双分支。
- 没有实现 CLI endpoint/API Key 自动迁移，这仍是后续阶段 D 的独立工作；本轮 free Sonnet 凭据验证未覆盖真实 Kiro API Key 上游协议。
- 没有补充 `TOOL_USE_RESULT_MISMATCH`、`Expected toolResult`、524 的更细错误分类，这仍是后续阶段 E。

## 9. 最终真实测试证据

最终复测时间：2026-06-16 17:57-18:07 CST。  
本地服务：`127.0.0.1:9022`，进程 `96038`，日志 `.local-run/protocol-before-after/server-after-latest.log`。  
Claude Code 服务商：`ccman cc current` 指向 `http://127.0.0.1:9022/cc`。  
模型约束：本地凭据为 free，真实请求只使用 `sonnet`。

HTTP/API 真实请求结果：

- `final-healthz.json`：`/healthz` 返回 JSON 成功。
- `final-cc-models.json`：`/cc/v1/models` 返回 19 个模型条目。
- `final-cc-minimal.json`：`/cc/v1/messages` 最小 Sonnet 请求返回文本 `成功。`。
- `final-tool-use.json`：`/v1/messages` 自定义工具 schema 返回结构化 `tool_use`，工具名 `get_city_time`，参数 `{"city":"上海"}`。

Claude Code CLI 真实交互结果：

- `final-claude-smoke.*`：读取项目文件并完成回答，exit 0。
- `final-claude-tools.*`：使用 `Grep` 搜索 `src/kiro` 中 `profileArn` 逻辑并完成回答，exit 0。
- `final-long-1.*` / `final-long-2.*`：新 session `61a8eecd-9db0-41fa-842a-44cdbd621beb` 两轮 resume 成功，第二轮基于上一轮上下文继续搜索并回答，exit 0。
- `final-claude-mcp-allowed.*`：通过 MCP filesystem 的 `mcp__fs__list_directory` 读取项目根目录，确认 `README.md` 和 `docs` 存在，exit 0。
- `final-claude-agent-clean.*`：自定义 `protocol_auditor` agent 完成子代理 + 工具调用链路，最终 result success，exit 0。

扫描结论：

- 最终 Claude CLI stream 结果中 `api_error_status` 均为 `null`，`terminal_reason` 均为 `completed`。
- 未出现 `assistant-prefill final message is not supported`、`last message must be user`、`TOOL_USE_RESULT_MISMATCH`、`Expected toolResult`、`profileArn is required`、`server_error`、`bad_request`。
- 未观察到重复 final message、重复工具调用、工具 XML 泄漏。
- Debug 日志中存在全局 MCP 配置噪声，例如 `exa`、`context7` 等外部 MCP server 初始化失败；这不是 Kiro 上游调用失败。显式配置的 filesystem MCP 已通过。
- 服务日志中出现个别凭据 token refresh 的瞬态失败和 AWS OAuth `500 Internal Server Error {"message":"Oops, something went wrong. Please try again later."}`，发生在认证刷新路径，并被记录为 auth transient cooldown；这不是本轮要修的 `generateAssistantResponse` 非流式/流式请求 500，也未导致当前 Sonnet 真实交互失败。

## 10. Release Blocker 判断

本轮修改符合“官方 Kiro 调用面分离”的目标：streaming body、header/query、MCP/model-list/usage、持久化写入不再共用同一个 profileArn 语义。真实 Sonnet 调用、Claude Code 多轮、工具、MCP、agent 均已跑通。

当前没有发现阻止发布的协议回归。后续仍建议单独排期 CLI endpoint/API Key 迁移、client-validation 5xx/524 错误分类、以及 Enterprise self-heal attempted cache，但这些不是本轮 profileArn 协议修复的 release blocker。
