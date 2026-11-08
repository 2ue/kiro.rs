# cctest 未通过问题清单

更新时间：2026-11-07（Round 60；UTC 证据时间，后续轮次继续追加）

## 最新进展

Round 84 的请求级 EOF 统计进一步固定了一个待验证差异：21 个流式上游结束均没有显式 `messageStatus=COMPLETED`，其中 8 个由本地推断 `end_turn`、12 个由本地推断 `tool_use`、1 个由显式 stop reason 收口；下游 22/22 流仍完整闭合，评分保持 `82/110`。这说明“上游无 COMPLETED”与“公开 SSE 不闭合”是两件事，暂不改状态机。矩阵见 `analysis/round-84-eof-shape.json`。

Round 84 后的受限回归中，WebSearch 隐私日志测试并发运行偶发未观察到 `query_bytes`，串行重跑通过；该问题属于测试日志观察竞态，不是 CCTest 抓包中的服务协议失败，未据此修改 WebSearch 实现。

- Round 28（`results/20261106T192858-round-28-keepalive-null.json`）将 noop keepalive 路径的隐藏 thinking 空帧统一为 `estimated_tokens: null`；抓包中 thinking delta 缺失字段从 Round 27 的少量降为 0，但结果仍为 `73/110`。这排除了“beta thinking delta 缺少字段”作为四个失败项的充分根因。

- Round 05（`results/20261106T105750-round-05-code-execution.json`）只开启代码执行 runtime 配置，结果仍为 `64/110`。
- Round 06（`results/20261106T111026-round-06-code-schema.json`）在原生代码执行工具补齐输入 schema 后，结果升至 `73/110`；`server_tool` 从 0 变为 10。
- Round 06 的 WebSearch 和 output_config 已完成请求级结构核对，公开 Anthropic 事件形状完整，但私有评分仍分别为 0；后续按最小矩阵继续定位，不把“外形完整”误记为“评分已通过”。

## 证据范围与版本边界

- 生产抓包：`/var/lib/docker/volumes/65265a2bb319768ff130ad0a88ee5d08aba49d9ba89286f2c0ebdf4a3b12bda7/_data/llm-capture/capture-20261106T074649Z-bd94a24b/events.jsonl`
- 抓包大小：`4294967184` 字节，接近 4 GiB 预算；事件行 `459688`，序号 `1..459694`，时间 `2026-11-06T07:46:49.494080Z` 至 `07:50:48.299038Z`。文件全部 JSON 可解析，但不能假设覆盖完整 cctest 测试窗口。
- 生产监听：`59138`，容器 `kiro-rs-2ue-59138-app`。
- 生产镜像版本：`0.0.239`，OCI revision `30edc00c12431371e22498298715adc7bea539fd`，二进制 SHA-256 `475d04c96d60e07bf2c36ae9e7902781a8e575f458e1899da7f06595e816f706`。
- 当前仓库 HEAD：`2e625e52`，tag `v0.0.240`。生产版本落后当前仓库，至少缺少 `57332cca`、`e4015a69`、`1b473d08`、`2e625e52` 这些提交；59138 未重启、未写配置。
- 基线 cctest：`docs/testing/cctest-kirotest/results/baseline-9afa06c2.json`，73 个检查，未通过 `stream_structure`、`server_tool`、`websearch`、`signature_proto`。

## 统计总览

抓包事件统计：

| 事件 | 次数 |
|---|---:|
| `upstream_response` | 2010 |
| `upstream_frame` | 286821 |
| `upstream_request` | 2016 |
| `client_sse` | 159141 |
| `request_summary` | 1676 |
| `http_ingress` | 1733 |
| `client_request` | 1709 |
| `http_egress` | 1700 |
| `client_stream_end` | 1146 |
| `client_response` | 496 |
| `upstream_error_body` | 33 |
| `mcp_response_body` | 4 |

上游状态：`200=1977`、`400=31`、`429=1`、`500=1`。

客户端 SSE 类型：`message_start=1216`、`content_block_start=2282`、`content_block_delta=150111`、`content_block_stop=2216`、`message_delta=1137`、`message_stop=1137`、`ping=1033`、`error=9`。

## I-01：历史 thinking synthetic signature 被送入 Kiro 原生 reasoning

### 现象

抓包出现 31 次上游 HTTP 400，错误完全一致或同类：

```json
{
  "message": "messages.N.content.0: Invalid `signature` in `thinking` block",
  "reason": "THINKING_SIGNATURE_INVALID"
}
```

索引包括 `messages.3`、`9`、`13`、`19`、`25`、`27`、`35`、`51`、`63`、`87`、`101`、`131`、`151`、`187`、`195`、`223`、`231`、`259`、`315`、`349`、`541`、`793` 等。覆盖 `/dfcache/mine-haa`、`/dfcache/minea`、`/dfcache/onlylocal`，覆盖 stream/non-stream、`claude-opus-5-5`/`claude-sonnet-5-5`，不是单一账号偶发故障。

典型 request：`req_01M479QZforNeohN2QPZ9AAj`，第一次 upstream request 序号 `2321`，上游错误序号 `2664`，同账号 retry 序号 `2667`。第一次 body 中：

- `conversationState.history.9.assistantResponseMessage.reasoningContent.reasoningText.text` 长度 `234`；
- `signature` 长度 `304`；
- signature SHA-256 `7dda3f9755d7a41ac833433abb8df24d6cbacc72eb80cd5c69cf4e826d1d56a2`；
- Base64 前缀 `a2lyb2dv...`，解码后是 `kirogo-thinking-signature-block-0-pad-...`，明确是代理生成的 synthetic transport marker，不是 Kiro 签发的 opaque signature。

retry body 的 `thinking_signature_retry` 阶段删除了历史 reasoningContent，随后返回 200；这证明当前兼容重试可以绕过错误，但首次请求仍然把不被 Kiro 接受的 synthetic signature 发给了上游。

### 根因定位

当前 `src/anthropic/converter/history.rs` 的历史转换只把本代理识别出的 synthetic signature 转成可见 `<thinking>...</thinking>` 文本；但已经构造出的 Kiro request 仍可能在后续 payload guard/历史合并/重试前保留 `reasoningContent`。`src/anthropic/handlers.rs` 通过 `has_history_reasoning_content()` 触发兼容重试，说明错误发生在首次序列化后的 Kiro body，而不是下游 SSE 生成阶段。

这与 `src/anthropic/signature.rs` 的 synthetic marker 设计一致：synthetic signature 只应存在于 Anthropic 对外传输，绝不应进入 Kiro 的原生 `reasoningText.signature`。

### 影响

- 首次 upstream 请求稳定失败，增加约一次上游往返延迟；
- 若采样把首次失败计入，会直接影响 `signature_proto`；
- 多轮工具历史会反复触发，影响 `stream_structure`、工具调用和错误重试稳定性；
- 当前 retry 成功后多数请求仍能完成，所以生产表面成功率掩盖了协议错误。

### 修复方案候选

1. **首选：构造 Kiro request 前做不可逆边界清理。** 在 `build_history`/payload guard 结束后，对所有 `ReasoningContent::ReasoningText` 再检查 signature；若是 synthetic，转为无 reasoningContent 的兼容文本，或在 Anthropic 非严格兼容 profile 中直接删除该 native block。保证任何进入 `serialize_kiro_request` 的 synthetic marker 都被拒绝。
2. 保留真实 Kiro opaque signature 原样转发；不能把所有签名都删除，否则会破坏真实多轮 thinking。
3. 增加单测：synthetic thinking、synthetic signature 文本被改写、redacted thinking、真实 opaque signature、多个 assistant/tool 交错历史。
4. 增加审计计数，记录 `synthetic_reasoning_removed_before_upstream`，但日志只记录数量、索引和 hash，不记录正文。

验收：同一测试窗口上游 `THINKING_SIGNATURE_INVALID` 为 0；首次请求不再依赖 thinking-signature retry；真实 opaque signature 仍保留。

## I-02：一次上游 429 最终变成客户端 502

### 现象

request `req_01jsWk9pMp2ZmTQG7NNfVAJY`，endpoint `/dfcache/minea/v1/messages`，`claude-opus-5`，stream=true：

- credential `2422`、`2428`、`2398` 返回 200；
- credential `2408` 返回 429，body `Too many requests, please wait before trying again.`；
- 后续 credential `2399`（attempt=2）、`2400`、`2401`、`2402`、`2403`、`2377` 继续返回 200；
- request_summary 却是 `status=stream_error`、`routeSubtype=local_error_no_fallback`；客户端返回 502。

### 根因状态

抓包现象说明“尝试过成功 upstream response”不等于“下游 stream 已经形成合法终止”。需要逐事件核对这些 200 尝试是否：

- 在 `message_start` 前 EOF；
- 只返回 Kiro toolUseEvent，没有 assistantResponseEvent；
- 成功 retry 后复用了旧 `SseStreamState` 的错误标记；
- 已发送部分下游事件后又走了不可重试分支；
- 缺少 `message_delta`/`message_stop` 或 usage finalization。

相关代码：`src/anthropic/handlers.rs` 的 `SseStreamState`、`retry_stream_before_downstream_commit`、`finish_stream_error`、`mark_stream_downstream_committed`，以及 `src/anthropic/stream.rs` 的 `record_stream_error`/终止事件生成。

### 修复方向

- 为每次 upstream attempt 建立独立 assembler；retry 成功时清理旧 attempt 的事件、error、stop 状态；
- 只有向客户端写出合法首事件后才设置 committed；仅收到 HTTP 200 header 不得提交；
- 成功 attempt 必须满足 `message_start -> 至少一个 content block 或合法空响应 -> message_delta(stop_reason) -> message_stop`；否则在 committed 前继续 retry；
- committed 后不能再返回 HTTP 502，只能发单个规范 SSE `error` 或规范终止事件；
- 增加 429/500 首输出前重试的请求级测试，覆盖“前几次 200 但 EOF/空帧，后续 200 完整”的情况。

## I-03：一次上游 500 后重试 200，仍记录 stream_error

### 现象

request `req_01JUp92oAMpR7ESdfd4jvDNt`，endpoint `/dfcache/mcc/v1/messages`，`claude-opus-5-5`，stream=true：credential `2431` 返回 500，随后 `2432`、`2430` 返回 200；request_summary 仍为 `stream_error`。

### 根因假设与验证要求

与 I-02 同一状态机家族，但需要用该 request 的所有 `client_sse` 事件确认是否发生：重复 `message_start`、重复 block index、error 后继续正常事件、缺失 `message_stop`、usage 未 finalized。不能仅按 HTTP 状态判断成功。

### 修复方向

- retry 从 fresh stream context 开始，保留请求级 attempts 诊断，不保留旧流的协议事件状态；
- 把上游 500/429 分类为 pre-commit retry；
- 成功的完整重试清除 `stream_error`，但保留 `last_stream_error` 只用于 usage/日志；
- 对 `message_start`、`content_block_start/stop`、`message_delta`、`message_stop` 建立状态机断言。

## I-04：WebSearch 及 server tool 失败，开发实例已证明“外形完整”仍不足以通过

### 已确认的信号

- `client_sse` 中 `route=websearch` 共 157 个事件；
- `mcp_response_body` 共 4 个；
- 上游事件类型没有名为 `web_search` 的事件，说明本地把 MCP 搜索结果转换成 Anthropic server tool 事件；
- 基线 `server_tool=0`、`websearch=0`，不能仅凭请求数判断返回结构正确。

Round 02 的开发抓包进一步确认：普通 WebSearch 冒烟请求可以生成完整的 `server_tool_use`/`web_search_tool_result` 配对、连续 block index、`end_turn` 和 `message_stop`，但同一份代码在 cctest 仍为 `websearch=0`、`server_tool=0`。因此失败条件至少还包括请求来源、工具调用闭环、历史回传或更严格字段校验，不能只修 SSE 外形。

Round 04 已精确找到 cctest 的 WebSearch 子请求 `req_01ANsL8eKchpjQM3JF88CrX9`。它经过 Kiro upstream 和 MCP，最终 SSE 中 `server_tool_use.id`、`web_search_tool_result.tool_use_id` 完全匹配，usage 搜索次数为 1，结果非空，文本含 `web_search_result_location` 引用，最终 `end_turn/message_stop` 均存在。该请求仍未让 cctest 得分，因此“未启用模型驱动 WebSearch”已不再是当前 0 分的充分解释；剩余差异应从私有评分对文本 block 数量、历史回传、工具闭环或请求模式的要求继续定位。

### 待提取字段

按 requestId 对 `client_sse` 重建：

1. `message_start`；
2. 每个 `content_block_start` 的 `index` 和 `content_block.type`；
3. `server_tool_use.id/name/input`；
4. `web_search_tool_result.tool_use_id/content`；
5. 每个 block 的 delta/stop；
6. `message_delta.stop_reason` 和 final usage；
7. 是否有 `message_stop`、重复 index、error 后继续正常事件。

### 代码核对点

