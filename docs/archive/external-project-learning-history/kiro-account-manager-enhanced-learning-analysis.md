# kiro-account-manager 3460 enhanced 学习分析

本文档分析本地外部项目：

`/Users/yuanfeijie/Desktop/procode/kiro-account-manager-3460-enhanced-20260524`

分析目标：

1. 判断该项目是否有值得当前 `kiro.rs` 学习的地方。
2. 重点关注 Kiro 账号请求、通过代理系统调用 Kiro 上游、Kiro 上游响应处理。
3. 明确哪些点适合迁移，哪些点不应照搬。

分析时间：2026-05-24

对比对象：

| 项目 | 路径 | 版本/状态 |
| --- | --- | --- |
| 当前项目 | `/Users/yuanfeijie/Desktop/procode/kiro.rs` | branch `main`，HEAD `dcf1b1f`，工作区有未提交改动 |
| 外部项目 | `/Users/yuanfeijie/Desktop/procode/kiro-account-manager-3460-enhanced-20260524` | `package.json` 版本 `kiro-account-manager@1.6.8`，本地目录不是 git 仓库 |

## 结论

这个外部项目值得学习，但不适合整套迁移。

当前 `kiro.rs` 的优势在服务端化、多实例状态、Redis/PgSQL 调度、路径级缓存上报、并发排队、后台统计和配置热更新。外部项目的优势主要在协议细节和客户端兼容经验：它更贴近抓包还原 Kiro IDE / Amazon Q CLI 的请求头、端点选择、模型列表拉取、流式事件解析、thinking/tool 转换和错误分类。

最值得学习的方向：

1. 上游协议兼容：端点 profile、`agent-mode`、UA、`profileArn`、模型 ID 解析和 `ListAvailableModels`。
2. 账号状态识别：把 `TEMPORARILY_SUSPENDED`、`AccountSuspendedException`、423 locked 等风控/封禁错误识别成独立凭据状态，而不是普通 403/429。
3. 上游响应处理：补齐 `messageMetadataEvent` / `metadataEvent` 中 token usage 的兼容解析、`meteringEvent` credits、metadata-only 事件忽略、`invalidStateEvent` 结构化错误。
4. thinking/tool 兼容测试：签名 thinking 历史、redacted thinking、tool use 分片、tool result 排序、stop sequence / max tokens 本地控制。
5. 模型能力同步：用真实账号调用 `ListAvailableModels` 拉取模型能力和 context window，作为 `/v1/models` 的非致命增强。

不建议照搬的方向：

1. 不迁移它的内存账号池。当前 Redis/PgSQL 调度、并发 lease、排队等待、跨实例冷却更适合当前项目。
2. 不照搬它把 429 直接当 quota exhausted 的策略。当前项目把 429/408/5xx 作为瞬态冷却更符合之前需求。
3. 不照搬它的 prompt cache 内存模拟。当前项目的路径级 high-cache、writer 上报和可配置策略更完整。
4. 不引入 Electron 桌面/K-Proxy/MITM/IDE 切号类能力，除非后续明确要做桌面客户端。
5. 不直接移除当前特殊业务逻辑，例如 `/cc` writer、路径级 usage、Write/Edit 分块策略、后台完整错误展示。

## 外部项目的 Kiro 上游调用链

核心文件：

1. `src/main/proxy/kiroApi.ts`
2. `src/main/proxy/proxyServer.ts`
3. `src/main/proxy/accountPool.ts`
4. `src/main/proxy/translator.ts`
5. `src/main/proxy/promptCacheTracker.ts`

### 端点与请求头

外部项目定义了三类上游端点：

| 端点 | URL | 用途 |
| --- | --- | --- |
| CodeWhisperer | `https://codewhisperer.us-east-1.amazonaws.com/generateAssistantResponse` | CodeWhisperer streaming service |
| AmazonQ | `https://q.us-east-1.amazonaws.com/generateAssistantResponse` | Amazon Q generateAssistantResponse |
| AmazonQCLI | `https://q.us-east-1.amazonaws.com/SendMessageStreaming` | Amazon Q CLI streaming |

