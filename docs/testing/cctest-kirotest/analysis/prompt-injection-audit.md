# CCTest 路由提示词注入审计

更新时间：2026-11-08（Round 84，59139 单变量 A/B 与进程内抓包）

## 结论

提示词注入会影响评分，但影响方式取决于注入项是否进入 CCTest 使用的路由。当前 `tune-ss6-99` 路由的可配置注入项基本全部关闭，Round 63 的上游请求没有发现代理注入的 identity、prompt steering、WebSearch guidance、language reminder 或 tool choice 文本。Round 63 仍为 `82/110`，所以不能把“注入项很多”本身当作当前三项失败的根因。

已确认会影响评分或公开响应形状的注入项：

| 注入项 | 当前 tune 路由 | 已知影响 | 证据 |
|---|---|---|---|
| `identityPrompt` | 关闭 | 可能改变结构化输出里的身份语义，但单独开启未改善 `output_config` | Round 42、生产对照 |
| `identityTurnReminder` | 关闭 | 会让非流响应多出 thinking block，`non_stream` 从 10 降到 0 | Round 43/44、I-25/I-26 |
| `thinkingOutputPolicy` | 关闭 | 对 Opus 5.x 可能触发空响应；代码已有模型过滤与降级重试 | `src/anthropic/converter/thinking.rs` |
| `thinkingControls` | 全局启用，但 tune 路由本轮未单独触发 | 协议控制标签，影响 Kiro 兼容 thinking 解析 | Round 63 上游 16 个请求含 `<thinking_mode>` |
| `toolChoicePrefix` / `toolChoiceTurnReminder` | 关闭 | 改变工具调用概率和 stop_reason；开启时可能影响 `server_tool` | 配置预览、转换器代码 |
| `webSearchGuidance` | 关闭 | 改变内部 WebSearch 模型是否搜索、query 形状和引用文本 | 配置预览、`model_driven_websearch.rs` |
| `emptyTurnNudge` | 关闭 | 只用于空轮首输出前重试；会改变重试 user turn，不能混入普通评分样本 | 配置预览、`handlers.rs` |
| `taskQuality` / `languageConstraint` / `custom` | 关闭 | 改变模型遵从、语言和工具描述之外的语义；当前没有进入 tune 上游请求 | Round 63 字段扫描 |
| `chunkedWritePolicy` / `chunkedWriteToolSuffix` | 关闭 | 只改变 Write/Edit 工具说明；可能影响工具参数和续接轮次 | 转换器工具注入点 |
| `languageTurnReminder` | 关闭 | 追加当前 user turn，可能改变文本内容与非流输出 | 转换器注入点 |

## Round 63 现场证据

59139 Admin API 的 `/api/admin/prompt-injection/preview` 对 `/dfcache/tune-ss6-99` 返回：

- `taskQuality`、`languageConstraint`、`languageTurnReminder`、`custom`、`identityPrompt`、`identityTurnReminder`、`chunkedWritePolicy`、`chunkedWriteToolSuffix`、`thinkingOutputPolicy`、`toolChoicePrefix`、`toolChoiceTurnReminder`、`webSearchGuidance`、`emptyTurnNudge`：`blocked`，来源为该路由显式 `off`；
- `thinkingControls`：`not_triggered`；
- `thinkingModelName=true`，`thinkingTrigger=off`，`channelLeakGuard=false`，`modelDrivenWebSearch=false`。

Round 63 抓包：

```text
logs/llm-capture/capture-20261107T-round-63-capture-20261107T165556Z-f866fbae/events.jsonl
```

对客户端请求和 Kiro `upstream_request` 做 marker 统计：

- 客户端正文含 `<system-reminder>`：17 个；这是 CCTest/Claude Code 自己发送的系统内容，不是 tune 路由注入；
- 上游正文含 `<system-reminder>`：17 个；代理保留了客户端历史内容；
- 上游正文含 `<thinking_mode>`：16 个；来自 Claude Code thinking 请求和兼容转换；
- 上游正文含 `<assistant_identity>`：0 个；
- 上游正文含 `<prompt_steering>`：0 个；
- 上游正文含 `<web_search_guidance>`：0 个；
- 上游正文含 `<language_reminder>`：0 个；
- 上游正文含 `<thinking_output_policy>`：0 个；
- 上游正文含工具选择提示（`MUST call the tool` / `Begin this reply by calling`）：0 个。

同一轮的 22 个请求全部完成客户端 `message_start → content_block* → message_delta → message_stop` 生命周期，25/25 上游 HTTP 200，总分仍为 `82/110`。

## 为什么注入会影响 CCTest

CCTest 的检查对象是模型行为和 Anthropic 外层协议的组合。注入文本虽然不一定直接修改 SSE 序列，但会改变模型的：