- `src/anthropic/websearch.rs` 的 stream/non-stream block 生成；
- `src/anthropic/stream.rs` 的 `server_tool_use` 与 `web_search_tool_result` block serializer；
- `src/anthropic/handlers.rs` 的 model-driven WebSearch 分支、MCP 失败映射和 usage finalization；
- `src/anthropic/converter/history.rs` 已将历史 server-tool/search block 渲染为文本，但不能替代当前响应中的结构化 block。

### 修复方向

先以实际失败序列为准，再最小化修改。优先检查：server tool id 是否以 `srvtoolu_` 开头且结果 `tool_use_id` 完全匹配；结果 block 是否在 server tool block stop 后出现；最终 stop_reason 是否为 `end_turn`；流式是否有完整 `message_stop`；usage 是否仍非零且没有被错误清零。

### Round 06 新证据

`req_01mjMPZXgmqaYg761dfGNjJa` 已逐事件重建：`server_tool_use → web_search_tool_result → text(多个 block) → message_delta(end_turn) → message_stop`。ID、结果、引用、usage 和终止事件均满足公开协议；评分仍为 0，说明剩余差异位于私有检查器的文本 block 拆分、历史回传、请求模式或额外字段检查。当前没有证据支持直接修改 WebSearch 事件顺序。

Round 05 的 `server_tool=0` 则已定位为原生代码执行工具输入 schema 缺失；Round 06 补齐 schema 后，`req_01FvsaydBLHGFxmA8zeuk17y` 生成 `code/language` 输入并返回 `return_code=0`，`server_tool` 升为 10。

### Round 07–09 新证据

- Round 07/08/09 的 WebSearch 分数连续为 0；Round 08 把十个连续 text block 合并为一个后仍为 0，已排除“多 text block 是唯一原因”。Round 09 把完整 query 放入 `server_tool_use` 的首个 `content_block_start` 后仍为 0，也不是充分原因。
- Round 09 抓包：`logs/llm-capture/capture-20261106T115125Z-7a33e1cf/events.jsonl`，request `req_01ywQjCszeZ3LLVSgoXiw2P2`。`server_tool_use.input.query` 在 `content_block_start` 已完整，但紧接着又发送相同 `input_json_delta`。这是明确的重复输入事件；Round 10 已移除 server tool 的增量输入事件，并保留普通 `tool_use` 的增量输入行为。
- Round 09 同一事件序列含非空 `web_search_tool_result`、配对 id、citation、`usage.server_tool_use.web_search_requests=1`、`end_turn` 与 `message_stop`；这些字段继续不能解释评分为 0。
- Anthropic 官方 TypeScript SDK `messages.ts`（提交 `590e9494f0214710614c1d6b929b07666f1e3080`，访问时间 2026-11-06 UTC）将响应 `ServerToolUseBlock.caller` 和 `WebSearchToolResultBlock.caller` 定义为必填字段，`DirectCaller` 是 `{"type":"direct"}`。Round 09 两个 block 都缺少 `caller`。这是目前最强的公开协议差异候选，尚未通过加字段实验验证是否为 cctest 0 分根因。
- 官方来源：<https://github.com/anthropics/anthropic-sdk-typescript/blob/590e9494f0214710614c1d6b929b07666f1e3080/src/resources/messages/messages.ts>。该 SDK 类型用于协议参照，不代表 cctest 私有判分规则。

### Round 10：WebSearch 子请求的独立失败链

Round 10 检测在 `websearch` 步骤失败，结果文件 `results/20261106T122556-round-10-server-tool-lifecycle.json` 只包含部分分数，不能用于比较 Round 09 的协议改动。Caddy 与开发实例此时均返回 401 冒烟响应，说明 502 是任务内部 Anthropic API 错误被 cctest 包装，不是反向代理断连。

抓包 `logs/llm-capture/capture-20261106T122544Z-dc32fb4f/events.jsonl` 显示两次测试子请求（`req_01YV63ZJtuqRxE36sicFh6fT`、`req_01W9cGCTLPsCTTREdrNNJg56`）的 Kiro HTTP header 均为 200，但在收到可解码的 eventstream 之前，`call_model` 的 body 读取/完整流解析路径返回 `websearch_model_call_failed`，最终 HTTP 502。随后另外一个 WebSearch 请求有完整回答。原始事件序列和响应体均留在抓包，不在文档复制。

同一抓包还记录到共享账号池中的一个 Kiro 凭据 402 月度额度耗尽并被自动禁用；另外两个凭据返回 403 暂停状态。该状态来自上游 Kiro 响应，开发实例和生产实例使用共享账号，因此记为外部账号池变化。此现象可能造成账户选择波动，但不是上述两个 `websearch_model_call_failed` 的直接原因；相关请求在收到任何 SSE 内容前即失败。

### Round 11：caller 实验被内部上游空响应阻断

结果 `results/20261106T124108-round-11-server-tool-caller.json` 在 WebSearch 阶段 HTTP 502 中断，不是完整评分轮。抓包 `logs/llm-capture/capture-20261106T124048Z-2b9d0560/events.jsonl` 有 1016 个事件、5,987,994 字节、零丢弃；上游状态统计为 200=27、402=16、403=3。

三个内部请求 `req_01YaD5qAPTaZwLhYZA1As4Lh`、`req_018RWrKuqVXpZqWsMtbPJYxs`、`req_01K6XVF66KeEbYAXT1GsGwwx` 都是上游响应头 200，request_summary 标记 `websearch_model_call_failed`，对应请求没有 `upstream_frame`。当轮也没有 `upstream_body`，所以空 body、JSON 错误体、截断 EventStream 和解码失败尚不能区分。

代码核对发现 WebSearch 内部读取完整响应体后没有调用公共抓包 helper，导致诊断路径缺失。已在该内部路径添加响应体采集，并让完整 EventStream EOF 校验保留未解码尾部字节诊断。下轮应能在 `upstream_body` 中区分空体/JSON/解码失败，或在 `upstream_frame` 中看到实际帧。只有上游返回完整响应后，cctest 才能检验 Round 11 的 `caller` 字段效果。

### Caddy 与版本状态（2026-11-06 复核）

`https://kirotest.okmcode.com/v1/models` 无凭据返回 HTTP 401，响应包含 `via: 1.1 Caddy`、`server: cloudflare`，TLS 验证通过；同一公网入口已承载多轮 cctest 请求。因此 `kirotest` 站点可访问，裸 curl 的 401 是 API key 鉴权，不是 Caddy 连接失败。Round 10/11 的 502 来自站内 WebSearch 上游模型调用。

59138 容器 `ghcr.io/2ue/kiro-rs:latest` 的镜像 OCI version 为 0.0.239、revision 为 `30edc00c12431371e22498298715adc7bea539fd`；工作区 HEAD 为 `2e625e52`、crate version 为 0.0.240。因此 59138 没有部署当前源码 HEAD。此处仅读镜像元数据和容器状态，没有重启或修改生产容器。

### Round 19–21：固定模型与账号池边界

- Round 19 使用 `claude-opus-4-8` 得到 `68/110`；其中 `output_config=10`、`signature_proto=0`、`model_attribution=0`，证明不同模型会改变采样项结果。
- Round 20 固定 `claude-opus-5-5` 得到 `73/110`；抓包 `capture-20261106T171520Z-ca1025dd` 有 27 个 HTTP 200 上游响应，没有 429/500/502。
- Round 21 固定同一模型连续三次得到 `73/73/82`；抓包 `capture-20261106T172613Z-0121b911` 有 75 个 HTTP 200 上游响应。`output_config` 为 `0/0/10`，是当前最明确的随机项；`stream_structure=5`、`signature_proto=5`、`websearch=0` 三次稳定失败。
- 账号清理只在 59139 独立数据库进行。后续仍只删除有明确认证失败、月额度耗尽或永久暂停证据的开发账号；单次 429、暂时冷却、普通网络错误不作为永久剔除依据。
- Caddy/Cloudflare 链路已排除为当前失败根因；59138 仍只读，且版本落后工作区 HEAD。

### I-07：thinking SSE 的隐藏字段组合

Round 21 的请求级抓包显示 thinking block 形状为：`content_block_start(type=thinking, thinking="", signature="")`，随后存在一个或多个空 `thinking_delta`，部分 `thinking_delta` 带 `estimated_tokens=50`，最后使用 `signature_delta` 传签名并关闭 block。该组合与 Claude Code beta 兼容逻辑有关，但 cctest `stream_structure` 固定为 5/10。

目前只能确认它是高优先级差异候选，不能直接删除字段：项目已有 `thinking-token-count-2026-05-13` 兼容逻辑，且官方协议要求 signature 在 thinking block 结束前通过 delta 发送。应按 `estimated_tokens`、空 delta、start signature 三个变量分别实验，并保留 Claude CLI 回归证据。

### I-08：WebSearch opaque 字段未被证明等价

Round 21 的三个内部 WebSearch 请求均返回 HTTP 200，并产生完整 server tool/result 生命周期、10 个结果、引用、最终 usage 和 `message_stop`，但 `websearch=0`。当前 `encrypted_content` 是明文 snippet，`encrypted_index` 是代理自定义的 Base64(`number:url`)；官方 SDK 只规定二者为字符串 opaque 字段，未公开生成算法。

因此当前应把 opaque 字段作为高概率私有检查候选记录，不能随机生成值冒充官方格式。下一步先对比真实 Anthropic/Claude 抓包或官方 SDK 行为；若无可验证算法，则保留现状并在结论中标注未证实。

### I-09：output_config 评分具有采样随机性

Round 21 三次固定模型、代码不变、上游全 200，`output_config` 仍为 `0/0/10`。抓包中请求都将结构化输出映射到内部 `structured_output` 工具，再还原成 text JSON。单个响应可合法解析 JSON 不能解释整轮分数；必须记录每个结构化输出请求的输入 schema、内部 toolUseEvent、最终 text、stop_reason 和采样轮次，避免把模型输出波动误当作链路或账号问题。

### Round 13：输入增量生命周期单变量对照仍未改善

结果文件：`results/20261106T134159-round-13-input-json-delta.json`；抓包：`logs/llm-capture/capture-20261106T134142Z-43dcb476/events.jsonl`。

- 总分仍为 `73/110`，`websearch=0`、`stream_structure=5`、`signature_proto=5`、`output_config=0`，其余保护项保持不变。
- WebSearch 子请求 `req_01gRFXbBxBGKdNcDMXtLE6ZY` 完整成功：`message_start` → `server_tool_use(input={})` → 单条 `input_json_delta` → `content_block_stop` → `web_search_tool_result` → text/citations → `message_delta(end_turn, web_search_requests=1)` → `message_stop`。
- `caller={"type":"direct"}` 已同时存在于 `server_tool_use` 和 `web_search_tool_result`；结果数量为 10，引用数量为 10，ID 配对和 block index 连续。
- 只改变 server tool 输入生命周期而分数完全不变，排除“缺少输入增量事件是 WebSearch 0 分的唯一根因”。
- 仍未解决的强候选是字段语义而非事件顺序：`web_search_result.encrypted_content` 当前直接放入明文 snippet；`web_search_result_location.encrypted_index` 当前是代理自定义的 Base64(`number:url`)。官方 SDK 只定义这些字段为字符串，要求它们在协议上是不透明值，不能证明当前自定义编码满足 cctest 私有检查。
- 下一轮保持事件生命周期不变，只对 `encrypted_content` / `encrypted_index` 做最小单变量实验；若不能从官方资料确定可生成值，则先增加非流/流字段集合对照和抓包统计，不盲目伪造随机“加密”值。

## I-05：普通工具调用需要完成请求级闭环重建

基线 `server_tool=0` 只代表服务端工具检查失败，不能直接等同于普通 client tool_use 失败。抓包有大量 `toolUseEvent`，并且生产日志已记录 `local_inferred_tool_use`。需要按请求抽取：

- `tool_use` block 的 id/name/input JSON 是否完整；
- delta JSON 是否可拼接；
- `content_block_stop` 是否与 index 一致；
- `message_delta.stop_reason` 是否为 `tool_use`；
- 下轮 `tool_result.tool_use_id` 是否匹配且只出现一次；
- tool_use 后是否意外追加 `message_stop` 前的普通 text。

初步修复方向是先补事件级诊断和回归测试，禁止通过修改工具 id 或硬编码 stop_reason 掩盖上游断流。

Round 02 已确认 7 个普通工具请求的首轮 `tool_use` block、工具名、ID 和 `stop_reason=tool_use` 均存在；这仍不能证明 cctest 的 server_tool 检查通过。待核对的关键差异是：下一轮 `tool_result.tool_use_id` 是否逐个匹配、工具 input 是否为完整 JSON、是否出现并行工具调用与 `disable_parallel_tool_use` 冲突，以及 cctest 是否把普通 client tool 与 server tool 分开计分。

Round 04 的普通工具历史请求包含 3、5、9、11、15、17、19、21 条消息，首轮 tool_use 的 input JSON 分片均可拼接，`stop_reason=tool_use`。当前抓包未发现与这些 tool_use 对应的下游 `tool_result` 请求被 cctest 单独纳入检查，因此仍不能把普通工具闭环当作已通过。

## I-06：output_config 评分为 0，但当前响应文本已证实是合法 JSON

