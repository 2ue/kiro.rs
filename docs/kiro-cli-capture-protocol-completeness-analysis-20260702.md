# Kiro CLI 抓包协议完整性对照分析

日期：2026-07-02  
范围：只分析 `tmp/cap/kiro-cli/` 抓包与当前 `kiro.rs` 本地代码，不改生产代码。  
目的：判断当前项目在调用 Kiro 上游时，针对不同调用面是否还需要完善，避免只看主聊天请求而漏掉模型发现、profile 查询、telemetry 等真实 Kiro CLI 行为。

## 1. 总结

当前项目的主聊天链路已经比较接近抓包里的 Kiro CLI `GenerateAssistantResponse`。也就是说，真正影响“能不能对话”的 `runtime.us-east-1.kiro.dev` 请求，目前方向基本对。

但如果目标是更完整地贴近官方 Kiro CLI 的真实调用协议，当前项目还不完整。抓包里至少有四类调用面：

| 调用面 | 抓包里的真实 host | 当前项目覆盖情况 | 结论 |
| --- | --- | --- | --- |
| 主聊天生成 | `runtime.us-east-1.kiro.dev` | 已有 `CliEndpoint`，基本对上 | 优先级低，主要是细节校准 |
| profile 查询 | `management.us-east-1.kiro.dev` | 没看到 `GetProfile` 实现 | 明确缺口 |
| 模型列表 | `management.us-east-1.kiro.dev` | 当前 CLI 走 `q.* /ListAvailableModels`，并且是 GET | 明确偏差 |
| chat telemetry | `q.us-east-1.amazonaws.com` | 没看到 `SendTelemetryEvent` 实现 | 明确缺口 |
| client metrics | `client-telemetry.us-east-1.amazonaws.com` | 没看到 `/metrics` 实现 | 明确缺口 |

简单说：**聊天请求主链路没有明显跑偏；管理面和上报面还没按 Kiro CLI 抓包补齐。**

## 2. 抓包事实

### 2.1 `GetProfile`

抓包文件：

- `tmp/cap/kiro-cli/002_182659_management.us-east-1.kiro.dev:443_.json`

抓包里的请求事实：

| 字段 | 值 |
| --- | --- |
| method | `POST` |
| url | `https://management.us-east-1.kiro.dev:443/` |
| host | `management.us-east-1.kiro.dev` |
| content-type | `application/x-amz-json-1.0` |
| x-amz-target | `AmazonCodeWhispererService.GetProfile` |
| body | `{"profileArn": "...profile/YKXDHKXAWHQX"}` |
| user-agent API 名称 | `api/codewhispererruntime/... app/AmazonQ-For-CLI` |

返回体里有这些信息：

- `profile.arn`
- `profile.profileName`
- `profile.profileType`
- `profile.status`
- `profile.optInFeatures.dashboardAnalytics`
- `profile.optInFeatures.overageConfiguration`
- `profile.referenceTrackerConfiguration`

这说明 `GetProfile` 不是“可有可无的另一个模型列表接口”，它能拿到 profile 状态、超额配置、引用推荐配置等信息。当前项目如果以后要更像 Kiro CLI，或者要在后台展示/判断这些状态，就需要补这条管理面调用。

### 2.2 `ListAvailableModels`

抓包文件：

- `tmp/cap/kiro-cli/003_182700_management.us-east-1.kiro.dev:443_?origin=KIRO_CLI&profileAr.json`

抓包里的请求事实：

| 字段 | 值 |
| --- | --- |
| method | `POST` |
| url | `https://management.us-east-1.kiro.dev:443/?origin=KIRO_CLI&profileArn=...` |
| host | `management.us-east-1.kiro.dev` |
| content-type | `application/x-amz-json-1.0` |
| x-amz-target | `AmazonCodeWhispererService.ListAvailableModels` |
| body | `{"origin":"KIRO_CLI","profileArn":"...profile/YKXDHKXAWHQX"}` |
| x-amz-user-agent | `api/codewhispererruntime/... m/F,C app/AmazonQ-For-CLI` |

抓包里的响应事实：

- 顶层有 `defaultModel`，例如 `{"modelId":"auto"}`。
- `models[]` 里有 `modelId`、`modelName`、`description`。
- `models[].supportedInputTypes` 包含 `TEXT`、`IMAGE`。
- `models[].tokenLimits` 包含 `maxInputTokens`、`maxOutputTokens`。
- `models[].promptCaching` 包含：
  - `supportsPromptCaching`
  - `maximumCacheCheckpointsPerRequest`
  - `minimumTokensPerCacheCheckpoint`