参考位置：

1. `src/main/proxy/kiroApi.ts:119`
2. `src/main/proxy/kiroApi.ts:136`
3. `src/main/proxy/kiroApi.ts:1062`

请求头特征：

1. `x-amzn-kiro-agent-mode` 根据认证方式区分：非 IDC 走 `spec`，IDC/CLI 风格走 `vibe`。
2. `x-amz-user-agent` / `user-agent` 尽量还原 Kiro IDE 或 AmazonQ CLI 的 SDK UA。
3. 每次请求带 `amz-sdk-invocation-id` 和 `amz-sdk-request`。
4. Bearer token 来自当前账号 access token。
5. `profileArn` 会按账号 provider 默认补齐。

参考位置：

1. `src/main/proxy/kiroApi.ts:143`
2. `src/main/proxy/kiroApi.ts:152`
3. `src/main/proxy/kiroApi.ts:170`
4. `src/main/proxy/kiroApi.ts:1044`

当前 `kiro.rs` 对比：

1. 当前已有 endpoint 抽象：`src/kiro/endpoint/mod.rs`。
2. 当前默认 IDE endpoint 使用 `https://q.{region}.amazonaws.com/generateAssistantResponse`：`src/kiro/endpoint/ide.rs:62`。
3. 当前已配置化 `kiroVersion`、`systemVersion`、`nodeVersion`，这一点比外部项目硬编码版本号更适合长期维护。
4. 当前 `ide` endpoint 固定发送 `x-amzn-kiro-agent-mode: vibe`：`src/kiro/endpoint/ide.rs:73`。
5. 当前只在凭据显式配置 `profileArn` 时注入：`src/kiro/endpoint/ide.rs:108`。

建议：

1. 可以学习外部项目的端点 profile 思路，把 CodeWhisperer / AmazonQ / AmazonQCLI 做成可配置 endpoint，而不是替换现有 endpoint。
2. `agent-mode` 不建议直接改默认值。应先通过真实账号类型和请求成功率验证，再做成凭据级或 endpoint 级配置。
3. `profileArn` 可以增加“缺省推导”能力：当凭据未配置 `profileArn` 时，按 `authMethod` / provider 推导 Builder ID 或 social profile ARN。但该能力应可观测、可关闭。
4. Kiro/AWS SDK UA 应继续保留当前项目的配置化方式，不要把外部项目的 `0.12.155` 直接写死。

### 模型映射与模型列表

外部项目的两个点值得关注。

第一，模型映射允许未来 Claude 模型原样透传：

```text
if /^claude-(sonnet|haiku|opus)-/.test(lower) return modelId
```

参考位置：`src/main/proxy/kiroApi.ts:222`

这个逻辑可以避免新模型发布后被错误降级到旧默认模型。

当前 `kiro.rs` 的 `map_model` 是静态映射：

1. `src/anthropic/converter.rs:100`
2. 对未知 Sonnet 4 可能会归到 `claude-sonnet-4.5`。
3. 对非 Claude 模型会返回 `None`。

建议把模型映射改成更保守的向前兼容：

1. 已知别名继续映射，例如 `opus`、`sonnet`、`haiku`。
2. 看起来像 Kiro/Claude 原生模型 ID 的字符串优先原样透传。
3. 明显不支持的模型才返回 unsupported。
4. 不要把未来 Claude 模型自动降级到旧模型。

第二，外部项目会调用 Kiro 上游 `ListAvailableModels`：

1. 通过当前账号 access token 请求。
2. 传 `origin=AI_EDITOR`、`profileArn`、分页 `nextToken`。
3. 缓存模型列表和 context window。

参考位置：

1. `src/main/proxy/kiroApi.ts:1793`
2. `src/main/proxy/tokenCounter.ts:43`
3. `src/main/proxy/proxyServer.ts:1251`

当前 `kiro.rs` 的 `/v1/models` 是静态列表：`src/anthropic/handlers.rs:1277`。

