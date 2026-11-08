# cctest 评分规则推断

更新时间：2026-11-08（补充 Round 84）。以下结论按基线结果和生产抓包推断；带“待证实”的内容必须通过开发实例新抓包确认。

## Round 73–76 最新矩阵

| 轮次 | 模型 | 总分 | stream | WebSearch | signature | 关键变量 |
|---|---|---:|---:|---:|---:|---|
| 73 | claude-opus-4-8 | 68 | 5 | 0 | 0 | 模型条件错误，attribution 也为 0，不用于代码回归 |
| 74 | claude-opus-5-5 | 82 | 5 | 0 | 5 | 正确模型基线 |
| 75 | claude-opus-5-5 | 82 | 5 | 0 | 5 | thinking 起始块增加空 `signature` |
| 76 | claude-opus-5-5 | 82 | 5 | 0 | 5 | 完整抓包；13/13 真实签名逐请求一致 |

Round 75 的空 `signature` 是官方协议修复，不能因分数不变而回滚。Round 76 已排除真实 signature 被代理改写；`signature_proto=5` 继续归类为私有通道/抽样候选。

## 当前最高分与稳定性

正确模型条件下当前最高分为 **82/110**。Round 56、58–62、64–66、69、71、72、74–76 多次达到 82；Round 57、68 等轮次因结构化输出采样或上游条件落到 73。距离同一代码连续三次 `110/110` 仍有 28 分，尚未进入 release 冻结门禁。

## 证据保留规则

原始抓包采用追加保留：证据未固定前不清理；证据固定后仅清理重复展开副本。唯一 `events.jsonl`、`capture.zip`、`manifest.json` 和能恢复字段的结果文件必须保留。生产 59138 原始抓包不删除、不移动、不压缩覆盖。

Round 84 固定了提示词非触发边界：开启 tune 路由的 `thinkingOutputPolicy` 后，Admin preview 仍为 `not_triggered`，因为 Opus 5 系列自动跳过该项；完整检测仍为 `82/110`。这轮不能作为该策略对评分无影响的有效 A/B。提示词开关必须和公开 SSE 字段、Kiro opaque 字段分开归因；不能因为开关存在，就把 `stream_structure`、`websearch` 或 `signature_proto` 失败直接归因于注入。

Round 84 的 EOF 统计是 `stream_structure` 当前最具体的待证实差异：21 个上游流结束没有显式 `COMPLETED`，但 22/22 下游流均有完整生命周期，且评分仍为 5/10。该差异可能被私有检查器抽样，也可能只是 Kiro 常态；没有失败请求 ID 或 429/500 后异常链之前，不把本地 `stopReasonSource` 改成伪造的上游完成状态。

## Round 85–86：WebSearch block 边界与字段矩阵

Round 85 将引用文本 block 的合并范围收窄到“两个相邻且都没有 citations 的 text block”。13 个定向测试通过，完整检测为 `82/110`，`websearch=0` 不变。

Round 86 启动完整 capture 后，WebSearch 请求的公开事件序列闭合，且 block 结构为：

```text
server_tool_use(index=0)
web_search_tool_result(index=1)
text(index=2..11)
```

10 个带引用的 text block 各自拥有独立 `citations_delta` 和 text delta，末尾还有 1 个无引用 text block。`server_tool_use` / result 的 caller、ID 配对、usage 和 `message_stop` 均符合公开协议。详细脱敏矩阵见 `analysis/round-86-websearch-fields.json`。

该轮 `websearch=0`，但 `output_config` 从上一轮 10 变为 0；所以总分从 82 降到 73 不能作为 WebSearch block 修改的因果证据。

Round 86 仍观察到两个未解决的公开形状差异：`encrypted_content` 是 MCP snippet 明文，`encrypted_index` 是可逆 Base64(`number:url`)。Anthropic 官方文档要求这两个值在多轮中作为 opaque 字段原样回传；当前 capture 没有第二轮原样回传，也没有完整 fixture 或生成算法。Round 77/78 的本地伪 protobuf 外形 A/B 均保持 `websearch=0`，因此不再随机伪造字段。

## Round 87：同形状复核与 output_config 解耦

Round 87 在同一份工作树上重新启动完整抓包，得到 `82/110`。WebSearch 请求 `req_01EDHQKQFKnuEkcSmkZ61UNQ` 的公开生命周期与 Round 86 相同：`server_tool_use(index=0)`、`input_json_delta`、`web_search_tool_result(index=1)`、10 个结果、引用 delta、最终 usage 和 `message_stop` 均存在；MCP/API 均为 200。该轮 `output_config=10`，而 Round 86 为 0，三个长期失败项没有共变。由此把 73/82 总分变化继续归类为结构化输出抽样或上游语义差异，不能把它当作 WebSearch 修复收益。

Round 87 的 result block 仍显示 10 个 `encrypted_content` 明文 snippet（112–302 字符），citation 的 `encrypted_index` 仍可直接解码为 `number:url`；没有 provider-issued opaque 字段，也没有多轮原样回传。该证据与 Round 86 一致，不能支持修改 opaque 字段、重试状态机、EOF 终止状态或提示词注入。脱敏字段矩阵见 `analysis/round-87-websearch-fields.json`，完整记录见 `rounds/round-87.md`。

## CCTest 结果接口可见性

Round 84 的任务 ID `cc3904d6-46d5-4f7a-bf90-a18306497273` 通过官方结果接口重新读取。接口只返回 `scores`、`stepStatus`、`streamChannel`、`tokenAudit`、`attribution` 等汇总字段，不返回失败请求 ID、单个事件判定、签名验证错误或 WebSearch 字段差异。因此当前无法从 CCTest 服务端把 `stream_structure=5` 的失败样本直接映射到本地 requestId；后续必须依靠本地抓包和可复现上游异常建立关联，不能把平台汇总分数当作具体根因。

## Round 55–60 评分与账号调度边界

| 轮次 | 结果 | 关键分项 | 上游/客户端证据 | 评分用途 |
|---|---:|---|---|---|
| Round 55 初次 | 不计协议分 | 账号池耗尽，tag 阶段 503 | 未完成完整检测 | 仅记为容量前置故障 |
| Round 55 恢复后 | 见结果文件 | WebSearch 内部 200 空体→本地 502 | credential 2530，local_error_no_fallback | 与 websearch=0 分离 |
| Round 56 | 82/110 | stream=5、websearch=0、signature=5、output=10 | 25/25 上游 200，22/22 完整流 | 空响应账号已排除后仍复现 |
| Round 57 | 73/110 | 仅 output_config 变为 0 | 同代码重复轮次 | output_config 是概率命中项 |
| Round 58 | 82/110 | 四项与 Round 56 相同 | 22/22 完整流，丢弃 0 | 公开生命周期完整 |
| Round 59 | 82/110 | 四项与 Round 58 相同 | 25/25 上游 200 | 多账号优先层生效 |
| Round 60 | 82/110 | 四项与 Round 59 相同 | 22/22 完整流，49 个 block 配对 | sticky 主账号切换成功 |

当前完整协议检测区间是 73–82/110，稳定底线为 73/110，已确认最高分为 82/110。目标仍是同一份代码连续三次 110/110；当前尚未达到收尾门禁。

### 账号池故障不计入协议评分

Round 55 初次运行因开发账号池耗尽在 tag 阶段返回 503，未完成 12 项检测；该结果不能当作 stream_structure、websearch 或 signature_proto 的失败样本。恢复账号后，Round 56–60 都形成完整评分轮次。

Round 55 恢复轮还出现 credential 2530 的上游 HTTP 200 空响应体，应用返回 502。禁用该账号后 Round 56–60 没有再出现相同 502，但 websearch 仍为 0。由此把“账号可用性/空响应”归入运行前置条件，把“WebSearch 私有判分”保留为独立问题。

### output_config 的统计口径

Round 51–56、58–60 多次得到 output_config=10；Round 57 在代码不变时为 0。因此文档使用“高相关、尚未确定性保证”，不写“已稳定通过”。当前条件式 identity/platform guidance 保留，因为它显著提高了 claude_code 语义响应的命中率；普通 schema 不追加这段 guidance，响应字段也不由代理硬编码。

### sticky session 与额度消耗

Round 58–60 的主账号约承载 18 个请求，外围请求才分散到其他账号。CCTest 同一轮复用 conversation，sticky 绑定使轮内请求集中到一个账号；Fast 优先层只能在新轮次/新 conversation 切换主账号，不能在不改变会话语义的前提下强行均匀分摊。

测试前后应记录：开发账号 creditRemaining 快照、Fast 优先层、request_summary.credentialId 分布、stickyBound 和 conversation 标识。额度归零账号只在 59139 关闭 Fast；59138 只读，不调整生产账号。

