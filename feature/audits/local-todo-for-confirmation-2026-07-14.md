# 本地待裁定 Todo：生产错误、usage 与长上下文问题（2026-07-14）

本文用于把上一轮“后续问题清单 / 建议执行顺序”落到本地中文文档，便于继续裁定。
口径：当前本地工作区 + 已采集生产证据 + `feature/issues/` 既有分析。本文不是发布说明，也不表示所有待办已经实现。

## 0. 当前已完成项概览

### P001 / P002：外部池 `model_unavailable` 导致队列异常

已完成并验证的处理：

- `model_unavailable` 不再默认触发整个外部池冷却。
- 新增运行时配置：
  - `externalPoolModelUnavailableCooldownMode`：`model | pool | disabled`
  - `externalPoolModelUnavailableCooldownSecs`：默认 `10`
- 默认行为改为“按外部池 + 模型”粒度冷却，避免某个不支持模型把整个外部池打入冷却，进而导致低 RPM 下也出现 `external_pool_queue_full`。
- 两套 UI 都已补充对应配置。
- UI 文案已明确：
  - `maxConcurrentRequests` 是并发限制，不是 RPM。
  - `externalPoolMaxQueuedRequests` 是外部池等待队列上限，不是本地凭据调度队列。

本地验证已完成：

- `cargo fmt --check`
- `cargo test --all-targets`
- `cargo build --release`
- `pnpm -C ui build`
- `pnpm -C admin-ui build`
- 本地服务重启后健康检查通过。
- Admin API 对新增配置的读写与恢复通过。
- 真实 `/v1/messages` 调用成功。
- 真实外部池 fake upstream 验证：
  - 第一次请求命中 fake upstream 并返回 `model_not_found`。
  - 第二次相同模型请求没有再次打到 fake upstream。
  - 第二次没有退化为 `external_pool_queue_full`。
  - 两条 usage record 均归类为 `errorType=model_unavailable`。
  - 临时外部池删除，运行配置恢复。

关键解释：

- 这次没有把 RPM 限制改成并发限制。
- 问题不在于外部池 RPM 数值本身。即使外部池 RPM 配到 3000，只要请求无法被正常出队，例如可用目标因错误冷却被整体排除，等待队列仍然会满。
- 本地 18 个账号能支撑的并发与外部池队列不是同一个资源池。请求进入外部池 fallback 后，是否能被外部池消费，取决于外部池选择器、并发、冷却、模型可用性和队列策略。
- P001 不需要精确匹配不支持模型；它解决的是“单模型错误不应拖垮整个外部池可用性”的调度问题。
- P002 需要输出完整不支持模型清单，用于后续配置 alias / 映射。

### P002：生产证据中的不支持模型完整清单

| 模型 | 条数 |
|---|---:|
| `claude-opus-4-8` | 321 |
| `claude-sonnet-5` | 75 |
| `claude-sonnet-4-6` | 64 |
| `claude-opus-4-6` | 58 |
| `claude-haiku-4-5-20251001` | 30 |
| `claude-opus-4-7` | 24 |
| `claude-sonnet-4-5-20250929` | 16 |
| `claude-3-5-sonnet-20241022` | 1 |
| `claude-opus-4-5-20251101` | 1 |
| `claude-sonnet-4-20250514` | 1 |
| `claude-opus-4-1-20250805` | 1 |
| `claude-3-5-haiku-20241022` | 1 |
| `claude-3-7-sonnet-20250219` | 1 |

后续裁定点：

- 这些模型是否应该全部进入显式 alias / 映射配置。
- 是否保留 `model_unavailable` 的模型级短冷却默认值 10 秒。
- 是否需要给外部池配置一个“模型不支持时直接本地失败，不进入队列等待”的更强策略。

## 1. P003：本地 Kiro upstream idle timeout / stream error

本轮状态（2026-07-14）：已实现并完成静态/定向验证，待发布后继续观察生产复发率。

### 现象

生产证据中存在上游流式空闲、stream read error、上游 status error 等问题。典型表现是上游在较长时间没有输出，或者流中途出错。

### 当前判断

这类问题可以考虑“流式重试”，但必须先定义安全边界：

