# 历史问题分析状态总索引与证据链（2026-07-15）

本文把 2026-07-12 至 2026-07-15 对 `kiro.rs` 的主要问题分析、修复状态、证据链、复现/验证入口统一登记。它不是替代各专题文档；各专题文档和 evidence 目录仍是细节来源。本文的职责是回答三个问题：

1. 每个问题当前到底是什么结论；
2. 结论依赖哪些证据，证据在哪里；
3. 要证明修复没有引入新问题，需要怎样复现和验证。

状态口径：

- `已修复并验证`：当前本地工作区已有实现，并且已有本地真实服务或自动化验证记录。
- `已确认待修复`：生产或代码证据足够，方案明确，但当前文档口径下尚未完成实现/回归。
- `已实现待生产观察`：本地已验证，但仍需上线后看 recurrence。
- `待补证据`：已有用户现象或代码风险，但证据不足以定性成已确认故障。

安全口径：本文不保存 SSH 密码、Admin Key、请求 API Key、Kiro API key、refresh token、cookie、Authorization 原文、用户完整 prompt 或未脱敏请求体。

## 1. 证据根目录

### 1.1 生产 usage/error 离线分析包

- [`tmp/analysis-usage-llm-errors/SUMMARY.md`](../../tmp/analysis-usage-llm-errors/SUMMARY.md)：0.0.101 / `737f9f1`，最近 12 小时 50,405 条 usage，281 条非成功记录，8 类根因分类。
- [`tmp/analysis-usage-llm-errors/root-causes/`](../../tmp/analysis-usage-llm-errors/root-causes)：每类根因的 compact usage、完整样本、分析说明。
- [`tmp/analysis-usage-llm-errors/debug/tool-format-debug.compact.jsonl`](../../tmp/analysis-usage-llm-errors/debug/tool-format-debug.compact.jsonl)：tool-format debug 索引。

### 1.2 生产取证包

- [`tmp/prod-evidence/20260713-172403-kiro-rs-2ue-59137/summary/evidence-chain.md`](../../tmp/prod-evidence/20260713-172403-kiro-rs-2ue-59137/summary/evidence-chain.md)：7/13 外部池、模型不可用、request body invalid、usage projection、大 payload 证据链。
- [`tmp/prod-evidence/20260714-101230-kiro-rs-2ue-59137-scheduler-external-pool/summary/usage-summary-scheduler-impact-analysis.md`](../../tmp/prod-evidence/20260714-101230-kiro-rs-2ue-59137-scheduler-external-pool/summary/usage-summary-scheduler-impact-analysis.md)：usage summary 高基数 Redis 查询影响本地调度与外部池 fallback 的完整链路。
- [`tmp/prod-evidence/20260715-125054-152.53.243.159/`](../../tmp/prod-evidence/20260715-125054-152.53.243.159)：升级后旧 slowlog、Redis degraded、本地 transient fallback、usage clear 风险。
- [`tmp/prod-evidence/20260715-165347-152.53.243.159-redis-scheduler-channel/`](../../tmp/prod-evidence/20260715-165347-152.53.243.159-redis-scheduler-channel)：0.0.109 当前版本下 Redis scheduler degraded、retry amplification、session state pressure、channel attribution gap。

### 1.3 主要专题文档

- [`feature/README.md`](../README.md)：LLM 调用异常根因索引。
- [`feature/audits/runtime-usage-error-followup-2026-07-13.md`](runtime-usage-error-followup-2026-07-13.md)：7/13 runtime usage、错误提示、真实调用验证记录。
- [`feature/audits/local-todo-for-confirmation-2026-07-14.md`](local-todo-for-confirmation-2026-07-14.md)：7/14 待裁定/已处理问题、验证记录。
- [`feature/audits/scheduler-production-followups-20260715.md`](scheduler-production-followups-20260715.md)：7/15 生产调度 follow-up。
- [`docs/plantree/plans/runtime-correctness-and-release-gates/history/evidence-index.md`](../../docs/plantree/plans/runtime-correctness-and-release-gates/history/evidence-index.md)：runtime correctness / release gate 历史验证索引。

## 2. 当前问题总览

| 编号 | 问题 | 当前状态 | 主要证据 | 复现/验证状态 |
|---|---|---|---|---|
| A1 | 工具 `description` 空、`input_schema:null` | 已修复并验证 | feature 文档 + usage 根因 01/05 | 单测 + 真实本地服务通过 |
| A2 | schema property key 非法、tool name 映射 | 已修复并验证 | feature 文档 + COR-007 + 源码 | 单测 + stream/non-stream 真实调用通过 |
| A3 | request body invalid / 空图片 / malformed | 已修复部分，仍需生产观察 | 7/12 根因 + 7/14 Todo | 空图片/坏图已真实验证；复杂 malformed 待生产复查 |
| A4 | `Tool results provided` / `<function_results>` 泄漏 | 已实现保护，待更多真实长会话观察 | stream 文档 + transcript sanitizer 源码 | 单测已覆盖；真实 CLI 常规工具链路通过 |
| B1 | `/cc`、`/ha` usage input 异常大 | 已修复并验证 | 7/13 follow-up | 真实 `/cc`/`/ha` 流式调用通过 |
| B2 | cache read/write 模拟与 first miss 口径 | 已修复并验证 | cache 策略文档 + usage 测试 | 单测 + 真实调用通过 |
| B3 | `output_tokens` 放大与最终上限 | 已修复并验证 | 7/13 follow-up + 7/14 Todo | 隔离 DB 真实调用 + 单测通过 |
| B4 | 费用小数展示 6/8 位 | 待补代码级核对 | 用户需求 + UI 代码搜索 | 尚未形成完整专题验证记录 |
| C1 | stream idle/read/status 首输出前重试 | 已实现，待生产观察 | 7/14 Todo | 正常流回归通过；故障注入待补 |
| C2 | `end_turn` vs silent truncation | 观测盲区已修，截断未证实 | feature/10 + CLI JSONL | 真实 CLI/`/cc` 验证字段已落库 |
| C3 | 日文/葡语/中英混用与 prompt steering | 已实现提示词增强，待统计观察 | prompt injection 审计 + feature/10 | 真实 CLI 小样本未复现外语混入 |
| C4 | 错误返回策略 | 已修复并验证 | 7/13 follow-up + 7/14 Todo | 单测 + direct API 验证 |
| D1 | 外部池不支持模型导致队列/冷却异常 | 已修复并验证 | 7/13 evidence P001/P002 + 7/14 Todo | fake external upstream 真实验证 |
| D2 | 本地容量存在仍可能外部池 | 已确认待修复 | 7/15 production follow-up P1 | 待构造本地池 transient exhaustion 复现 |
| D3 | Redis scheduler degraded 导致本地错误/外部池 | 已确认待修复 | 7/14、7/15 evidence | 生产只读取证充分；本地 fault 注入待补 |
| D4 | usage 清空影响诊断和 Redis/PG | cleanup 组 3 次外层通过，完整回归/混沌阻断 | 7/15 production follow-up P3/P004 + F03 cleanup evidence | 36/36 x3 外层通过；full/writer-performance、UI browser 和 chaos 待关闭 |
| D5 | 健康均衡/其他模式账号并发偏斜 | 高度怀疑，待复现 | 7/15 production follow-up P4/P5 | 需要 60 账号/同 session/异 session 压测 |
| D6 | 下游低 RPM 但上游 HTTP attempts 放大 | 已确认放大存在；token refresh process-local 60/8 与 final-attempt focused pass，集群/渠道归因仍未关闭 | 7/15 evidence P002 + [token refresh 专题](../issues/token-refresh-failure-wave-and-cluster-rpm.md) | 需要 per-key channel、refresh cluster/PG/cancellation/load 和冻结候选闭环验证 |
| D7 | Redis 调度热路径 75ms 脆弱与 queue renewal 放大 | finite waiter 周期续租已 focused 修复；整体 Redis chaos/隔离仍未关闭 | 7/15 production follow-up P7 + [queue lease 专题](../issues/dispatch-queue-lease-renewal-rpm-amplification.md) | 需要真实 Redis deadline、延迟/持久化压力、两实例和冻结 fault test |
| E1 | 旧版本升级/启动迁移卡死 | 已修复设计口径，待版本回归 | deployment 文档 + 7/14 evidence | 需要旧版本数据集升级 smoke |
| E2 | 发布/构建/两套 UI gate | 有历史例外，后续需严格 gate | plantree release evidence | 后续发布需完整记录 |
| F1 | AWS Kiro API Key + region 凭据 | 已实现迹象，需补完整验证文档 | 源码 + protocol 文档 | 用户给定 key 的导入/使用测试未在本文证据中复核 |
| F2 | 请求 API Key 作为下游渠道限流实体 | 已确认缺口，待设计实现 | 7/15 production follow-up P6 | 需实现 per-key admission 后验证 |
| G1 | 生产证据采集 skill / 脱敏 | 已有能力，需按问题补采集 | `.codex/skills` + prod evidence | skill yaml 校验缺 PyYAML，打包脚本已跑过 |