- 部分模型有 `additionalModelRequestFieldsSchema`，里面描述 `max_tokens`、`output_config.effort`、`thinking` 等上游支持字段。

这里有一个非常具体的结论：**当前项目的 CLI 模型列表调用和抓包不一致，不只是 host 不一样，而是 method、host、target、body、user-agent 语义都不一样。**

### 2.3 `GenerateAssistantResponse`

抓包文件：

- `tmp/cap/kiro-cli/005_182803_runtime.us-east-1.kiro.dev:443_.json`
- `tmp/cap/kiro-cli/011_183112_runtime.us-east-1.kiro.dev:443_.json`
- `tmp/cap/kiro-cli/015_183123_runtime.us-east-1.kiro.dev:443_.json`

抓包里的请求事实：

| 字段 | 值 |
| --- | --- |
| method | `POST` |
| url | `https://runtime.us-east-1.kiro.dev:443/` |
| host | `runtime.us-east-1.kiro.dev` |
| content-type | `application/x-amz-json-1.0` |
| x-amz-target | `AmazonCodeWhispererStreamingService.GenerateAssistantResponse` |
| body 顶层字段 | `conversationState`、`profileArn` |
| currentMessage origin | `KIRO_CLI` |
| agentTaskType | `vibe` |
| chatTriggerType | `MANUAL` |

三次连续请求中的对话增长情况：

| 抓包文件 | history 条数 | 当前消息 toolResults 条数 | tools 条数 | modelId |
| --- | ---: | ---: | ---: | --- |
| `005...runtime...json` | 2 | 0 | 14 | `claude-opus-4.8` |
| `011...runtime...json` | 4 | 1 | 14 | `claude-opus-4.8` |
| `015...runtime...json` | 6 | 1 | 14 | `claude-opus-4.8` |

这说明官方 CLI 会把多轮历史放进 `conversationState.history`，并把工具定义和工具结果放在 `currentMessage.userInputMessage.userInputMessageContext` 里。当前项目在这块已经有相近实现。

这批 runtime 抓包里没有出现 `additionalModelRequestFields`。所以仅按这批证据看，“没有该字段时不要发送该字段”是符合抓包的。至于开启 thinking/output_config 时官方 CLI 如何发，需要另抓有对应场景的包，不能靠这批样本下结论。

### 2.4 `SendTelemetryEvent`

抓包文件：

- `tmp/cap/kiro-cli/008_183038_q.us-east-1.amazonaws.com:443_.json`
- `tmp/cap/kiro-cli/013_183117_q.us-east-1.amazonaws.com:443_.json`
- `tmp/cap/kiro-cli/017_183132_q.us-east-1.amazonaws.com:443_.json`

抓包里的请求事实：

| 字段 | 值 |
| --- | --- |
| method | `POST` |
| url | `https://q.us-east-1.amazonaws.com:443/` |
| host | `q.us-east-1.amazonaws.com` |
| content-type | `application/x-amz-json-1.0` |
| x-amz-target | `AmazonCodeWhispererService.SendTelemetryEvent` |
| body 顶层字段 | `clientToken`、`modelId`、`optOutPreference`、`profileArn`、`telemetryEvent`、`userContext` |
| telemetryEvent 示例 | `chatAddMessageEvent` |

`chatAddMessageEvent` 里包含：

- `conversationId`
- `messageId`
- `responseLength`
- `timeBetweenChunks`
- `timeToFirstChunkMilliseconds`

这条调用看起来不是生成回复必须依赖的接口，但它是官方 CLI 的真实上报链路。如果目标是“尽量完整复现 Kiro CLI 协议”，这条需要补；如果目标只是保证代理聊天可用，它不是最高优先级。

### 2.5 `client-telemetry /metrics`

抓包文件示例：

- `tmp/cap/kiro-cli/001_182658_client-telemetry.us-east-1.amazonaws.com:443_metrics.json`
- `tmp/cap/kiro-cli/004_182801_client-telemetry.us-east-1.amazonaws.com:443_metrics.json`
- `tmp/cap/kiro-cli/025_183701_client-telemetry.us-east-1.amazonaws.com:443_metrics.json`

抓包里的请求事实：

| 字段 | 值 |
| --- | --- |
| method | `POST` |
| url | `https://client-telemetry.us-east-1.amazonaws.com:443/metrics` |
| host | `client-telemetry.us-east-1.amazonaws.com` |
| content-type | `application/json` |
| authorization | `AWS4-HMAC-SHA256 ...` |
| x-amz-user-agent | `api/toolkittelemetry/1.0.0 ... app/AmazonQ-For-CLI` |
| body 顶层字段 | `AWSProduct`、`AWSProductVersion`、`ClientID`、`MetricData`、`OS`、`OSArchitecture`、`OSVersion` |