- 如果已经向下游发送过 `message_start`、`content_block_start`、`content_block_delta`、`tool_use`、thinking 或任何可见文本，再换号重试会造成重复事件、乱序事件、重复工具调用或 usage 不一致。
- 只有在“下游尚未收到任何已提交 SSE 字节 / 业务事件”之前，才适合做自动重试。
- 当前实现如果过早提交初始 SSE，则不能直接硬加换号重试；需要先做“延迟响应提交”或等价机制。

### 已实现方案

新增本地 Kiro stream retry 策略，默认保守启用，且安全边界固定为“首个下游 SSE 字节提交前”：

- `kiroUpstreamStreamRetryEnabled`：是否启用首输出前流式换号重试，默认 `true`。
- `kiroUpstreamStreamRetryMaxAttempts`：最大尝试次数，默认 `2`，包含第一次调用。
- `kiroUpstreamStreamRetryOnIdleTimeout`：首输出前 idle timeout 是否重试，默认 `true`。
- `kiroUpstreamStreamRetryOnReadError`：首输出前 stream read error 是否重试，默认 `true`。
- `kiroUpstreamStreamRetryOnStatusError`：首输出前 2xx JSON 错误体 / 上游 status error 是否重试，默认 `true`。

实现约束：

- 重试开启时，`message_start` 等初始 SSE 事件会延迟到首个真实下游事件或最终错误事件一起发送；这样在首输出前发生 idle/read/status 失败时，可以安全换号。
- 一旦已经向客户端发送任何 SSE 字节，包括 ping / noop keepalive，就不再自动换号重试，避免重复 `message_start`、重复 `tool_use`、乱序事件和 usage 拼接。
- 失败尝试会通过 `KiroStreamCompletion::report_upstream_stream_failure` 释放并发槽并进入短暂 stream 冷却；最终 usage record 记录 `streamRetryAttempts` / `streamRetryReasons`。
- 两套 UI 都已补充开关和原因展示字段；usage 详情会显示首输出前重试次数和原因。

### 需要补充的证据

只在失败或采样时记录，不应影响主路径：

- 是否已经向下游提交过 SSE 字节。
- 首个上游事件时间。
- 首个下游事件时间。
- retry eligibility：为什么允许 / 为什么拒绝重试。
- retry account mode：原账号重试还是换号重试。
- 每次尝试的 upstream status、error type、stop reason。

### 验收口径 / 本轮验证

- 首输出前上游 idle / read error 可以按配置重试。
- 已经向下游发送任何业务事件后，不做自动换号重试，只记录错误。
- Claude CLI 和 direct SSE 的事件顺序不被破坏。
- usage record 能看出每次 attempt、是否重试、是否换号、最终结果。
- 已通过：
  - `cargo check --all-targets`
  - `pnpm -C ui build`
  - `pnpm -C admin-ui build`
  - `cargo test default_runtime_controls_are_conservative -- --nocapture`
  - `cargo test on_too_long_initial_guard_repairs_without_size_trimming -- --nocapture`
  - `cargo test external_pool_model_unavailable_cooldown_is_model_scoped_and_does_not_queue -- --nocapture`

## 2. P004：`request body invalid` / 空图片 / tool-format 错误

本轮状态（2026-07-14）：已补齐空图片显式拒绝；其他已存在的 tool/schema/payload guard 修复通过回归测试。

### 现象

生产证据中仍有请求体结构错误：

- `Image data cannot be empty.`：6 条。
- `Improperly formed request.`：3 条。
- `Bedrock image processing error`：2 条。
- `invalid_request_error`：2 条。
- tool-format debug 采样：
  - 14 条 JSONL 结构化诊断。
  - 13 条 `request_body_invalid`。
  - 1 条 `invalid_tool_use_format`。
  - 全部 `errorReason=REQUEST_BODY_INVALID`。

### 已处理过的同类问题

以下问题当前源码已有处理或已有文档记录：

- 空 / 空白工具 `description` 归一为非空占位。
- `input_schema:null` 入口归一为空 object。
- schema property key 非法时默认可逆清洗并能映射回原始 key。
- tool name 合法化使用 request-local 映射，避免跨会话串数据。
- 坏图 / 伪图增加轻量结构校验，能提前返回更清晰的本地 400。

### 仍需分析的点

- 空图片是否来自当前消息、历史消息、tool result、文件转换，还是中间转换器生成了空 `data`。
- `Improperly formed request` 是缺字段、字段类型不对、content block 顺序错误，还是工具调用 / 工具结果配对问题。
- Bedrock image processing error 是否只来自外部池 / Bedrock 兼容层，还是本地 Kiro upstream 也会返回。
- tool-format 诊断是否已经覆盖所有失败样本；如果没有，需要补充最小化请求结构摘要，而不是记录完整敏感 body。