建议：

1. 增加上游模型能力同步，启动后异步执行，失败不影响调度。
2. 模型列表按账号/profileArn 拉取，结果可以写 PgSQL，短 TTL 缓存在 Redis 或进程内。
3. `/v1/models` 可以合并“静态保底 + 上游同步结果”。
4. 同步结果中保存 `maxInputTokens`、`maxOutputTokens`、thinking/cache support 等能力。
5. 模型计价仍保持统计用途，不参与调度硬失败。

优先级：P0。

## 账号调度与错误分类

### 外部项目的账号池

外部项目 `AccountPool` 是进程内 Map，提供 round-robin/sticky、断路器、指数退避、概率重试、quota exhausted、suspended 标记。

参考位置：

1. `src/main/proxy/accountPool.ts:5`
2. `src/main/proxy/accountPool.ts:116`
3. `src/main/proxy/accountPool.ts:193`
4. `src/main/proxy/accountPool.ts:235`
5. `src/main/proxy/accountPool.ts:347`

它的错误分类大致是：

| 错误 | 外部项目处理 |
| --- | --- |
| 402 | recoverable，切账号，标记 quota exhausted |
| 403 | recoverable，尝试刷新 token 或切账号 |
| 429 | recoverable，切账号/端点，标记 quota exhausted |
| 400 context 超限 | fatal，直接返回 |
| 422 | fatal |
| 5xx | 多处逻辑里会 retry，但 `classifyError` 里标成 fatal |

值得学习：

1. 明确区分请求错误和账号错误。
2. suspended 状态独立于 transient cooldown。
3. 错误消息里带账号邮箱/id，便于定位。
4. no account 错误里包含 quota/cooldown 摘要。

不应照搬：

1. 它是单进程内存池，不适合当前 Redis/PgSQL 多实例架构。
2. 它的 `getAccountWithShortestCooldown` 会在没有可用账号时返回最短冷却账号；当前项目已经改成可排队等待，更符合“不要直接报错”的要求。
3. 它把 429 也标成 quota exhausted，当前项目不宜这么激进。很多 429 是瞬态 high traffic，不应直接禁用或长期冻结账号。
4. 概率重试会造成行为不可解释，当前项目用明确 cooldown、rate limit、lease、队列更可控。

### 封禁/风控状态识别

外部项目有一段专门识别 suspended 错误：

1. JSON reason：`TEMPORARILY_SUSPENDED`、`ACCOUNT_SUSPENDED`、`PERMANENTLY_SUSPENDED`。
2. 文本：`User ID is temporarily suspended`。
3. `AccountSuspendedException`。
4. HTTP 423 locked。

参考位置：`src/main/proxy/proxyServer.ts:1177`

当前 `kiro.rs` 没有等价的 suspended 分类，搜索当前代码没有 `TEMPORARILY_SUSPENDED` / `AccountSuspendedException` 识别。

建议在当前项目增加独立的 `DisabledReason` 或 runtime state：

1. 新增 `Suspended` / `TemporarilySuspended` / `AccountLocked` 类型。
2. 这类错误不要和普通 403、429 混在一起。
3. 被识别后应跳过该凭据，并在后台凭据卡片显示完整上游错误。
4. 是否自动永久禁用要谨慎：`PERMANENTLY_SUSPENDED` 可禁用；`TEMPORARILY_SUSPENDED` 更适合进入长冷却或需要人工确认。
5. usage/error record 中记录 credential id、label、endpoint、model、status、reason。

优先级：P0。

### 402/429 failover

外部项目 `callWithRetry` 对 402/429/quota/rate-limit 会切端点或切账号：

参考位置：

1. `src/main/proxy/proxyServer.ts:1480`
2. `src/main/proxy/proxyServer.ts:1555`

当前项目已经具备更细策略：