### 59138 与 59139 版本边界

59138 当前仍为镜像 0.0.239 / revision 30edc00c12431371e22498298715adc7bea539fd；仓库 HEAD 为 2e625e52、tag v0.0.240。开发测试运行在 59139 debug 增量热更新二进制，不能把 Round 55–60 的分数描述成 59138 已部署行为。release 编译仅在连续三次 110/110 后执行。

## Round 05/06 分数变化

| 轮次 | 关键变化 | 总分 | 结论 |
|---|---|---:|---|
| Round 05 | 仅开启 code execution runtime 配置 | 64/110 | 开关不能替代原生工具输入 schema |
| Round 06 | 补齐 `code_execution` / `bash_code_execution` schema | 73/110 | `server_tool` 由 0 升为 10，执行结果返回码为 0 |

Round 06 抓包 `logs/llm-capture/capture-20261106T111016Z-7d98710b/events.jsonl` 已按 requestId 重建 WebSearch 和 output_config 事件；两项公开结构均完整，但私有评分仍为 0，评分器还有未公开的额外条件。

## 基线结果

来源：`results/baseline-9afa06c2.json`。

| 检测项 | 分数 | 当前结论 |
|---|---:|---|
| tag_check | 10 | 未退化保护项 |
| stream_structure | 5 | 采样结构不稳定，重点关联首输出前重试、block 顺序、终止事件 |
| server_tool | 0 | server tool 响应结构或配对不符合检查器预期 |
| non_stream | 10 | 非流基本 JSON 结构通过 |
| doc_recognition | 10 | 未退化保护项 |
| knowledge | 10 | 未退化保护项 |
| output_config | 0 | Round 02 的响应 text 已是合法 schema JSON，失败条件尚未定位 |
| image_recognition | 10 | 未退化保护项 |
| websearch | 0 | WebSearch 请求链路存在，但结构化结果未满足评分条件；Round 08/09 的最小形状变化仍未命中 |
| signature_proto | 5 | 部分请求通过，31 次首次上游 400 是主要不稳定来源 |
| model_attribution | 10 | 未退化保护项 |

## stream_structure

### 已知证据

生产抓包共有 `1216` 个 `message_start`、`2282` 个 `content_block_start`、`2216` 个 `content_block_stop`、`1137` 个 `message_delta` 和 `1137` 个 `message_stop`，但抓包包含多轮普通请求，不能直接映射 cctest 采样分母。

一次上游 400/429/500 后重试会改变首 token 延迟，并可能在状态机中留下旧错误或旧 block。I-02/I-03 是该分数只有 5 的优先候选根因。

### 判定条件推断

每个流式请求大概率至少检查：

- HTTP 200 与 `text/event-stream`；
- 首事件 `message_start`；
- block start/delta/stop 的 index 和类型配对；
- `message_delta.stop_reason` 存在且值与内容一致；
- `message_stop` 恰好一个；
- error 事件不与完整成功事件混排；
- usage 在最终事件可读。

这些条件中哪些是硬失败、哪些是采样扣分，尚未从 cctest 私有实现完全证实。

## signature_proto

### 已知证据

31 次上游 400 的错误是 `THINKING_SIGNATURE_INVALID`。第一次请求携带 synthetic signature，retry 删除历史 reasoningContent 后 200。说明当前实现能在失败后恢复，但不满足“首次请求直接符合上游协议”的稳定性要求。

### 判定条件推断

检查器可能发起多轮带 thinking 历史的请求，检查：

- thinking block 的 `signature` 非空；
- 历史签名与 thinking 文本重放后仍可被代理接受；
- 签名不被错误地转成 Kiro 原生签名；
- stream/non-stream 均不因签名导致 400；
- thinking block 与 tool_use/tool_result 交错时索引和顺序稳定。

当前 5/10 表明部分短请求通过、包含历史或工具历史的采样失败。

## server_tool 与 websearch

### 官方协议形状作为实现参照

本项目已有历史证据文档 `feature/issues/protocol-conversion-audit-20260928/13-websearch-emulation-incomplete.md` 和 `docs/analysis/websearch-tool-error-stability-fix-20260927.md`：成功 native WebSearch 应返回配对的 `server_tool_use` 与 `web_search_tool_result`，流式最终应有 `message_delta(stop_reason=end_turn)` 与 `message_stop`。这些文件是本项目已有的协议审计记录，具体 cctest 判定仍需新抓包确认。

### 判定条件推断

- `server_tool_use.id` 应为 `srvtoolu_` 前缀；
- `web_search_tool_result.tool_use_id` 必须等于 server tool id；
- `input.query` 非空且是对象；
- 结果 `content` 应为 `web_search_result[]` 或规范错误块；
- stream 中 block index 连续，start/delta/stop 配对；
- `message_delta.stop_reason` 在搜索成功后为 `end_turn`；
- final `message_stop` 必须存在；
- 失败搜索也应保持 HTTP 200 和结构化 `web_search_tool_result_error`，不能只返回 502。

### 待证实项

抓包中目前只确认 `route=websearch` 和 MCP 响应数量，没有完成每个 requestId 的 SSE 事件重建。不能据此断定是 ID、block 顺序、usage、错误状态或请求工具版本导致 0 分。

Round 06 已完成该事件重建：`req_01mjMPZXgmqaYg761dfGNjJa` 的 ID 配对、结果非空、引用、usage、`end_turn` 和 `message_stop` 均存在，但 `websearch=0`。因此“事件未配对”这一候选已排除；剩余候选是文本 block 拆分、历史回传、请求模式或私有字段检查。

### Round 07–09 的排除与新候选

| 假设 | 检验 | 结果 |
|---|---|---|
| 多个最终 text block 导致评分失败 | Round 08 合并成一个 text block，WebSearch 仍 0 | 不是唯一根因 |
| 首事件没有 query | Round 09 `content_block_start.server_tool_use.input.query` 有值，WebSearch 仍 0 | 不是充分原因 |
| server tool 输入 delta 是必须事件 | Round 09 在完整 start input 后又发相同 `input_json_delta` | 构成重复输入；Round 10 已移除此 delta，待测 |
| server tool/result 缺少 caller | Round 09 两块均无 `caller`；Anthropic 官方 TypeScript SDK 当前响应类型将二者定义为必填，direct caller 为 `{"type":"direct"}` | Round 11 已添加字段，但 WebSearch 检测在内部调用 HTTP 502 中断；评分效果未验证 |

Round 09 的 cctest WebSearch 序列仍是完整成功流：非空结果、匹配 id、citation、final usage、`end_turn` 与 `message_stop`。所以分数 0 更可能来自未覆盖的 block 字段或子请求回放条件，而不是 MCP 不可达。

官方 SDK 参照：Anthropic TypeScript SDK `messages.ts`，commit `590e9494f0214710614c1d6b929b07666f1e3080`，访问时间 2026-11-06 UTC：<https://github.com/anthropics/anthropic-sdk-typescript/blob/590e9494f0214710614c1d6b929b07666f1e3080/src/resources/messages/messages.ts>。该文件定义 `ServerToolUseBlock.caller` 和 `WebSearchToolResultBlock.caller` 为必填字段。此处用于公开协议比对，不将其误写为 cctest 的已知判分规则。

Round 11 结果文件 `results/20261106T124108-round-11-server-tool-caller.json` 在 WebSearch 步骤 HTTP 502 结束，不能拿来给 caller 字段实验计分。抓包中三个内部调用有 Kiro HTTP 200 响应头，但没有关联 `upstream_frame`，且原实现漏记了 WebSearch 完整 body；因此上一版证据无法证明 200 后的 body 是否为空或不可解码。已补齐内部 body 抓包和 EOF 尾部检查，等下一轮拿到有效 WebSearch 流再分析评分。

Round 12 完整评分仍为 73/110，`websearch=0`。对应请求 `req_01g5PprU3gSqnNwKaL1TdEPa` 的两次内部调用均成功 HTTP 200；流包含 10 个结果、10 个 citations、匹配 server tool/result、`web_search_requests=1`、`end_turn` 和 `message_stop`，因此不是上游不可用导致的部分评分。`caller=direct` 已同时出现在两个 block，但该轮还包含 server tool 输入 lifecycle 变化，不能将字段效果单独归因。

Round 12 抓包证实 server tool 的 SSE start 携带完整 `input.query`，之后没有任何 `input_json_delta`。官方 WebSearch 流式资料要求 server tool 输入通过 `input_json_delta` 传输；因此 Round 10 的“start 携带完整 input 并移除 delta”修复仍留下一个协议差异。下一轮采用官方 lifecycle：start 的 `input={}`，随后唯一一条带 query 的 `input_json_delta`；HTTP 非流 message block 仍完整携带 input。该变化是有官方来源支持的最小对照实验。