### 已实现 / 保留方案

优先做低风险入口防护：

- 空图片 `data`：直接本地 400，message 明确为 `Image data cannot be empty.`，普通 image 与 tool_result 内嵌 image 走同一入口。
- 图片 `media_type` 与 base64 结构不匹配：已有轻量结构校验，本地 400。
- content block 类型缺字段 / 字段类型错误：本地 400。
- tool_result 指向不存在的 tool_use：按现有 payload guard 策略处理；如果当前策略已经处理，补齐诊断字段即可。
- tool_use / tool_result 顺序异常：只做可以确定安全的修正；不确定时明确拒绝，不猜测。

开关建议：

- 对明显非法输入（空图片、JSON 类型错误）不需要开关，直接处理。
- 对可能改变语义的修复（重排工具消息、删除孤儿 tool_result、历史工具文本化）继续保留 payload guard 开关和诊断。

### 需要补充的证据

只在 400 / 上游 `REQUEST_BODY_INVALID` / 采样时记录：

- content block 类型计数。
- 空图片数量与位置：current / history / tool result / file。
- tool_use 数量、tool_result 数量、孤儿数量、重复数量。
- schema 清洗数量、tool name 清洗数量。
- 被 payload guard 修改的项目数量。
- 上游 error reason / code / safe message。

### 验收口径 / 本轮验证

- 已知坏请求在本地直接给出明确 400。
- 合法图片、合法工具调用、Claude CLI 工具链不受影响。
- 不把账号、凭据、外部池、调度、token 等内部信息返回给下游。
- usage/debug 能定位是哪类请求结构问题，不需要回看完整敏感请求体。
- 已通过：
  - `cargo check --all-targets`
  - `cargo test empty_ -- --nocapture`（包含普通空 base64、空 data URL、tool_result 内空图片）
  - `cargo test tool_schema_key -- --nocapture`
  - `cargo test tool_name -- --nocapture`

## 3. P005：external pool usage projection 导致 reported usage 异常膨胀

### 现象

`/ha`、`/cc` 样本中出现过“上报输入”异常大：

- `/ha` 样本：`上报输入=317,054`，`cache write=28,779`，`output=1`。
- `/cc` 样本：`上报输入=104,005`，`cache write=5,266`，`output=412`。

这些样本与 schema key 清洗无关，主要与长上下文、历史图片、工具定义和 reported usage 策略有关。

### 当前理解

- 请求体本身很大：历史消息、历史图片、当前工具定义都会推高本地估算输入。
- `max_tokens` 是输出上限，不代表一定输出那么多。
- `上报输入` 不应该在 `/cc`、`/ha` 的 sample 策略下直接展示几十万。
- 没有 cache-read 证据时，不应该把差额塞进 `cache_read_input_tokens`。
- 为保持总量口径，差额更适合进入 cache writer 口径，即 `cache_creation_input_tokens` / `cache_creation_5m_input_tokens`。

### 已确定原则

- 下游展示 input 应按路径策略压低。
- 有 cache-read 证据时，差额可以转入 cache read。
- 没有 cache-read 证据时，`cache_read_input_tokens=0`，差额转入 cache writer。
- 原始本地估算 input 必须保留在诊断字段中，不能丢。
- 页面要清楚区分：
  - 本地估算输入。
  - 返回给下游的展示 input。
  - cache write / cache read。
  - 内部成本输入。

### 需要防止的回归

- 修 usage 问题时不能导致所有 usage case 口径错乱。
- external pool `current_path_policy`、本地 Kiro upstream、`/v1`、`/cc`、`/ha`、`/na` 都要分别覆盖。
- stream 和 non-stream 都要覆盖 final usage。
- 成功、错误、fallback、重试都不能重复计费或漏记录。

### 需要补充的证据

只记录结构化数字，不记录大 body：

- raw upstream usage。
- local estimated usage。
- path reported usage policy。
- final reported usage。
- delta 去向：cache read / cache write / dropped / preserved。
- usage projection mode：pass-through / current-path-policy。
- external pool response 是否有可信 usage。

### 验收口径