1. 402 且识别额度用尽：禁用该凭据并切换，见 `src/kiro/provider.rs:1215`。
2. 401/403：尝试强制刷新，再累计失败，见 `src/kiro/provider.rs:1274`。
3. 429/408/5xx：作为瞬态错误，写 cooldown/soft failure，不直接禁用，见 `src/kiro/provider.rs:1345`。
4. 后续 acquire 失败时保留之前真实上游错误，避免覆盖成误导性“所有凭据不可用”，见 `src/kiro/provider.rs:1017`。

建议：

1. 当前 402 策略保持。
2. 当前 429 不禁用策略保持。
3. 可以补一个“同凭据多 endpoint fallback”的能力：如果当前 endpoint 是 AmazonQ，可以尝试 CodeWhisperer；但应有 endpoint profile 和失败边界，不要在所有错误上盲目切。
4. 最终暴露给下游前仍应优先返回真实上游错误摘要，同时后台保存完整错误。

优先级：P1。

## Token 刷新与机器 ID

外部项目的 token 刷新有两个细节：

1. 同一账号刷新单飞，避免并发刷新。
2. 刷新前随机 jitter 0-3 秒，避免一批账号同时刷新。

参考位置：`src/main/proxy/proxyServer.ts:1328`

当前项目已经更强：

1. 本进程 refresh lock。
2. Redis refresh lock：`src/storage/redis_cache.rs:797`。
3. 跨实例等待/同步 PgSQL 凭据。

可学习点：

1. 在后台批量刷新或启动同步时增加轻微 jitter。
2. refresh 日志继续带 credential id/label，错误记录进入后台。

机器 ID 方面，外部项目会按账号获取 K-Proxy device id 或生成稳定 machineId：`src/main/proxy/kiroApi.ts:1035`。

当前项目已经有凭据级 machineId 派生：`src/kiro/machine_id.rs`，不需要迁移 K-Proxy 相关逻辑。

## Kiro 上游响应处理

### AWS Event Stream 解析

外部项目手写 AWS Event Stream 二进制解析：

1. 解析 total length、headers length、CRC、payload。
2. 识别 `assistantResponseEvent`、`codeEvent`、`toolUseEvent`、`metadataEvent`、`messageMetadataEvent`、`usageEvent`、`meteringEvent`、`contextUsageEvent`、`reasoningContentEvent`、`invalidStateEvent`。
3. 对 UI metadata 事件只记录日志，不注入 assistant text。

参考位置：

1. `src/main/proxy/kiroApi.ts:1262`
2. `src/main/proxy/kiroApi.ts:1364`
3. `src/main/proxy/kiroApi.ts:1389`
4. `src/main/proxy/kiroApi.ts:1465`
5. `src/main/proxy/kiroApi.ts:1537`
6. `src/main/proxy/kiroApi.ts:1554`
7. `src/main/proxy/kiroApi.ts:1580`
8. `src/main/proxy/kiroApi.ts:1605`
9. `src/main/proxy/kiroApi.ts:1633`

当前项目已有类型化事件解析：

1. `src/kiro/model/events/base.rs`
2. `src/kiro/model/events/additional.rs`
3. `src/anthropic/stream.rs:729`

当前已有能力：

1. `reasoningContentEvent` 支持 signature/redacted content。
2. `metadataEvent` 支持 tokenUsage。
3. `contextUsageEvent` 可用于估算 input tokens。
4. `invalidStateEvent` 会变成 SSE error。
5. unknown/UI metadata 默认不会注入正文。

需要补齐或验证的点：

1. 外部项目同时把 `messageMetadataEvent` 和 `metadataEvent` 都当成可能包含 tokenUsage 的事件。当前 `MessageMetadataEvent` 只解析 conversation id / utterance id。如果真实上游某些版本把 tokenUsage 放在 `messageMetadataEvent`，当前项目会漏掉 usage。
2. 外部项目解析 `meteringEvent` 并累加 credits。当前项目把 `Metering` 解析为 `()`，没有记录 credits。如果后续需要按 Kiro credit 做统计，可以补。
3. 外部项目处理 `codeEvent`，当前项目事件枚举没有 `codeEvent`。如果要支持 AmazonQCLI endpoint，需要补这个事件。
4. 外部项目对 metadata-only 事件有摘要日志。当前项目 unknown 事件基本静默，排查协议变化时不够直观。