Round 02 请求 `req_01mYVjzpTMyY4wdNULamUFu8` 的输入含 `output_config.format.type=json_schema`，schema 要求 `identity_platform` 和 `desc`。响应 SSE 只有一个 text block，拼接 145 个 `text_delta` 后可直接 `json.loads`，顶层键正好匹配 schema，`stop_reason=end_turn`，并有 `message_stop`。上游同时出现内部 `structured_output` toolUseEvent，说明本地 structured-output 重写路径确实执行。

已排除：响应是普通自然语言、JSON 尾随垃圾、顶层键缺失。

尚未确认：cctest 是否要求非流响应；是否验证 schema 的 `additionalProperties=false`、类型和枚举；是否要求响应中的 `output_config` 语义保留到某个 usage/头字段；是否对内部 tool 转换产生的流式 text block做不同判定。下一步必须用一组最小请求分别覆盖 `stream=false/true`、有无工具、`output_config` 与顶层 `output_format`，并保留请求级抓包。

### Round 06 新证据

`req_019m2NKVf1kFCujVKoRpKHSW` 的响应只有一个 `text` block；拼接结果可解析为对象，顶层键严格为 `identity_platform`、`desc`，`identity_platform=kiro` 在枚举内，`additionalProperties=false` 没有被违反；流尾为 `message_delta(end_turn)` 和 `message_stop`。私有评分仍为 0，因此下一步需要对比 `stream=false`、旧版 `output_format`、thinking 与无 thinking 的组合，确认评分器实际命中哪条请求。

## 记录规范

后续每轮把新抓包放仓库外，只在本文件增加脱敏统计、requestId、事件序列摘要、代码版本和结论。不得复制 4 GiB 原始抓包，不得记录凭证、token、完整系统提示词或用户正文。

### Round 14–16：失效账号池造成的独立 502 阻断

- Round 14（`results/20261106T143547-round-14-account-pruned.json`）和 Round 15（`results/20261106T144524-round-15-pruned-accounts.json`）均明确使用 `kirotest.okmcode.com`；首项 `tag` 在 3 次目标请求重试后返回 Cloudflare 502。Round 14/15 的本地 usage 记录和脱敏抓包显示：每轮 6 个请求分别经历 60 次上游尝试，Round 14 为 402=56、403=4，Round 15 为 402=60，最终没有成功 SSE。
- 应用把这些响应分类为 `quota_exhausted`/`temporarily_suspended` 并设置 `disabled=true`。只在 59139 独立数据库软删除了抓包明确失败且已禁用的账号；没有改 59138 生产数据库或账号状态。
- Round 16 抓包 `logs/llm-capture/capture-20261106T144740Z-ffe86528/events.jsonl` 仍有 57 次 402，4 个请求最终 502，但其余 23 个请求完成，cctest 得到完整 73/110。因此账号池失败会造成检测中断，协议分数必须只用成功 request 评估。
- 新增脚本保护：`scripts/cctest.py check --target-base` 明确覆盖目标，且拒绝非 `kirotest.okmcode.com` 主机；默认也固定为开发域名，避免仓库外 `.env.local` 的生产默认值把检测误投到 59138。

### I-10：Round 25–27 的 thinking 字段实验均未改变评分

- Round 25 结果 `results/20261106T185313-round-25-null-estimate.json`，总分 `73/110`；抓包 `logs/llm-capture/capture-20261106T185102Z-3d6f7251/events.jsonl`。新增 `estimated_tokens: null` 后，`stream_structure=5`、`signature_proto=5` 不变。
- Round 26 结果 `results/20261106T190307-round-26-no-plain-null.json`，抓包 `logs/llm-capture/capture-20261106T190258Z-4f375874/events.jsonl`。关闭过滤器生成的普通空 delta 后，分数仍为 `73/110`。
- Round 27 结果 `results/20261106T191423-round-27-all-estimate-fields.json`，抓包 `logs/llm-capture/capture-20261106T191423Z-9de0cf6e/events.jsonl`。过滤器在 beta 下统一发送数值或 null 后，分数仍为 `73/110`。
- Round 27 请求级抓包显示，少量不带 `estimated_tokens` 的空 thinking delta 来自 Claude Code noop keepalive，不在 `OmittedThinkingFilter` 内；因此“所有 thinking delta 字段统一”尚未完成，但过滤器字段本身已完成单变量对照。

结论：字段形状是协议差异候选，不是当前采样项的充分根因。下一步应单独验证 keepalive noop 是否应发送带 `estimated_tokens: null` 的 thinking delta，或改发 `ping`；该变量必须与过滤器逻辑分开记录。

### I-11：59139 开发账号池中的禁用账号已清理

Round 26 检测前只读快照显示开发数据库共有 129 个凭据，其中 120 个已禁用且剩余额度为零或已过期，9 个仍可调度。快照保存在仓库外 `/tmp/kirotest-dev/artifacts/disabled-accounts-before-delete-20261106T1900Z.json`，只含本地诊断用途的账号状态数据。

随后只对 59139 调用 `DELETE /api/admin/credentials/disabled`，返回 `totalMatched=120`、`success=120`。删除对象均已是禁用状态，未触碰 59138；59138 未调用 Admin 写接口、未写 PostgreSQL/Redis。今后仍不得因单次 429、临时冷却、普通网络错误删除账号。

### I-12：开发模式可用于循环验证，不需要每轮 release 冻结编译

Round 27 使用 `feature/tests/run-cargo-scoped.sh cctest-kirotest-dev -- cargo build --bin kiro-rs -j2` 进行增量开发编译，随后只替换 59139 二进制并重启开发进程。资源监控显示可用内存约 9 GiB，生产 59138 端口和进程保持不变。release 编译、fmt、clippy、test 仍留到满分后的最终 gate。

### I-13：Round 29–33 的采样结果不能用整轮总分归因

Round 29–33 的结果矩阵如下：

| 轮次 | 模型 | 总分 | 流结构 | 签名 | WebSearch | 结构化输出 | 上游状态 |
|---|---|---:|---:|---:|---:|---:|---|
| 29 | `claude-opus-4-8` | 73 | 5 | 0 | 0 | 10 | 26/26 为 200 |
| 30 | `claude-opus-4-8` | 68 | 5 | 0 | 0 | 10 | 25/25 为 200 |
| 31 | `claude-opus-4-8` | 68 | 5 | 0 | 0 | 10 | 25/25 为 200 |
| 32 | `claude-opus-4-8` | 68 | 5 | 0 | 0 | 10 | 26/27 为 200，1 次 402 |
| 33 | `claude-opus-4-8` | 73 | 5 | 0 | 0 | 10 | 26/26 为 200 |

Round 29 的 `output_config=10`、Round 33 的 `model_attribution=5` 与代码单变量无关，因为模型响应和采样路径不同。Round 30/31 的完全相同结果说明 `discardHistoricalThinking` 不是分数下降原因。以后比较必须使用分项、模型、上游状态和抓包窗口，不能只比较总分。

### I-14：Round 33 WebSearch 公开事件完整，但 opaque 字段仍不等价

抓包 `logs/llm-capture/capture-20261106T205417Z-4984b328/events.jsonl` 中，WebSearch 请求 `req_01w2txi4CRZq3SGPhPcboUsT` 的事件重建为：

```text
message_start
→ text block
→ server_tool_use(index=1, input={})
→ input_json_delta(query)
→ content_block_stop(index=1)
→ web_search_tool_result(index=2, matching tool_use_id)
→ text block + citations_delta
→ message_delta(stop_reason=end_turn, web_search_requests=1)
→ message_stop
```

两个 server block 都有 `caller={"type":"direct"}`；结果 10 个、引用 10 个；23 个流全部通过本地状态机检查，没有重复 `message_start`、缺失 `message_stop`、未知 block index 或 error 与成功事件混排。该证据排除了公开流生命周期和 caller 缺失作为 WebSearch 0 分的充分根因。

剩余强候选：

- `web_search_result.encrypted_content` 直接使用 MCP 明文 snippet；
- `web_search_result_location.encrypted_index` 使用代理自定义 Base64(`序号:URL`)；
- cctest 可能检查搜索结果历史回传或官方 opaque 值语义。

Anthropic 官方 SDK 只约束这些字段为字符串，不公开生成算法。没有真实官方 fixture 前，不随机替换字段，也不把自定义编码写成“已兼容”。

### I-15：上游模型的 opaque thinking signature 按模型变化

Round 33 的 `claude-opus-4-8` 上游 `reasoningContentEvent.signature` 原样下发，抓包中 Base64 前缀为 `Ep...`；Round 28 的 `claude-opus-5-5` signature 前缀为 `CAQS...`。两者都包含 protobuf 风格二进制数据，不能根据前缀或长度互相转换。

补充的只读线格式检查：对 Round 33 的 9 个、Round 45 的 12 个上游 signature 做 Base64 解码后，通用 Protobuf wire-format 解析均能完整走到末尾；Round 33 顶层字段布局为 field 2/3，Round 45 为 field 1/2/3。它只证明两组字节不是 Base64 损坏或明显非法 wire encoding，不证明符合 Claude/Kiro 的具体 schema，也不验证签名与 thinking 内容的密码学绑定。

Round 33 `signature_proto=0`、Round 28/45 `signature_proto=5` 与模型输出格式变化相关，不能据此删除真实签名或改发 proxy marker。代理生成的 synthetic signature 只能作为 Claude Code 兼容传输标记，不能满足 cctest 对 Kiro opaque signature 的私有验证。

### I-16：账号池补充条件已具备，但当前开发池未不足

当前 59139 开发实例仍有 9 个可调度账号，未出现新的容量阻断；当前不从 59138 复制账号。若后续出现容量不足，只允许从 59138 做只读导出，逐条保留账号与绑定代理关系，再写入 59139 独立存储。不得在 59138 删除、禁用、刷新、调整优先级或配额，也不得写 PostgreSQL/Redis。

本轮只读核对：59138 健康检查 HTTP 200，监听仍为 59138，容器镜像 `0.0.239` / revision `30edc00c12431371e22498298715adc7bea539fd`；59139 健康检查 HTTP 200。生产容器没有重启或配置变更。

### I-17：固定 Opus 5.5 后签名稳定为 5，但三项仍连续失败

Round 34/35/36 固定 `claude-opus-5-5`，结果为 `73/110`、`73/110`、`73/110`、`73/110`。签名从 Opus 4.8 的 0 恢复为 5，模型归因恢复为 10；流结构始终 5，WebSearch 始终 0，结构化输出在 Round 35/36 始终 0。Round 36 两轮上游有 50 次 200 和 1 次 500，500 在首输出前重试后请求成功，不能解释评分失败。

这组固定模型证据把“单轮模型切换造成全部失败”排除为主因。当前主线应聚焦：

- `stream_structure` 的私有事件组合检查；
- WebSearch 的 opaque 字段/历史闭环；
- `output_config` 的 effort 与 schema 请求组合。

### I-18：Round 37 补齐标准 usage 可选字段，但评分不变

Round 37 结果 `results/20261107T014855-round-37-usage-fields.json` 为 `73/110`；抓包 `logs/llm-capture/capture-20261107T014843Z-e30c7f4b/events.jsonl`。代码在标准最终 usage 中增加 `output_tokens_details:null` 与 `server_tool_use:null`，thinking/WebSearch 请求分别覆盖对应字段。22 个客户端流的 `message_start`、`message_delta` 都带字段，25 个上游请求全部 200，但 `stream_structure=5`、`websearch=0`、`output_config=0`、`signature_proto=5` 均未变化。

结论：这些 SDK 可选字段的公开形状不是当前私有评分项的充分根因，停止继续围绕 usage null 字段微调。

### I-19：Round 38 非流 WebSearch caller 已补齐，但评分不变

Round 38 结果 `results/20261107T020647-round-38-websearch-caller.json` 为 `73/110`；抓包 `logs/llm-capture/capture-20261107T020634Z-2b78f498/events.jsonl`。非流 `server_tool_use` 与 `web_search_tool_result` 增加官方 SDK 类型要求的 `caller: {"type":"direct"}`；本轮客户端生命周期完整、上游 25 次全部 200，但 `websearch` 仍为 `0/10`。

结论：本轮采样主要命中流式 WebSearch，或 `caller` 不是隐藏评分的充分条件。保留该公开协议修复，不再把 caller 当作 WebSearch 根因。

### I-20：Round 39 结构化输出非法保活已修复，但评分不变

Round 39 结果 `results/20261107T022433-round-39-structured-keepalive.json` 为 `73/110`；抓包 `logs/llm-capture/capture-20261107T022419Z-a6c7dfaf/events.jsonl`。内部 `structured_output` 工具被重写为下游 text block 后，原先可能落入 text block 的空 `input_json_delta` 改为标准 `ping`；结构化请求 `req_01BvdTgpkUy53Zt4vQTKY4uU` 的所有 delta 均为 `text_delta`，JSON 可解析，`stop_reason=end_turn`，但 `output_config` 仍为 `0/10`。

结论：该轮修复了公开 SSE 类型错误，却不是隐藏 `output_config` 评分的充分根因。后续应对比结构化请求的语义输出与采样条件，不能把合法 JSON 单例直接当成通过条件。