这条和前面的 Bearer token 调用不同，它用了 AWS SigV4 签名。实现成本和风险都比 `SendTelemetryEvent` 高一些，而且不影响主聊天链路。是否补要看目标：如果是“聊天代理稳定可用”，可以不急；如果是“完整模拟官方 CLI”，后续应单独设计。

## 3. 当前代码事实

### 3.1 当前只注册了 `ide` 和 `cli` 两类端点

代码位置：

- `src/main.rs:221-228`

当前启动时只注册：

- `IdeEndpoint`
- `CliEndpoint`

没有看到 `ManagementEndpoint`、`TelemetryEndpoint` 或类似独立调用面注册。

这意味着当前 endpoint 抽象主要覆盖聊天 API、MCP、模型列表，没有把抓包里的 `management` 和 telemetry 当成独立调用面来建模。

### 3.2 当前 `CliEndpoint` 的主聊天 runtime 基本对

代码位置：

- `src/kiro/endpoint/cli.rs:30-39`
- `src/kiro/endpoint/cli.rs:68-74`
- `src/kiro/endpoint/cli.rs:95-107`
- `src/kiro/endpoint/cli.rs:162-168`

当前 `CliEndpoint` 做了这些事：

- runtime host 是 `runtime.{region}.kiro.dev`。
- content type 是 `application/x-amz-json-1.0`。
- `api_url()` 返回 `https://runtime.{region}.kiro.dev/`。
- `decorate_api()` 设置 `x-amz-target=AmazonCodeWhispererStreamingService.GenerateAssistantResponse`。
- `transform_api_body()` 会把 body 改成 CLI 风格，并注入 streaming profile arn。

这些和 runtime 抓包方向一致。

### 3.3 当前 `CliEndpoint` 的模型列表和抓包不一致

代码位置：

- `src/kiro/endpoint/cli.rs:34-39`
- `src/kiro/endpoint/cli.rs:80-93`
- `src/kiro/endpoint/cli.rs:139-148`
- `src/kiro/provider.rs:1637-1641`

当前实现：

- `q_host()` 是 `q.{region}.amazonaws.com`。
- `models_url()` 拼的是 `https://q.{region}.amazonaws.com/ListAvailableModels?...`。
- 参数里有 `origin=KIRO_CLI`、`maxResults=50`、可选 `profileArn`、可选 `nextToken`。
- provider 调用模型列表时用的是 `client.get(&url)`。

抓包事实：

- host 是 `management.us-east-1.kiro.dev`。
- method 是 `POST`。
- `x-amz-target` 是 `AmazonCodeWhispererService.ListAvailableModels`。
- body 里有 `origin` 和 `profileArn`。
- URL query 里也带了 `origin` 和 `profileArn`。
- 没看到 `maxResults=50`。

所以这不是一个小字段不同，而是“当前项目把 CLI 模型列表当成 q host GET 接口，抓包显示官方 CLI 把它当成 management host POST AWS JSON 1.0 接口”。

### 3.4 当前模型列表响应结构只部分承载抓包内容

代码位置：

- `src/kiro/model/available_models.rs:7-13`
- `src/kiro/model/available_models.rs:16-32`
- `src/kiro/model/available_models.rs:35-52`

当前结构能承载：

- `models`
- `nextToken`
- `modelId`
- `modelName`
- `description`
- `supportedInputTypes`
- `tokenLimits`
- `promptCaching`
- 未显式建模的字段通过 `extra` 保留

但当前没有显式承载：

- 顶层 `defaultModel`
- `additionalModelRequestFieldsSchema` 的业务含义
- `rateMultiplier`
- `rateUnit`

其中 `additionalModelRequestFieldsSchema` 虽然会进 `extra`，但只是“保留下来”，不是“用于后续请求构造”。如果未来要根据真实模型能力决定是否发送 `thinking`、`output_config`、`max_tokens`，这里需要进一步使用这份 schema。

### 3.5 当前主请求体构造与抓包基本一致

代码位置：

- `src/kiro/model/requests/conversation.rs:14-30`
- `src/kiro/model/requests/conversation.rs:92-153`
- `src/anthropic/converter.rs:1343-1455`

当前结构包含：

- `conversationState`
- `currentMessage.userInputMessage`
- `userInputMessageContext.tools`
- `userInputMessageContext.toolResults`
- `history`
- `agentTaskType`
- `chatTriggerType`