- `/cc`、`/ha` 长上下文真实调用 final `input_tokens` 不超过配置上限。
- 无 cache-read 证据时，差额进入 cache writer，不伪造 cache read。
- 有 cache-read 证据时，差额进入 cache read。
- usage record 与 SSE final usage 一致。
- 两套 UI 展示口径一致，避免“上报输入”和“展示输入”混用造成误解。

## 4. P006：大 payload / 工具名映射 / 长上下文压力

### 当前判断

schema key / tool name 映射不是当前长上下文内存压力的主要来源：

- 只对不合法 key 做清洗；合法 key 不清洗、不映射。
- 映射是 request-local，不写 Redis，不跨会话共享，请求结束即释放。
- 映射规模与非法 key 数量相关，通常远小于图片、历史消息和工具定义。
- 不建议把 schema key 映射写 Redis。写 Redis 会引入 TTL、跨实例一致性、清理、性能和串数据风险，但这里并不需要跨请求复用。

主要压力来源：

- 大历史图片。
- 长历史消息。
- 大工具定义。
- payload guard 诊断。
- token estimate。
- usage/detail 持久化。
- 并发下多个大请求同时解析和转发。

### 建议方案

- schema key / tool name 映射继续保持 request-local。
- payload guard 对大历史图片、历史工具结果、重复工具结果继续做可配置处理。
- success 路径不要无条件持久化完整大诊断 JSON。
- 对超大请求只保存摘要和 hash，失败时再保留必要证据。
- 增加资源观测时只做采样，不在主路径同步重计算。

### 需要补充的证据

低成本字段：

- request body bytes。
- history bytes。
- image bytes / image count。
- tools bytes / tool count。
- schema mapping count。
- tool name mapping count。
- payload guard 修改数量。
- usage diagnostic bytes。

采样字段：

- RSS 前后差。
- FD 前后差。
- 请求耗时分段。
- 是否触发 payload guard。

### 验收口径

- 长上下文低并发真实调用不会出现 FD 泄漏。
- RSS 不随请求次数线性上涨。
- schema/tool 映射不跨请求串数据。
- 大 payload 诊断不导致成功路径额外写入巨型 JSON。

## 5. P008：生产 evidence 打包与脱敏策略

### 当前判断

脱敏可以做，但不能脱到无法分析，也不能因为脱敏改变问题方向。

### 建议策略

保留问题分析必需字段：

- request id / error id：可做稳定 hash，但同一 id 在包内应保持可关联。
- 时间窗口。
- endpoint / path。
- requested model / upstream model / alias source。
- HTTP status。
- error type / error source / error reason / safe message。
- route subtype。
- attempt trace。
- latency trace。
- usage 数字字段。
- payload breakdown。
- config 摘要。
- app version / revision / image digest。

必须脱敏或裁剪：

- API key、bearer token、refresh token、access token。
- cookie。
- client secret。
- 原始 Authorization。
- 完整请求正文中的用户文本。
- 外部池完整 URL 中的敏感 query。
- 账号凭据原文。

保留有限原文片段的条件：

- 只保留错误 message 的安全片段。
- 做长度上限。
- 对内部词、token-like 字符串、邮箱、长 base64 做过滤。
- 外部池 raw message 默认不进下游，但可进入本地证据包的脱敏字段。

### 需要避免

- 一次性拉全量容器日志。
- 对生产数据库做重查询、大范围全表扫描或长事务。
- 写生产文件、改生产配置、重启生产服务。
- 把未脱敏证据提交到仓库。

### 验收口径

- 一个问题一个文件夹。
- 同类问题合并，保留 2 到 3 个典型样本即可。
- 每个问题目录包含：
  - `summary.md`
  - `evidence.jsonl` 或裁剪后的结构化证据。
  - `config-snapshot.redacted.json`
  - 必要日志片段。
  - 复现推测和待补证据。
- 打包产物默认放 `tmp/`，不进入 git。

## 6. P009：错误信息下游返回策略

### 需求

不能把所有错误都归一化成同一种模糊提示；下游需要更具体的错误。但外部池不可信，不能把广告、推广、HTML、第三方内部信息或敏感内容原样返回。

### 建议策略

Kiro 官方上游：

- 可透出结构化 JSON 中安全的 `message` / `reason` / `code`。
- 需要先过敏感词和长度过滤。
- 适合透出：
  - `Invalid tool use format.`
  - `Image data cannot be empty.`
  - `Could not process image.`
  - `prompt is too long` 的安全改写版本。

外部池：