### I-21：59139 监听地址必须对 Caddy 容器可达

Round 40 前两次任务在 Cloudflare 返回 502，失败结果分别为 `results/20261107T030549-round-40-thinking-start.json` 和 `results/20261107T030723-round-40-thinking-start-retry.json`。根因是开发进程未绑定到 Caddy 容器可访问的接口；以 `KIRO_RS_HOST=0.0.0.0` 启动后，第三次完整任务成功。Caddy 配置无需改动，59138 未受影响。后续启动 59139 都要保留该环境变量，并检查 `ss -ltnp` 显示 `0.0.0.0:59139`。

### I-22：Round 40 移除 thinking start 空 signature 后评分不变

Round 40 成功结果为 73/110；Round 41 抓包验证 start 只含 `type=thinking` 与空 `thinking`，真实签名仍在后续 `signature_delta` 发送。Round 41 每个含 reasoning 的 block 都有对应签名事件，完整 SSE 生命周期通过，但 `stream_structure=5`、`signature_proto=5` 未变。公开事件外形不再支持继续猜测 start 字段作为这两个分项的充分根因。

### I-23：Round 41 抓包排除公开 SSE 生命周期缺失

抓包 `logs/llm-capture/capture-20261107T031657Z-b2493f4f/events.jsonl` 共 9,593 个有效 JSON 事件、0 丢弃、0 上游 HTTP 错误。23 个客户端流均有完整 message 生命周期；block index 连续、start/stop 配对，WebSearch 和 tool delta 也完整。尽管如此，本轮四个失败项仍是 `stream_structure=5`、`websearch=0`、`signature_proto=5`、`output_config=0`。这些失败不能用公开 SSE 事件缺少终止帧或上游 HTTP 状态解释。

### I-24：identityPrompt 单变量没有改善 output_config

Round 42 只在 59139 的测试路由开启 `identityPrompt`，preview 与上游抓包都确认文本确实送达；结构化响应仍返回 `identity_platform=kiro`，`output_config` 仍为 0。与 Round 07 对照时，模型、用户消息 SHA-256（`4e5f21de2649fc6e84b4c9e6706a307611c1db08b6c6f669da57604dc823303a`）及 schema 字段和 enum 相同，但 Round 07 实际返回 `identity_platform=claude_code` 并得 10，Round 42 返回 `kiro` 并得 0。

先前将 Round 07 的值记为 `kiro` 是抓包复核错误，已由 Round 07 原始响应纠正。该单变量实验只证明 identityPrompt 没有把模型输出稳定改成 `claude_code`；评分条件仍需结合跨轮样本判断，不能再引用“同为 kiro、分数不同”作为排除身份语义的证据。

### I-28：output_config 评分与结构化响应的身份语义高度相关

全量抓包与完整结果按时间窗口、requestId 和 `output_config` 分项重新关联后，共有 40 个符合相同 schema 与用户消息哈希的抓包请求，其中 38 个配对到完整 CCTest 分数。所有 40 个响应均为单一 text block，拼接结果均为合法 JSON，键集合、类型、必需字段、`additionalProperties=false` 与 enum 均满足入参 schema。Kiro 上游请求包含 `structured_output` 工具，响应帧中该工具调用存在，代理再将其还原为 Anthropic text block。

| 抓包 / 样本 | CCTest `output_config` | `identity_platform` | JSON/schema 形状 |
|---|---:|---|---|
| Round 02、04–06、08–09、12–13、16、18、20 | 均为 0/10 | 均为 `kiro` | 合法，严格匹配 schema |
| Round 07 | 10/10 | `claude_code` | 合法，严格匹配 schema |
| Round 19 | 10/10 | `claude_code` | 合法，严格匹配 schema |
| Round 21 第 1、2 次 | 0/10、0/10 | `kiro`、`kiro` | 合法，严格匹配 schema |
| Round 21 第 3 次 | 10/10 | `claude_code` | 合法，严格匹配 schema |
| Round 23–28 | 均为 0/10 | 均为 `kiro` | 合法，严格匹配 schema |
| Round 29–33 | 均为 10/10 | 均为 `claude_code` | 合法，严格匹配 schema |
| Round 35、Round 36 两次、37–39、41–45 | 均为 0/10 | 均为 `kiro` | 合法，严格匹配 schema |

Round 10/11 的检测未完成，没有可比较的 `output_config` 分数；Round 43 非流重放不是 CCTest 样本，但 Round 43 完整检测纳入。38 个完整配对样本中，8 个 `claude_code` 响应全部得 10，30 个 `kiro` 响应全部得 0。另有 2 个中断请求，共 40 个抓包请求。模型分层为 Opus 4.8 的 6/6 个 `claude_code` 响应得 10，Opus 5.5 的 2 个 `claude_code`/10 与 30 个 `kiro`/0；Opus 5.5 Round 21 同代码、同模型、同入参还出现 `0/0/10`。因此模型或身份输出本身都不足以解释全部评分变化。“schema 字段丢失”“JSON 无效”或“结构化工具未调用”不能解释已观察差异；响应语义与采样路径仍是候选，因果关系未证实。

通过样本的 8 个 `desc` 都提到 Claude Code、Kiro 和身份冲突；30 个失败样本都自称 Kiro，其中 21 个也提到 Claude Code、18 个明确谈及身份冲突。结构化输出重写器把上游工具 input 转成 JSON text block，没有改写身份值。Round 42 开启的默认 `identityPrompt` 文本本身还包含“后续指令指定其他身份时服从后续指令”的让步句，因此该轮不能证明更清晰的调用方/后端身份区分提示无效。它也不能证明 CCTest 明确按 `identity_platform` 单字段打分；在没有公开评分规则前，不把响应枚举硬编码成固定值。

生产只读抓包提供了一个重要对照：同一结构化 schema、同一消息哈希的生产请求在 Opus 4.8 上曾返回 `claude_code`；请求系统消息含真实 Claude Code 标识和 `cc_version/cc_entrypoint`，上游 Kiro conversation history 也保留了 Claude Code 身份块。后续 I-29 复核发现该身份块并非与 59139 独有的上下文差异：Round 42 已向上游发送相同块，但 Opus 5.5 仍返回 `kiro`。因此生产样本只能作为上下文线索，不能直接复制到开发路由或单独解释评分差异，更不能写入 59138。

置信度：身份语义与分数的相关性高；评分器是否明确要求 `claude_code`、是否也评价 `desc`，目前没有公开判分细节可证实。Round 42 开启 identityPrompt 仍返回 `kiro`，所以现有证据不支持继续调 JSON 转换或硬编码某个字段值。保持结构化输出通用行为不变，后续若要验证此项，应以 CCTest 官方公开规则或同请求可控评分证据为依据。


### I-25：identityTurnReminder 让非流响应增加 thinking block

Round 43 只开启 `/dfcache/tune-ss6-99` 的 identityTurnReminder，总分由原始配置常见的 73 降至 64；唯一变化的评分分项是 `non_stream`，从 10 变成 0。完整抓包中 22 个流仍完整、25 个上游响应全为 200。

使用抓包中的同一非流请求 `req_016bx122B9G6RrPomwDyK5V7`（用户消息 `1+1=?`）在 59139 做 A/B：关闭 reminder 时返回单一 text block `1+1=2`；开启时返回 thinking 和 text 两个 block，可见文本为 `2`。两次均为 HTTP 200、`stop_reason=end_turn`。这解释了非流检查可能不接受增加的 thinking block，但 CCTest 私有判分条件未公开，结论仍属高相关候选。

已恢复原路由配置，identityPrompt 和 identityTurnReminder 均为关闭。配置项不保留；先跑原始配置基线确认分数恢复。


### I-26：identityTurnReminder 退化可通过配置回滚恢复

Round 44 恢复原始路由配置后完整检测回到 `73/110`，`non_stream` 从 Round 43 的 0 恢复为 10；其余分项与 Round 42 一致。Round 43 的单变量提示导致非流回答多出 thinking block，是非流回归的强候选原因。identityPrompt 和 identityTurnReminder 均保持关闭。


### I-27：Round 41/44 的真实 signature 均逐字节透传

对照完整抓包中的上游 `reasoningContentEvent.signature` 与下游 `signature_delta.signature`，Round 41 的 8/8 和 Round 44 的 8/8 SHA-256 完全一致；所有值均以 `CAQS` 开始，未发现 synthetic signature 或代理重编码。两轮 `signature_proto` 仍为 5/10，因此当前证据排除代理改写签名作为失败原因，但不能说明 verifier 对哪些样本判为失败。

这些请求使用 `thinking.type=adaptive`，没有显式 `thinking.display`；Opus 5.5 默认 omitted，故下游 thinking delta 内容为空而签名仍保留。这与隐藏显示路径一致，不能仅凭空文本判定签名丢失。下一步应确认 `signature_proto` 的 10 个评分样本和实际产生签名的请求之间如何对应；没有 verifier 规则或有效官方 fixture 前不改 opaque 字节。

### I-29：生产 identity 块不是 output_config 的单独根因

为复核 I-28 中生产 history 的差异，对 59138 原始抓包做了只读扫描，并按 schema SHA-256 `93136f64e9d34e3d5bfdaee2e7d0d04173d03b435325748bea2dd9fbe13f77c0`、messages SHA-256 `4ee3036002a9304a616fc15d085908446751e3282b658acefc09d684db2a23cb` 精确筛选。文件为 4,294,967,184 字节、459,688 行，JSON 错误 0；未复制、删除或修改原始抓包。

生产有两条匹配请求，均为 `stream=true`、`claude-opus-4-8`；system 含 Claude Code 标识、`cc_version`、`cc_entrypoint`。两条 Kiro 请求的 history 都是 user 后接 assistant，首条 user history 含 `<assistant_identity version="v1">`。块内文本长度为 218，SHA-256 为 `31a16d8005a04487282edf7ebfd2a08e8a111293c4d15296ec5af36d53919989`。两条中一条下游为合法 JSON 且 `identity_platform=claude_code`，另一条无法解析为合法 JSON，不能算通过。生产请求无法与一份确定的 CCTest 评分结果逐条配对。

和本地轮次对照：

| 样本 | 模型 | 首条 Kiro history 含 identity 块 | 身份输出 / 分数 |
|---|---|---|---|
| Round 07 `req_01TBQpxnhwntXvWGkVC6fwnh` | Opus 5.5 | 否 | `claude_code` / 10 |
| Round 42 `req_01ZnhtqgFMr23xDcJwBv1HTD` | Opus 5.5 | 是；块长度与 SHA-256 和生产完全相同 | `kiro` / 0 |
| Round 47 `req_01LBzJ7U9AN775n3z3q3LnFN` | Opus 5.5 | 否 | `kiro` / 0 |

Round 42 是关键反证：identity 块确已进入 Kiro history，但没有改变 Opus 5.5 的输出或评分。Round 07 又在没有该块时通过。故 identity 块的有无不能单独解释分数，也不支持把它强行注入所有结构化输出请求。Round 47 与生产样本之间仍有模型差异，生产分数也未能和请求可靠配对，现有证据不能分离模型采样、其他上下文和分数之间的因果关系。

结论：降低“生产 identity 块是下一步首要改进”的优先级；不硬编码响应字段，不向普通结构化输出请求强制添加该标记。继续通过同模型、同输入和同路由的请求级重复样本记录上游 body 与 JSON 语义；只有身份字段和得分再次稳定共变，才做针对性提示实验。

### I-30：Round 49 抓包复核仍未找到四个失败项的直接修复证据

结果：`results/20261107T083331-round-49-captured-identity-context.json`，2026-11-07 08:33–08:36 UTC（北京时间 16:33–16:36），模型 `claude-opus-5-5`，总分 `73/110`。四个目标项仍为 `stream_structure=5`、`websearch=0`、`signature_proto=5`、`output_config=0`；保护项无变化。

完整开发抓包为 `logs/llm-capture/capture-20261107T083321Z-f51516a7/events.jsonl`，9,690 行、8,639,706 字节，JSON 错误 0、丢弃 0；25/25 上游响应 HTTP 200，22 个客户端流的 `message_start/message_delta/message_stop` 各 22 次，54 个 block start/stop 配对且 index 连续。没有上游错误、断流或缺少终止事件可解释分数。

WebSearch 子请求 `req_0196Yw1aARTPnQ2M77gj3sbS` 的下游序列完整：`server_tool_use(index=0, input={})` → 单条 `input_json_delta` → stop → `web_search_tool_result(index=1)` → 空 `ping` → text/citations(index=2) → `message_delta(end_turn, web_search_requests=1)` → `message_stop`。server/result 都有 `caller=direct`，id 匹配；10 个 citation delta；上游 3 次请求均为 200。评分仍为 0，因此本轮只确认公开生命周期，不足以区分 cctest 的 opaque 字段、结果内容或历史回放判分条件。

本轮有 9 个对外 `signature_delta`，均与对应上游 `reasoningContentEvent.signature` SHA-256 一致。另有 5 个完整请求上游含 reasoning signature，但下游因本请求不是可见 thinking 输出而未发送 signature_delta；没有证据显示代理篡改或丢失了需要下发的签名。`signature_proto=5` 仍无法从公开数据解释。