### 2.1 已修复 / 已优化 / 已有保护

这些项当前文档口径下已有实现和至少一类验证证据；仍需发布后观察的会在各专题剩余动作中单独说明。

- A1 工具 `description` 空、`input_schema:null`：已修复并验证。
- A2 schema property key 非法、tool name 映射：已修复并验证；合法 key/name 不清洗，不建映射；非法 key/name request-local 可逆映射。
- A3 request body invalid 中的空图片、坏图、伪图：已补本地明确 400 与轻量结构校验；复杂 malformed 仍需继续观察。
- A4 `Tool results provided` / `<function_results>` 泄漏：已加 transcript sanitizer 与 end_turn anomaly 诊断；需要更多长会话样本观察。
- B1 `/cc`、`/ha` usage input 异常大：已修复 input sampling 漏应用，无 cache-read 证据时差额进入 cache writer。
- B2 cache read/write first miss 口径：已修复，不再凭空制造首轮 cache read。
- B3 `output_tokens` 放大与最终上限：已实现并验证，会影响下游 usage 与落库 usage。
- C1 stream idle/read/status 首输出前重试：已实现安全边界和正常流回归；故障注入仍待补。
- C2 `end_turn` vs silent truncation：已补 `sawUpstreamCompleted`、`stopReasonSource` 等观测；静默截断本身未证实。
- C3 语言混用 / prompt steering：已加可配置提示词增强；长期效果待统计观察。
- C4 错误返回策略：已改为官方上游安全 message 可透出，外部池/内部错误继续脱敏。
- D1 外部池不支持模型导致队列/冷却异常：已改为按外部池 + 模型短冷却，并完成 fake upstream 验证。
- E1 旧版本升级/启动迁移卡死：启动迁移设计口径已调整为轻量 schema 补齐，不自动扫描历史 usage；仍需旧版本数据集回归。
- E2 发布/构建/两套 UI gate：已有 release gate 文档和历史例外记录；后续发布必须按 gate 执行。
- G1 生产 evidence skill / 脱敏：已有直接登录只读取证和打包能力；按具体问题继续补采集。

### 2.2 未处理完成 / 待修复 / 待补证据

这些项不能标记为完成；后续实现或验证时必须补证据链。

- B4 费用小数展示 6/8 位：待复核两套 UI 的 usage detail formatter 和页面展示；当前不能标记完成。
- C1 stream 首输出前重试：故障注入验证未补齐；当前只证明正常流未破坏。
- C2 silent truncation：只能说观测盲区已修，不能说静默截断已被证实或已根治。
- C3 语言混用 / prompt steering：小样本真实验证通过，但长期减少幻觉效果待统计。
- C4 模型不可用类官方 400 换号重试：仍需补 retry classifier 与实现验证。
- D2 本地容量存在时仍可能外部池：已确认待修复，需要 local route-state 复查和严格 local-first fallback 逻辑。
- D3 Redis scheduler degraded：已确认待修复，需要调度 Redis 与 usage/dashboard 隔离、分操作超时/退避等改造。
- D4 usage 清空风险：后台分批 cleanup、bounded `UNLINK`、持久审计与 soft/hard rollup 一致性已有三次外层证据（36/36 x3）；same-ID soft-tombstone、1000 external billing 和 in-flight commit guard 均覆盖，full/writer-performance、browser 和生产规模 chaos 未关闭。
- D5 健康均衡/其他模式账号偏斜：高度怀疑，待压测复现和 sticky/load-aware/topK/lease 重选修复。
- D6 下游低 RPM 但上游 HTTP attempts 放大：放大已确认；shared inference budget 不能替代 token refresh 通道。短 TTL 16 caller/30 sends、timeout 32 caller 和旧 invalid-bearer force-refresh fan-out 已独立登记；post-correction process-local 60/8、limit/config/revision 与真实 API/MCP final-attempt zero-refresh 各五轮通过，但 live Redis、cluster/PG CAS/cancellation/load/frozen candidate 继续阻断。详见 [token refresh 专题](../issues/token-refresh-failure-wave-and-cluster-rpm.md)。
- D7 Redis 调度热路径 75ms 脆弱：除 usage/dashboard 共因外，新增确认 finite queue lease 本已覆盖等待期却仍为每 waiter 每 20 秒 renewal。local/external finite renewal 已删除、unlimited 保留，500 guard 与动态 deadline 各五轮通过；真实 Redis 22 秒 score、两实例和联合 chaos 仍待执行。详见 [queue lease 专题](../issues/dispatch-queue-lease-renewal-rpm-amplification.md)。
- E1 旧版本升级：设计口径已改，但还需要 101/102/103 等旧数据集升级 smoke 才能关闭。
- F1 AWS Kiro API Key + region 凭据：源码已有实现迹象，但缺用户给定 key 的导入到使用的完整可引用验证文档。
- F2 请求 API Key 作为下游渠道限流实体：已确认缺口，待设计实现并验证。
- G1 evidence skill：PyYAML 缺失导致 quick validate 未跑通；后续需要补本地校验依赖或替代校验。

## 3. A 类：请求体、工具、schema、工具结果

### A1. 空工具 `description` / `input_schema:null`

状态：已修复并验证。

证据链：

- 生产根因：[`tmp/analysis-usage-llm-errors/SUMMARY.md`](../../tmp/analysis-usage-llm-errors/SUMMARY.md) 中 `01-local_bad_request_tool_use_format` 201 条、`05-request_entry_invalid_json_body` 7 条。
- 专题分析与复现：[`feature/issues/empty-tool-description-400-invalid-tool-use-format.md`](../issues/empty-tool-description-400-invalid-tool-use-format.md)。
- 汇总状态：[`feature/README.md`](../README.md) A 类程序缺陷表。

说明：

- 空 `description` 会被 Kiro/Bedrock 拒绝为 `Invalid tool use format / REQUEST_BODY_INVALID`。
- `input_schema:null` 原本在入口 serde 解析阶段失败，尚未进入 converter 或上游调用。
- 这两类是“客户端工具字段边界值”兼容问题，不是账号、Redis、外部池、长上下文问题。

复现/验证：

- 修复前真实调用：空 `description` HTTP 400；正常 `description` HTTP 200；`input_schema:null` HTTP 400；缺失 `input_schema` HTTP 200。
- 修复后真实调用：空/空白/正常 `description` 均 HTTP 200；`input_schema:null` 和缺失均 HTTP 200。
- 自动化验证见专题文档：`tool_input_schema`、`tool_description`、`tool_use_format_diagnostics`、全量测试、fmt、build。

剩余动作：

- 发布后按 `REQUEST_BODY_INVALID / Invalid tool use format` recurrence 检查是否已回落。
- 如果仍有同类 400，优先看 A2/A3，而不是重复修 A1。