### Round 13 对照结果

Round 13 采用了上述生命周期：`server_tool_use` 的 start 使用 `input={}`，随后发送唯一一条 `input_json_delta`，并保留 `caller={"type":"direct"}`。请求 `req_01gRFXbBxBGKdNcDMXtLE6ZY` 的 10 个搜索结果、10 个引用、`end_turn`、`message_stop` 和最终 usage 均完整；结果仍为 `websearch=0`，总分仍为 `73/110`（`results/20261106T134159-round-13-input-json-delta.json`）。

这次单变量实验排除了“server tool 输入缺少 delta”作为唯一评分根因。下一步候选集中到结果字段：`encrypted_content` 当前为明文 snippet，`encrypted_index` 当前为 Base64(`number:url`)。Anthropic 官方 SDK（commit `590e9494f0214710614c1d6b929b07666f1e3080`，访问时间 2026-11-06 UTC）只规定二者为字符串字段，没有公开生成算法；因此目前只能记录为私有检查候选，不能把自定义 Base64 当作官方等价实现。应先做非流/流字段集合对照，再决定是否改动。

## 非流与保护项

`non_stream=10` 说明基本 message JSON、content、usage 在当前样本满足检查器。`tag_check`、`doc_recognition`、`knowledge`、`image_recognition`、`model_attribution` 均为回归保护项，后续修复不得牺牲这些字段。`output_config` 曾在生产基线为 10，但 HEAD 两轮均为 0；Round 02 已证明至少一个 cctest 请求的 JSON 响应内容本身符合 schema，因此该项不能再按“响应没有结构化”解释。

## output_config

Round 02 请求 `req_01mYVjzpTMyY4wdNULamUFu8` 的输入特征为 `stream=true`、`output_config.format.type=json_schema`，schema 的必需键为 `identity_platform` 与 `desc`，且 `additionalProperties=false`。客户端事件序列完整，text 拼接后 JSON 可解析，顶层键集合精确匹配，最终 `stop_reason=end_turn`。

当前可证实的实现路径是：请求转换追加内部 `structured_output` 工具；Kiro 返回 `toolUseEvent`；响应转换把工具 input 变为单一 text block。待证实的评分条件包括非流/流模式要求、JSON schema 严格类型与枚举验证、以及 cctest 是否只读取特定响应形状。后续先做最小矩阵复现，再决定是否修改代码。

Round 06 的 cctest 请求 `req_019m2NKVf1kFCujVKoRpKHSW` 已验证：流式、单 text block、严格 JSON、枚举值和 `end_turn/message_stop` 全部满足；评分仍为 0。下一步不改 JSON 文本生成器，先用最小请求矩阵确认隐藏检查到底选择流式还是非流式、`output_config.format` 还是旧版 `output_format`，以及是否检查 thinking/usage 语义。

## 证据与置信度

| 结论 | 证据 | 置信度 |
|---|---|---|
| 首次 synthetic signature 被上游拒绝 | 31 个 400，错误正文完全一致，脱敏结构显示固定 marker | 高 |
| retry 删除 reasoningContent 后可恢复 | 同一 request 的 `thinking_signature_retry` 后 200 | 高 |
| stream_structure 与 retry 状态机有关 | 429/500 后仍 `stream_error`，但事件级序列尚未完成 | 中 |
| server_tool 0 是结构不匹配 | Round 05/06 代码执行 schema 改动后由 0 升 10 | 高 |
| websearch 0 的具体字段根因 | Round 07–09 请求级映射完整；发现重复 input delta 和缺少官方 SDK 必需 caller 字段，尚未做对照试验 | 中 |
| output_config 0 是 JSON 内容不合法 | Round 02 已对响应 text 做 JSON 解析并验证顶层键 | 低，已排除 |
| output_config 0 与流式内部工具重写有关 | cctest 请求及抓包均显示 stream=true、内部 toolUseEvent 后转 text | 中 |

### Round 16 账号池控制与评分边界

Round 14/15 的 502 不是 Caddy 链路问题：目标入口和 59139 本地探针均正常，失败请求在本地应用内耗尽 10 次上游账号尝试，所有尝试为 402/403，未产生 SSE。Round 16 清理后至少有 23 个成功请求，结果恢复为完整 73/110；因此账号池可用性是评分前置条件，不属于四个协议项的评分差异。

## Round 19–21 固定模型复核

### 结果矩阵

| 轮次 | 模型 | 总分 | stream | signature | WebSearch | output_config | 上游状态 |
|---|---|---:|---:|---:|---:|---:|---|
| 19 | claude-opus-4-8 | 68 | 5 | 0 | 0 | 10 | 记录中无协议级 HTTP 错误结论 |
| 20 | claude-opus-5-5 | 73 | 5 | 5 | 0 | 0 | 27/27 HTTP 200 |
| 21.1 | claude-opus-5-5 | 73 | 5 | 5 | 0 | 0 | 75/75 HTTP 200 |
| 21.2 | claude-opus-5-5 | 73 | 5 | 5 | 0 | 0 | 75/75 HTTP 200 |
| 21.3 | claude-opus-5-5 | 82 | 5 | 5 | 0 | 10 | 75/75 HTTP 200 |

这组对照把账号池、Caddy、Cloudflare 和上游 HTTP 状态从 `output_config` 的主要候选中排除；该项需要请求级响应矩阵，而不是只看整轮总分。

### stream_structure 的新证据

Round 21 普通工具续接请求均具备 `message_start`、连续 `content_block_start/delta/stop`、`message_delta`、`message_stop`，并且请求摘要为 `local_success`。然而 thinking 片段还会出现空 `thinking_delta` 与 `estimated_tokens`。因此当前 5/10 更可能来自 thinking 事件的私有严格检查，或来自未被本轮抽样覆盖的重试/历史组合。下一步按字段单变量实验，不把公开事件顺序完整等同于 cctest 通过条件。

### signature_proto 的新证据

Round 21 历史 assistant thinking block 中有 Kiro 原生 opaque signature；服务端下游会通过 `signature_delta` 原样发送。另有旧版本生成的 synthetic marker，需要在进入 Kiro 原生 `reasoningContent` 前清理。固定 Opus 5.5 的 `signature_proto=5` 表明只删除所有 signature 会破坏通过样本，正确方向是区分 native 与 synthetic，并分别验证历史重放。

### WebSearch 的新证据

三个内部 WebSearch 请求均有 MCP 200、结果数量 10、引用和最终终止事件；server tool/result 的公开生命周期已完整。当前评分仍为 0，剩余可验证差异集中在 `encrypted_content`、`encrypted_index` 的 opaque 语义，以及 cctest 是否验证搜索请求的历史闭环。没有官方算法前不生成随机值。

### output_config 的新证据

Round 21 三次固定模型、相同代码，结果为 `0/0/10`。每个上游请求都携带 `additionalModelRequestFields.output_config`，结构化输出路径使用内部 `structured_output` 工具。评分器至少不是简单检查“是否能解析 JSON”；需要把每个结构化输出请求的最终 response、`content` block 类型、schema 约束、`stop_reason`、是否发生工具回放列成矩阵，定位通过轮与失败轮的具体差异。

### Round 25–27 thinking 字段对照

| 轮次 | 单变量 | 总分 | stream_structure | signature_proto |
|---|---|---:|---:|---:|
| 25 | 无估算时增加 `estimated_tokens: null` | 73 | 5 | 5 |
| 26 | 删除过滤器生成的普通空 delta | 73 | 5 | 5 |
| 27 | beta 下过滤器所有空帧统一发送数值或 null | 73 | 5 | 5 |

Round 27 抓包 `logs/llm-capture/capture-20261106T191423Z-9de0cf6e/events.jsonl` 说明过滤器生成的 thinking delta 已符合官方 SDK 对 beta 字段的公开形状：估算到期时为非负量化数值，无估算时为 `null`。但仍有少量不带字段的空 delta，它们来自 Claude Code noop keepalive，而不是过滤器。三轮评分完全不变，说明该字段差异不是 cctest `stream_structure`/`signature_proto` 的充分判定条件。

Round 28 抓包 `logs/llm-capture/capture-20261106T192849Z-9ce45cb9/events.jsonl` 已把 noop keepalive 也改为带 `estimated_tokens: null`。统计为数值 16 次、null 8 次、缺失 0 次；总分仍 `73/110`，`stream_structure=5`、`signature_proto=5`、`websearch=0`、`output_config=0` 完全不变。因此可以停止该字段方向的微调，继续查请求级状态机和各失败项的语义差异。

当前采样项应拆成至少两个独立变量：

1. 过滤器处理上游 reasoning 的公开 delta；
2. handler 在长间隔期间插入的 noop keepalive delta。