结构化输出请求 `req_01kfpvZrddVgog6Vcrt8V2nq` 是单一 text block，JSON 合法且 `identity_platform=kiro`；评分 0，与此前样本一致。不同于 Round 47，本轮恢复默认 `structured_output` 工具描述，未发生代码提示变更。

结论：Round 49 没有支持新的协议修复，保持代码不变。后续测试应聚焦于能够改变判分的单变量证据：获取 WebSearch `encrypted_content/encrypted_index` 的真实官方 fixture；定位 signature verifier 采样的请求与失败分类；对结构化身份样本按同模型和同请求固定语义做受控提示矩阵。不要把结构合法或一轮结果当成私有判分条件。

### I-31：Round 50 公开结构完整，但三项失败保持不变

Round 50 结果为 `73/110`，抓包 `logs/llm-capture/capture-20261107T085652Z-dd65a1e1/events.jsonl` 共 10,368 行、JSON 错误 0。22 个客户端请求和 25 个上游请求全部 HTTP 200；22 个流的 `message_start/message_delta/message_stop` 均完整，`content_block_start/stop=54/54`。`stream_structure=5`、`websearch=0`、`signature_proto=5` 仍未变化。

本轮没有代码变量，排除了“Round 49 抓包窗口或开发循环不稳定”这一解释，但没有提供新的失败根因。WebSearch 仍需真实 opaque 字段 fixture 或评分级证据；signature 仍不能离线生成；流结构继续按私有采样差异处理。抓包全部保留。

### I-32：Round 51 条件式 identity/platform guidance 使 output_config 得分上升

Round 51 在 `src/anthropic/structured_output.rs` 中增加条件式工具描述：只有 schema 属性名含 `identity` 或 `platform` 时，才要求模型区分调用方运行平台与模型提供方，并依据显式系统/请求元数据判断。普通 schema 描述保持不变，代码没有硬编码 `claude_code`。

结果 `results/20261107T091814-round-51-identity-schema-guidance.json` 为 `82/110`，唯一分数变化是 `output_config: 0→10`。抓包 `logs/llm-capture/capture-20261107T091814Z-596c8a7c/events.jsonl` 中结构化请求 `req_01BK4d5vVxS3tVnwzRWUK3A8` 返回合法 JSON，`identity_platform=claude_code`；22 个流完整，25/25 上游 HTTP 200，50 个 block 成对。

这是一次完整的单变量强相关实验：guidance 进入上游工具描述，响应身份值改变，评分同步上升。Round 52 已在代码不变时复现 `output_config=10`，见 I-33。两轮证据支持保留条件式 guidance，但不代表私有判分器的完整规则已经公开，也不允许把字段值硬编码到代理。

### I-33：Round 52 同代码复现 output_config 满分

Round 52 结果 `results/20261107T093827-round-52-guidance-recheck.json` 与 Round 51 相同，为 `82/110`；`output_config=10`，其他项目无变化。抓包 `logs/llm-capture/capture-20261107T093736Z-0dd4aa47/events.jsonl` 共 9,112 行、8,405,507 字节、JSON 错误 0、丢弃 0；25/25 上游 HTTP 200，22/22 客户端流完整，52 个 block start/stop 配对。

结构化请求 `req_01QAF9V59PcAg8eZxr12yXdM` 返回合法 JSON，`identity_platform=claude_code`。更新脱敏请求关联样本后，44 个完整配对中 `claude_code` 10/10 得分、`kiro` 34/34 失分。由此确认 Round 51 的条件式 guidance 在同代码第二轮可复现；仍不能将相关性解释成评分器仅检查单一字段。

### I-34：Round 53 再次复现 output_config，noop thinking keepalive 字段仍未定位流结构评分

Round 53 结果 `results/20261107T101524-round-53-guidance-recheck.json` 为 `82/110`，与 Round 51/52 完全相同。抓包 `logs/llm-capture/capture-20261107T101523Z-d8aa9f7d/events.jsonl` 共 10,108 行、8,813,674 字节，JSON 错误 0、丢弃 0；25/25 上游 HTTP 200，22 个客户端流完整，51/51 block start/stop 配对。结构化响应仍返回合法 JSON 和 `identity_platform=claude_code`，`output_config=10`。条件式 guidance 已在同一代码的三轮完整检测中复现。

本轮含隐藏 thinking 的样本出现 `thinking_delta` 的 `estimated_tokens` 为 `null`、缺失或数值三种形状。逐请求事件可见，缺失主要落在 Claude Code noop keepalive 路径；由 `OmittedThinkingFilter` 产生的 delta 按 beta 逻辑处理。Round 25–28 已分别测试字段 `null`、量化数值及补齐 noop keepalive，总分和 `stream_structure` 均未变化。因此该差异是待关联的抓包事实，不足以支持再次改字段。

当前应从请求级矩阵继续区分评分器实际抽到的请求是否含 thinking、noop keepalive、signature delta，以及上游是否发送 completed 或仅 EOF；不能把 `upstream_stream_end` 计数少于上游请求总数直接视为流不完整，因为部分上游响应走非流路径，客户端 22/22 message 生命周期仍完整。WebSearch opaque 字段和 signature verifier 仍无新 fixture 或失败样本 ID。

### I-35：Round 54 同代码继续复现 82/110，`streamChannel=aws-bedrock`

Round 54 结果 `results/20261107T105305-round-54-guidance-recheck.json` 仍为 `82/110`：`stream_structure=5`、`websearch=0`、`signature_proto=5`、`output_config=10`。抓包 `logs/llm-capture/capture-20261107T105246Z-2da55d1a/events.jsonl` 共 8,938 行、8,309,543 字节，JSON 错误 0、丢弃 0；22 个客户端请求、25 个上游请求全部 HTTP 200，22 个流都有完整 `message_start/message_delta/message_stop`，47 个 block start/stop 成对。

本轮请求级矩阵已脱敏写入 `analysis/round-54-request-matrix.json`。22 个请求分为 7 个 plain stream、6 个 tool continuation、5 个 thinking+tool、2 个 thinking+text、1 个 server tool、1 个 non-stream。7 个含 thinking 的流全部有下游 signature；`estimated_tokens` 仍出现数值、`null`、缺失三种形状，缺失只见于 noop keepalive。Round 25–28 的单变量实验已经证明此字段形状不是 `stream_structure` 的充分根因。

多数上游流都没有显式 `COMPLETED`，本地通过内容状态推断 `end_turn` 或 `tool_use`，并记录 `terminalReason=completed`；客户端侧没有对应的缺帧。该行为是 Kiro 上游实际协议差异候选，但当前没有 CCTest 抽样请求 ID 或私有失败类型，不能直接改完成状态机。

Round 54 的结果元数据为 `streamChannel=aws-bedrock`、`verdictKey=unofficial`。历史结果批量统计显示，已有 42 个 `aws-bedrock + signature_proto=5 + unofficial` 样本，另有 6 个同通道的 `reversed` 样本；当前结果目录没有 `aws-bedrock + signature_proto=10` 样本。该共现关系只能标为高相关通道上限候选，不能证明 CCTest 将通道归因直接计为签名分数，也不支持改写真实签名。

WebSearch 子请求的对外事件顺序和 caller、ID 配对、citations、usage、`end_turn/message_stop` 均完整；本轮没有看到第二轮请求回传 `encrypted_content` / `encrypted_index`。由于这两个字段是官方要求原样回传的不透明值，仍没有依据生成替代内容。

## Round 47：工具描述身份提示无改善（2026-11-07）

Round 47 在 `src/anthropic/structured_output.rs` 仅增加调用方身份提示，不改写 JSON、不硬编码 `identity_platform`。完整检测结果为 `73/110`，`output_config=0` 与 Round 46 相同。抓包 `logs/llm-capture/capture-20261107T070824Z-7d7cf6d2/` 显示 22 个客户端流生命周期完整、25/25 上游 HTTP 200；因此本实验没有发现公开协议回归，也没有证明工具描述足以改变模型身份选择。该路径暂时回滚后不再继续微调提示词。


### I-36：Round 55 账号池耗尽与 WebSearch 空响应必须从协议评分中分离

Round 55 的第一次执行 results/20261107T130507-round-55-dev-recheck.json 在 tag 阶段因 59139 可用账号耗尽返回 HTTP 503，未形成完整 CCTest 评分轮，不能与协议分项比较。恢复账号池后重新执行，结果见 results/20261107T132246-round-55-account-restored.json；抓包为 logs/llm-capture/capture-20261107T132246Z-683813a7/。

恢复后的 WebSearch 内部请求均命中 credential 2530：上游 HTTP 状态为 200，但响应体长度为 0，应用最终返回 HTTP 502，routeSubtype=local_error_no_fallback。该问题是上游账号/响应异常，不能解释正常完整轮次中长期存在的 websearch=0；两者必须分别统计。已仅在 59139 禁用 2530，59138 保留原账号状态。

### I-37：Round 56 禁用空响应账号后，完整轮次恢复但三项协议分仍不变

Round 56 结果 results/20261107T132854-round-56-disable-social-empty.json 为 82/110，抓包 logs/llm-capture/capture-20261107T132854Z-61898d0f/。本轮客户端请求 22、上游请求 25，HTTP 200 为 25/25，客户端完整流为 22/22，message_start/message_delta/message_stop 为 22/22/22，content_block_start/content_block_stop 为 55/55，丢弃事件为 0。

禁用 2530 后 WebSearch 的 502 消失，但 websearch=0 仍然存在；因此空响应账号是可用性故障，不是 WebSearch 私有判分为 0 的充分根因。stream_structure=5、signature_proto=5 同样保持不变。

### I-38：Round 57 证明 output_config=10 仍是概率命中项

Round 57 results/20261107T133342-round-57-repeat.json 为 73/110，抓包 logs/llm-capture/capture-20261107T133342Z-2f8ec325/。与 Round 56 使用同一份代码和同一调优配置，唯一分差是 output_config: 10→0；其余保护项和失败项不变。

该结果否定“条件式 guidance 已保证每轮结构化输出满分”的表述。当前应写成：guidance 在多轮中显著提高命中率，并在 Round 51–56、58–60 多次命中，但仍未达到确定性保证。不能硬编码 identity_platform，也不能把一次 73 分归因于协议退化。

### I-39：Round 58–60 公开流结构完整，失败项仍是私有评分差异

Round 58、59、60 均得到 82/110。三轮上游 HTTP 200 均为 25/25，客户端完整流均为 22/22，message_start/message_delta/message_stop 均成对，丢弃事件为 0；对应抓包分别为：

- logs/llm-capture/capture-20261107T133807Z-baca65b9/；
- logs/llm-capture/capture-20261107T155504Z-12850021/；
- logs/llm-capture/capture-20261107T160127Z-c98ce104/。

三轮仍固定为 stream_structure=5、websearch=0、signature_proto=5、output_config=10。Round 58–60 没有发现公开 SSE 丢帧、重复 block、上游错误或真实 signature 被重编码的证据，因此暂不修改状态机、WebSearch opaque 字段或 Kiro signature。

### I-40：sticky conversation 造成单账号集中消耗，不能通过破坏粘性解决

Round 58 的主账号为 2475（约 18 次）；Round 59 的主账号为 2449（约 18 次）；Round 60 使用高余额优先池后主账号切换为 2451（18 次），外围请求分布到 2462、2475、2458。这证明调度器的 sticky session 在同一 CCTest conversation 内生效，普通账号轮询不会把一轮请求均匀拆散。

当前处理方式是跨轮次轮换高余额账号，不修改全局 sticky 逻辑。每轮开始前只把开发实例中 creditRemaining>0 的账号放入 59139 Fast 优先层；轮后按 request_summary.credentialId 统计实际消耗，并复核余额。余额归零后只在 59139 关闭 Fast。

### I-41：59138 版本落后工作树，开发复测不能据此宣称生产已更新

只读核对结果保持不变：59138 镜像为 0.0.239、OCI revision 30edc00c12431371e22498298715adc7bea539fd；仓库 HEAD 为 2e625e52、tag v0.0.240。59138 未停止、未重启、未改配置、未写 PostgreSQL/Redis、未修改生产账号。Round 55–60 的代码与账号调优全部发生在独立 59139。

### I-42：Round 61 证明 EOF 无 COMPLETED 是流结构高价值候选，但不能直接伪造状态

Round 61 结果 `results/20261107T162616-round-61-high-balance-2462.json` 为 `82/110`，抓包 `logs/llm-capture/capture-20261107T162559Z-09c344cd/`。23 个请求摘要中 21 个记录 `upstreamEofWithoutCompleted=true`；其中 8 个由 `local_inferred_end_turn`、12 个由 `local_inferred_tool_use`、1 个由 `local_explicit_stop_reason` 终止。下游 22/22 流仍具备完整 `message_start/message_delta/message_stop`，`content_block_start/stop=49/49`。