- 不透 raw message。
- 返回本系统 public message + error id。
- raw message 只进入内部 usage/debug，且需要脱敏、截断。
- 对可识别类型做归类，例如 model unavailable、prompt too long、rate limited、upstream timeout。

本地调度 / 账号 / 内部错误：

- 继续归一化。
- 不暴露 credential、account、proxy、external pool name、scheduler、lease、capacity snapshot、Redis key、PgSQL detail。

### 需要补充的证据

- upstream error origin：official kiro / external pool / local scheduler / local validation。
- raw error type。
- public error type。
- public message source。
- 是否经过敏感过滤。
- 是否因敏感词命中而降级成归一化 message。

### 验收口径

- 官方上游请求格式类错误对下游可定位。
- 外部池异常不泄漏第三方脏内容。
- 内部调度错误不泄漏实现细节。
- usage/detail 能看到 raw 分类和 public 分类的映射关系。

## 7. P010：`output_tokens` 放大与最终上限

### 需求理解

该策略属于 `reportedUsage` 的 `output_tokens` 改写链路，不应叫“输出后处理”这种含糊名字。它应该集中放在“输出字段改写（output_tokens）”配置区域，并且有明确开关控制是否启用。

计算顺序：

1. 先执行已有四种 output 改写策略：
   - `raw`
   - `preserve`
   - `sample-max`
   - `sample-target`
2. 得到 output 基准值后，如果开启最终输出限制 / 放大策略：
   - 当 output 大于某个阈值时，按百分比放大。
   - 放大后不能超过配置上限。
   - 不直接硬截到最大值，而是在最大值下扣一个可配置随机区间。

### 建议配置名

放在 `reportedUsage.default` 和 `reportedUsage.pathOverrides.<path>`：

- `finalOutputGuardEnabled`：是否启用最终 output guard。
- `outputUpliftMinTokens`：超过多少 output tokens 后开始放大。
- `outputUpliftPercent`：放大百分比。
- `finalOutputMaxTokens`：最终 output 上限；`0` 表示不限制。
- `finalOutputJitterMinTokens`：从上限扣减的 jitter 下限。
- `finalOutputJitterMaxTokens`：从上限扣减的 jitter 上限。

默认值建议：

| 字段 | 默认 |
|---|---:|
| `finalOutputGuardEnabled` | `true` |
| `outputUpliftMinTokens` | `1000` |
| `outputUpliftPercent` | `50` |
| `finalOutputMaxTokens` | `200000` |
| `finalOutputJitterMinTokens` | `5000` |
| `finalOutputJitterMaxTokens` | `12000` |

### UI 要求

- 两套 UI 都要嵌入。
- 位置应在 usage / reported usage 的 output_tokens 改写区域。
- 不使用“输出后处理”作为用户可见标题。
- 本地缓存和 `kiro-rs-tool` 缓存配置不要两列布局，避免之前反复提到的页面问题继续出现。

### 验收口径

- 输出字段改写最终值会返回给下游，并写入 usage record。
- stream final `message_delta.usage.output_tokens` 与落库一致。
- non-stream response usage 与落库一致。
- 百分比放大只在超过阈值时发生。
- 最终上限使用 `max - jitter`，不会稳定撞 200k / 1m。
- jitter 对同一请求稳定，不依赖跨请求全局状态，不引入并发串数据。

## 8. 建议执行顺序

等待裁定的建议顺序：

1. **P005 usage projection**
   先确保 `/cc`、`/ha`、外部池、本地 Kiro 的 usage 口径稳定，否则后续错误、重试和长上下文验证都会被错误 usage 干扰。

2. **P004 request body invalid / image / tool-format**
   这是明确的输入兼容与本地校验问题，能减少上游 400，且容易做真实调用验证。

3. **P003 stream retry**
   需要先确认是否做“响应提交延迟 / 首字节前安全重试”重构。这个风险高于 P004/P005，不能直接硬加换号重试。

4. **P009 error message strategy**
   在 P004/P003 分类更准确后，再统一错误对下游的 public message 策略。

5. **P006 大 payload / 长上下文压力**
   在 usage 和错误分类稳定后做资源压测与诊断瘦身，避免误判。

6. **P010 output token uplift / final cap**
   如果当前实现仍需调整 UI 命名、默认值或配置位置，放在 usage 口径稳定后处理。