建议：

1. 扩展 `MessageMetadataEvent`，允许携带可选 `token_usage`。
2. 新增 `MeteringEvent { usage }`，进入 usage record 的可选字段，非硬依赖。
3. 新增 `CodeEvent`，仅在启用 AmazonQCLI endpoint 时使用。
4. 增加流事件摘要 debug 日志：事件类型计数、是否收到 tokenUsage、是否收到 contextUsage、是否收到 reasoning/tool。
5. 所有新增解析失败都不能影响已有正常事件流；未知字段保持兼容。

优先级：P0。

### Usage 优先级

外部项目的 usage 优先级是：

1. 真实 `tokenUsage` 最高。
2. `usageEvent` 次之。
3. `contextUsageEvent` 反推，只在没有真实 tokenUsage 时覆盖 input。
4. tiktoken / 字符估算兜底。

参考位置：

1. `src/main/proxy/kiroApi.ts:1297`
2. `src/main/proxy/kiroApi.ts:1465`
3. `src/main/proxy/kiroApi.ts:1527`
4. `src/main/proxy/kiroApi.ts:1554`
5. `src/main/proxy/kiroApi.ts:1682`

当前项目也基本遵循这个思路：

1. `metadataEvent` 优先：`src/anthropic/stream.rs:1437`
2. `contextUsageEvent` 兜底：`src/anthropic/stream.rs:749`
3. high-cache 本地模拟只在没有真实 metadata cache 时参与下游上报：`src/anthropic/stream.rs:646`
4. 非流式同样只对 `UsageSource::LocalPromptCache` 做路径级上报改写：`src/anthropic/handlers.rs:215`

建议保持当前原则：

1. 真实上游 metadata 永远优先。
2. 本地 high-cache 只用于下游 usage 投影和统计，不影响 reader 计算、不影响上游请求。
3. `/cc`、`/ha`、`/na` 路径级上报策略保持独立覆盖。

## Prompt Cache 处理

外部项目的 `promptCacheTracker.ts` 模拟 Anthropic prompt cache：

1. flatten tools/system/messages。
2. 识别 `cache_control: { type: "ephemeral" }`。
3. 支持 5m 和 1h TTL。
4. Opus 最小 4096 tokens，其他模型最小 1024 tokens。
5. 按账号保存缓存条目。
6. 首次请求 creation，后续最长前缀命中变 read。

参考位置：

1. `src/main/proxy/promptCacheTracker.ts:52`
2. `src/main/proxy/promptCacheTracker.ts:105`
3. `src/main/proxy/promptCacheTracker.ts:164`
4. `src/main/proxy/promptCacheTracker.ts:342`

当前项目已经有更完整且更贴近当前业务需求的 prompt cache：

1. 支持 high-cache stable prefix：`src/anthropic/prompt_cache.rs:104`
2. 按 credential + conversation + model 建 scope：`src/anthropic/prompt_cache.rs:20`
3. 支持目标读缓存比例和路径级 writer/input/read/output 改写。
4. canonical JSON 是递归实现，并忽略 `cache_control` 字段：`src/anthropic/prompt_cache.rs:573`
5. `/v1`、`/cc`、`/ha`、`/na` 路径策略已收束成 `reportedUsage` 配置。

外部项目不建议照搬：

1. 它是进程内 Map，不适合当前多实例。
2. 它命中后会刷新 expiresAt，当前项目也有类似行为；如果未来要更贴近 Anthropic TTL，需要单独讨论，不能混在本次学习迁移里。
3. 它的 canonicalize 是 JS `JSON.stringify` replacer 形式，复杂嵌套对象下不如当前 Rust 递归 canonical 稳。
4. 它没有当前 `/cc` writer 3k 自然采样和路径级独立覆盖能力。

可以学习的只是测试和边界：