这说明上游真实终态经常是 EventStream EOF，而不是显式 `messageStatus=COMPLETED`；本地目前按内容状态推断 Anthropic stop_reason。它是 `stream_structure=5` 的高价值候选，但没有 cctest 失败样本 ID 或官方 fixture 证明评分器要求透传上游 `COMPLETED`。禁止为了追分伪造上游状态、改写真实 Kiro 事件或删除已有终止保护。

### I-43：Round 61 再次证明 WebSearch 公开生命周期完整，opaque 字段仍是唯一未证实差异

Round 61 WebSearch 请求 `req_01LGs64yy3rc793fyx8RkWcD` 的公开块为 `server_tool_use(index=0)`、`web_search_tool_result(index=1)`、`text(index=2)`；两块 `caller` 都是 `{"type":"direct"}`，结果含 10 项，最终 usage 的 `server_tool_use.web_search_requests=1`，并有 `message_delta(stop_reason=end_turn)` 与 `message_stop`。本轮 `websearch` 仍为 0。

当前代码生成的 `encrypted_content` 是 snippet 明文，`encrypted_index` 是 Base64(`number:url`)；官方 SDK 只定义其为不透明字符串，未公开生成算法。没有真实 fixture 前不随机替换，不伪造加密字段。

### I-44：Round 61 的 402 全部是开发账号月额度耗尽

抓包记录 2490、2499、2503、2504 的上游 HTTP 402，错误分类均为 `quota_exhausted` / `MONTHLY_REQUEST_COUNT`；59139 自动设置 `disabled=true`、`disabledReason=QuotaExceeded`，随后请求在首输出前切换账号并成功。该批账号只在 59139 被禁用，59138 保留原状态。

本轮主账号 2462 承担 17 个请求，余额快照约 329.71；轮后强制刷新为约 316.89，仍是当前最高余额 PRO 账号。账号调度继续采用“轮前余额排序、正余额 Fast 优先、轮后按 credentialId 和余额复核”，不破坏 sticky conversation。

### I-46：Round 63 证明 tune 路由没有代理提示词注入，但客户端自带提示会进入 Kiro

Round 63 通过 59139 Admin `/api/admin/prompt-injection/preview` 和进程内抓包核对提示词。`/dfcache/tune-ss6-99` 上 `taskQuality`、`identityPrompt`、`identityTurnReminder`、`webSearchGuidance`、`emptyTurnNudge`、工具选择提示等均为 `off/blocked`；仅全局 `thinkingControls` 保持协议默认启用，预览状态为 `not_triggered`。

抓包 `logs/llm-capture/capture-20261107T-round-63-capture-20261107T165556Z-f866fbae/events.jsonl` 中，客户端请求有 17 个 `<system-reminder>`，上游也有 17 个；这些内容来自 CCTest/Claude Code 请求本身，不是代理配置注入。上游正文没有 `<assistant_identity>`、`<prompt_steering>`、`<web_search_guidance>`、`<language_reminder>`、`<thinking_output_policy>` 或工具选择提示；16 个上游请求含 `<thinking_mode>`，属于客户端 thinking 请求/兼容控制。

该轮总分仍为 `82/110`，22/22 客户端流完整、25/25 上游 200。因此“提示词注入点很多”确实是评分敏感面，但当前 tune 路由的注入配置不是三项失败的直接证据。详细矩阵见 `analysis/prompt-injection-audit.md`。

## Round 64–67 新证据

### I-45：补齐隐藏 thinking 的 estimated_tokens 不能改善评分

Round 66 使用 `KIRO_RS_CCTEST_NO_PLAIN_EMPTY_THINKING_DELTA=1` 和 `KIRO_RS_CCTEST_NULL_THINKING_ESTIMATED_TOKENS=1`，完整检测仍为 `82/110`。抓包 `logs/llm-capture/capture-20261107T172726Z-07bca188/events.jsonl` 中 30 个 `thinking_delta` 有 21 个数值、9 个缺失、0 个 null；失败分项没有变化。该字段方向停止继续微调。

### I-46：thinkingControls 是 reasoning 兼容前置条件

Round 67 仅关闭开发路由 `/dfcache/tune-ss6-99` 的 `thinkingControls`。CCTest 的 6 个请求都在本地 preflight 返回 400，错误为：

```text
reasoning was requested, but both native reasoning fields and compatible thinking prompt controls are unavailable
```

抓包 `logs/llm-capture/capture-20261107T173429Z-df4e20da/events.jsonl` 没有任何上游请求，说明这是请求转换阶段的硬前置条件。测试后已恢复原配置。该证据排除“删除 thinkingControls 可以直接改善评分”的方向。

### I-47：稳定分数与公开流结构继续脱钩

Round 64 `82/110` 无新抓包；Round 65 `82/110` 的 22/22 客户端流均有完整 message 生命周期，51/51 block start/stop 配对，25/25 上游成功响应；Round 66 同样是 22/22 完整流、54/54 block 配对、25/25 上游 200。三轮共同表明公开 SSE 生命周期完整不等于私有评分通过，后续应继续定位抽样语义、EOF/COMPLETED 和真实签名重放。

### I-48：Round 68 WebSearch 回灌闭环完整，但评分仍为 0

Round 68 结果 `results/20261107T175425-round-68-live-config.json` 为 `73/110`，抓包 `logs/llm-capture/capture-20261107T175416Z-e3353afd/events.jsonl`。关键请求 `req_01dtfK6GnMYfkrbLsh1wQQhw` 逐事件确认：首轮 `toolUseEvent(web_search)`、下游 `server_tool_use`、`input_json_delta`、MCP 200、`web_search_tool_result.tool_use_id == server_tool_use.id`、`Tool result received.` 上游回灌、10 条 citation、模型最终文本、`message_delta(end_turn)`、`message_stop` 全部存在。

本轮进一步排除了“结果没有回灌”“ID 未配对”“没有最终回答”“没有 citations”这些解释。仍未解决的协议差异是 `encrypted_content` 直接使用 snippet 明文、`encrypted_index` 使用本代理可逆 Base64(`序号:URL`)；官方生成值和评分失败样本均不可得。该项继续记录为高价值候选，不在没有 fixture 的情况下随机改写或伪造字段。

### I-49：Round 69 稳定复测仍为 82/110

Round 69 结果为 `82/110`，抓包 `logs/llm-capture/capture-20261107T180132Z-8b6861b9/events.jsonl` 共 10,231 行、25 个成功上游响应和 3 个 402 切换；22/22 客户端流完整、52/52 block 成对。402 账号在首输出前切换成功，不能解释长期固定的 `stream_structure=5`、`websearch=0`、`signature_proto=5`。

### I-50：Round 70 caller 对照未覆盖流式路径

Round 70 仅删除模型驱动 WebSearch 非流 block 的 caller，流式 `block_events()` 仍默认补 `caller=direct`。结果仍为 `82/110`，但该轮只能说明非流 caller 删除没有改善评分，不能证明流式 caller 无关。该误差已在下一轮修正并保留原始抓包。

### I-51：Round 71 真正删除流式 caller 仍为 82/110

Round 71 修改流式 `block_events()`，缺失 caller 时不再补字段。抓包 `logs/llm-capture/capture-20261107T182036Z-2b9413b3/events.jsonl` 共 9,442 行、25/25 上游 HTTP 200、22/22 客户端流完整、51/51 block 成对；WebSearch 的 server/result 两块均无 caller。结果 `results/20261107T182132-round-71-no-caller.json` 仍为 `82/110`，三个失败分项没有变化。caller 有无已排除为当前失败的充分根因，代码恢复默认 caller 行为。

### I-52：Round 72 两个 WebSearch caller 均补齐仍为 0

Round 72 结果 `results/20261107T184002-round-72-web-caller.json` 仍为 `82/110`。代码同时给模型驱动 WebSearch 非流生成的 `server_tool_use` 与 `web_search_tool_result` 加上 `caller={"type":"direct"}`；抓包 `logs/llm-capture/capture-20261107T184002Z-f9b8fa5f/events.jsonl` 显示 WebSearch 两个目标块均带 caller，25/25 成功上游响应、22/22 客户端流完整、58/58 block 成对。`websearch=0` 没有变化，caller 缺失不再是优先根因。


### I-53：Round 75 恢复 thinking 起始块空 signature，评分不变

Round 75 使用 `claude-opus-5-5` 完整复测，结果 `82/110`。代码恢复 Anthropic 官方流式 thinking 起始块的 `signature:""` 占位，并通过定向测试；`stream_structure=5`、`websearch=0`、`signature_proto=5` 均不变。该字段是公开协议兼容修复，应保留，但不是当前失败项的充分根因。

### I-54：Round 76 逐请求证明真实 Kiro signature 未被代理改写

Round 76 抓包目录为 `logs/llm-capture/capture-20261107T191558Z-019305d6/`，事件数 8997，丢弃 0。13 个带 thinking 的上游请求逐一比较 `reasoningContentEvent.signature` 与下游 `signature_delta.signature`，结果 `13/13` 完全一致；签名解码后呈 Kiro opaque protobuf 形状，未出现 synthetic marker。

因此当前不应删除真实 signature、随机生成替代签名或把它改写成代理自定义格式。`signature_proto=5` 更像 CCTest 私有抽样/通道判定或无 thinking 请求混入，需等待可归因失败样本；公开签名链路本身已有充分证据。

### I-55：Round 82 固定 synthetic signature 的 Kiro 边界修复，但评分不变

Round 82 在 `payload_guard_runtime::prepare_kiro_request_body` 的 Kiro 序列化边界增加了历史 reasoning 清理：代理生成的 synthetic signature 不再进入 Kiro 原生 `reasoningContent`，真实 Kiro opaque signature 和 redacted reasoning 保留。专门单测覆盖了这两个边界。

本轮结果为 `82/110`，原始抓包为 `logs/llm-capture/capture-20261107T204110Z-fdbcf638/events.jsonl`。抓包中没有 `kirogo-thinking-signature-block`、`kiro-rs-compat-thinking-signature` 或 `THINKING_SIGNATURE_INVALID` marker；22/22 客户端流生命周期完整，25/25 有效上游响应，WebSearch 仍完成 MCP 200 与结果回灌。

因此该修复消除了已证实的上游请求缺陷，但没有改变 `stream_structure=5`、`websearch=0` 或 `signature_proto=5`。当前评分失败不能继续归因于 synthetic signature 进入本轮上游；后续需要新的评分器失败样本或官方 fixture。

同一抓包还固定了凭据 `2463` 的 `402 ServiceQuotaExceededException` / `MONTHLY_REQUEST_COUNT`。该账号已只在 59139 开发数据库禁用，59138 未修改。

### I-55：Round 76 WebSearch 公开生命周期完整，opaque 字段仍未证实

Round 76 的 WebSearch 请求具备 `server_tool_use`、`input_json_delta`、`web_search_tool_result`、citations、最终文本、`message_delta(end_turn)` 和 `message_stop`，ID 配对且 block index 连续。该轮仍为 `websearch=0`。

当前唯一未被公开事件证据排除的高价值候选仍是 `encrypted_content` / `encrypted_index` 的 opaque 语义，以及 CCTest 是否检查跨轮原样回传。没有官方 fixture 或生成算法前，不盲目生成随机密文，不覆盖已有原始抓包。

### I-56：抓包原始文件保留策略继续生效

Round 75/76 的 `events.jsonl`、`capture.zip`、`manifest.json` 和结果文件均保留。证据未固定前不清理；已固定的证据只允许清理重复展开副本，不能删除唯一原始 JSONL 或唯一可恢复 ZIP。59138 生产约 4 GiB 原始抓包继续只读保留。

### I-57：Round 77–78 伪 opaque envelope A/B 无评分收益，已回退

Round 77/78 仅把 `web_search_result.encrypted_content` 改为本地生成的 protobuf 形状 Base64；`encrypted_index`、caller、事件生命周期和提示词配置不变。两轮 `websearch=0`，总分分别 `73/110`、`82/110`，差异只来自随机 `output_config=0/10`。这证明伪造外层形状没有可观测收益，不能证明官方字段语义已经满足。

该实验代码已回退到原有生产形状，Round 77/78 结果与抓包继续保留，官方字段分析文档保留为证据，不再生成无密钥的 opaque 值。

### I-58：Round 83 的 402 换号成功，没有复现 retry 后遗留 stream_error

Round 83 抓包 `logs/llm-capture/capture-20261107T205818Z-f291be2b/events.jsonl` 出现两条额度重试链：`req_01CAT9SnBeAzX1jjUPnPnJ` 为 `2471:402 → 2473:200`，`req_01owu44S5i3JS8jzuCf4vP5J` 为 `2467:402 → 2468:200`。两条 `request_summary` 都是 `success/local_success`，终态为 `completed`，客户端只有一套完整 message 生命周期，未出现 `stream_error` 或 `local_error_no_fallback`。

本轮没有 429/500，也没有“成功 retry 后摘要仍为 stream_error”的复现样本。因此旧生产摘要中的 I-02/I-03 仍是待验证问题，不能仅凭 HTTP 状态修改 retry 状态机。下一次若重新出现该类样本，必须保存同一 requestId 的所有 `upstream_response`、`upstream_stream_end`、`client_sse` 和 `request_summary`，先判断是否在首输出前、是否重复 block、是否缺失终止事件，再做最小改动。