这和 runtime 抓包主结构一致。抓包里连续三轮 `history` 逐步增长，当前转换器也在构造历史、工具定义、工具结果和当前消息。

### 3.6 当前 `additionalModelRequestFields` 是本地按模型名判断，不是直接用抓包里的模型 schema

代码位置：

- `src/kiro/model/requests/kiro.rs:52-64`
- `src/anthropic/converter.rs:838-968`
- `src/anthropic/converter.rs:1438-1448`

当前代码有 `AdditionalModelRequestFields`，能表达：

- `thinking`
- `output_config`
- `reasoning`

但是否发送这些字段，主要来自本地模型名判断和请求参数，不是直接来自 `ListAvailableModels` 返回的 `additionalModelRequestFieldsSchema`。

这不是立即错误，因为这批 runtime 抓包没有发送该字段。但如果以后遇到某些模型、账号、版本支持字段变化，最好以 management `ListAvailableModels` 的真实 schema 为准，而不是只靠本地硬编码。

### 3.7 当前已有 `ListAvailableProfiles` 自愈，但它不是 `GetProfile`

代码位置：

- `src/kiro/provider.rs:1092-1133`
- `src/kiro/provider.rs:1136-1194`

当前项目会对 Enterprise/IdC 缺失真实 profileArn 的情况调用 `ListAvailableProfiles`，用于找到真实 profileArn 并写回凭据。

这条逻辑有价值，但它不能替代抓包里的 `GetProfile`：

- `ListAvailableProfiles` 解决的是“这个账号有哪些 profile，选一个真实 ARN”。
- `GetProfile` 解决的是“这个 profile 当前具体状态和配置是什么”。

所以不能因为已有 `ListAvailableProfiles`，就认为 management 面已经完整覆盖。

### 3.8 当前没有看到 telemetry 实现

代码搜索事实：

- 当前 `src` 中没有看到 `SendTelemetryEvent` 实现。
- 当前 `src` 中没有看到 `client-telemetry` 或 `toolkittelemetry` 实现。
- 当前 `src/kiro/endpoint/cli.rs:42-54` 只有 `codewhispererstreaming` 风格的 CLI runtime user-agent。

这说明 telemetry 这两类抓包调用目前还没有被建模。

## 4. 分调用面的完善判断

### 4.1 主聊天 `GenerateAssistantResponse`

当前状态：基本可用，结构接近抓包。

不建议现在大改的地方：

- 不要为了追求“看起来更像官方”去改 `conversationState` 主结构。
- 不要从 Kiro 其他项目里引入额外 system prompt 或行为 prompt，这会改变模型行为，不是协议修复。
- 不要因为这批抓包没有 `additionalModelRequestFields` 就删除现有 reasoning/output_config 能力。这里只能说明“没有字段时不发送”符合样本，不能说明“永远不该发送”。

可以后续微调的地方：

- runtime `user-agent` / `x-amz-user-agent` 版本号和抓包有差异。当前代码是 `0.1.16551`，抓包是 `0.1.17593`。这通常不是最高风险，但如果上游做严格识别，后续可以跟进。
- 未来可用 `ListAvailableModels.additionalModelRequestFieldsSchema` 决定哪些模型支持哪些扩展字段。

结论：主聊天链路不是当前最大问题。

### 4.2 management `GetProfile`

当前状态：缺失。

为什么需要考虑补：

- 抓包证明官方 CLI 会调。
- 它返回 profile 状态、overage 配置、dashboard analytics、reference tracker 配置。
- 这些信息可能影响账号状态展示、费用/超额判断、后续功能开关。

实现时应注意：

- 不要把它混进 runtime `CliEndpoint` 的 `api_url()`。
- 应该有单独的 management host 构造：`management.{region}.kiro.dev`。
- 请求是 AWS JSON 1.0 POST，不是普通 GET。
- header 里的 API 名称是 `codewhispererruntime`，不是 `codewhispererstreaming`。

优先级：高，但不一定阻塞聊天可用性。

### 4.3 management `ListAvailableModels`

当前状态：CLI 路径和抓包明显不一致。

为什么优先级最高：

- 它直接影响模型列表、模型能力、上下文窗口、图片支持、缓存能力、thinking/output_config 支持。
- 当前项目后续很多逻辑都依赖模型能力目录。如果模型列表来源不准，就可能导致请求构造误判。

建议完善方向：