在没有新的官方 fixture 之前，不应删除签名、合并所有空 delta，或把 `estimated_tokens` 猜测值写入 usage；usage 仍以最终 `usage.output_tokens` 为权威。

## Round 29–33 请求级证据

### 流结构

Round 33 抓包 `logs/llm-capture/capture-20261106T205417Z-4984b328/events.jsonl` 有 23 个完整流。逐请求检查结果：

- `message_start`、`message_delta`、`message_stop` 各 23 次；
- block index 连续，所有 delta 都指向已 start 的 index；
- start/stop 数量一致，没有重复 start；
- 每个 thinking delta 都有 `estimated_tokens`，最终有 `signature_delta`；
- WebSearch 请求的 server tool/result 配对、caller、query delta、citations 和终止事件完整。

因此 `stream_structure=5` 不能由当前抓包发现的公开事件缺失解释。评分器还可能检查 beta 字段组合、重试前后历史、某种模型特定事件，或只抽取部分请求。Round 25–28 已证明把 `estimated_tokens` 改成数值/null/补齐 noop keepalive 都不改变分数；继续猜字段没有证据收益。

### signature_proto

Round 33 的上游 signature 由 Kiro 原样返回，代理没有重编码；Round 28 的上游 signature 使用另一种模型格式。Round 32–33 的 synthetic 路径没有让 `signature_proto` 上升，证明“非空字符串”不是私有验证条件。当前最可靠的判定是：cctest 需要能被其 verifier 识别的真实 Kiro opaque/protobuf signature；代理无法离线生成该值。

### WebSearch

Round 33 的公开事件序列满足 Anthropic 文档要求，`websearch` 仍为 0。结果 block 使用明文 `encrypted_content`，citation 使用自定义 Base64 `encrypted_index`。官方 SDK 类型没有生成算法，故这里只能把它们标为高置信度候选，不能做无依据改码。`caller`、`input_json_delta`、连续 index、usage 和 `message_stop` 均已通过请求级检查。

### output_config

Round 29–31 均使用相同结构化输出重写路径；Round 29 为 10 分，Round 30/31 为 10 分，说明该项在这组样本中没有出现结构缺失。历史 Round 21 的 `0/0/10` 仍表明它是采样项，不能把单个成功 JSON 响应推广为稳定通过。当前保留内部 `structured_output` 工具路径，不再为分数随机改写 JSON 文本。

## 当前可执行结论

1. 保留真实 Kiro signature；synthetic marker 不宣称等价，也不继续调 Base64/哈希编码。
2. 保留 WebSearch 的公开生命周期和 caller；opaque 字段等待真实官方 fixture，再做单变量替换。
3. 把 `stream_structure` 继续当作“私有/采样差异未定位”，不删除公开 beta 字段。
4. 账号池只在 59139 维护；不足时从 59138 只读复制账号与绑定代理，禁止任何生产写操作。

## Round 34–36 固定模型复测

固定 `claude-opus-5-5` 后，Round 34、35、36（连续两次）均为 `73/110`，分项稳定为：

| 项 | 得分 |
|---|---:|
| 流结构 | 5 |
| 签名 | 5 |
| WebSearch | 0 |
| 结构化输出 | 0 |
| server tool | 10 |

Round 36 抓包包含 44 个完整流，`message_start/message_delta/message_stop` 都为 44 次；其中一次上游 500 在首输出前重试，最终请求状态为 `success/local_success`。因此 `stream_structure=5` 不是简单的上游 HTTP 失败或终止事件缺失。

Round 35 的两个 `output_config` 请求给出新边界：

- `output_config.effort=medium` + adaptive thinking：自然语言最终答案，带 `output_tokens_details.thinking_tokens`；
- `output_config.format=json_schema`：单一 text block，拼接后 JSON 合法，schema 顶层键正确。

两者公开形状都可读，但 `output_config` 仍为 0；Round 21 的固定模型矩阵曾出现 `0/0/10`。因此该分项至少还检查模型行为、effort 与 schema 组合或隐藏请求条件，不能直接把“JSON 可解析”当作通过标准。

## Round 37–39 单变量结果

| 轮次 | 单变量 | 总分 | 流结构 | 签名 | WebSearch | 结构化输出 |
|---|---|---:|---:|---:|---:|---:|
| 37 | usage 增加 `output_tokens_details:null`、`server_tool_use:null` | 73 | 5 | 5 | 0 | 0 |
| 38 | 非流 WebSearch block 增加 `caller=direct` | 73 | 5 | 5 | 0 | 0 |
| 39 | 结构化输出保活改用 `ping` | 73 | 5 | 5 | 0 | 0 |

Round 39 已修复公开协议错误：text block 内不再出现 `input_json_delta`。该错误不是 `output_config=0` 的充分根因。公开 usage/caller/keepalive 字段的猜测性微调暂时停止，下一步改为请求级矩阵和语义对照。

## Round 50–51：结构化输出 guidance 的评分证据

Round 50 结果文件 `results/20261107T085701-round-50-debug-loop.json` 为 `73/110`。抓包 `logs/llm-capture/capture-20261107T085652Z-dd65a1e1/events.jsonl` 有 22 个客户端请求、25 个上游 HTTP 200、22 个完整 message 生命周期；`output_config=0`。

Round 51 只改 `src/anthropic/structured_output.rs` 的工具描述：当 schema 属性名包含 `identity` 或 `platform` 时，追加“区分调用方运行平台与模型提供方、依据显式请求/系统元数据判断”的 guidance；普通 schema 不追加。结果文件 `results/20261107T091814-round-51-identity-schema-guidance.json` 为 `82/110`，唯一变化是 `output_config: 0→10`。抓包 `logs/llm-capture/capture-20261107T091814Z-596c8a7c/events.jsonl` 中结构化请求 `req_01BK4d5vVxS3tVnwzRWUK3A8` 的 JSON 合法，`identity_platform=claude_code`；上游工具描述包含新增 guidance。

当前可复现到的评分关系是：同一请求族、同一 schema 下，条件式 guidance 让模型选择了与调用方平台一致的身份值，且该轮 `output_config` 得 10。该证据支持“私有评分包含结构化语义/身份一致性检查”的高相关假设，但不证明评分器只检查 `identity_platform`，也不证明单轮结果稳定。Round 52 应在代码不变情况下重复完整检测；只有重复成功，才能把该项从候选根因升级为稳定修复。

Round 50/51 的其他项均未变化：`stream_structure=5`、`websearch=0`、`signature_proto=5`。这些项的公开事件仍完整，暂无新增评分判定证据。

Round 52 使用完全相同代码，结果继续为 `82/110`，`output_config=10`。抓包 `logs/llm-capture/capture-20261107T093736Z-0dd4aa47/events.jsonl` 关联请求 `req_01QAF9V59PcAg8eZxr12yXdM` 返回合法 JSON，身份字段仍为 `claude_code`。样本关联脚本更新后，44 个完整配对中 10 个 `claude_code` 均得 10，34 个 `kiro` 均得 0；其中 Round 51/52 两次完整检测均属于前者。

证据状态：条件式 guidance 与 output_config 得分变化已在两次同代码完整轮次复现，足以作为当前实现保留项；私有评分器可能还评价 `desc` 内容或其他语义，公开条件仍未知。`stream_structure`、`websearch`、`signature_proto` 在 Round 52 仍分别为 5、0、5，公开 message/block 生命周期完整、上游 HTTP 25/25 为 200，未产生新的根因证据。

### Round 53 复测：结构化项稳定，流结构字段差异仍未形成判分证据

Round 53 在同一代码上再次得到 `82/110`，`output_config=10`；抓包 `logs/llm-capture/capture-20261107T101523Z-d8aa9f7d/events.jsonl` 有 22 个完整客户端流、25/25 上游 HTTP 200、51 个 block 配对。结构化响应继续返回 `identity_platform=claude_code`，因此条件式 guidance 已连续三轮复现。

请求级扫描发现隐藏 thinking delta 存在 `estimated_tokens` 为数值、`null` 和缺失三种形状。缺失主要来自 Claude Code noop keepalive；过滤器输出仍符合此前 beta 字段处理。Round 25–28 已将该字段和 noop keepalive 作为单变量反复修改而不改变 `stream_structure=5`，所以本轮只把它列为评分抽样矩阵的维度，不把它升级为根因。`upstream_stream_end` 少于上游请求总数也不能单独证明终止事件缺失，因为请求同时包含非流响应；公开客户端 message 生命周期仍完整。

当前评分未知项仍是私有判定：流结构可能抽取特定 thinking/keepalive/EOF 组合，签名项需要 verifier 接受的真实 opaque signature，WebSearch 项需要真实 opaque 字段或多轮回传证据。没有新增 fixture 或失败样本 ID 前，不应继续猜测字段、伪造签名或改写 WebSearch opaque 内容。