### A2. schema property key 非法与 tool name 可逆映射

状态：已修复并验证。

证据链：

- 专题分析：[`feature/issues/tool-property-key-invalid-400-tool-schema-invalid.md`](../issues/tool-property-key-invalid-400-tool-schema-invalid.md)。
- 目标决策：[`docs/plantree/plans/system-architecture-modernization/decisions/012-tool-definition-compatibility-and-reversible-schema-mapping.md`](../../docs/plantree/plans/system-architecture-modernization/decisions/012-tool-definition-compatibility-and-reversible-schema-mapping.md)。
- 源码实现：[`src/anthropic/tool_schema_keys.rs`](../../src/anthropic/tool_schema_keys.rs)、[`src/anthropic/converter/tools.rs`](../../src/anthropic/converter/tools.rs)、[`src/anthropic/stream.rs`](../../src/anthropic/stream.rs)。

说明：

- 上游实测 schema property key 正则是 `^[a-zA-Z0-9_.-]{1,64}$`。
- 合法 key 不清洗、不建映射。
- 非法 key 默认 `sanitize` 为 `key<16 hex>`，hash 输入包含版本前缀、上游工具名、schema path、原始 key、attempt 序号，避免同工具/多工具/并发请求碰撞。
- 映射是 request-local，不写 Redis，不跨会话共享，避免 TTL、串数据、Redis 往返和状态残留。
- 响应侧会把 `tool_use.input` key 递归映射回客户端原始 key；stream 路径仅在存在 schema map 时缓冲 `input_json_delta`。

复现/验证：

- 真实 non-stream `/v1/messages`：请求 schema 中含 `bad key`，返回 `tool_use.input` 包含原始 `bad key` 和 `valid_key`，不包含内部 hash key。
- 真实 stream `/cc/v1/messages`：SSE 聚合后的 input 为 `{"bad key":"alpha","valid_key":"beta"}`，未泄漏内部 hash key。
- 自动化验证覆盖：默认 sanitize、reject、disabled、自定义正则、hash-only、碰撞规避、多工具隔离、`required`/`dependentRequired`/`dependentSchemas`/legacy `dependencies` 同步改写、`$defs`/`patternProperties` 不误报。

剩余动作：

- 发布后观察 `TOOL_SCHEMA_INVALID` 是否复发。
- 如出现复发，应优先采集 schema 结构摘要（hash、大小、深度、关键字统计），不要记录完整 schema 原文。

### A3. request body invalid / 空图片 / malformed

状态：已修复部分，仍需生产观察。

证据链：

- 根因包：[`tmp/analysis-usage-llm-errors/root-causes/`](../../tmp/analysis-usage-llm-errors/root-causes) 中 `request_body_invalid`、`unsupported_image_format`、`stream_upstream_status_error` 等分类。
- 7/14 Todo：[`feature/audits/local-todo-for-confirmation-2026-07-14.md`](local-todo-for-confirmation-2026-07-14.md) 第 2 节 P004。
- 400 malformed 专题：[`docs/kiro-400-improperly-formed-request-analysis.md`](../../docs/kiro-400-improperly-formed-request-analysis.md)。

说明：

- 空图片 `data`、空 data URL、tool_result 内空图片属于明确非法输入，应在本地直接 400。
- `Improperly formed request` 更宽泛，可能来自 tool schema、tool_use/tool_result 配对、content block 组合、非流式路径差异、历史裁剪后结构不完整。
- 这类问题不能只用“账号测试正常”排除；账号可用只说明凭据能调用普通请求，不能说明某个复杂请求体可被 Kiro 接受。

复现/验证：

- 已验证：`/cc/v1/messages` 空图片真实请求返回 HTTP 400，message 为 `Image data cannot be empty. media_type=image/png`。
- 已验证：坏图/伪图本地轻量结构校验拒绝，合法图片和工具调用不受影响。
- 自动化验证：`cargo test empty_ -- --nocapture`、`cargo test tool_schema_key -- --nocapture`、`cargo test tool_name -- --nocapture` 在 7/14 Todo 中登记为通过。

剩余动作：

- 对仍复发的 `Improperly formed request` 采集脱敏结构摘要：message count、tool count、tool_result count、image/document count、schema depth/size/hash、orphan/duplicate tool result 数量、payload guard 修改数量。
- 不应保存完整请求体、完整工具 schema、图片 base64 或用户 prompt。

### A4. `Tool results provided` / `<function_results>` 泄漏

状态：已实现保护，待更多真实长会话观察。

证据链：

- 流式专题：[`feature/issues/10-stream-end-turn-vs-silent-truncation.md`](../issues/10-stream-end-turn-vs-silent-truncation.md) 第 7/8 节。
- 源码 marker：[`src/anthropic/stream.rs`](../../src/anthropic/stream.rs) 中 `TOOL_CONTEXT_LEAK_MARKERS` 包含 `Tool results provided`、`Tool results:`、`<function_results>`、`</function_results>`、`readHash`/`editHash`/`writeHash`/`bashHash`。
- 过滤器：[`src/anthropic/transcript_sanitizer.rs`](../../src/anthropic/transcript_sanitizer.rs)。

说明：

- 这是“模型把内部工具 transcript 脚手架当作可见正文输出”的问题，不等价于 schema key 映射，也不等价于 Redis 调度问题。
- 当前处理是检测并抑制完整工具 transcript 泄漏块，同时保留真实结构化 `tool_use`。
- 检测要避免误杀普通文本，例如单独出现 `Tool results provided.` 但随后有真实 `tool_use` 的正常工具轮不能被判为异常 end_turn。

复现/验证：

- 单测覆盖：长文本含 marker 且 `end_turn + no tool_use` 命中 `tool_context_leak_text_only_end_turn`；同样 marker 后续有真实 `tool_use` 不误报。
- 单测覆盖：`user Continue` + hash 工具输出泄漏被抑制，真实结构化工具调用仍存在。
- 真实 CLI 回归：Claude Code CLI Bash 工具请求成功产生 `tool_use` 并回传 `tool_result`，最终包含 `tool-ok`，说明 sanitizer 没破坏正常工具链路。

剩余动作：

- 用长会话多轮 Claude Code CLI 采集 `suppressedToolContextLeak*`、`toolContextLeakMarkers`、`endTurnAnomalyReason` 分布。
- 如果用户再次观察到裸 `Tool results provided`，优先查对应 usage latencyTrace 和本地 CLI JSONL。

## 4. B 类：usage、cache、output、费用展示

### B1. `/cc`、`/ha` usage input 异常大

状态：已修复并验证。

证据链：

- 用户给出的 `/ha` 样本：`上报输入=317,054`、`cache write=28,779`、`output=1`，payload breakdown 显示 `totalBytes=3,831,046`、`historyImagesBytes=3,142,500`、`currentToolCount=53`、`historyEntries=556`。
- 用户给出的 `/cc` 样本：`上报输入=104,005`、`cache write=5,266`、`output=412`。
- 专题解释与修复：[`feature/audits/runtime-usage-error-followup-2026-07-13.md`](runtime-usage-error-followup-2026-07-13.md) 第 4 节。
- 7/14 汇总：[`feature/audits/local-todo-for-confirmation-2026-07-14.md`](local-todo-for-confirmation-2026-07-14.md) 第 3 节 P005。

说明：

- 这不是 schema key 清洗导致的 token 膨胀。
- 样本请求体本身很大，主要来自长历史、多图片、多工具定义；`max_tokens` 是输出上限，不代表输出实际很大。
- 旧问题在 reported usage 策略应用：`/cc`、`/ha` 的 `sample-max` 没有始终压低展示 input。为了避免首轮伪造 cache read，旧逻辑在无 cache-read 证据时跳过 input sampling，导致几十万估算 input 被展示成“上报输入”。
- 当前策略：展示 input 按路径策略压低；有 cache-read 证据时差额进入 cache read；无 cache-read 证据时差额进入 cache writer（`cache_creation_input_tokens/cache_creation_5m_input_tokens`），不伪造 read，也不丢差额。