1. endpoint 抽象要能表达“模型列表是 POST，有 body，有 x-amz-target”，不能只返回一个 URL 后固定 `GET`。
2. CLI endpoint 的模型列表应该走 `management.{region}.kiro.dev`。
3. body 至少包含 `origin=KIRO_CLI` 和真实可用的 `profileArn`。
4. header 使用 `AmazonCodeWhispererService.ListAvailableModels`。
5. `x-amz-user-agent` 对齐 management 模型列表的 `m/F,C`。
6. 响应结构应显式增加 `defaultModel`。
7. `additionalModelRequestFieldsSchema` 最好从 `extra` 升级为可用信息，后续用于请求字段判断。

兼容注意：

- 这只应该影响 CLI endpoint 的模型列表，不应把 IDE endpoint 一起改坏。
- 现有 `ide` endpoint 之前已经按 `q.{region}.amazonaws.com/ListAvailableModels` 工作，不能因为补 CLI 抓包就破坏 IDE 路径。
- 如果仍要支持某些旧账号或旧 endpoint 的 q-host model list，可以做 endpoint 内部策略，但不要把 CLI 抓包事实和 IDE 历史行为混成一个接口。

优先级：最高。

### 4.4 `SendTelemetryEvent`

当前状态：缺失。

是否必须补：

- 对“聊天能不能成功”不是必须。
- 对“完整贴近 Kiro CLI 调用协议”是缺口。

建议：

- 可以作为独立 telemetry client，不要塞进主聊天请求函数。
- 失败不应影响主请求成功，因为 telemetry 失败不应该导致用户聊天失败。
- 如果补，要注意它需要记录 stream 过程中的 `timeToFirstChunkMilliseconds`、`timeBetweenChunks`、`responseLength`，不能为了上报而把整个流缓存到内存里。

优先级：中。

### 4.5 `client-telemetry /metrics`

当前状态：缺失。

是否必须补：

- 对主聊天完全不是必须。
- 如果要“完全像官方 CLI”，它属于真实调用链的一部分。

实现风险：

- 这条使用 AWS SigV4，不是 Bearer token。
- 它的凭据来源、签名、失败处理需要单独设计。
- 不建议为了补这条影响现有 Kiro token 调度。

优先级：低到中，取决于是否要完整模拟官方 telemetry。

### 4.6 MCP

当前抓包没有 MCP 请求样本。

当前代码里 CLI MCP 走：

- `src/kiro/endpoint/cli.rs:76-78`
- `src/kiro/endpoint/cli.rs:118-137`

但因为这批抓包没有 MCP，不能下结论说它完全对，也不能说它错。后续如果要完整判断 MCP，需要单独抓 `q.us-east-1.amazonaws.com/mcp` 或真实 Kiro CLI MCP 相关请求。

结论：本次不把 MCP 列为确认问题。

## 5. 不应混淆的几个点

### 5.1 `ListAvailableProfiles` 不等于 `GetProfile`

当前项目已有 `ListAvailableProfiles` 自愈逻辑，这是为了拿到真实 profileArn。  
抓包里的 `GetProfile` 是拿 profile 配置和状态。  
这两条不是同一个接口，也不解决同一个问题。

### 5.2 CLI endpoint 不等于只改 runtime host

当前已经有 `CliEndpoint`，但抓包说明 Kiro CLI 至少还有：

- runtime host：聊天生成
- management host：profile 和模型列表
- q host：chat telemetry
- client-telemetry host：metrics

所以“已有 CLI endpoint”只能说明 runtime 主链路被覆盖了一部分，不能说明整个 Kiro CLI 协议面都覆盖了。

### 5.3 telemetry 不应影响主请求稳定性

如果后续补 telemetry，必须保证：

- telemetry 失败不影响用户请求。
- telemetry 不缓存完整大响应到内存。
- telemetry 上报最好异步、限流、有超时。

否则为了“协议完整”引入稳定性问题，得不偿失。

### 5.4 模型能力不应只靠本地硬编码

抓包里的 `ListAvailableModels` 已经给了模型能力：

- 最大输入 token
- 最大输出 token
- 是否支持图片
- 是否支持 prompt cache
- cache checkpoint 数量和最小 token
- `additionalModelRequestFieldsSchema`

当前项目可以有兜底硬编码，但如果真实上游能给这些信息，后续最好优先用上游目录。这样遇到新模型或账号能力变化时，代理更不容易构造出上游不接受的请求。

## 6. 建议后续改造顺序

### 第一优先级：修 CLI `ListAvailableModels`

原因：

- 当前和抓包差异最大。
- 直接影响模型能力目录。
- 和主聊天构造、缓存、图片能力、thinking 字段都有间接关系。

最小目标：

- CLI endpoint 模型列表走 management POST。
- 请求带 `x-amz-target=AmazonCodeWhispererService.ListAvailableModels`。
- body 使用 `origin=KIRO_CLI` 和真实 profileArn。
- 保留 IDE endpoint 现有行为，不把 IDE 一起改掉。
- 响应结构补 `defaultModel`。