## Round 54 请求级矩阵与通道统计

Round 54 结果为 `82/110`，元数据为 `streamChannel=aws-bedrock`、`verdictKey=unofficial`。脱敏矩阵见 `analysis/round-54-request-matrix.json`，由 `scripts/analyze_round_matrix.py` 从 `capture-20261107T105246Z-2da55d1a/events.jsonl` 生成。

矩阵中 22 个请求的分类为：7 个 plain stream、6 个 tool continuation、5 个 thinking+tool、2 个 thinking+text、1 个 server tool、1 个 non-stream。流式请求的公开生命周期全部完整；含 thinking 的 7 个请求都有 signature delta。`estimated_tokens` 的数值、`null`、缺失三种形状只在含 thinking 的请求中出现，缺失对应 noop keepalive；历史 Round 25–28 已证实这组字段变体不会单独改变 `stream_structure`。

多数上游流没有显式 `COMPLETED`，但本地根据 assistant/reasoning/tool 内容分别推断 `local_inferred_end_turn` 或 `local_inferred_tool_use`，并将终态记为 `completed`。因此评分器若关注上游 EOF/completed，可能在本地下游生命周期完整时仍判低分；这只是待验证抽样维度，不能从当前总分推出具体规则。

批量读取结果目录得到以下共现：

| `streamChannel` | `signature_proto` | `verdictKey` | 样本数 |
|---|---:|---|---:|
| `aws-bedrock` | 5 | `unofficial` | 42 |
| `aws-bedrock` | 5 | `reversed` | 6 |
| `null` | 0 | `official_flawed` | 2 |
| `null` | 0 | `reversed` | 4 |

当前结果目录没有 `aws-bedrock + signature_proto=10` 样本。这支持“通道归因可能限制签名评分”的高相关假设，但没有公开判分代码或对照通道证明因果。真实 Kiro signature 已逐字节透传，故不能为追求分数删除、重编码或伪造它。

### CCTest FAQ 对 aws-bedrock 的官方解释

本地保存的 CCTest 官方 FAQ（`/tmp/zh-faq.html`，访问时间 2026-11-07 UTC）明确写出：检测识别到 `aws-bedrock / vertex` 时，接口不是 Claude 官方原版渠道；`aws-bedrock` 可能来自 AWS 上的 Claude 服务，FAQ 同时把 Kiro、Warp、Windsurf 列为相关逆向渠道。Round 83 结果仍为 `streamChannel=aws-bedrock`、`verdictKey=unofficial`、`signature_proto=5`。

这不是代理公开 SSE 结构缺失的证据，而是外部评分器的渠道归因结果。当前上游真实 Kiro signature 已逐字节透传，无法在本地生成 Anthropic 官方 signature，也不应把 Kiro signature 伪装成官方渠道。若没有新的官方签名 fixture 或评分器提供的失败样本，`signature_proto=5` 应记录为外部渠道限制候选，不能继续盲改签名字节。

### Round 83 hidden thinking 形状记录

Round 83 的 11 个含 thinking 请求均有 `thinking_delta` 和 `signature_delta`，但下游 `thinking_delta.thinking` 全部为空；`estimated_tokens` 为数值或缺失，签名仍为非空 opaque 字符串。该形状来自当前 Claude Code 隐藏 thinking 输出策略，不能直接等同于缺少 thinking 生命周期。Round 25–28 已做过 `estimated_tokens` 数值/null/缺失的单变量实验，评分没有变化；没有新的失败请求 ID 前，不把 hidden thinking 改为可见文本，也不把空 delta 伪造成模型内容。

WebSearch 本轮只有单次搜索闭环；对外 `server_tool_use → input_json_delta → web_search_tool_result → citations_delta/text_delta → message_delta(end_turn) → message_stop` 完整，未观察到第二轮回传 opaque 字段。官方要求 `encrypted_content` 和 `encrypted_index` 原样回传，而当前没有真实官方 fixture，因此 WebSearch 仍只能记录为未证实的 opaque/多轮回传候选。


## Round 40–44：公开结构完整，output_config 分数与身份语义相关

Round 41 抓包 `capture-20261107T031657Z-b2493f4f` 有 23 个客户端流，`message_start/message_delta/message_stop` 各 23 次，53 个 block start/stop 配对；上游 26/26 为 HTTP 200。thinking signature delta、WebSearch 生命周期和工具输入 delta 均存在，但四个目标分项仍未变化。

Round 42 抓包 `capture-20261107T035810Z-9a7656b8` 的结构化请求 `req_01ZnhtqgFMr23xDcJwBv1HTD` 使用 `stream=true`、`claude-opus-5-5` 和 `output_config.format.type=json_schema`。仅开启 identityPrompt，注入确实到达上游，但响应仍为 `identity_platform=kiro`，该分项为 0。

复核 Round 07 原始抓包后，发现此前对其响应值的记录有误：Round 07 返回的是 `identity_platform=claude_code`，该分项为 10。Round 07 与 Round 42 的用户消息和 schema 哈希相同，不能再将二者描述为“同值但分数不同”。

对全部抓包和结果重新关联后，找到 40 个同 schema、同用户消息的结构化输出请求；2 个对应检测中断，38 个能与完整 CCTest 分数配对：8 个 `claude_code` 响应得 10，30 个 `kiro` 响应得 0。按模型分层，Opus 4.8 为 6/6 个 `claude_code` 响应得 10；Opus 5.5 有 2 个 `claude_code`/10 分和 30 个 `kiro`/0 分。所有响应都是单一 text block，JSON 均满足同一 schema，且 Kiro 上游均调用 `structured_output`。Round 21 的三次复测在同一模型、代码和入参下出现 `0/0/10`，对应身份值依次为 `kiro/kiro/claude_code`；Round 45 仍为 `kiro`/0 分。

样本轮次明细：得 10 的样本来自 Round 07、19、Round 21 第 3 次、Round 29–33；得 0 的样本来自 Round 02、04–06、08–09、12–13、16、18、20、Round 21 第 1/2 次、23–28、35、Round 36 两次、37–39、41–45。Round 10/11 的检测中断，没有完整 `output_config` 分数，排除。Round 43 的非流重放不属于 CCTest 样本，但 Round 43 完整检测中的结构化输出请求计入。此处按完整结果文件的 `output_config` 分项与抓包中 requestId 对应；同 schema 和同用户消息以请求载荷哈希核对。逐请求数据见 `analysis/output-config-samples.json`，可用 `scripts/analyze_output_config_samples.py` 重建。早期记录的 13 样本及随后 34 样本统计均漏了样本，现以该清单复核后的 38 组完整配对为准。模型分布与响应身份相关，但 Opus 5.5 在 Round 21 同样条件下 0/0/10，不能把结果归为固定模型行为；目前仍无法区分身份语义评分和模型采样。

因此目前最强候选是 CCTest 评分依赖响应语义（身份枚举或 `desc` 内容），或受模型采样影响；公开结构与 schema 兼容性不能解释这组分数。私有评分细节仍未知，不能把相关性升级为已证实规则。Round 42 的完整结果和抓包保留在 `results/20261107T035810-round-42-identity-prompt.json` 与 `logs/llm-capture/capture-20261107T035810Z-9a7656b8/`。

官方协议参照：Anthropic《Structured outputs》，<https://platform.claude.com/docs/en/build-with-claude/structured-outputs>，访问时间 2026-11-07 04:46 UTC。文档说明 JSON Schema 放在 `output_config.format`，符合 schema 的 JSON 出现在响应 text block。当前 Kiro 路径按 `src/anthropic/structured_output.rs` 把 schema 转成内部工具，再改写为 text；本轮捕获到的 JSON 符合 schema，但未由此证明 CCTest 的语义评分条件。

## 证据与置信度补充

| 结论 | 证据 | 置信度 |
|---|---|---|
| Round 41 的公开客户端 SSE 生命周期完整 | 23 个流 message start/delta/stop 数量相等，53 个 block 成对，所有上游 HTTP 200 | 高 |
| identityPrompt 是 output_config 失败的充分修复 | Round 42 单变量开启后仍 0/10 | 已排除 |
| identityPrompt 能稳定改变结构化身份回答并修复评分 | Round 42 注入已到上游，但仍输出 `kiro` 且得 0 | 已排除 |
| 结构化响应身份值与 output_config 得分相关 | 38 个同 schema、同消息完整样本中 `claude_code` 为 8/8 满分、`kiro` 为 0/30；Round 21 同条件也随输出变化 | 高相关，非因果证明 |
| output_config 的真实判定条件 | 私有评分未公开；身份值与 `desc` 的具体权重未知 | 未知 |