复现/验证：

- 真实 `/ha/v1/messages`：`req_01k222eaTkizhgQyp1gKFG7H`，final usage `input_tokens=16/cache_read=0/cache_creation=36450/output=1`，落库一致。
- 真实 `/cc/v1/messages`：`req_01TnQjvbtN5sSsggukgKpLRW`，final usage `input_tokens=13/cache_read=0/cache_creation=18524/output=1`，落库一致。
- 4 并发真实 smoke：全部 HTTP 200，final `input_tokens <= 96`，`cache_read=0`，`cache_creation > 1000`。
- 7/14 复测记录：`/cc` stream usage `input_tokens=25/cache_read=0/cache_creation=9921/output=60`；`/ha` stream usage `input_tokens=9/cache_read=0/cache_creation=9037/output=69`，均与落库一致。

剩余动作：

- 后续修改 usage 时必须覆盖 `/v1`、`/cc`、`/ha`、`/na`、外部池 current-path-policy、stream/non-stream、成功/错误、落库/下游响应一致性。
- UI 文案应统一“展示输入/上报输入”的含义，避免误导为真实上游 input。

### B2. cache read/write 模拟与 first miss 口径

状态：已修复并验证。

证据链：

- 原问题说明：[`docs/current-cache-strategy-issues-readable-20260701.md`](../../docs/current-cache-strategy-issues-readable-20260701.md)。
- 策略族分析：[`docs/prompt-cache-strategy-family-refactor-analysis-20260701.md`](../../docs/prompt-cache-strategy-family-refactor-analysis-20260701.md)。
- 7/13 修复说明：[`feature/audits/runtime-usage-error-followup-2026-07-13.md`](runtime-usage-error-followup-2026-07-13.md)。

说明：

- first miss 不应该在标准 `usage` 字段里出现 `cache_read_input_tokens`。
- 如果没有真实 read 证据，却把压低 input 后的差额塞进 cache read，下游会误以为本轮读了缓存。
- 当前修复把无 read 证据的差额放进 writer；有 read 证据时才允许把差额并入 read。

复现/验证：

- `cargo test reported_usage -- --nocapture`。
- `cargo test usage_projection -- --nocapture`，记录为 35 个 external pool usage projection 相关测试通过。
- 真实 `/cc`、`/ha` 长上下文调用已验证无 read 证据时 `cache_read=0`，差额进入 `cache_creation`。

剩余动作：

- 如果后续新增本地模拟缓存策略，必须把“actual upstream usage / local estimate / simulated projection / downstream reported usage”四类字段区分清楚。

### B3. `output_tokens` 放大与最终上限

状态：已修复并验证。

证据链：

- 需求与配置：[`feature/audits/runtime-usage-error-followup-2026-07-13.md`](runtime-usage-error-followup-2026-07-13.md) 第 5 节。
- UI/配置定位要求：[`feature/audits/local-todo-for-confirmation-2026-07-14.md`](local-todo-for-confirmation-2026-07-14.md) 第 7 节 P010。
- 相关源码测试：[`src/external_pool/tests.rs`](../../src/external_pool/tests.rs) 中 `usage_projection_final_output_guard_caps_after_external_output_uplift`、`external_pool_billing_uses_output_uplift_as_final_reported_cost` 等。

说明：

- 这不是“输出后处理”这种模糊能力，而是 `reportedUsage` 的 `output_tokens` 改写链路的一部分。
- 执行顺序固定：先执行既有 `raw/preserve/sample-max/sample-target` 四种 output 策略；再按 `outputUpliftMinTokens/outputUpliftPercent` 放大；最后按 `finalOutputMaxTokens - deterministic jitter` 限制最终值。
- 该值会返回给下游，并写入 usage record；不是只影响后台统计。

复现/验证：

- 隔离数据库 `kiro_rs_output_uplift_validation` + 独立 Redis prefix 真实调用：`req_01uxhatXzFF7DCfvLCLKuhEQ`，raw output `73`，放大为 `110`，再按有效上限 `80-10=70` 裁剪，最终响应和落库 output 均为 `70`。
- 自动化覆盖：四种 output 策略之后再放大、严格大于阈值才放大、放大后按 jitter 有效上限裁剪、外部池 projection 后再 cap。

剩余动作：

- 两套 UI 后续调整时必须把字段放在“输出字段改写（output_tokens）”区域，不应恢复“输出后处理”命名。
- 本地缓存和 `kiro-rs-tool` 缓存页面布局要求保持单列或清晰分组，不恢复之前用户反复指出的两列布局问题。

### B4. 费用小数展示 6/8 位

状态：待补代码级核对。

证据链：

- 用户需求：usage 详情里的“多少刀计费”展示 6 位或 8 位小数；统计等汇总页面不必要。
- 当前代码搜索显示至少 `admin-ui/src/components/usage-dashboard-panel.tsx` 的 dashboard 汇总费用格式是 `number >= 1 ? 2 : 6`，这属于统计页，不一定是用户指定的 usage 详情页。

说明：

- 该项需要区分 usage detail 与 dashboard/summary：用户明确说统计等页面不必要。
- 文档当前不能标记为已完成，因为本文没有复核两套 UI 的 usage detail 费用 formatter，也没有截图/真实页面验证。

复现/验证要求：

- 找到两套 UI usage detail 中估算费用、原始计费、Kiro 计量相关 formatter。
- 构造小费用记录，页面打开后确认 detail 展示 6 或 8 位小数；dashboard 汇总不被强制改成高精度。
- 如果使用 API 返回数值，不应只做前端四舍五入而导致复制/导出与展示不一致；需要明确“展示格式”还是“API 字符串格式”。

## 5. C 类：stream、Claude Code CLI、错误提示、语言/prompt

### C1. stream idle/read/status 首输出前重试

状态：已实现，待生产观察；故障注入仍需补。

证据链：

- 生产根因：[`tmp/analysis-usage-llm-errors/SUMMARY.md`](../../tmp/analysis-usage-llm-errors/SUMMARY.md) 中 `02-stream_upstream_idle_timeout` 41 条、`06-stream_upstream_status_error` 5 条、`07-stream_internal_read_error` 2 条。
- 7/14 方案与验证：[`feature/audits/local-todo-for-confirmation-2026-07-14.md`](local-todo-for-confirmation-2026-07-14.md) 第 1 节 P003。

说明：

- 自动重试必须受“下游尚未收到任何 SSE 字节/业务事件”约束。已经发出 `message_start`、可见文本、thinking、tool_use、ping 等之后不能换号重试，否则会产生重复事件、重复工具调用、usage 拼接错误。
- 当前实现延迟 initial SSE，在首输出前遇到 idle/read/status 失败时可安全重试；一旦提交下游字节就不再自动重试。

复现/验证：

- 已通过：正常 direct SSE / Claude CLI 流式回归，事件顺序不被破坏。
- 已记录：`streamRetryAttempts` / `streamRetryReasons` 进入 usage。
- 尚未完成：隔离 DB / fake upstream 下的首输出前 idle、read error、status error 故障注入。

剩余动作：

- 构造 fake upstream：首输出前 idle timeout、首输出前 stream read error、首输出前 2xx JSON 错误体，分别验证换号/重试次数/最终 usage。
- 构造已提交下游事件后的 read error，验证不重试，只返回 SSE error/记录失败。

### C2. `end_turn` vs silent truncation

状态：观测盲区已修；“静默截断”本身未证实。

证据链：