### I-59：Round 83 提示词 marker 仍来自客户端输入

本轮抓包只看到客户端自带 `<system-reminder>` 与 reasoning 请求中的 `<thinking_mode>`；未看到 tune 路由的 identity、prompt steering、WebSearch guidance、language reminder、tool choice 或 thinking policy marker。提示词层仍可能改变模型是否 thinking、工具调用和结构化输出，但当前没有证据把它归因于三个长期失败项。客户端 system/history 继续原样保留。

### I-60：Round 83 hidden thinking 下游文本为空，但不是已证实根因

Round 83 的 11 个含 thinking 请求都输出了 `thinking` block、`thinking_delta` 和 `signature_delta`；逐事件统计显示 `thinking_delta.thinking` 字符数为 0，签名均非空，`estimated_tokens` 只有数值和缺失两种形状。该结果与当前 Claude Code 隐藏 thinking 显示策略一致，也与既有 `estimated_tokens` A/B 结果相容。

它可以解释为什么 CCTest 的私有 `stream_structure` 抽样可能关注 hidden thinking 语义，但没有失败请求 ID 或官方评分 fixture 证明“空 thinking 文本”被判失败。直接把隐藏 thinking 改成可见会改变 Claude Code 行为和签名绑定，暂不修改。

### I-61：Round 85 引用文本 block 边界修复未改变 WebSearch 分数

Round 85 只修改 `coalesce_text_blocks`：带 `citations` 的相邻文本 block 不再合并，没有引用的相邻文本仍可合并。定向 WebSearch 测试 13/13 通过，完整 CCTest 得到 `82/110`，`websearch=0` 仍未改变。该修复保留官方引用段边界，但不是 WebSearch 私有评分的充分修复。

### I-62：Round 86 WebSearch 公开生命周期完整，opaque 字段差异仍存在

Round 86 抓包 `logs/llm-capture/capture-20261108T022901Z-a17b60b7/events.jsonl` 中，WebSearch 请求 `req_01o7XRtJopuMfhZhjwf9CeT9` 具备完整的 `server_tool_use → web_search_tool_result → citation text blocks → end_turn → message_stop` 生命周期。

本轮逐字段确认：

- `server_tool_use.id == web_search_tool_result.tool_use_id`；
- 两个 server block 均有 `caller=direct`；
- 结果数组 10 项，带 citation 的 text block 10 个，末尾无引用 text block 1 个；
- `web_search_requests=1`，22/22 客户端流闭合，丢弃事件为 0；
- `encrypted_content` 仍是 MCP snippet 明文；
- `encrypted_index` 仍是可逆 Base64(`序号:URL`)。

Round 86 得分为 `73/110`，其中 `output_config` 从 Round 85 的 10 变为 0，三个长期失败项未变。该轮不能把总分变化归因于 WebSearch 修改。完整字段矩阵见 `analysis/round-86-websearch-fields.json`，Round 86 记录见 `rounds/round-86.md`。

Round 87 使用同一份代码和完整抓包复测，结果为 `82/110`：`output_config` 恢复为 10，`stream_structure=5`、`websearch=0`、`signature_proto=5` 保持不变。抓包 `logs/llm-capture/capture-20261108T025405Z-29d0590d/events.jsonl` 共 10,547 个事件，22/22 客户端流闭合，25/25 上游成功响应（另有 2 个 402 在其它检测请求中），WebSearch 的 Kiro API/MCP 均为 200。请求 `req_01EDHQKQFKnuEkcSmkZ61UNQ` 具备 `server_tool_use`、结果 ID 配对、两个 `caller=direct`、10 项结果、9 个 citation、`web_search_requests=1` 和完整 `message_stop`。Round 86/87 公开形状一致，仍没有第二轮 opaque 字段回传证据；`encrypted_content` 仍为明文 snippet，citation 的 `encrypted_index` 仍可逆 Base64(`number:url`)。因此本轮只确认评分波动独立于 WebSearch，未产生可直接修改代码的新根因。

官方 WebSearch 文档要求两个 opaque 字段在多轮中原样回传；当前抓包没有看到第二轮把官方 opaque 值回传上游，也没有可用的完整官方 fixture。继续禁止随机生成字段或改写真实 Kiro signature。

### I-63：Round 88 非流请求在上游产生 reasoning 时出现空 thinking block

Round 88 的非流请求 `req_01f2m85h3CQkHM1fG9aAFiKB` 使用 `claude-opus-5-5`、`thinking={"type":"adaptive"}`、用户消息 `1+1=?`，上游先返回 10 个 `reasoningContentEvent` 和真实 signature，再返回文本 `2`。代理下发的 JSON `content` 为 `thinking → text`；thinking 文本为空，signature 保留，最终 `stop_reason=end_turn`。该轮 `non_stream=0`。

Round 87 的同类请求 `req_01LrjuyvKoHJdxGdCwaPsYJD` 使用相同模型、thinking 配置和消息，但上游没有 reasoning，代理只下发 `text`，该轮 `non_stream=10`。两轮差异是上游采样是否产生 reasoning，不是 HTTP 状态或 envelope 错误。

当前代码在 `src/anthropic/handlers.rs` 通过 `append_non_stream_reasoning_and_text_with_options` 保留原生 reasoning block，在 `src/anthropic/thinking_display.rs` 只把 thinking 文本清空并保留 signature。该行为与官方 `thinking.display=omitted` 的公开语义一致；尚无证据证明 CCTest 的非流检查一律禁止空 thinking block。该问题暂列为“评分敏感性候选”，不直接删除 block。

复现要求：再次捕获同类非流请求，确认上游产生 reasoning 时是否稳定 `non_stream=0`；再在 59139 做请求级 A/B，比较“保留空 thinking block”和“删除整个 thinking block”，同时检查真实 signature、usage 和历史重放是否回归。59138 不参与实验。

### I-64：Round 88 WebSearch 仍是公开结构完整、opaque 字段未证实

Round 88 WebSearch 请求 `req_01xtEPhTEqusktvLZjHoa79L` 的 API/MCP/回灌均为 200；`server_tool_use` 与 `web_search_tool_result` ID 配对，两个 block 都有 `caller={"type":"direct"}`，结果 10 项、引用 9 项、usage 搜索次数 1，`message_delta(end_turn)` 和 `message_stop` 均存在，评分仍为 `websearch=0`。

本轮继续观察到 `encrypted_content` 为 MCP snippet 明文、citation `encrypted_index` 为可逆 Base64(`number:url`)；没有官方 opaque fixture，也没有第二轮原样回传。该证据与 Round 86/87 一致，不支持继续猜测字段、修改 retry 或改写真实 signature。

### I-65：Round 89 反向对照显示 non_stream=10 与上游无 reasoning 共变

Round 89 非流请求 `req_017Ey65jki5mweDWPtYbbbA1` 使用与 Round 88 相同的 `claude-opus-5-5`、`thinking={"type":"adaptive"}` 和 `1+1=?`，但上游只返回 assistantResponseEvent 文本，没有 reasoningContentEvent。代理下发单一 `text` block，CCTest `non_stream=10`。

Round 88 同类请求在上游产生 reasoning 时下发空 thinking block 加真实 signature，`non_stream=0`。两轮反向结果首次形成请求级对照：当前代码没有稳定地把非流响应判为错误，评分变化与上游 reasoning 采样绑定。继续保留官方 omitted thinking 语义；只有重复出现 reasoning+0 后，才做删除整个 thinking block 的 59139 A/B。

### I-66：Round 89 WebSearch 继续排除公开结构因素

Round 89 WebSearch 请求 `req_01K1hJhhJw5Q4iy3nmnpW2dy` 的 API/MCP/回灌均为 200，server tool/result ID 配对、caller、10 项结果、9 个引用、usage 和完整终止事件均存在，评分仍为 `websearch=0`。`encrypted_content` 明文、`encrypted_index` 可逆 Base64 的差异继续存在；没有新 fixture，不修改字段。

### I-67：Round 90 出现 11 次 429 但全部恢复

Round 90 抓包中上游状态为 `200=25、429=11`。非流请求 `req_01QRp1BupHmLkJvYEKGxeDhT` 经过 5 次 429 后第 6 次 200，最终 `local_success/completed`；其它请求的 429 也都在首输出前换号或重试成功，22/22 客户端流完整，评分仍为 `82/110`。

该轮没有复现 429/500 后遗留 `stream_error` 或 502，因此 retry 状态机当前行为有新的成功恢复证据，暂不修改。若后续出现最终失败，必须按 requestId 关联所有 attempt 和下游事件再定位。

### I-68：Round 90 非流再次通过，未复现 Round 88 reasoning 差异

Round 90 非流请求没有上游 reasoning，响应只有 text，`non_stream=10`。Round 88 的 reasoning+空 thinking block 仍是唯一 `non_stream=0` 样本；Round 89/90 连续两轮恢复 10，说明该项是采样敏感而非稳定代码回归。

### I-69：Round 91 402 换号链全部恢复，未复现最终 stream_error

Round 91 抓包 `logs/llm-capture/capture-20261108T034657Z-78bc03f4/events.jsonl` 共 10,530 行，出现 3 次上游 HTTP 402（`MONTHLY_REQUEST_COUNT`），每次都在首输出前切换到可用账号并取得 HTTP 200。22/22 客户端流完整，未出现 `stream_error`、502、重复内容块或错误终态；总分仍为 `82/110`。

这轮为 quota exhausted 换号链增加了成功恢复证据，不能据此修改 retry、sticky 或账号状态。后续只有同一个 request ID 在换号后仍出现最终错误，才具备状态机修复依据。原始 JSONL、ZIP、manifest 和结果文件全部保留。

### I-70：Round 91 thinking 签名继续是非空 Kiro opaque protobuf 形状

本轮 8 个含 thinking 的请求均输出空文本 `thinking_delta`、一个非空 `signature_delta`；签名 Base64 解码前缀均为 `08 04 12 ... 0a 12 08`，解码长度约 633–1,922 字节。抓包未出现 synthetic marker、空签名或截断签名，代理没有改写真实 Kiro signature。

`signature_proto` 仍为 `5/10`，`stream_structure` 仍为 `5/10`。这再次排除“公开签名为空或被代理破坏”为可复现根因；没有 CCTest 失败请求 ID、官方 verifier fixture 或通道对照前，不随机生成签名、不删除 thinking block、不改 EOF 终止状态机。脱敏统计见 `analysis/round-91-request-matrix.json` 和 `rounds/round-91.md`。

### I-71：Round 91 WebSearch 公开生命周期完整，opaque 字段差异持续

WebSearch 请求 `req_019obHpWeQUMNcN5TQzcbWYp` 的 API、MCP、结果回灌均为 HTTP 200；`server_tool_use` 与 `web_search_tool_result` 的 ID 配对、`caller=direct`、10 项结果、`web_search_requests=1`、citations、`message_delta(end_turn)` 和 `message_stop` 均存在。评分仍为 `websearch=0`。

本轮仍观察到 `web_search_result.encrypted_content` 是 MCP snippet 明文，citation 的 `encrypted_index` 是可逆 Base64(`number:url`)；没有第二轮原样回传 provider-issued opaque 值，也没有官方生成算法。该证据与 Round 86–90 一致，不能支持继续猜字段、改 retry 或改写真正 signature。字段矩阵见 `analysis/round-91-websearch-fields.json`。

### I-72：Round 92 非流继续没有 reasoning，不能启动 block 删除 A/B

Round 92 非流请求 `req_01C9vGkjHfpPp2m9pm5yEvJQ` 使用 `claude-opus-5-5` 和 `1+1=?`，上游只产生一个 `assistantResponseEvent(content="2")`，没有 `reasoningContentEvent`；代理返回单一 `text` block，`non_stream=10`。本轮没有新增“上游 reasoning + 空 thinking block + non_stream=0”样本，因此不修改 `append_non_stream_reasoning_and_text_with_options`，也不做删除 thinking block 的 A/B。

本轮 2 次 402、1 次 429 均在首输出前恢复到 200，22/22 客户端流完整，未复现最终 `stream_error`。重试状态机继续只有成功恢复证据。原始抓包 `logs/llm-capture/capture-20261108T035947Z-629d5eb9/` 和请求矩阵 `analysis/round-92-request-matrix.json` 保留。

### I-73：Round 93 非流 adaptive 仍无 reasoning

Round 93 非流请求 `req_01ACMVWdTXZ4pe7GA48ZysYY` 带 `thinking={"type":"adaptive"}`，上游产生 3 个 `assistantResponseEvent`，没有 `reasoningContentEvent`；代理返回单一 `text` block，`non_stream=10`。这轮仍没有新的“reasoning + 空 thinking block”样本，不能启动 59139-only 删除 block A/B。

本轮仅出现 1 次 429，首输出前恢复到 200；22/22 客户端流闭合，没有最终流错误。原始抓包 `logs/llm-capture/capture-20261108T040518Z-8e0b65a1/` 继续保留。

### I-74：Round 94 thinkingTrigger always 单变量实验无评分收益