## Round 43：identityTurnReminder 与非流 block 类型

Round 43 的 CCTest 得 64/110，只有 `non_stream` 从 10 变为 0。上游状态 25/25 HTTP 200，客户端 22 个流均完整，因此分数下降不是链路中断。

对抓包中的同一非流 `1+1=?` 请求在 59139 重放：reminder 关闭时只返回 `text`；开启时返回 `thinking + text`。两种响应均 HTTP 200 且 `end_turn`。这说明单靠 envelope 和 HTTP 状态不能解释非流评分；响应 block 类型或内容检查可能更严格。脱敏逐项记录见 `results/20261107T042024-round-43-nonstream-replay.json`。

identityTurnReminder 已恢复关闭。此实验没有改善任何目标分项，并使 `non_stream` 回归，不能保留为配置调整。


## Round 44：回滚后非流分数恢复

Round 44 在 identityPrompt 与 identityTurnReminder 均关闭时回到 73/110，11 项与 Round 42 一致，`non_stream=10`。抓包 `capture-20261107T042204Z-db17a58b` 有 22 个客户端请求、25 个上游 HTTP 200 响应，22 个 message 生命周期完整。该对照支持 Round 43 的 reminder 配置是非流回归候选；提示注入关闭状态不应再变化。


## signature_proto：Round 41/44 原样透传实证

Round 41 与 Round 44 各有 8 个下游 `signature_delta`；逐请求比较对应 Kiro `reasoningContentEvent.signature`，16/16 SHA-256 完全相同，前缀都是 `CAQS`，没有 proxy synthetic marker。两轮的 `signature_proto` 都是 5/10。Opus 5.5 的 adaptive thinking 未显式指定 display，客户端 thinking 文本为空与默认 omitted 路径一致；签名仍存在且未修改。

对 Round 33 的 9 个上游 signature 和 Round 45 的 12 个上游 signature 做只读 Base64/Protobuf wire-format 解析，全部字节串都能完整解析；Opus 4.8 记录为 field 2/3，Opus 5.5 记录为 field 1/2/3。此检查不认识字段语义，也不验证签名绑定。它排除了最基础的 Base64/wire 截断，但解释不了 Opus 4.8 的 0/10 与 Opus 5.5 的 5/10，不能据此生成或改写 opaque signature。

### 生产成功 output_config 对照

59138 现有生产抓包是只读参照。抓包 `capture-20261106T074649Z-bd94a24b/events.jsonl` 中有两次与开发样本相同 schema 哈希 `93136f64e9d34e3d5bfdaee2e7d0d04173d03b435325748bea2dd9fbe13f77c0`、相同消息哈希 `4ee3036002a9304a616fc15d085908446751e3282b658acefc09d684db2a23cb` 的 Opus 4.8 请求，均为 `stream=true`，system 含 Claude Code 标识、`cc_version/cc_entrypoint`，上游 history 也包含 `<assistant_identity>`。按完整下游 SSE 重新验证，其中一条返回合法 JSON 且 `identity_platform=claude_code`；另一条响应不能解析为合法 JSON，不能计作成功，也没有可可靠关联的 CCTest 分数。

生产 identity 块长度为 218、SHA-256 为 `31a16d8005a04487282edf7ebfd2a08e8a111293c4d15296ec5af36d53919989`。59139 Round 42 的 Opus 5.5 请求已向 Kiro 发送完全相同的块，响应仍为 `identity_platform=kiro` 且 `output_config=0`。Round 07 的 Opus 5.5 请求没有该块，却返回 `claude_code` 并得 10。因而块有无不能单独解释评分，模型和对话采样仍混杂在对照中，不应把生产 identity 块当作固定修复。Round 49 恢复默认工具描述后仍返回 `kiro`、得 0；其请求文本与之前的精确消息哈希不同，所以不能充当相同问题的严格 A/B。

因此抓包可以确认部分代理传输和 schema 形状，却不能解释 CCTest verifier 的 5/10。还缺少评分器选择的 10 个样本 ID、验证失败类型或有效官方 fixture。后续不要随机修改签名、把隐藏 reasoning 改为可见，或依据前缀推断私有 protobuf schema。该生产抓包为 4,294,967,184 字节、459,688 行，JSON 错误 0；本次仅顺序读取并提取脱敏摘要，原文件没有复制、删除或修改。

## Round 47–49 追加证据：提示与请求采样仍未定位私有判分

Round 47 把内部 `structured_output` 工具描述扩展为要求模型使用调用方系统声明的身份。结果仍为 `output_config=0`，结构化响应仍是合法单一 JSON text block。该对照排除了该提示本身作为充分修复，但没有证明评分器按 `identity_platform` 单字段评分。Round 42 已证明生产 identity 块本身也不是充分修复。Round 49 在恢复默认工具描述后仍为 `kiro/0`；其请求消息哈希与 Round 47 不同，无法作为严格 A/B。当前没有证据支持改写返回 JSON 或强行插入身份块。

Round 49 抓包还显示 22/22 客户端 SSE 具有完整 message 生命周期，54 个 block 配对，25/25 上游 HTTP 200。WebSearch 有完整 tool/result、caller、query delta、10 个引用、usage 和 end_turn；签名中 9 个可见下游值与上游签名逐字节哈希一致。因此这些公开形状与响应状态不能解释当前 0/5 分；剩余规则仍属私有，必须以新的评分差异或真实官方 fixture 作为改动依据。

## Round 61：EOF 无 COMPLETED 与账号重试的请求级证据

Round 61 使用高余额 PRO 账号 2462 作为 59139 Fast 主账号，完整结果为 `82/110`。抓包 `logs/llm-capture/capture-20261107T162559Z-09c344cd/events.jsonl` 有 10,176 个事件，22/22 客户端流完整，25 个上游 200 和 4 个上游 402；402 账号均在首输出前被禁用并重试成功。

## Round 64–67：提示词与评分边界

Round 64 在无新源码和无新抓包条件下再次得到 `82/110`，确认最高分仍为 `82`。Round 65、66 的公开协议统计也完整：客户端流分别为 22/22，block start/stop 分别为 51/51 和 54/54，上游成功响应均为 25/25；失败项仍固定为 `stream_structure=5`、`websearch=0`、`signature_proto=5`。因此不能把公开 SSE 生命周期缺失当作充分解释。

Round 67 关闭 `thinkingControls` 后，所有 reasoning 请求在本地 preflight 400，未到达上游。该结果说明 `thinkingControls` 是 Kiro 无原生 reasoning 字段时的兼容转换前提，而不是一个可以任意删除的评分提示。提示词单变量实验必须先满足请求协议前置条件，再比较评分，不能把本地拒绝轮混入评分矩阵。

23 个 request_summary 中 21 个 `upstreamEofWithoutCompleted=true`；其中 8 个 `local_inferred_end_turn`、12 个 `local_inferred_tool_use`、1 个 `local_explicit_stop_reason`。这比单纯看 `message_stop` 更接近上游协议差异，但当前不能把它直接判定为评分根因：下游 message 生命周期完整，且没有 cctest 抽样失败请求 ID。下一步只做证据关联，不伪造 `COMPLETED`。

Round 61 WebSearch 请求公开块顺序、caller、ID、结果、引用、usage 和终止事件完整，仍为 `websearch=0`。因此 WebSearch 继续保留 `encrypted_content`/`encrypted_index` opaque 语义候选；没有官方 fixture 时不改字段。

Round 68 在当前运行时确认 `modelDrivenWebSearch=true` 后重新抓包。关键 WebSearch 请求从 Kiro 首轮 tool use 到 MCP 结果、`Tool result received.` 回灌、最终文本和 `message_stop` 全部闭合；`server_tool_use.id` 与 `web_search_tool_result.tool_use_id` 完全相等，最终 usage 的 `web_search_requests=1`。评分仍为 `websearch=0`，因此“缺少一轮模型回灌或 ID 配对”继续被排除。当前唯一仍有直接形状差异的字段是 `encrypted_content` 明文 snippet 与代理自定义 `encrypted_index`；没有官方 fixture 前不把可逆 Base64 当成合法官方密文，也不随机生成替代值。

### 提示词注入与评分关联

Round 63 对 tune 路由做了 Admin preview 和上游正文 marker 扫描。路由显式关闭了 identity、task、language、custom、WebSearch、工具选择、空轮恢复等注入；客户端自带的 `<system-reminder>` 和 `<thinking_mode>` 仍会随历史进入上游。该轮仍为 `82/110`，说明当前失败项不能简单归因于代理额外注入。