- 专题文档：[`feature/issues/10-stream-end-turn-vs-silent-truncation.md`](../issues/10-stream-end-turn-vs-silent-truncation.md)。
- 相关问题 09：[`feature/issues/09-intent-preamble-end-turn-no-tool-use.md`](../issues/09-intent-preamble-end-turn-no-tool-use.md)。
- 源码：[`src/anthropic/stream.rs`](../../src/anthropic/stream.rs) 的 `stopReasonSource`、`sawUpstreamCompleted`、`suspectedIntentPreambleEndTurn`、tool context leak diagnostics。

说明：

- Kiro 上游不直接给 Anthropic `end_turn`；代理的 `end_turn` 是“流结束 + 无 tool_use/max_tokens/context 信号”时的本地推断。
- 修复前 usage 中 `success/end_turn/completed` 无法证明上游真的发过完成标志。
- 2026-07-14 真实 `/cc` 与 Claude CLI 验证显示，成功轮可以落为 `sawUpstreamCompleted=false`、`stopReasonSource=local_inferred_end_turn/tool_use`。这证明观测盲区存在，但不能单独证明静默截断。

复现/验证：

- 真实 `/cc/v1/messages` 语言采样 6 次，成功轮均为中文、`stop_reason=end_turn`。
- 真实 Claude Code CLI 简单回答、工具写文件、重复工具场景，多条 usage 记录包含 `local_inferred_end_turn/tool_use`。
- 单测覆盖有 `COMPLETED` 与无 `COMPLETED` 的 EOF，断言 `sawUpstreamCompleted/stopReasonSource` 不改变下游 SSE。

剩余动作：

- 继续采集长会话样本，记录 EOF 前最后若干上游事件类型；仅有 `sawUpstreamCompleted=false` 仍不足以判定截断。
- 如果要把“无 completed 的 EOF”改成异常或触发重试，必须先证明 H2 确实存在并定义安全边界。

### C3. 日文/葡语/中英混用与 prompt steering

状态：已实现提示词增强，待统计观察。

证据链：

- 用户样本：`続けて本体を追記する。`、`让me等一会儿`、葡语/日语/中英混用片段。
- 本地 JSONL 复核：[`feature/issues/10-stream-end-turn-vs-silent-truncation.md`](../issues/10-stream-end-turn-vs-silent-truncation.md) 第 1.2、6.5、7 节。
- 注入点审计：[`feature/plans/prompt-injection-inventory-and-centralization-plan-20260715.md`](../plans/prompt-injection-inventory-and-centralization-plan-20260715.md)。
- 源码：[`src/anthropic/prompt_steering.rs`](../../src/anthropic/prompt_steering.rs)、[`src/model/config.rs`](../../src/model/config.rs)。

说明：

- 用户目标不是禁止正常中文夹杂英文术语，而是减少错误的语言混用，例如“让me看一下”。
- 真实复核中，日文例子不是 silent truncation：有 `requestId`，`stop_reason=tool_use`，下一条就是 `Edit` 工具调用。
- 当前方案走可配置 prompt steering：语言约束、任务质量提示、工具调用行为提示集中处理，避免无差别污染所有请求。

复现/验证：

- 2026-07-14 真实 `/cc` 语言采样：成功 4 次均中文输出，未出现日文/韩文/俄文/阿语/泰文。
- 真实 Claude CLI 简单/工具/重复工具：成功样本未出现上述外语混入。

剩余动作：

- 需要更大样本的 Claude Code CLI 多轮测试；单次小样本只能说明没有立即破坏，不足以证明长期减少幻觉。
- prompt 变更不应记录进 usage 原文；只记录是否启用、版本、长度摘要即可。

### C4. 错误返回策略

状态：已修复并验证。

证据链：

- 7/13 错误策略：[`feature/audits/runtime-usage-error-followup-2026-07-13.md`](runtime-usage-error-followup-2026-07-13.md) 第 3 节。
- 7/14 P009：[`feature/audits/local-todo-for-confirmation-2026-07-14.md`](local-todo-for-confirmation-2026-07-14.md) 第 6 节。
- 外部池 public error 测试：[`src/external_pool/tests.rs`](../../src/external_pool/tests.rs)。

说明：

- Kiro 官方上游结构化错误可以透出安全 message/reason/code，但必须去掉 `kiro` 相关品牌/内部字样，以及 credential/token/api key/scheduler/external pool 等敏感词。
- 外部池不可信，可能返回广告、推广、HTML、第三方内部信息；不能原样透给下游。
- 本地调度/账号/队列/内部错误继续归一化，不泄露内部资源状态。

复现/验证：

- 单测：`official_kiro_upstream_400_message_is_exposed_without_internal_prefix`、`malformed_upstream_error_exposes_safe_official_message`、外部池 prompt too long public message。
- 真实 bad image 调用返回明确本地 400，未暴露账号/凭据/外部池/调度内部词。
- 无效模型 direct API 返回 public error，包含定位用 request/error id，不含内部敏感词。

剩余动作：

- 对 `The requested model is not available for this endpoint` 这类官方上游 400，需要按模型不可用/账号不支持进行可配置换号重试，但只在能确认是账号/模型能力问题而非请求结构错误时重试。
- 需补一张 retry classifier 表：可重试、不可重试、只换号、只同号、外部池禁止透出。

## 6. D 类：外部池、本地调度、Redis、账号分布、渠道放大

### D1. 外部池不支持模型导致队列/冷却异常

状态：已修复并验证。

证据链：

- 7/13 证据目录：[`tmp/prod-evidence/20260713-172403-kiro-rs-2ue-59137/problems/P001-external-pool-dispatch-saturation/problem.md`](../../tmp/prod-evidence/20260713-172403-kiro-rs-2ue-59137/problems/P001-external-pool-dispatch-saturation/problem.md)。
- 不支持模型完整清单：[`tmp/prod-evidence/20260713-172403-kiro-rs-2ue-59137/problems/P002-external-pool-model-unavailable-milus/evidence/model-unavailable-complete-list.md`](../../tmp/prod-evidence/20260713-172403-kiro-rs-2ue-59137/problems/P002-external-pool-model-unavailable-milus/evidence/model-unavailable-complete-list.md)。
- 7/14 Todo P001/P002：[`feature/audits/local-todo-for-confirmation-2026-07-14.md`](local-todo-for-confirmation-2026-07-14.md) 第 0 节。

说明：

- 问题不在“把 RPM 限制改成并发限制”。外部池 `maxConcurrentRequests` 是并发，不是 RPM。
- 队列会满的原因是请求进入外部池 fallback 后，目标池因模型不可用/冷却/选择器排除无法正常消费；即使外部池 RPM 配到 3000，队列仍可能被不可消费请求堆满。
- 修复方向是 `model_unavailable` 默认按“外部池 + 模型”粒度短冷却，而不是冷却整个池。

复现/验证：

- fake external upstream：第一次请求命中 fake upstream 并返回 `model_not_found`；第二次相同模型没有再次打到 fake upstream；未退化为 `external_pool_queue_full`；usage 均归类 `errorType=model_unavailable`。
- Admin API 新配置读写与恢复通过，两套 UI build 通过。

剩余动作：

- 根据完整不支持模型清单配置 alias/映射或 supported models。
- 发布后检查 `external_pool_queue_full` 和 `model_unavailable` 是否回落。

### D2. 本地容量存在时仍可能外部池

状态：已实现聚焦保护；完整回归、UI 合同和混沌验证仍阻断发布。

证据链：

- [`feature/audits/scheduler-production-followups-20260715.md`](scheduler-production-followups-20260715.md) P1。
- 7/15 evidence：[`tmp/prod-evidence/20260715-125054-152.53.243.159/problems/P002-local-transient-fallback-before-pool-exhausted/problem.md`](../../tmp/prod-evidence/20260715-125054-152.53.243.159/problems/P002-local-transient-fallback-before-pool-exhausted/problem.md)。

说明：