### 第二优先级：补 `GetProfile`

原因：

- 抓包明确存在。
- 能拿到 profile 状态和配置。
- 可以用于账号诊断和后续 UI 展示。

最小目标：

- 增加 management `GetProfile` 调用。
- 保存或至少日志化 profile 状态。
- 不影响主聊天链路。

### 第三优先级：按需补 `SendTelemetryEvent`

原因：

- 官方 CLI 确实发。
- 能记录首 token 时间、chunk 间隔、响应长度。
- 但不是聊天成功的必要条件。

最小目标：

- 作为独立上报，不影响主请求。
- 不引入大内存缓存。
- 有短超时和失败吞吐策略。

### 第四优先级：再评估 `client-telemetry /metrics`

原因：

- 有真实抓包。
- 但需要 AWS SigV4，复杂度更高。
- 对主代理功能收益较低。

建议单独排期，不要和 runtime/management 改造绑在一起。

## 7. 后续测试建议

如果后续按本文改造，至少要验证：

1. CLI runtime 主聊天仍能成功。
2. CLI `ListAvailableModels` 实际打到 `management.*.kiro.dev`，method 是 POST。
3. IDE endpoint 原有模型列表行为不被破坏。
4. `defaultModel` 能被解析。
5. `promptCaching`、`tokenLimits`、`supportedInputTypes` 仍能进入模型能力目录。
6. `additionalModelRequestFieldsSchema` 不丢失，最好能参与字段选择。
7. `GetProfile` 失败不导致主聊天失败。
8. telemetry 失败不导致主聊天失败。
9. 开启 telemetry 后，不因为记录 chunk 数据造成内存上涨。
10. Claude Code CLI 连续多轮、工具、MCP、agent 场景仍能跑通。

## 8. 最终判断

当前项目没有必要为了这批抓包去大改主聊天请求。主聊天请求方向基本正确。

真正需要完善的是：

1. **CLI 模型列表调用面**：当前和抓包不一致，应优先修。
2. **management `GetProfile`**：当前缺失，应补。
3. **telemetry 两条链路**：当前缺失，但不应影响主请求，适合独立补。
4. **模型能力响应使用**：当前能保存一部分字段，但 `defaultModel` 和 `additionalModelRequestFieldsSchema` 还没有被充分使用。

所以这次抓包给出的关键结论不是“聊天协议整体错了”，而是：**当前项目把 CLI runtime 主链路做得比较接近，但还没有完整建模 Kiro CLI 的 management 和 telemetry 调用面。**

## 9. 具体证据索引

这一节把前面的判断按“抓包证据 -> 代码证据 -> 结论”再列一遍，方便后续实施时逐条核对。

### 9.1 runtime 主聊天生成

抓包证据：

- `tmp/cap/kiro-cli/005_182803_runtime.us-east-1.kiro.dev:443_.json`
- `tmp/cap/kiro-cli/011_183112_runtime.us-east-1.kiro.dev:443_.json`
- `tmp/cap/kiro-cli/015_183123_runtime.us-east-1.kiro.dev:443_.json`

这些抓包共同证明：

- 请求 host 是 `runtime.us-east-1.kiro.dev`。
- 请求 method 是 `POST`。
- `content-type` 是 `application/x-amz-json-1.0`。
- `x-amz-target` 是 `AmazonCodeWhispererStreamingService.GenerateAssistantResponse`。
- body 顶层字段是 `conversationState` 和 `profileArn`。
- `conversationState.currentMessage.userInputMessage.origin` 是 `KIRO_CLI`。
- 三轮请求里 `history` 从 2 条增长到 4 条，再增长到 6 条，说明官方 CLI 真实发送完整多轮历史。

代码证据：

- `src/kiro/endpoint/cli.rs:30-32`：`runtime_host()` 构造 `runtime.{region}.kiro.dev`。
- `src/kiro/endpoint/cli.rs:68-73`：CLI endpoint 使用 `application/x-amz-json-1.0`，API URL 是 runtime 根路径。
- `src/kiro/endpoint/cli.rs:95-107`：`decorate_api()` 设置 `AmazonCodeWhispererStreamingService.GenerateAssistantResponse`、runtime host、Bearer token。
- `src/kiro/endpoint/cli.rs:162-168`：`transform_api_body()` 做 CLI body 改写并注入 `profileArn`。
- `src/kiro/endpoint/cli.rs:183-190`、`src/kiro/endpoint/cli.rs:221-236`：把请求体里的 origin 改成 `KIRO_CLI`。
- `src/kiro/model/requests/conversation.rs:14-30`：当前项目的数据模型包含 `conversationState`、`currentMessage`、`conversationId`、`history`。
- `src/anthropic/converter.rs:1343-1455`：当前项目会构造 history、tools、toolResults、currentMessage 和 `agentTaskType=vibe`。