1. 是否产生 thinking、thinking 是否可见、是否输出签名；
2. 是否调用工具、调用哪个工具、工具调用次数和 `stop_reason`；
3. WebSearch 的 query、搜索次数、结果后的引用文本；
4. 结构化输出里的身份字段和描述字段；
5. 非流响应的 content block 数量和类型；
6. 空轮重试时是否继续原任务或输出恢复提示。

因此注入开关必须和协议字段分开做单变量实验。当前已有证据表明 `identityTurnReminder` 会造成非流回归；`identityPrompt` 单独开启不够；`thinking_delta.estimated_tokens` 的多种形状经过多轮实验仍未改变流结构评分。

## 当前处理规则

- CCTest 主路由继续保持当前 tune 配置，不把所有注入全局打开；
- 需要研究某个注入项时，先调用 Admin preview 记录实际注入，再开启限时抓包并只改一个项；
- 结果必须同时记录模型响应 block 形状、上游请求 marker、评分分项和回归保护项；
- 不把 CCTest 自己携带的 `<system-reminder>` 等客户端内容误记为代理注入；
- 客户端 system/history 中的提示词属于 Claude Code 会话输入，直接删除或改写会破坏历史回放，当前没有证据支持这么做；
- 不在没有评分共变证据时删除 `thinkingControls` 或改写客户端历史 system 内容。

## Round 84：thinkingOutputPolicy 非触发边界

Round 84 只在 59139 的 `/dfcache/tune-ss6-99` 将 `thinkingOutputPolicy.enabled` 从 `false` 改为 `true`，完整跑一轮后恢复原值。结果从 Round 83 的 `82/110` 变为 `82/110`，但 Admin preview 显示该项为 `not_triggered`，恢复后为 `blocked`。代码目录明确写出：该项对 `-thinking` 模型、`thinking.type=enabled` 或强制可见 thinking 生效，而 Opus 5 系列自动跳过。

抓包显示：22/22 客户端流生命周期完整，上游 25/25 最终成功，只有两次 402 后换号成功；代理自定义 identity、prompt steering、WebSearch guidance、language reminder、tool choice、thinking policy marker 均为 0。说明“系统存在该注入点”不等于“该路由本轮实际把提示词文本送入上游”，更不等于当前失败分项的根因。

提示词注入对评分的影响边界目前这样划分：

| 影响面 | 可能改变的评分观察对象 | 当前证据 |
|---|---|---|
| thinking 控制、输出策略 | thinking block 是否出现、`thinking_delta` 形状、签名数量、空响应 | Round 43、67；关闭 `thinkingControls` 会本地 400。Round 84 的 `thinkingOutputPolicy` 对 Opus 5.5 未触发，不能用于评分因果判断 |
| identity / language / task | 文本语义、结构化输出字段、模型归因、非流 block 类型 | Round 42、43；`identityTurnReminder` 使 `non_stream` 从 10 降到 0 |
| tool choice | `tool_use` / `server_tool_use` 数量、`stop_reason`、后续 `tool_result` 配对 | 配置预览与转换器路径；当前 tune 路由显式关闭 |
| WebSearch guidance | 是否搜索、query 形状、引用文本、搜索次数 | 当前 tune 路由关闭；完整搜索链仍为 `websearch=0`，剩余候选是 opaque 字段 |
| 客户端自带 system/history | 上游上下文和模型行为 | Round 63、80、83、84；必须保留，不能当作代理注入清洗 |

当前结论：提示词注入确实会影响评分相关的请求形状；Round 84 只固定了 Opus 5.5 tune 路由上的非触发边界，不能把它当作 `thinkingOutputPolicy` 对评分无影响的有效 A/B。后续继续采用“先 preview，必须看到 `applied`，一次只改一个开关，保存上游 marker 和 SSE，结束后恢复”的流程。

只读草稿 preview 的触发矩阵已固定：把 `thinkingTrigger` 临时设为 `real_request` 并打开策略时，`claude-sonnet-4-5-thinking`、`claude-sonnet-5-5-thinking`、`claude-opus-4-8-thinking` 为 `applied`；`claude-opus-5-5` 仍为 `not_triggered`。这解释了 Round 84 未改变分数，也说明后续不能拿固定 Opus 5.5 的结果否定该注入在其它模型上的影响。

## Round 67：thinkingControls A/B 结果

Round 67 在 59139 只关闭 `/dfcache/tune-ss6-99` 的 `thinkingControls`，其它配置保持不变。CCTest 的 6 个首项请求全部在本地 preflight 返回 HTTP 400：

```text
reasoning was requested, but both native reasoning fields and compatible thinking prompt controls are unavailable
```