- 已确认路径不是旧的 Redis degraded preflight，而是本地请求尝试到 `credentialRetryMaxAttempts` 上限后，错误被归类为 `local_transient_exhausted`，再因为 `fallbackOnLocalTransientExhausted=true` 进入外部池。
- 当前 fallback 只检查外部池是否可用，没有重新证明本地池已经不可调度。
- 用户期望是 local-first：本地账号有容量、不全冷却、不全禁用、不全模型不兼容时，不应因为前 N 个账号瞬态失败就打外部池。

复现/验证要求：

- 构造 60 本地账号、每账号有容量、`credentialRetryMaxAttempts=6`，前 6 个账号返回 500，其他账号可成功。
- 期望：本地 route state 仍 Ready 时不得 external fallback；要么继续尝试本地，要么返回本地瞬态错误，但不能外部池。
- 验证 usage 中 route subtype 不出现 `external_fallback_after_local_attempts`，同时记录本地 route-state 快照。

剩余动作：

- 把“本地尝试预算耗尽”和“本地池耗尽”拆开。
- 在 `local_transient_exhausted` fallback 前重新计算当前模型 local route state。
- 增加严格 `fallbackOnlyWhenLocalPoolUnavailable` 或等价配置。

### D3. Redis scheduler degraded 导致本地错误或误路由

状态：已确认待修复。

证据链：

- 7/14 完整链路：[`tmp/prod-evidence/20260714-101230-kiro-rs-2ue-59137-scheduler-external-pool/summary/usage-summary-scheduler-impact-analysis.md`](../../tmp/prod-evidence/20260714-101230-kiro-rs-2ue-59137-scheduler-external-pool/summary/usage-summary-scheduler-impact-analysis.md)。
- 7/15 当前版本证据：[`tmp/prod-evidence/20260715-165347-152.53.243.159-redis-scheduler-channel/problems/P001-redis-scheduler-degraded/problem.md`](../../tmp/prod-evidence/20260715-165347-152.53.243.159-redis-scheduler-channel/problems/P001-redis-scheduler-degraded/problem.md)。
- 调度 follow-up：[`feature/audits/scheduler-production-followups-20260715.md`](scheduler-production-followups-20260715.md) P2/P7。

说明：

- 0.0.109 当前版本仍能出现 Redis scheduler degraded：生产样本 `globalInFlight=63..66`，总容量约 625，`queueDepth=0`、`sampledAccounts=[]`、`rejectedAccountCount=0`、`waitableAccountCount=0`。
- 这不是账号容量耗尽，而是 Redis 调度协调热路径超过 75ms 后进入进程级退避。
- `fallbackOnSchedulerRedisDegraded=false` 能避免这类错误直接打外部池，但不能恢复本地服务能力；结果是本地 429/no-fallback。
- 旧版 usage high-cache 大 HMGET 是明确证据，但 7/15 当前 app 容器启动后，旧 slowlog 时间早于启动，不能再用它证明当前版本仍在执行同一条大 HMGET。当前仍存在 key/cardinality、RDB save、session key 和 scheduler hot path 压力。

复现/验证要求：

- 本地/测试环境注入 Redis 延迟：session binding 读写、dispatch lease、release soft cleanup 分别超过阈值。
- 验证：账号容量充足时，不能因为非 lease 关键操作（例如 session binding）使整进程长时间停摆。
- 验证：usage/dashboard 查询、usage cleanup 不触发 scheduler Redis degraded。

剩余动作：

- 调度 Redis 与 usage/dashboard Redis 分离或至少分连接池/前缀/DB。
- `SCHEDULER_REDIS_HOT_OP_TIMEOUT=75ms` 配置化，并按 operation 分类。
- session binding 超时降级为非 sticky，不应等价于 dispatch lease 不可用。
- 评估单实例保守本地内存准入 fallback，多实例默认关闭。

### D4. usage 清空影响诊断和 Redis/PG

状态：已确认待修复。

证据链：

- [`feature/audits/scheduler-production-followups-20260715.md`](scheduler-production-followups-20260715.md) P3。
- 7/15 evidence：[`tmp/prod-evidence/20260715-125054-152.53.243.159/problems/P004-usage-clear-operational-risk/problem.md`](../../tmp/prod-evidence/20260715-125054-152.53.243.159/problems/P004-usage-clear-operational-risk/problem.md)。
- Admin 路由：[`src/admin/router.rs`](../../src/admin/router.rs) `/usage-records/clear`。

说明：

- 清 usage 不只是“清页面展示”，会删除诊断证据、范围内累计统计和费用贡献，并可能对 Redis/PG 造成压力。
- 当前实现已改为持久后台小批任务、PostgreSQL 同事务 rollup 负增量和 Redis bounded invalidation；same-ID soft-tombstone、in-flight commit 和 UI 源码合同已统一，cleanup 组 36/36 x3 外层通过；剩余风险是完整套件/writer performance、长期 PostgreSQL fallback 与生产规模 Redis/scheduler 共因。

复现/验证要求：

- 验证 Admin 请求只入队任务，不同步执行大范围删除。
- Redis 删除用 `UNLINK` 或小批量让出调度；高基数 usage key 清理不触发 scheduler degraded。
- 清理前后保留审计记录：范围、预估条数、batch、pause、状态、取消结果。
- soft cleanup 对范围内 detail/summary/Dashboard/cost/credential/cache/duration 只扣一次，hard cleanup 不双扣；soft tombstone 存在期间 same-ID 不复活和 in-flight commit guard 已随 cleanup 组三次外层通过，full/writer-performance 待补。

剩余动作：

- UI 文案已明确“会删除明细、诊断证据和对应累计统计/费用贡献，且可能有 Redis/PG 压力”；仍需最终 browser gate 验证确认文本和交互。
- 清理后如果用户还要排查历史问题，应优先保留导出的 evidence 包，而不是直接清空。

### D5. 健康均衡/其他模式账号并发偏斜

状态：高度怀疑，待复现。

证据链：

- [`feature/audits/scheduler-production-followups-20260715.md`](scheduler-production-followups-20260715.md) P4/P5。
- 7/15 evidence：[`tmp/prod-evidence/20260715-165347-152.53.243.159-redis-scheduler-channel/problems/P003-scheduler-session-state-pressure/problem.md`](../../tmp/prod-evidence/20260715-165347-152.53.243.159-redis-scheduler-channel/problems/P003-scheduler-session-state-pressure/problem.md)。

说明：

- 可能共因包括：sticky 会话优先于所有模式、`schedulerTopK=3` 对 60 账号过小、单账号 lease 竞争失败后普通等待模式直接排队而不重选、固定 tie-break、latency 权重压过负载权重、warmup 分组。
- 这不是单独 `health_balanced` 的问题；sticky 和重试在进入算法前就可能改变分布。

复现/验证要求：

- 60 账号、每账号 10 并发、无冷却、不同 session 并发：验证无 sticky 时不应少数账号满 10 而大量账号 0/1。
- 同 session 并发：验证 `load_aware` sticky 绑定账号超过阈值时临时扩散。
- 模拟 Redis 状态延迟/单账号 lease 竞争失败：普通 `WaitForCapacity` 应先排除失败账号并重选，而不是直接进入队列。
- 记录峰值 in-flight 分布，不只看最终 selection count。

剩余动作：

- 实现 `stickyMode=strict|load_aware|disabled` 与阈值。
- 扩大/自适应 topK，同分随机，最低负载桶优先。
- 区分单账号满、全局满、Redis degraded、队列满，只有全候选不可调度才排队。

### D6. 下游低 RPM 但上游 attempts 放大 / channel 归因缺失

状态：已确认放大存在；channel 归因缺失已确认。

证据链：

- [`feature/audits/scheduler-production-followups-20260715.md`](scheduler-production-followups-20260715.md) P6。
- 7/15 evidence：[`tmp/prod-evidence/20260715-165347-152.53.243.159-redis-scheduler-channel/problems/P002-retry-amplification-channel-attribution/problem.md`](../../tmp/prod-evidence/20260715-165347-152.53.243.159-redis-scheduler-channel/problems/P002-retry-amplification-channel-attribution/problem.md)。
- raw evidence：[`tmp/prod-evidence/20260715-165347-152.53.243.159-redis-scheduler-channel/raw/pg-channel-and-model-diagnostics-v2.txt`](../../tmp/prod-evidence/20260715-165347-152.53.243.159-redis-scheduler-channel/raw/pg-channel-and-model-diagnostics-v2.txt)。