1. cache_control 断点 flatten 测试。
2. 5m/1h breakdown 测试。
3. Opus 4096 最小可缓存阈值测试。
4. 工具定义变更导致 cache fingerprint 变化的测试。

优先级：P2，当前不是最缺口。

## Claude/OpenAI 到 Kiro 的协议转换

外部项目 `translator.ts` 有不少值得学习的兼容细节。

### 值得学习的点

1. `cache_control` 只接受 `ephemeral`，映射成 Kiro `cachePoint`：`src/main/proxy/translator.ts:60`。
2. 清理 Anthropic billing header，避免污染 Kiro prompt：`src/main/proxy/translator.ts:75`。
3. 清理当前 user text 里的 MCP server instructions：`src/main/proxy/translator.ts:80`。
4. OpenAI assistant 历史里的 `reasoning_content` 不传给 Kiro，避免 Kiro schema 400：`src/main/proxy/translator.ts:577`。
5. Tool result 重新排序到最近的 tool use 顺序：`src/main/proxy/translator.ts:635`。
6. Claude 历史强制 user/assistant 交替，连续 user/tool result 合并：`src/main/proxy/translator.ts:967`。
7. 只保留带 signature 的 Claude thinking 历史，未签名 thinking 丢弃：`src/main/proxy/translator.ts:1043`。
8. tool-result-only continuation 不注入 thinking prefix，避免当前轮 content 形状变化：`src/main/proxy/translator.ts:1105`。
9. Kiro 返回 `<thinking>...</thinking>` 时拆成 Claude thinking block：`src/main/proxy/translator.ts:1282`。
10. 本地 stop sequence / max tokens 控制会移除后续 tool_use，避免 stop 后工具调用泄漏：`src/main/proxy/responseControls.ts:1`。

当前项目已有大量相似能力：

1. thinking tag 流式提取：`src/anthropic/stream.rs:953`
2. 原生 reasoningContentEvent：`src/anthropic/stream.rs:871`
3. tool_use/text block 状态机：`src/anthropic/stream.rs:1217`
4. tool pairing 测试：`src/anthropic/converter.rs` 测试区已有多组用例。

建议补强：

1. 对照外部项目的测试，补齐“签名 thinking 历史保留、未签名 thinking 丢弃、tool-result-only 不注入 thinking prefix、leading thinking tag literal 不误判”的回归测试。
2. 如果当前没有本地 stop sequence 控制，应增加轻量实现；Kiro 上游不一定严格遵守 Anthropic `stop_sequences`，代理层兜底更可靠。
3. OpenAI 兼容入口如果后续加入，应复用这些转换规则，特别是不要把 OpenAI `reasoning_content` 直接写进 Kiro history。

优先级：P1。

### 需要谨慎的点

外部项目有 `test_translator_prompt_pollution.py`，明确防止注入默认 prompt 污染。

当前项目存在特殊业务逻辑，例如：

1. Write/Edit 工具 description 后缀。
2. 分块写入策略。
3. `/cc` 兼容路径。

这些是当前项目已有需求，不能因为外部项目测试禁止某些字符串就直接删除。最多学习“不要无目的注入语义 prompt”的原则。

## 测试资产

外部项目的测试值得转成当前 Rust 侧回归测试或行为清单。

推荐迁移测试思想：

| 外部测试 | 价值 |
| --- | --- |
| `tests/test_thinking_tags_runtime.py` | thinking tag 跨 chunk 边界、literal `<thinking>` 不误拆 |
| `tests/test_thinking_history_runtime.py` | signed thinking history、redacted thinking、tool-result-only continuation |
| `tests/test_response_controls_runtime.py` | stop sequence 跨 chunk、max token 截断、stop 后不泄漏 tool_use |
| `tests/test_translator_prompt_pollution.py` | 防止无意义默认 prompt 注入 |
| `tests/test_kirocc_model_quality_static.py` | 模型映射和质量 guard |
| `tests/test_web_search_runtime.py` | server tool / web search 特殊链路 |