7. **P008 evidence packaging / redaction**
   证据采集能力按上述问题补齐，不做大面积无脑采集；采集必须低成本、失败不影响主业务。

## 9. 总验收原则

每个问题完成时都需要满足：

- 有本地真实服务调用验证，不只依赖单元测试。
- 如果涉及 `/cc`，需要覆盖 Claude CLI 或 direct SSE 协议行为。
- 如果涉及 usage，需要覆盖 stream / non-stream、成功 / 错误、落库 / 下游响应一致性。
- 如果涉及 UI，两套 UI 都要改。
- 如果涉及生产证据采集，只能读，不能改生产、不能重启生产、不能做重扫描。
- 证据字段必须服务于定位问题，不增加主路径大对象持久化。

## 10. 本轮发布候选验证记录

口径：P008 生产 evidence 打包与脱敏策略本轮不纳入发布提交；以下记录只覆盖 P001、P002、P003、P004、P005、P006、P009、P010 及相关回归。

### 静态与完整测试

- `git diff --check`：通过。
- `cargo fmt --check`：通过。
- `cargo check --all-targets`：通过。
- `cargo test --all-targets`：通过。
- `cargo test --all-targets --no-default-features`：通过。
- `cargo build --release --locked`：通过。
- `pnpm -C ui build`：通过。
- `pnpm -C admin-ui build`：通过。

### 定向回归

- 外部池模型不可用 / 队列：`model_unavailable`、`external_pool_capacity`、`supported_model_filter` 相关测试通过；默认按“池 + 模型”短冷却，不再因单个不支持模型把整个外部池冷却。
- request body invalid：空 base64 图片、空 data URL 图片、tool_result 内空图片均本地返回明确 400；合法图片、合法工具、schema key、tool name 回归通过。
- usage：`reported_usage`、`usage_projection`、`external_pool_max_input_preflight`、prompt too long public error 回归通过；`/cc`、`/ha` 无 cache-read 证据时差额计入 cache writer，不伪造 cache read。
- schema/tool 映射：只对非法 key/name 做 request-local 映射；合法 key 不映射；stream/non-stream 响应能还原原始 key/name；并发会话不需要 Redis，也避免 TTL/跨实例串数据风险。
- 错误提示：Kiro 官方上游结构化安全错误可透出；外部池、本地调度、账号、队列类错误继续返回脱敏 public message + error id。
- output tokens：输出字段四种策略先执行；之后在“输出字段改写（output_tokens）”区域的最终 guard 中按阈值放大并用 `max - jitter` 限制上限；该值返回给下游并写入 usage record。

### 真实本地服务验证

- 使用临时 release 服务端口 `127.0.0.1:19022` 验证，未触碰本地 live `9022`。
- `/cc/v1/messages` 空图片真实请求返回 HTTP 400，message 为 `Image data cannot be empty. media_type=image/png`。
- `/cc/v1/messages` 工具名与 schema 非法 key 真实流式请求返回工具输入时，`foo-bar`、`中文 key`、`legal_key` 均正确映射回客户端原始 key，未泄漏内部 hash key。
- `/cc/v1/messages` stream usage 真实请求：final usage 与落库一致，示例为 `input_tokens=25`、`cache_read_input_tokens=0`、`cache_creation_input_tokens=9921`、`output_tokens=60`。
- `/ha/v1/messages` stream usage 真实请求：final usage 与落库一致，示例为 `input_tokens=9`、`cache_read_input_tokens=0`、`cache_creation_input_tokens=9037`、`output_tokens=69`。
- Claude Code CLI `2.1.197` 通过临时服务验证：
  - 普通 `stream-json` 请求成功返回 `pong`，usage 非零，无内部调度/凭据/外部池词泄漏。
  - Bash 工具请求成功产生工具调用并返回 `tool-ok`，工具链路未被 schema/tool 映射破坏。
- 低并发资源烟测：3 批、每批 3 并发，混合 `/cc` 和 `/ha`，全部 HTTP 200；FD 从 30/31 附近恢复稳定，RSS 未观察到线性上涨。

### 尚未过度承诺的部分

- P003 首输出前 retry 已实现安全边界和正常流回归，但没有在共享 PgSQL 运行配置上做破坏性故障注入；如果发布后仍复发，再用隔离 DB / fake upstream 补首输出前 idle/read/status 失败注入证据。
- P008 本轮只保留分析要求，不提交 `.codex/skills/kiro-prod-evidence-audit/` 相关文件。