说明：

- 一个下游请求可以产生多次本地 credential attempts。例如 `req_01VvA1SGFSwGVjDKReyPz4Nu` 一个请求 13 次本地尝试，前 12 次 429/500，最终成功；`req_01xhMNkUuwf9Pkys8dUCfNp6` 一个流式请求 8 次尝试。
- 下游“20 并发/RPM 不高”不等于服务内上游 attempts 低：短失败、客户端重连、首输出前重试、payload/cachePoint retry、external fallback 都会放大。
- “系统内部 RPM 高”还可能是 Redis queue renewal 等非 HTTP 操作；它与本节的 inference/refresh/profile HTTP attempts 必须分通道记录，不能合并成一个模糊 RPM。finite queue renewal 的独立修复见 [queue lease 专题](../issues/dispatch-queue-lease-renewal-rpm-amplification.md)。
- 当前 Request API Key 只是鉴权 hash set，不是 channel 实体；usage 没有 request key id/hash/channel name，无法精确归因到某个下游渠道。

复现/验证要求：

- 实现 per-key channel 后，给某 key 配 20 并发/RPM，超过限制应在入口直接 429，不消耗本地账号槽位。
- 构造上游 429 storm，验证单请求最多消耗全局 retry budget，不会把 60 个账号全部打一遍。
- Dashboard 同时展示 downstream request RPM 和 upstream credential-attempt RPM。

剩余动作：

- 请求 API Key 升级为 channel：`id/name/hash/enabled/rpm/maxConcurrent/modelRules/routeRules`。
- 鉴权后写入 request extension 和 UsageRecord，只保存不可逆标识。
- 增加 Redis per-channel admission 与单请求全局 retry budget。

### D7. Redis 调度热路径 75ms 脆弱

状态：已确认待修复。

证据链：

- 另一台机器分析结论已汇入：[`feature/audits/scheduler-production-followups-20260715.md`](scheduler-production-followups-20260715.md) P7。
- 当前机器只读取证：[`tmp/prod-evidence/20260715-165347-152.53.243.159-redis-scheduler-channel/problems/P001-redis-scheduler-degraded/problem.md`](../../tmp/prod-evidence/20260715-165347-152.53.243.159-redis-scheduler-channel/problems/P001-redis-scheduler-degraded/problem.md)。

说明：

- 触发条件可以是 Redis IO、RDB save、AOF rewrite、usage/dashboard 高基数读写、session key 数量、scheduler key 数量、容器 CPU 抢占等。
- 程序侧脆弱点是调度、sticky、RPM、usage/dashboard 共用 Redis 压力面，且 75ms 超时后进入进程级退避。
- 默认本地 finite queue 旧逻辑还会让每个 waiter 每 20 秒 renewal；500 waiter 约形成 25 Redis ops/s。当前 dirty tree 已改为有限等待一次 TTL、无限等待才 renewal，但真实 Redis/冻结负载尚未关闭。
- 只把 75ms 调大不是根修；只开启 `fallbackOnSchedulerRedisDegraded=true` 也不是根修，会把错误成本转给外部池。

复现/验证要求：

- Redis chaos：延迟 100/200/500ms、RDB save、high-cardinality usage key、session binding 读写失败。
- 验证本地账号有容量时不会因非关键 Redis 操作导致整进程拒绝本地调度。
- 验证多实例下不允许不安全本地 fallback；单实例 fallback 必须显式配置并限流。

剩余动作：

- 拆 scheduler Redis / usage Redis。
- 分操作超时、局部退避、低频探测恢复。
- 热路径 Lua 降复杂度，把过期 lease 清理和 selection window 大范围维护移到后台或 rolling bucket。

## 7. E 类：升级、迁移、发布、两套 UI

### E1. 旧版本升级/启动迁移卡死

状态：已修复设计口径，待版本回归。

证据链：

- 用户线上日志：启动时报 `column "revision" does not exist`，应用容器反复重启；另有启动迁移扫描 7.4GB `usage_records` 卡住。
- 部署文档已更新口径：[`docs/ai-docker-compose-deployment.md`](../../docs/ai-docker-compose-deployment.md) 中 `postgres.migrateOnStart` 说明为轻量 schema 补齐和小表修复，不自动扫描历史 `usage_records`。
- 7/14 evidence 提到旧 Redis 高基数 key 与启动升级不能全量迁移/重建。

说明：

- 正确策略是启动迁移只做轻量表/列补齐、兼容旧数据；历史 usage 回填、rollup compression、索引补齐等重任务必须是手动 maintenance job 或后台分批任务。
- 升级不能因为旧 usage 大表而阻塞应用启动；否则 `docker compose pull && docker compose up -d` 会让服务不可用。

复现/验证要求：

- 准备旧版本数据快照：缺 `revision` 字段、已有大 usage_records、大 Redis usage summary key。
- 从 101/102/103 等典型版本升级到当前版本，启动阶段只补齐 schema，不扫描全量 usage，不超过固定启动时长阈值。
- maintenance 命令可在低峰期独立运行，支持进度、batch、statement_timeout、失败重试。

剩余动作：

- 建立“旧版本数据集升级 smoke”固定脚本，至少覆盖缺列、旧 runtime_config、旧 usage 表、Redis 高基数 summary。

### E2. 发布/构建/两套 UI gate

状态：有历史例外，后续必须严格 gate。

证据链：

- 发布例外：[`docs/plantree/plans/runtime-correctness-and-release-gates/history/release-exception-v0.0.102.md`](../../docs/plantree/plans/runtime-correctness-and-release-gates/history/release-exception-v0.0.102.md)。
- release gate：[`docs/plantree/baseline/test-and-release-gates.md`](../../docs/plantree/baseline/test-and-release-gates.md)。
- 7/13/7/14 验证记录：[`feature/audits/runtime-usage-error-followup-2026-07-13.md`](runtime-usage-error-followup-2026-07-13.md)、[`feature/audits/local-todo-for-confirmation-2026-07-14.md`](local-todo-for-confirmation-2026-07-14.md)。

说明：

- 102 有明确一次性 release exception，不得推广到后续版本。
- 后续发布应记录 exact source/tag/version、fmt/diff/check/test/release build、两套 UI build、真实本地服务验证、Docker/image/action 结果。
- 如果发布失败后重推同一版本，不应无意义递增版本号；但必须确保 tag/commit/image/action 状态可追溯，不能混淆失败产物和成功产物。

复现/验证要求：

- `cargo fmt --check`、`git diff --check`。
- `cargo check --all-targets`、`cargo test --all-targets`、`cargo test --all-targets --no-default-features`。
- `pnpm -C ui build`、`pnpm -C admin-ui build`。
- `cargo build --release --locked`，并确认两套 UI dist 是嵌入输入。
- 临时本地服务真实 `/cc`/`/ha`/`/v1` 调用，Claude Code CLI 多轮工具场景。
- GitHub Action / image digest / tag 对齐记录。

## 8. F 类：凭据/API Key/channel

### F1. AWS Kiro API Key + region 凭据

状态：已实现迹象，需补完整验证文档。

证据链：

- 协议文档：[`docs/kiro-upstream-protocol-refactor-analysis-and-test-plan.md`](../../docs/kiro-upstream-protocol-refactor-analysis-and-test-plan.md)。
- 源码搜索显示当前已有 `authMethod=api_key`、`kiroApiKey`、CLI endpoint、API Key model discovery、API Key 不刷新 token 等实现：[`src/kiro/endpoint/cli.rs`](../../src/kiro/endpoint/cli.rs)、[`src/kiro/token_manager/manager.rs`](../../src/kiro/token_manager/manager.rs)、[`src/admin/service.rs`](../../src/admin/service.rs)。
- 测试中有 API Key 示例：[`src/admin/service_tests.rs`](../../src/admin/service_tests.rs)。