迁移原则：

1. 不按 Python+Node 运行时照搬，改成 Rust 单元测试或集成测试。
2. 不破坏当前已有特殊业务逻辑。
3. 重点覆盖协议行为，不依赖真实 Kiro 网络。

## 推荐落地优先级

### P0：建议优先做

#### 1. Suspended / 风控错误独立状态

当前没有显式识别 `TEMPORARILY_SUSPENDED`、`AccountSuspendedException`、423 locked。

建议：

1. 在 endpoint 或 provider 层增加 `detect_suspended_error(status, body)`。
2. 新增 disabled/runtime reason：`TemporarilySuspended`、`AccountSuspended`、`AccountLocked`。
3. `TEMPORARILY_SUSPENDED` 不一定永久禁用，建议默认长冷却 + 后台醒目标记。
4. 后台凭据卡片和 usage error record 显示完整 reason/message。

收益：

1. 避免把真实风控错误误报为普通 403/429。
2. 避免反复调度已被 suspended 的账号。
3. 便于用户判断是账号问题还是代理/请求问题。

#### 2. 上游模型能力同步

当前 `/v1/models` 是静态列表。建议增加非致命模型同步：

1. 每个可用账号按 profileArn 调 `ListAvailableModels`。
2. 结果写 PgSQL，启动时加载。
3. 同步失败不影响请求。
4. 页面允许手动同步并展示上次同步错误。
5. `/v1/models`、`/cc/v1/models`、`/ha/v1/models`、`/na/v1/models` 仍都能返回模型。

收益：

1. 新模型发布后不需要频繁改代码。
2. context window、thinking/cache 能力更准确。
3. 模型计价和模型选择页面更可靠。

#### 3. 模型 ID 向前兼容

建议调整 `map_model`：

1. 已知别名继续映射。
2. `claude-(sonnet|haiku|opus)-...` 形式默认原样透传。
3. 不把未来模型静默降级到旧模型。
4. 对不支持的非 Claude 模型继续明确报错。

收益：

1. 降低新模型上线时误用旧模型的风险。
2. 避免用户以为调用了新模型，实际被代理降级。

#### 4. Event-stream usage 兼容补齐

建议补：

1. `messageMetadataEvent.tokenUsage` 兼容解析。
2. `meteringEvent.usage` 解析。
3. `codeEvent` 解析，为 AmazonQCLI endpoint 预留。
4. 事件摘要 debug 日志。

收益：

1. usage 更准确。
2. 上游协议变化时更容易定位。
3. 后续支持 AmazonQCLI endpoint 的成本更低。

### P1：有价值，但要配置化/渐进

#### 1. Endpoint profile 和 fallback

建议把外部项目的 CodeWhisperer / AmazonQ / AmazonQCLI 思路迁移成 endpoint profile：

1. `ide-q`：当前默认。
2. `codewhisperer`：可选。
3. `amazonq-cli`：可选，独立 protocol，不默认 fallback。

注意：

1. 不要直接改变默认 endpoint。
2. 不要在所有错误上盲目切 endpoint。
3. 每个 endpoint 的 headers、body transform、event parser 都要独立测试。

#### 2. agent-mode/profileArn 缺省策略

建议做成配置：

1. `agentModeStrategy`: `auto` / `spec` / `vibe`。
2. `profileArnStrategy`: `explicitOnly` / `inferByAuthMethod`。

默认先保守，不破坏现有成功链路。

#### 3. 本地 response controls

如果当前代理没有严格处理 `stop_sequences` 和 `max_tokens`，可以学习外部项目的轻量控制：

1. stop sequence 要支持跨 chunk。
2. 命中 stop 后不能继续发送 tool_use。
3. max_tokens 本地截断只作为兜底，不替代上游 `maxTokens`。

#### 4. Token refresh jitter

当前已有 Redis refresh lock。可在后台批量刷新、启动自动刷新等场景增加 0-3 秒随机 jitter。

### P2：暂不建议做或仅保留为参考