结论：

- 这条主聊天链路当前实现方向基本正确。
- 后续不应该优先大改这块，除非抓到具体 runtime 失败包。

### 9.2 management `GetProfile`

抓包证据：

- `tmp/cap/kiro-cli/002_182659_management.us-east-1.kiro.dev:443_.json`

该抓包证明：

- 请求 host 是 `management.us-east-1.kiro.dev`。
- 请求 method 是 `POST`。
- `x-amz-target` 是 `AmazonCodeWhispererService.GetProfile`。
- body 是 `profileArn`。
- 返回体包含 `profile.status`、`profile.profileType`、`profile.optInFeatures`、`profile.referenceTrackerConfiguration` 等 profile 状态和配置。

代码证据：

- `src/main.rs:221-228`：当前只注册 `IdeEndpoint` 和 `CliEndpoint`，没有 management endpoint。
- 当前 `src` 中没有 `GetProfile` 字符串实现。
- `src/kiro/provider.rs:1092-1133` 是 `ListAvailableProfiles`，不是 `GetProfile`。

结论：

- 当前项目缺 `GetProfile` 调用面。
- 已有 `ListAvailableProfiles` 不能替代 `GetProfile`，因为前者是找真实 profileArn，后者是读取某个 profile 的状态和配置。

### 9.3 management `ListAvailableModels`

抓包证据：

- `tmp/cap/kiro-cli/003_182700_management.us-east-1.kiro.dev:443_?origin=KIRO_CLI&profileAr.json`

该抓包证明：

- 请求 host 是 `management.us-east-1.kiro.dev`。
- 请求 method 是 `POST`。
- `x-amz-target` 是 `AmazonCodeWhispererService.ListAvailableModels`。
- body 里有 `origin=KIRO_CLI` 和 `profileArn`。
- URL query 里也有 `origin=KIRO_CLI` 和 `profileArn`。
- `x-amz-user-agent` 是 `api/codewhispererruntime/... m/F,C app/AmazonQ-For-CLI`。
- 响应顶层有 `defaultModel`。
- 响应模型项包含 `promptCaching`、`tokenLimits`、`supportedInputTypes`、`additionalModelRequestFieldsSchema`。

代码证据：

- `src/kiro/endpoint/cli.rs:34-39`：当前 CLI 的非 runtime host helper 是 `q.{region}.amazonaws.com`。
- `src/kiro/endpoint/cli.rs:80-93`：当前 CLI `models_url()` 拼的是 `q` host 的 `/ListAvailableModels?...`，并带 `maxResults=50`。
- `src/kiro/provider.rs:1637-1641`：当前 provider 用 `client.get(&url)` 调模型列表。
- `src/kiro/endpoint/cli.rs:139-148`：当前 `decorate_models()` 没有设置 `AmazonCodeWhispererService.ListAvailableModels` 这个 target，也没有 management host。
- `src/kiro/model/available_models.rs:7-13`：响应顶层当前只有 `models` 和 `nextToken`，没有显式 `defaultModel`。

结论：

- 当前 CLI 模型列表和抓包不一致，而且不是小字段差异。
- 这条应是协议完善第一优先级。

### 9.4 模型能力字段

抓包证据：

- `tmp/cap/kiro-cli/003_182700_management.us-east-1.kiro.dev:443_?origin=KIRO_CLI&profileAr.json`

该抓包证明模型目录里有：

- `supportedInputTypes`：判断是否支持文本、图片。
- `tokenLimits.maxInputTokens`：判断上下文窗口。
- `tokenLimits.maxOutputTokens`：判断最大输出。
- `promptCaching.supportsPromptCaching`：判断模型是否支持 prompt cache。
- `promptCaching.maximumCacheCheckpointsPerRequest`：判断最多几个缓存点。
- `promptCaching.minimumTokensPerCacheCheckpoint`：判断每个缓存点最低 token。
- `additionalModelRequestFieldsSchema`：判断模型支持哪些额外字段。

代码证据：