说明：

- 用户给过一组 `ksk_...|eu-central-1` 形式 key/region，要求作为一种凭证账号导入并从导入到使用真实验证。
- 本文当前不能声明该组 key 已完整验证，因为没有在本索引可引用的证据中看到导入、测试、真实调用、cleanup 的完整记录。

复现/验证要求：

- Admin 新增 API Key 凭据：`authMethod=api_key`、`kiroApiKey=<key>`、`apiRegion=eu-central-1` 或等价字段。
- 验证凭据测试按钮走指定账号直连，但要另做业务请求验证，证明正常调度也能选择该账号。
- API Key 凭据不应进入 refresh token 路径，不应伪造 profileArn。
- 真实 `/v1` 或 `/cc` 调用命中该凭据，usage attempt chain 能看到该 credential id，最终成功或明确返回模型/权限错误。

剩余动作：

- 补一份 `docs/kiro-api-key-credential-validation-YYYYMMDD.md` 或追加到 protocol 文档，记录不含密钥原文的导入/调用证据。

### F2. 请求 API Key 作为下游渠道限流实体

状态：已确认缺口，待设计实现。

证据链：

- [`feature/audits/scheduler-production-followups-20260715.md`](scheduler-production-followups-20260715.md) P6。
- 当前 auth 代码：[`src/anthropic/middleware.rs`](../../src/anthropic/middleware.rs)、[`src/common/auth.rs`](../../src/common/auth.rs)（如文件存在），当前 `RequestApiKeyStore` 只是认证索引。
- Admin 当前 key 管理：[`src/admin/service.rs`](../../src/admin/service.rs) `create_request_api_key` / `update_request_api_key`。

说明：

- 这是和“API Key 凭据账号”不同的概念。F1 是上游 Kiro 凭据；F2 是调用本服务的下游请求 Key。
- 当前请求 Key 没有 per-key RPM/并发，也没有 usage channel 归因，无法防止单个下游渠道把压力放大到本地账号池。

复现/验证要求：

- 新增 channel key A 限 20 并发/RPM，channel key B 不受 A 影响。
- A 超限时入口 429，不创建本地 credential attempt，不占本地调度槽位。
- usage 按 channel 聚合，能查到 downstream requests、upstream attempts、retry amplification ratio。

## 9. G 类：生产 evidence skill、脱敏、取证行为

### G1. 生产证据采集与脱敏

状态：已有能力，需按问题补采集；不能把 skill 当作一次性脚本。

证据链：

- skill 文件：[`./.codex/skills/kiro-prod-evidence-audit/SKILL.md`](../../.codex/skills/kiro-prod-evidence-audit/SKILL.md)。
- 打包脚本：[`./.codex/skills/kiro-prod-evidence-audit/scripts/package_evidence.py`](../../.codex/skills/kiro-prod-evidence-audit/scripts/package_evidence.py)。
- 已产出 evidence 包：`tmp/prod-evidence/*`。

说明：

- 用户已明确允许在提供机器信息时直接登录取证，但操作必须只读，不能影响现网。
- 日志不等于容器日志；需要结合业务落库 usage、Redis 状态、PgSQL schema/runtime_config、诊断文件、应用日志、compose/image label。
- 不能一次性拉全量日志、不能 `KEYS *`、不能全表无界扫描、不能写生产 Redis/PG、不能重启服务、不能清理数据。

复现/验证：

- 7/15 evidence 包已按问题拆分目录，并生成 redacted tar.gz。
- packaging script 已跑过；但 quick validate 因本地缺 PyYAML 报 `ModuleNotFoundError: No module named 'yaml'`，这只是本地校验依赖缺失，不代表打包脚本本身失败。

剩余动作：

- 每次取证应先记录目的和窗口，再做小范围只读查询；问题目录保留 2-3 个典型样本即可。
- 脱敏不能破坏分析：request id/error id 可 hash 但包内稳定；保留时间、endpoint、model、route、status、safe message、attempt trace、latency trace、usage 数字、payload breakdown、config 摘要、version/revision。

## 10. 统一复现/回归矩阵

后续每次修复如果触及对应区域，应至少跑下列矩阵中相关项。不能把“单元测试通过”当成“真实调用通过”。

| 区域 | 最小验证 | 真实验证 | 生产观察 |
|---|---|---|---|
| 工具/schema | `tool_schema_key`、`tool_name`、`tool_description`、`tool_input_schema` | `/v1` non-stream + `/cc` stream 工具调用，确认原始 key/name 映射回来 | `TOOL_SCHEMA_INVALID`、`REQUEST_BODY_INVALID` 复发率 |
| tool_result / transcript | transcript sanitizer 单测、converter pairing 单测 | Claude Code CLI Bash/MCP 工具多轮，确认 tool_use/tool_result 正常 | `Tool results provided`、`function_results` marker 诊断 |
| 图片 / request body invalid | 空图/坏图/合法图单测 | `/cc` 空图 400、合法图 200 或正常上游结果 | `Image data cannot be empty`、`IMAGE_FORMAT_UNSUPPORTED` |
| usage input/cache | `reported_usage`、`usage_projection` | `/cc`、`/ha` 长上下文 stream，final usage 与落库一致 | 高 input 样本、cache read/write 分布 |
| output uplift/cap | output policy/cap 单测 | 隔离 DB/Redis prefix 真实调用，响应与落库 output 一致 | 费用和 output 分布异常 |
| stream retry | retry eligibility 单测/fake upstream | 首输出前 idle/read/status 故障注入；正常 Claude CLI 多轮 | `streamRetryAttempts`、timeout/error recurrence |
| error message | official/external/local classifier 单测 | bad model/bad image/prompt too long direct API | 下游错误是否可定位且无内部词泄漏 |
| external pool | model unavailable/cooldown/queue 单测 | fake external upstream model_not_found | `external_pool_queue_full`、`model_unavailable` |
| local-first fallback | route state/fallback 单测 | 多账号 fake upstream 前 N 个 transient，后续账号可用 | 本地有容量时 external fallback 比例 |
| Redis scheduler | Redis storage/token_manager/fault tests | Redis 延迟/持久化压力 chaos，本地账号有容量不退避风暴 | `Redis 调度协调状态不可用` 分钟峰值 |
| account skew | scheduler distribution tests | 60 账号同/异 session 并发，记录峰值 in-flight | 账号并发槽位分布、sticky 命中率 |
| channel admission | per-key auth/admission tests | key A/B 限流并发真实请求 | channel RPM、upstream attempt ratio |
| release | fmt/diff/check/test/build/UI build | 临时 release 服务 `/v1`/`/cc`/`/ha` + Claude CLI | GitHub Action、image label、revision/version |

## 11. 当前明确不能再混淆的点

- schema key 清洗与 `/cc`、`/ha` usage input 异常不是同一个问题。
- 页面测试按钮通过，只证明指定 credential 的 token/上游调用能成功；不证明正常业务调度一定会选到该 credential。
- 本地账号未打满却外部池/本地错误，可能是 Redis scheduler degraded、local transient fallback、retry amplification、sticky/skew 等多条链路，不应强行揉成一个根因。
- 下游声称低 RPM/20 并发，不能替代服务端 per-key admission 证据；当前服务缺少 channel 归因，必须先补能力。
- `success/end_turn/completed` 不能单独证明上游真完成；但 `sawUpstreamCompleted=false` 也不能单独证明静默截断。
- usage 清空会破坏后续取证；生产排查前优先打 evidence 包，不应先清。
- 发布失败后是否重推同一版本是 release 管理问题；无论是否递增版本，都必须有 source/tag/image/action 的可追溯证据。