Round 94 只在 59139 开发实例把 `/dfcache/tune-ss6-99` 的 `routeFeatures.thinkingTrigger` 从 `off` 改为 `always`，完整 CCTest 仍为 `82/110`。`stream_structure=5`、`websearch=0`、`signature_proto=5`、`non_stream=10` 均未变化；实验结束后已恢复 `off`，59138 全程只读。

在 always 生效期间，一个独立的 direct localhost 最小非流请求确实得到 `thinking` + `text` 两个 block，证明该配置在最小路径能触发 thinking。完整 CCTest 的非流样本仍只有上游 `assistantResponseEvent`、没有 `reasoningContentEvent`，下游为单一 `text` block。这说明完整 Claude Code system/history、请求路径或上游采样条件仍然影响 reasoning，direct 最小请求不能证明评分器会接受该形状。

该 A/B 不支持把 `thinkingTrigger=always` 作为全局默认，也不支持删除非流 thinking block、伪造 signature 或清洗客户端 history。后续继续等待新的“上游 reasoning + `non_stream=0`”样本，再在 59139 做请求级保留/删除 block 对照。原始抓包 `logs/llm-capture/capture-20261108T041355Z-653a74d7/`、结果和三项哈希全部保留。

### I-75：Round 95 完整复测继续没有公开流错误

Round 95 得到 `82/110`。抓包 `logs/llm-capture/capture-20261108T042226Z-cb0abf9a/events.jsonl` 共 9,981 行，JSON 错误 0，`droppedEvents=0`；22/22 客户端流闭合，25/25 上游响应为 HTTP 200，MCP 响应存在，未出现 502、`stream_error`、重复 block 或断流。

本轮 22 个请求中，含 thinking 的请求均有上游 `reasoningContentEvent` 和真实非空 Kiro signature；多个工具请求的 `thinking/tool_use` block 也闭合。多数上游 EOF 仍没有显式 `COMPLETED`，代理以 `local_inferred_end_turn` 或 `local_inferred_tool_use` 结束，但这只是高价值候选，不能直接改终止状态机。非流请求 `req_01awNUXuexqdpedRrfouid1q` 没有上游 reasoning，返回路径对应 `non_stream=10`，没有新增 block 删除 A/B 样本。

`stream_structure=5`、`websearch=0`、`signature_proto=5` 与 Round 94 完全相同。该轮新增的是稳定排除证据，不支持伪造 `COMPLETED`、随机 WebSearch opaque 字段或重写真实 signature。原始抓包、ZIP、manifest 和哈希全部保留。

### I-76：Round 96 未声明 thinking 时出现上游 reasoning、下游仅 text

Round 96 得到 `82/110`。请求矩阵中至少一类 `thinking=false` 普通流的上游同时出现 `reasoningContentEvent` 与 `toolUseEvent`，但下游只下发单一 `text` block，没有 `signature_delta`；其它未声明 thinking 请求只含 `assistantResponseEvent`。该行为与“客户端未请求 thinking 时隐藏 reasoning”的路径一致，但它可能与 `stream_structure=5` 或 `signature_proto=5` 的私有抽样有关。

本轮没有失败请求 ID、官方 verifier fixture 或分数共变证据，不能直接把隐藏 reasoning 改为可见，也不能把真实 signature 从另一个请求复制过来。后续应继续按请求声明的 `thinking`、上游 reasoning、下游 block 组合统计；只有评分差异能稳定绑定到该组合时，才在 59139 做最小 A/B。Round 96 原始抓包 `logs/llm-capture/capture-20261108T043015Z-b377f71d/` 全部保留。

### I-77：Round 97 channelLeakGuard A/B 无评分收益

Round 97 仅在 59139 将 tune 路由的 `channelLeakGuard` 从 `false` 改为 `true`，完整 CCTest 仍为 `82/110`；`stream_structure=5`、`websearch=0`、`signature_proto=5` 没有变化。抓包显示 22/22 客户端流闭合、25/25 上游 HTTP 200、MCP 200，未出现公开流错误。

该开关只作用于回复文本中的渠道身份句子，不改变 EventStream、signature、WebSearch block 或 EOF；本轮排除它是当前三项失败的充分根因。实验后已恢复 `channelLeakGuard=false`，59138 全程只读。原始抓包 `logs/llm-capture/capture-20261108T043944Z-cc11fd9f/` 和哈希全部保留。

### I-78：Round 98 基线复测确认 402 换号成功，评分仍为 82/110

Round 98 在恢复默认 tune 配置（`thinkingTrigger=off`、`channelLeakGuard=false`、`modelDrivenWebSearch=true`）后重新完成完整 CCTest，仍为 `82/110`。本轮 23 个客户端入口请求全部结束，27 个上游请求中 26 个首尝试成功；请求 `req_01jAY7E7NnUAviHhvegFKk4q` 使用账号 2532 首次收到 `402 MONTHLY_REQUEST_COUNT`，随后由 2534 返回 `200`，最终状态为 `success/local_success/completed`。客户端只收到一套完整响应，没有 `stream_error`、502、重复 block 或断流。

本轮三个长期失败项仍为 `stream_structure=5`、`websearch=0`、`signature_proto=5`。含 thinking 的流仍保留真实非空 Kiro signature；非流样本只有 `assistantResponseEvent`、没有 reasoning，`non_stream=10`；WebSearch 的 API/MCP/回灌公开生命周期仍完整，未获得 opaque 字段第二轮原样回传 fixture。该轮没有新的可直接修改根因，不改 retry、EOF、WebSearch opaque 字段或真实 signature。原始抓包 `logs/llm-capture/capture-20261108T044554Z-59d09b8f/` 及三项哈希全部保留。

### I-79：Round 99 抓包复测仍为 82/110，未声明 thinking 的 reasoning 继续被隐藏

Round 99 在默认 tune 配置下得到 `82/110`。抓包 10,325 行、JSON 错误 0、`droppedEvents=0`；22 个客户端入口请求均有 request summary，26 个上游尝试中 25 个 200、1 个 402，402 请求随后换号成功，客户端未收到最终错误。

本轮 10 个声明 thinking 的请求都输出真实非空 Kiro signature；非流请求无 reasoning，`non_stream=10`。另有未声明 thinking 的普通流出现 `reasoningContentEvent`，代理只下发 text，这与当前按请求显示策略隐藏 reasoning 的逻辑一致；没有评分共变证据，暂不扩大过滤器。WebSearch 的公开生命周期完整但仍为 0 分，EOF 无 `COMPLETED` 仍是本地推断终止。该轮不支持修改 retry、EOF、WebSearch opaque 字段或 signature。原始抓包 `logs/llm-capture/capture-20261108T045852Z-e9969a35/` 和三项哈希全部保留。

### I-80：Round 100 全部上游 200，长期失败项仍无共变

Round 100 得到 `82/110`。抓包 7,285 行、JSON 错误 0、`droppedEvents=0`；22/22 客户端流结束，25/25 上游请求均为 HTTP 200，未复现 402、429、502 或最终 `stream_error`。

11 个 thinking 请求均保留真实非空 signature；非流入口上游只有 assistantResponseEvent，`non_stream=10`。WebSearch 原始 `client_sse` 明确存在独立 `websearch` 路由事件，公开 server tool、MCP、结果回灌、citation 和终止事件完整，分析矩阵的入口分类不能当作 WebSearch 缺失证据。EOF 无 `COMPLETED` 仍是多数请求的本地推断终止。评分仍为 `stream_structure=5`、`websearch=0`、`signature_proto=5`，本轮没有支持直接改代码的新因果证据。原始抓包 `logs/llm-capture/capture-20261108T050827Z-cf5654a2/` 和三项哈希全部保留。

### I-81：59138 生产只读抓包确认现有 WebSearch 字段形状，不能解释 CCTest 0 分

对 59138 生产原始抓包的顺序只读扫描再次确认：`encrypted_content` 是明文搜索摘要，`encrypted_index` 是可逆 Base64(`序号:URL`)，与当前 59139 生成逻辑和旧生产部署一致；生产旧版本的两个 WebSearch block 没有 `caller`。该扫描没有写入 59138、没有复制或删除 4GiB 原始文件，也没有读取生产账号状态。

官方文档给出的完整 fixture 仍是 protobuf 风格 opaque 值，生产样本不具备该形状；但生产旧部署同样采用明文/可逆字段，不能把字段差异单独判定为 CCTest `websearch=0` 根因。Round 77/78 的伪 protobuf A/B 也没有让 `websearch` 共变。当前停止继续猜测加密字段，保留生产原始文件作为版本边界证据。

### I-82：Round 101 的 9 次 429 全部恢复，重试不是长期失分根因

Round 101 得到 `82/110`。抓包 8,643 行、JSON 错误 0、`droppedEvents=0`；上游 34 次尝试中 9 次 HTTP 429、25 次 HTTP 200，所有 429 都发生在首输出前并成功恢复，22/22 客户端流闭合，没有最终 502、`stream_error`、重复 block 或错误终止。

12 个 thinking 请求的真实 signature、非流 text 路径和 WebSearch 公开生命周期均正常，三个长期失败项未共变。该轮增加了 retry 恢复证据，不支持修改重试状态机、账号冷却、sticky 或 EOF 推断。原始抓包 `logs/llm-capture/capture-20261108T051857Z-8136f530/` 和三项哈希全部保留。

### I-83：Round 102 两次 402 换号恢复，长期三项仍固定为 82/110

Round 102 在默认 tune 配置（`thinkingTrigger=off`、`channelLeakGuard=false`、`modelDrivenWebSearch=true`）下得到 `82/110`。抓包 `logs/llm-capture/capture-20261108T052832Z-9611b44d/` 共 10,597 行，JSON 错误 0，`droppedEvents=0`；22 个客户端入口全部结束，27 个上游尝试中 25 个 200、2 个 402。两次 402 都发生在首输出前，随后换号取得 200，未出现客户端 `stream_error`、502、重复 block 或错误终态。

本轮 8 个 thinking 请求均保留真实非空 Kiro signature；唯一非流请求仍是上游纯文本，`non_stream=10`。4 个未声明 thinking 的普通流出现上游 reasoning，但下游只发 text，与 Round 96/99 的显示策略观察一致。21 个流式上游 EOF 没有 `COMPLETED`，下游仍通过 `local_inferred_end_turn`/`local_inferred_tool_use` 闭合公开生命周期。WebSearch 公开 server/MCP/result/citation/终止链完整，独立 `websearch` 路由事件存在，但 `websearch=0` 未变。

本轮没有产生能把失分绑定到 402、retry、EOF、WebSearch opaque 字段、真实 signature 或 reasoning 隐藏策略的新因果证据；不修改这些路径。唯一原始 `events.jsonl`、`capture.zip`、`manifest.json` 和矩阵全部保留。

### I-84：历史结果显示 Bedrock 通道的签名评分上限为 5

对本地 103 个结果 JSON 做只读聚合：92 轮带 `streamChannel=aws-bedrock`，其中 92/92 的 `signature_proto` 都是 5；没有一轮 Bedrock 通道得到 10。8 个有分数且 `streamChannel=null` 的早期结果反而全部为 0。Round 102 继续落在第一组：`aws-bedrock + signature_proto=5`。

这把签名失分从“公开字段坏了”收敛为 CCTest 对通道/来源的私有判定。真实 Kiro signature 为空、截断、被代理重编码、synthetic marker 等候选均未在 Round 82–102 抓包中复现；继续改写只会丢失上游证据，不能证明能增加分数。

同时，官方 WebSearch 运行时样本 `/tmp/wscc_stream.out`、`/tmp/wscc_nonstream.out` 展示的 `encrypted_content` 明文摘要和 `encrypted_index=Base64(序号:URL)` 与当前实现一致，公开 server/result/citation/usage/终止形状也一致。因而 WebSearch 0 分不能再单独归因于“必须伪造 protobuf opaque 字段”，剩余候选是 CCTest 私有通道判定、请求回灌条件或内容采样。


### I-85：Round 103 再次固定三项长期失分，402 换号仍正常

Round 103 在默认 tune 配置下得到 `82/110`。抓包 `logs/llm-capture/capture-20261108T054953Z-fd2ddefb/` 共 7,390 行，JSON 错误 0，`droppedEvents=0`；22 个客户端入口全部结束，26 个上游尝试中 25 个 200、1 个 402。该 402 发生在首输出前，随后同一 request 换号取得 200，客户端没有最终错误。

本轮 11 个 thinking 请求均保留真实非空 Kiro signature；唯一非流请求没有上游 reasoning，`non_stream=10`；21 个流式请求的 EOF 仍由本地推断终止；WebSearch 的独立 `client_sse` 路由、MCP 200、公开 block 生命周期和终止事件均存在，但 `websearch=0`。`stream_structure=5`、`signature_proto=5` 与 Round 102 完全相同。

这轮没有新增失败请求级 verifier 证据、官方 signature fixture 或 WebSearch opaque 回传值。402、retry、EOF、公开 SSE 生命周期、真实 signature 和未声明 thinking 隐藏策略继续不能解释长期三项失分，不改这些路径。唯一原始 JSONL、ZIP、manifest、结果和矩阵全部保留。