抓包 `logs/llm-capture/capture-20261107T173429Z-df4e20da/events.jsonl` 只有 `http_ingress`、`client_response`、`request_summary` 和 `http_egress`，没有 `upstream_request`。这证明该注入是当前 reasoning 请求的协议前置条件，不能把它当成普通行为提示删除。测试后已从保存的 runtime JSON 恢复 `thinkingControls.enabled=true`，59138 未被触碰。

结论：提示词注入确实能改变评分相关的请求形状，但目前没有证据表明 tune 路由的 `thinkingControls` 是 `stream_structure=5` 或 `signature_proto=5` 的根因；关闭它会更早失败。客户端自带的 `<system-reminder>` 和 `<thinking_mode>` 仍需保留，不能直接清洗。

Round 69–71 进一步证明提示词注入与 caller 字段是两条独立链路：三轮均保持 tune 路由的注入配置，Round 71 只改流式 caller 是否补齐，评分仍为 82。不能把 caller 缺失或客户端自带 `<system-reminder>` 误判为代理提示注入根因；后续提示词实验仍需一次只开一个注入项，并同时保留上游正文 marker 与响应 block 统计。

Round 72 没有改变任何提示词注入开关，只修改了 WebSearch 对外 block 元数据；评分仍为 82。该对照进一步把“模型行为提示”与“响应字段协议”分离，后续 WebSearch 字段实验不能归因于提示词注入变化。

## Round 80 复核

Round 80 Admin preview 和抓包再次确认当前 tune 路由状态：

- `modelDrivenWebSearch=true`，但 `webSearchGuidance` 仍由路由显式关闭；
- `thinkingControls` 在 preview 中为 `not_triggered`，客户端带来的 `<thinking_mode>` 仍原样进入上游；
- 客户端 `<system-reminder>` 17 个，上游保留 17 个；上游 `<thinking_mode>` 16 个；代理自定义 identity、prompt steering、WebSearch guidance、language reminder、tool choice marker 均为 0；
- WebSearch 链路实际完成 MCP 200、结果回灌、citation 和最终回答，评分仍为 `websearch=0`。

因此 Round 80 把提示词来源与 WebSearch opaque 字段进一步分开：提示词影响搜索是否发生和 query 形状，但当前失败请求已经发生完整搜索；剩余高价值差异是官方要求的 `encrypted_content` / `encrypted_index` opaque 语义，而不是 tune 路由额外注入。不要清洗客户端历史 system，也不要为了追分开启 `webSearchGuidance` 或关闭 `thinkingControls`。

## Round 75–76 复核

Round 75 恢复 thinking 起始块 `signature:""`，未改变任何提示词开关，完整评分保持 `82/110`。Round 76 的 13 个 thinking 请求中，客户端自带 `<system-reminder>`/`<thinking_mode>` 仍按原样进入上游；逐请求 signature 对照为 `13/13` 一致。由此可把提示词影响与 signature 传输分开：提示词会改变模型是否 thinking、工具调用和结构化输出，但当前没有证据表明代理注入导致 `signature_proto=5`。

后续实验继续采用单变量规则：先保存 Admin preview，再扫描客户端/上游 marker，最后比较分项评分和保护项。不要为了追分清洗客户端 system/history，也不要同时开启多个注入开关。

## Round 82 复核

Round 82 的完整抓包仍只看到客户端自带的 `<system-reminder>` 与 `<thinking_mode>`；没有代理自定义 identity、prompt steering、WebSearch guidance、language reminder、tool choice 或 thinking policy marker。synthetic reasoning 边界清理发生在 Kiro 原生序列化层，不是提示词层。

本轮评分仍为 `82/110`，所以该边界修复既没有改变提示词注入行为，也没有证据表明当前三个失败项由 tune 路由额外提示词注入造成。客户端历史 system/thinking 内容继续保留，不做清洗。

## Round 88–90：提示词来源与 non_stream 波动分离

Round 88–90 的上游正文继续只出现 Claude Code 自带 `<system-reminder>` / `<thinking_mode>`，没有 tune 路由自定义 identity、language、WebSearch guidance、tool choice 或 thinking policy marker。三轮结果分别为 `73/110`、`82/110`、`82/110`。

Round 88 的非流请求在上游产生 reasoning，代理返回空 thinking block，`non_stream=0`；Round 89、90 同类请求没有 reasoning，只返回 text，`non_stream=10`。因此当前非流分数变化与模型采样和输出 block 形状共变，不能归因于代理提示词注入。继续保留客户端 system/history，不做清洗；如需 A/B，必须先看到提示词 preview 为 `applied`，并只在 59139 改一个开关。

Round 88–90 的 WebSearch 都完成 API/MCP/回灌闭环但 `websearch=0`，而提示词配置未变；该对照继续把 WebSearch 失败与 opaque 字段语义、跨轮回传或私有评分规则分离。