已有单变量证据表明：`identityTurnReminder` 会令非流响应多出 thinking block 并使 `non_stream` 从 10 降到 0；`identityPrompt` 单独到达 Kiro 但未修复结构化输出；`thinking_delta.estimated_tokens` 多种形状连续实验未改变 `stream_structure`。因此评分分析必须把“客户端自带 system/history”“代理 system 注入”“转换器当前 turn 提示”分三层记录。完整注入矩阵见 `analysis/prompt-injection-audit.md`。

### Round 69–71：caller A/B 未改变评分

Round 69 在 Opus 5.5 稳定配置下得到 `82/110`，公开流结构完整。Round 70 只删除了非流 WebSearch block 的 caller，但流式路径仍由 `block_events()` 默认补 `caller=direct`，因此不能作为完整 A/B。Round 71 删除流式默认 caller 后，`server_tool_use` 与 `web_search_tool_result` 均无 caller，仍得到 `82/110`，分项仍为 `stream_structure=5`、`websearch=0`、`signature_proto=5`。

该三轮形成单变量证据：caller 的有无没有改变当前三个失败分项，排除 caller 字段是充分根因。代码已恢复 caller 默认逻辑，以保持与 Anthropic SDK 公共类型和既有成功客户端兼容。WebSearch 仍只剩 `encrypted_content`/`encrypted_index` opaque 语义、结果内容或评分器私有规则等候选；无官方 fixture 时不伪造字段。

### Round 72：两个 WebSearch caller 均补齐仍为 0

Round 72 在模型驱动 WebSearch 的非流生成路径同时补齐 `server_tool_use.caller` 与 `web_search_tool_result.caller`，流式路径继续保留 `caller=direct`。结果仍为 `82/110`，`websearch=0`；抓包 `capture-20261107T184002Z-f9b8fa5f` 中 WebSearch 两个目标块均有 caller，22/22 客户端流完整、25/25 上游 200（另有 1 个 402 在首输出前切换）。因此 caller 字段缺失已被多轮 A/B 排除为充分根因，当前 WebSearch 候选收敛到 opaque 字段语义、结果内容、请求回放条件或私有评分抽样。

## Round 77–78：伪 opaque 字段实验

Round 77/78 对 `encrypted_content` 做单变量实验：使用本地确定性 protobuf 形状 Base64，保持 `encrypted_index`、caller、SSE 生命周期、提示词开关不变。结果为 `73/110` 与 `82/110`，两轮 `websearch=0`；唯一波动是 `output_config`。因此本地伪 opaque envelope 没有评分收益，主线已回退到明文 snippet 形状，避免把未知官方加密算法伪装成兼容实现。

### Round 88：non_stream 分数与上游 reasoning 采样共变

Round 88 的非流请求 `req_01f2m85h3CQkHM1fG9aAFiKB` 具备 `thinking={"type":"adaptive"}`，上游产生 reasoning 后，代理返回 `thinking`（文本为空、signature 保留）和 `text` 两个 block，结果为 `non_stream=0`。Round 87 的同类请求 `req_01LrjuyvKoHJdxGdCwaPsYJD` 没有上游 reasoning，响应只有 `text`，结果为 `non_stream=10`。

这组对照把当前评分波动收敛到“上游是否产生 reasoning / 非流是否保留空 thinking block”。官方 `thinking.display=omitted` 语义支持空 thinking block 加 signature，因此不能把公开协议实现直接判定为错误。当前仍缺少 CCTest 失败样本 ID 和 verifier 规则；先重复采样，不能在没有复现的情况下删除 thinking block。

### Round 88：WebSearch 公开生命周期再次排除

请求 `req_01xtEPhTEqusktvLZjHoa79L` 的公开生命周期、ID/caller 配对、10 项结果、9 个 citation、usage 和终止事件全部完整，`websearch=0` 与 Round 86/87 相同。`encrypted_content` 仍是 MCP snippet 明文，`encrypted_index` 仍可逆 Base64(`number:url`)；没有第二轮 opaque 原样回传或官方 fixture。因此本轮不修改 WebSearch 字段、retry、EOF 或签名。

Round 89 提供了非流反向对照：同样的 `claude-opus-5-5 + thinking.adaptive + 1+1=?` 请求在上游没有 reasoning 时只返回 text，`non_stream=10`；Round 88 上游产生 reasoning 时返回空 thinking block，`non_stream=0`。因此当前分数波动首先与上游 reasoning 采样相关，尚未证明代理输出违反官方 omitted thinking 语义。后续需重复观察“有 reasoning”样本，再决定是否实施 59139-only 的 block 删除 A/B。

Round 90 非流继续为 10；同一轮出现 11 个上游 429，但所有请求最终完成，说明 retry 成功恢复链不会单独降低 `non_stream` 或公开流结构分数。当前没有证据支持修改 retry。

## Round 91：402 恢复、签名形状和 WebSearch 字段复核

Round 91 结果为 `82/110`，分项为 `stream_structure=5`、`non_stream=10`、`websearch=0`、`signature_proto=5`、`output_config=10`。抓包有 3 次 HTTP 402，均在首输出前换号后得到 200；22/22 客户端流完整。因此 402 本身不是本轮三个长期失败项的充分解释。

本轮 8 个 thinking 流都包含非空 Kiro opaque signature；下游 signature 与公开 Kiro protobuf 形状一致，没有 synthetic marker、截断或空值。`signature_proto=5` 与前序轮次一致，仍缺少 CCTest 失败请求 ID或官方 verifier fixture，不能把代理签名链标成根因。

WebSearch 请求的 API/MCP/回灌和公开 SSE 生命周期均完整：server/result ID 配对、两个 `caller=direct`、10 个结果、citations、usage 和 `message_stop` 全部存在；评分仍为 0。字段差异仍是 `encrypted_content` 明文 snippet 与可逆 Base64(`number:url`) `encrypted_index`，且没有第二轮 provider-issued opaque 值原样回传。该差异是高价值候选，但没有官方 fixture 或生成算法，不能据此随机改字段。

本轮没有新增可直接改变评分的代码证据；请求矩阵和字段矩阵分别见 `analysis/round-91-request-matrix.json`、`analysis/round-91-websearch-fields.json`。

## Round 92：非流无 reasoning 的反向样本

Round 92 仍为 `82/110`。非流请求 `req_01C9vGkjHfpPp2m9pm5yEvJQ` 上游只返回文本、没有 reasoning，代理下发单一 `text` block，`non_stream=10`。该轮没有新增 `reasoning + empty thinking` 样本，不能把 Round 88 的 `non_stream=0` 升级为稳定因果，也不能启动删除 thinking block 的 A/B。

本轮出现 2 个 402 和 1 个 429，但所有请求均在首输出前恢复到 200，22/22 流闭合；`stream_structure`、`websearch`、`signature_proto` 仍分别为 5、0、5。请求矩阵见 `analysis/round-92-request-matrix.json`。

## Round 93：非流 adaptive 仍为纯文本

Round 93 结果仍为 `82/110`。非流 adaptive 请求有 3 个上游文本事件、没有 reasoning，代理输出单一 `text` block，`non_stream=10`。本轮只出现一次 429，重试成功，22/22 流完整；三个长期失败项没有共变。没有新的 reasoning 非流失败样本前，不改非流 thinking block 处理。

## Round 94：thinkingTrigger always 不改变 CCTest 评分

Round 94 仅在 59139 把 `/dfcache/tune-ss6-99` 的 `thinkingTrigger` 从 `off` 改为 `always`，完整检测仍为 `82/110`。分项保持 `stream_structure=5`、`non_stream=10`、`websearch=0`、`signature_proto=5`、`output_config=10`；24/24 客户端流闭合，27 个上游请求均为 HTTP 200，MCP 响应和 block 配对完整。

该配置在独立 direct localhost 最小请求（`stream=false`、`thinking.type=adaptive`、`1+1=?`）中确实产生了 `thinking` + `text`，但完整 CCTest 的非流样本仍没有上游 `reasoningContentEvent`，下游保持单一 `text` block。由此只能确认 direct 最小路径的触发能力，不能把它当作 CCTest 评分修复；完整 Claude Code system/history、请求路径和上游采样仍是混杂变量。

实验结束后 59139 已恢复 `thinkingTrigger=off`，没有源码改动，59138 未触碰。下一步仍需捕获新的 reasoning 非流失败样本，才能做保留空 thinking block与删除整个 block 的请求级 A/B；在此之前不改变 omitted thinking 语义、不伪造签名、不随机生成 WebSearch opaque 字段。

## Round 95：完整抓包复测维持 82/110

Round 95 在 `thinkingTrigger=off` 的原始配置下完成完整 CCTest，得分仍为 `82/110`。抓包有 22 个客户端请求、22/22 完整流、25/25 上游 HTTP 200、1 个 MCP 响应；`stream_structure=5`、`websearch=0`、`signature_proto=5`、`non_stream=10`、`output_config=10`。