1. 外部项目的进程内 AccountPool。
2. 概率重试。
3. 内存 prompt cache tracker。
4. K-Proxy MITM、telemetry 拦截、桌面端切号。
5. 硬编码 hidden model 列表。
6. 把 429 作为 quota exhausted 的默认逻辑。

## 与当前项目已有能力的关系

| 能力 | 当前 `kiro.rs` | 外部项目 | 建议 |
| --- | --- | --- | --- |
| 多凭据调度 | Redis/PgSQL + lease + queue + sticky | 进程内 AccountPool | 保留当前 |
| 并发控制 | 凭据级并发 + 排队等待 + lease 回收 | 基本没有同等能力 | 保留当前 |
| 429 处理 | 瞬态冷却，不直接禁用 | recoverable，常标 quota exhausted | 保留当前 |
| 402 处理 | 额度用尽禁用并切号 | 标 quota exhausted 并切号 | 策略接近，当前更稳 |
| 封禁状态 | 缺显式 suspended 分类 | 有 detectSuspendedError | 学习 |
| 模型列表 | 静态列表 | `ListAvailableModels` 同步 | 学习 |
| 模型映射 | 静态/包含式映射 | 新 Claude 模型原样透传 | 学习 |
| prompt cache | 路径级 high-cache + writer/input 改写 | 进程内 Anthropic cache 模拟 | 保留当前 |
| usage 优先级 | metadata > context > local simulation | tokenUsage > context > estimate | 基本一致，补事件兼容 |
| event-stream | 类型化解析，部分事件缺字段 | 覆盖事件更多 | 学习事件覆盖和日志 |
| thinking/tool | 已有大量兼容逻辑 | 测试样本更完整 | 补测试 |
| 错误日志 | 已带凭据 label，后台记录 | console 日志较多 | 当前更适合，补 suspended reason |

## 风险与边界

迁移时必须守住这些边界：

1. 不影响本地 reader 计算。路径级 writer/input 改写只影响下游响应和 usage record。
2. 不破坏 `/cc` writer 的自然采样策略。
3. 不改变 `/na` “关闭本地模拟 cache 上报，只保留真实上游 cache usage”的语义。
4. 不让模型计价、模型同步失败影响凭据调度。
5. 不把瞬态 429 变成长期禁用。
6. 不引入会绕过 Redis/PgSQL 调度状态的内存账号池。
7. 不让 endpoint fallback 导致同一请求无限重试或跨 endpoint 重复计费。
8. 不默认打开敏感请求体日志；后台可查看完整错误，但要避免泄露 token。

## 建议的实施顺序

1. 先补 suspended error detection 和后台展示。
2. 再补 event-stream 兼容解析与日志摘要。
3. 然后调整模型映射向前兼容。
4. 再做 `ListAvailableModels` 同步，结果持久化到 PgSQL。
5. 最后再评估 endpoint profile/fallback，不要和前面几个改动混在同一批。

每一步都应配套测试：

1. 单元测试：错误 body 分类、模型映射、event payload 解析。
2. 集成测试：模拟 402/429/403/suspended，多凭据 failover。
3. 回归测试：`/v1`、`/cc/v1`、`/ha/v1`、`/na/v1` usage 上报不变。
4. UI 测试：凭据卡片能展示完整错误和 suspended reason。

## 最终判断

外部项目最有价值的是“协议经验”和“边界测试”，不是它的整体架构。

当前 `kiro.rs` 已经更像一个可部署的服务端系统；外部项目更像一个桌面客户端增强版，包含大量针对 Kiro IDE/CLI 抓包兼容的经验。合理学习方式是把这些经验拆成小功能迁移：

1. 账号状态识别更准。
2. 模型能力来源更真实。
3. 上游 event-stream 覆盖更完整。
4. thinking/tool/response controls 测试更细。
5. endpoint profile 更可配置。

不应把外部项目的内存调度、桌面端切号、K-Proxy/MITM、硬编码模型和 prompt-cache 模拟整体搬进当前项目。