- `src/kiro/model/available_models.rs:16-32`：当前能建模 `supportedInputTypes`、`tokenLimits`、`promptCaching`，其他字段进 `extra`。
- `src/kiro/model/available_models.rs:44-52`：当前有 prompt caching 的三个字段。
- `src/anthropic/converter.rs:838-968`：当前 context window、native reasoning schema 等仍有本地 fallback / 本地规则。
- `src/anthropic/converter.rs:1438-1448`：是否发送 `additionalModelRequestFields` 由本地转换逻辑决定。

结论：

- 当前模型能力读取方向是对的，但没有充分使用抓包里的完整模型能力。
- `defaultModel` 应显式建模。
- `additionalModelRequestFieldsSchema` 后续最好用于判断字段是否可发，减少硬编码误判。

### 9.5 chat telemetry `SendTelemetryEvent`

抓包证据：

- `tmp/cap/kiro-cli/008_183038_q.us-east-1.amazonaws.com:443_.json`
- `tmp/cap/kiro-cli/013_183117_q.us-east-1.amazonaws.com:443_.json`
- `tmp/cap/kiro-cli/017_183132_q.us-east-1.amazonaws.com:443_.json`

这些抓包证明：

- 请求 host 是 `q.us-east-1.amazonaws.com`。
- 请求 method 是 `POST`。
- `x-amz-target` 是 `AmazonCodeWhispererService.SendTelemetryEvent`。
- body 里有 `clientToken`、`modelId`、`profileArn`、`telemetryEvent`、`userContext`。
- `telemetryEvent.chatAddMessageEvent` 会记录 `conversationId`、`messageId`、`responseLength`、`timeBetweenChunks`、`timeToFirstChunkMilliseconds`。

代码证据：

- 当前 `src` 中没有 `SendTelemetryEvent` 实现。
- `src/kiro/endpoint/cli.rs:42-54` 只有 runtime 的 `codewhispererstreaming` user-agent helper，没有 `codewhispererruntime` telemetry helper。

结论：

- 当前缺 chat telemetry。
- 它不应阻塞主聊天，但如果目标是完整贴近官方 CLI，需要独立补。

### 9.6 client telemetry `/metrics`

抓包证据：

- `tmp/cap/kiro-cli/001_182658_client-telemetry.us-east-1.amazonaws.com:443_metrics.json`
- `tmp/cap/kiro-cli/004_182801_client-telemetry.us-east-1.amazonaws.com:443_metrics.json`
- `tmp/cap/kiro-cli/025_183701_client-telemetry.us-east-1.amazonaws.com:443_metrics.json`

这些抓包证明：

- 请求 host 是 `client-telemetry.us-east-1.amazonaws.com`。
- 请求 path 是 `/metrics`。
- 请求 method 是 `POST`。
- `content-type` 是 `application/json`。
- authorization 是 `AWS4-HMAC-SHA256 ...`，不是 Bearer token。
- body 里有 `AWSProduct`、`AWSProductVersion`、`ClientID`、`MetricData`、`OS`、`OSArchitecture`、`OSVersion`。

代码证据：

- 当前 `src` 中没有 `client-telemetry` 字符串实现。
- 当前 `src` 中没有 `toolkittelemetry` 字符串实现。

结论：

- 当前缺 client telemetry。
- 因为它需要 SigV4，复杂度高于 `SendTelemetryEvent`，不建议和主聊天修复混在一起做。

### 9.7 endpoint 抽象能力

抓包证据：

- runtime、management、q telemetry、client-telemetry 是不同 host、不同 target、不同鉴权/签名方式。

代码证据：

- `src/kiro/endpoint/mod.rs:23-57`：当前 endpoint trait 只抽象了 `api_url`、`mcp_url`、`models_url`、`decorate_api`、`decorate_mcp`、`decorate_models`、`transform_api_body`、`transform_mcp_body`。
- `src/kiro/provider.rs:1637-1641`：模型列表调用被固定成 URL + GET 的形态。

结论：

- 当前 endpoint 抽象对 runtime/MCP 够用，但对 management POST 模型列表不够自然。
- 后续最好让模型列表调用也能由 endpoint 返回“请求方法、URL、body、target/header 装饰”，而不是只返回 URL。

## 10. 后续落地边界

后续如果按本文实施，建议明确边界：

- 不要改缓存策略。
- 不要改 `/v1`、`/cc`、`/ha`、`/na` 的路径缓存行为。
- 不要把 IDE endpoint 强行改成 CLI management 行为。
- 不要为了 telemetry 把完整响应内容缓存到内存。
- 不要引入额外 system prompt 或 agentic prompt。
- 不要让 profile / telemetry 失败影响主聊天成功。

真正需要优先动的是：

1. CLI 模型列表调用面。
2. management `GetProfile`。
3. 模型能力结构使用。
4. telemetry 独立上报。