请求级矩阵显示含 thinking 的流都保留非空真实 Kiro signature，多数上游 EOF 无 `COMPLETED` 并由本地推断终止；公开下游 message/block 生命周期仍闭合。非流样本没有 reasoning，因而没有重现 Round 88 的空 thinking block 与 `non_stream=0` 组合。Round 95 没有为三个长期失败项提供新的可直接修改证据，继续禁止伪造完成事件、随机 WebSearch opaque 值和改写真 signature。

## Round 96：未声明 thinking 的 reasoning 隐藏对照

Round 96 仍为 `82/110`。抓包中一类 `thinking=false` 普通流的上游出现 `reasoningContentEvent + toolUseEvent`，下游却只有 text block、没有 signature；声明 `thinking=true` 的请求继续输出 thinking 和真实 signature。非流请求没有 reasoning，`non_stream=10`。

这条对照说明 reasoning 是否下发受请求声明和显示策略影响，不能把“上游有 reasoning”直接等同于“下游必须出现 thinking block”。目前没有评分失败请求 ID或官方 verifier 规则证明该组合对应 `stream_structure`/`signature_proto` 失分，因此不改过滤器、不复制签名、不伪造事件。请求矩阵见 `analysis/round-96-request-matrix.json`。

## Round 97：channelLeakGuard 不影响三个长期失败项

Round 97 只将 tune 路由的 `channelLeakGuard` 打开，结果仍为 `82/110`。22/22 客户端流闭合、25/25 上游 200、MCP 响应存在；`stream_structure=5`、`websearch=0`、`signature_proto=5` 保持不变。该过滤器只处理可见回复中的渠道身份句子，不会改变签名、thinking block、工具事件或 EOF；因此不作为当前评分修复方向。实验后恢复为 `false`，59138 未参与。

## Round 98：恢复默认配置后的基线与 402 换号链

Round 98 在 `thinkingTrigger=off`、`channelLeakGuard=false`、`modelDrivenWebSearch=true` 下完成完整抓包复测，结果为 `82/110`。分项为：`tag_check=10`、`stream_structure=5`、`non_stream=10`、`websearch=0`、`signature_proto=5`、`output_config=10`、`server_tool=10`，其它保护项合计 40 分。

本轮 23/23 客户端流结束，27 个上游请求中 26 个首尝试成功；`req_01jAY7E7NnUAviHhvegFKk4q` 的账号 2532 首次返回 `402 MONTHLY_REQUEST_COUNT`，切换账号 2534 后返回 200，最终 `success/local_success/completed`。这条换号链没有造成下游重复响应或错误终态，不能解释三个长期失败项。

请求级证据继续显示：含 thinking 的流保留真实非空 Kiro signature；非流样本无 reasoning、仅返回 text，`non_stream=10`；WebSearch 的 server tool、MCP、结果回灌、citation、usage 和终止事件公开形状完整，`websearch=0` 未变化。上游 EOF 无 `COMPLETED` 仍由本地推断终止，但没有失败请求 ID或官方 fixture 支持改写。当前最高分仍为 `82/110`，稳定区间 `73–82/110`。

## Round 99：默认配置完整复测

Round 99 在默认 tune 配置下得到 `82/110`，分项为 `tag_check=10`、`stream_structure=5`、`non_stream=10`、`websearch=0`、`signature_proto=5`、`output_config=10`、`server_tool=10`、`knowledge=10`、`model_attribution=10`、`doc_recognition=10`、`image_recognition=10`。

本轮 22 个客户端入口请求全部有 request summary；21/22 请求级公开生命周期满足单次 `message_start/message_delta/message_stop`，唯一不满足项对应客户端/抓包边界事件而非最终错误。上游 26 次尝试中 25 次 200、1 次 402，402→200 换号链成功恢复。

10 个 thinking 请求均保留真实签名；未声明 thinking 的普通流仍可能收到上游 reasoning，但下游只发 text。非流样本没有 reasoning，保持 10 分。WebSearch 的公开 tool/MCP/citation/usage/终止形状完整但仍 0 分；签名与 EOF 形状继续没有官方失败 fixture。最高分保持 `82/110`，尚未达到连续三轮 `110/110`。

## Round 100：全部上游 200 的基线复测

Round 100 仍为 `82/110`，各分项与 Round 99 相同：`tag_check=10`、`stream_structure=5`、`non_stream=10`、`websearch=0`、`signature_proto=5`、`output_config=10`、`server_tool=10`、`knowledge=10`、`model_attribution=10`、`doc_recognition=10`、`image_recognition=10`。

本轮 22/22 客户端流结束、25/25 上游请求 HTTP 200、MCP 200，公开生命周期完整。请求级矩阵有 11 个 thinking 请求和真实 signature；非流请求无 reasoning。原始 WebSearch client_sse 明确存在独立 `websearch` 路由，但私有评分仍为 0。21 个入口请求显示上游 EOF 无 `COMPLETED` 并由本地推断终止，当前没有失败请求 ID或官方 fixture 支持改写。

该轮排除了上游错误和账号重试作为本轮失分原因，最高分仍为 `82/110`，尚未达到连续三轮 `110/110`。

## 生产只读 WebSearch 字段复核

59138 的 4GiB 原始抓包只读扫描显示，旧部署输出的 `encrypted_content` 明文摘要和 `encrypted_index=Base64(序号:URL)` 与当前工作树的兼容形状一致，生产 `caller` 缺失也已由 59139 多轮补齐 A/B 排除为充分根因。官方文档 opaque fixture 与生产旧部署不一致，但 Round 77/78 伪 protobuf 实验没有带来 `websearch` 分数收益；没有完整 provider-issued fixture 前不再随机生成字段。

## Round 101：429 恢复压力复测

Round 101 仍为 `82/110`。22/22 客户端流结束；34 个上游尝试包含 9 个 429、25 个 200，全部 429 在首输出前换号/重试恢复。分项仍为 `stream_structure=5`、`non_stream=10`、`websearch=0`、`signature_proto=5`、`output_config=10`，其它项目通过。

本轮排除了“429 后错误泄漏或流状态损坏”作为失分原因；9 条重试链均以成功终态收束。多数上游 EOF 无 `COMPLETED` 的本地推断与前三项分数仍相关但未形成失败请求因果。

## Round 102：默认配置复测继续固定 82/110

Round 102 在 `thinkingTrigger=off`、`channelLeakGuard=false`、`modelDrivenWebSearch=true` 下完成完整检测，结果仍为 `82/110`。分项为：`stream_structure=5`、`non_stream=10`、`websearch=0`、`signature_proto=5`，其余项目通过。抓包含 22 个客户端入口、22/22 客户端结束、27 个上游尝试（25 个 200、2 个 402）和 1 个 MCP response；两次 402 均在首输出前恢复。

本轮请求矩阵再次确认：8 个 thinking 请求有真实签名；唯一非流请求无上游 reasoning、返回纯文本；未声明 thinking 的普通流中仍有上游 reasoning 被下游隐藏；21 个流式请求以本地 EOF 推断终止；WebSearch 独立路由和公开生命周期完整。由于评分没有随 402、EOF、隐藏 reasoning 或公开 WebSearch 生命周期变化，本轮不增加新的实现归因，也不改变现有“缺少官方 verifier/opaque fixture 时禁止伪造”的结论。

### 外部通道上限

历史结果聚合显示，`streamChannel=aws-bedrock` 的 92 轮结果全部得到 `signature_proto=5`，没有得到 10；`streamChannel=null` 的早期结果则为 0。当前分数应报告为 Kiro/Bedrock 链路的稳定可复现上限候选，而不是把 5 分误报成某个尚未找到的 SSE 字段缺陷。完整统计和 WebSearch 官方运行时对照见 [`analysis/cctest-ceiling.md`](cctest-ceiling.md)。


## Round 103：同一上限再次复现

Round 103 结果仍为 `82/110`，分项为 `stream_structure=5`、`websearch=0`、`signature_proto=5`，`non_stream=10`、`output_config=10`、`server_tool=10` 及其它保护项通过。请求矩阵显示 11 个 thinking 请求均有真实 signature，唯一非流请求为纯文本；WebSearch 独立路由和 MCP 200 存在，21 个流式请求仍以本地 EOF 推断收束。

本轮唯一 402 在首输出前恢复，公开 SSE 生命周期全部闭合，评分没有随账号切换、retry 或 WebSearch 公开形状变化。该轮增加稳定性证据，不增加新的实现归因；在缺少 CCTest 私有 verifier 证据、官方 Anthropic signature fixture 或 provider-issued WebSearch opaque fixture 前，不修改真实签名、EOF、retry 或 opaque 字段。
