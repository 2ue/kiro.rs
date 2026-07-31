# WebSearch/MCP 协议、错误、usage、attempt 与隐私边界

Status: `implemented / focused-verified / native-cli-and-auxiliary-gates-open`

Severity: P1

Date: 2026-07-16

## 修复前问题、现象与影响

纯 `web_search` 请求走 `src/anthropic/websearch.rs` 的特殊路径。修复前它不经过正常 local/external response、usage 和 shared inference-attempt 状态机，存在以下互相独立的异常：

- MCP 网络、HTTP、JSON-RPC 或解析失败被吞掉并改成 `search_results=None`，下游仍收到 HTTP 200、完整 SSE terminal 和 `No results found`；真实错误被伪装成业务成功。
- `payload.stream=false` 仍无条件返回 SSE；stream/non-stream 协议合同不一致。
- query 从第一条 message 的第一个 block 提取。长会话中第一条通常是旧历史，当前用户 query 在最后一条 user message，可能搜索错误内容。
- 只按 `tool.name == "web_search"` 判断特殊路由；普通用户自定义同名工具即使没有 Anthropic server-tool type 也会被劫持。
- MCP provider retry 不使用 Messages 的 shared attempt ledger；usage records 也没有 MCP hit、错误、request-key channel 或恢复事实。
- WebSearch 直接 return，完全没有写 `UsageRecorder`；UI/生产证据会看到下游成功，却无法关联真实 MCP attempt 和失败。
- 默认 info log 写完整 query，debug log 写完整 MCP request/response；查询、prompt、URL、项目名、结果摘要或误贴 token 会进入普通日志。
- MCP 完整 response 使用 `response.text()` 聚合，只有时间上限，没有 Content-Length/chunked 字节上限。

这些问题不会出现 `bashHash...` 指纹，但属于用户要求的“搜索、tool、usage、显示与逻辑异常”同等发布阻断项。历史 MCP fixture 成功不能证明 native WebSearch；单次成功也不能覆盖错误被伪装成成功的分支。

## 当前结论

focused 修复与重复测试已经关闭上述 correctness/privacy/client-cancel 主缺陷：

- 只有唯一且 type/name 都匹配的 Anthropic server WebSearch 才走特殊路径；普通同名 client tool 与 mixed tools 走正常工具路径。
- query 只从最后一个 user turn 内提取，不再跨 turn 回退；当前 turn 只有 tool result 或空白时稳定 400、0 MCP hit。
- MCP HTTP、timeout、disconnect、body limit、JSON-RPC、`isError`、non-text、ID mismatch 均 fail closed；合法零结果仍保持真实成功。
- stream 与 non-stream 分别返回 SSE 与 Anthropic JSON message。
- success、error、stream drop、MCP body 中途取消都保留 request-scoped usage/credential/attempt 所有权。
- shared budget 增加显式 MCP channel，满足 `consumed = local + external + mcp`，MCP 不再混入 local attempts。
- raw query/result/private body 不进入公开错误、usage 或 DEBUG tracing。
- success renderer 只生成一次 summary；stream 以有界字符块增量构造事件，不再 clone 结果并重复生成 summary 或分配整段 `Vec<char>`。

仍不能标记为最终发布完成：Claude Code CLI 2.1.197 的 native WebSearch 5-session gate 尚未执行；Profile discovery、token refresh、model catalog 的生产 request-scoped auxiliary attribution 尚未完成；当前证据来自共享 dirty tree，而不是冻结 release candidate。

## 2026-07-29 本地账号复验

本轮使用本地账号 credential `7` / `8`，外部池关闭，只验证 `claude-sonnet-4.5` 相关路径。新的结论需要区分三种 WebSearch 形态：

- 纯 Anthropic native server-side WebSearch 可用：
  - body 只有 `tools: [{"type":"web_search_20250305","name":"web_search","max_uses":1}]`
  - request `req_01yPTQ3uUhHq89z8FGZQycZ9`
  - route `local_credential/local_success`
  - credential `8`
  - upstream `claude-sonnet-4.5`
  - response 包含 `server_tool_use` 与 `web_search_tool_result`
- Claude Code CLI `WebSearch` 当前没有形成真实工具调用：
  - CLI case `websearch_tool`
  - model `claude-sonnet-4.5`
  - exit `0`
  - `toolUseNames=[]`
  - `toolResultCount=0`
  - final usage `server_tool_use.web_search_requests=0`
  - assistant text 出现 `<search_web><query>...</query></search_web>` 伪 XML，但没有真实执行搜索
- mixed native WebSearch + 普通 tool 不是已支持路径：
  - request `req_01H7Q6sMoZEAN7kyan5zLYjL`
  - body 同时包含 native `web_search_20250305` 与 `echo_value`
  - route `local_credential/local_success`，credential `8`，upstream `claude-sonnet-4.5`
  - response `stop_reason="tool_use"`，内容是普通 `tool_use name="web_search" input={}`
  - 没有 `server_tool_use`，没有 `web_search_tool_result`

源码原因仍是 `src/anthropic/websearch.rs:260` 的 `has_web_search_tool` 只识别 `tools.len() == 1` 的 native WebSearch；`src/anthropic/handlers.rs:5707` 只有在该 predicate 为 true 时才进入 native WebSearch MCP branch。这个选择可以防止普通同名工具被劫持，但也意味着 mixed native WebSearch 需要明确产品决策：要么支持 server-side 执行并继续 turn，要么 fail closed 返回清晰错误，不能把 `web_search` 下发成无人执行的普通 tool。

## 根因与代码链

修复前链路：

```text
post_messages_inner
  -> has_web_search_tool（只有一个 tool + name=web_search）
  -> token estimate
  -> handle_websearch_request（early return）
     -> extract_search_query（messages.first / first block）
     -> provider.call_mcp（自己的 retry 链）
     -> Err => warn + None
     -> create_websearch_sse_stream（忽略 payload.stream）
     -> 生成成功 message_delta/message_stop
```

根因是特殊能力适配器拥有了一套平行的请求识别、retry、错误、response render 和 usage 逻辑，却没有复用 Messages request context、attempt ledger、stream renderer、usage recorder和公开错误归一化合同。

隐私问题另有直接根因：普通 tracing 把 content-bearing query、MCP request JSON 和完整 response body作为字段/消息写出，没有经过 bounded diagnostic recorder、redaction 或 opt-in。

## 红绿复现方案

使用本地 fake MCP、隔离 provider 和当前 Router，每类至少 5 轮。修复前按下列方式观察缺陷，修复后复用相同 fixture 验证相反合同：

1. `stream=false` 的 canonical WebSearch：检查旧实现仍返回 `text/event-stream`。
2. fake MCP 返回 400/429/500、header timeout、slow/chunked body、断流、malformed JSON、JSON-RPC error、`isError=true`、result content 非 text：检查旧实现仍 200/terminal/`No results found`。
3. 两条 user message，第一条 `OLD_QUERY_MARKER`、最后一条 `NEW_QUERY_MARKER`：fake MCP capture 旧实现发送了第一条。
4. tool name 为 `web_search`、`type` 为空或普通 client tool：检查被错误路由到 server WebSearch。
5. 对同一请求 API key 执行 429/500 burst：比较 downstream requests、MCP HTTP hits、ListAvailableProfiles hits 和 UsageRecord；旧路径 usage delta 为 0。
6. 在 query/MCP result 中放唯一 secret marker，捕获 info/debug 日志：旧日志能匹配 marker。
7. chunked response 持续超过选定字节上限：旧实现持续聚合到超时/内存边界。
8. 错误 burst 后发送正常搜索 5 次：记录是否恢复及 credential cooldown/refresh 是否异常。

长会话复现必须把 WebSearch 放在 20/100 tool cycle 和 120k history/resume 中，确认 query 取当前 turn、usage 不为 0、tool blocks/terminal 顺序正确且没有内部错误文本。

## 已落地修复与优化方案

已落地方案：

- canonical 特殊路由同时验证 server-tool type/name；普通同名 client tool 留在正常 tool path。
- query 从最后一个 user turn 的有效 text block 提取；空白、非 text 或歧义内容本地稳定 400、0 MCP hit。
- 在进入 WebSearch 前建立与普通 Messages 相同的 request context：request/error ID、request API key stable channel ID、shared upstream ledger、started time、model/stream/input usage。
- MCP 每次真实 HTTP send 消耗显式 `mcp` channel attempt；`localAttempts`、`externalAttempts`、`mcpAttempts` 三通道求和严格等于 `consumed`。
- MCP HTTP/JSON-RPC/`isError`/malformed/timeout/over-limit 在首输出前返回规范错误；不得合成“无结果”成功。真正的零搜索结果只能来自合法成功结果。
- 从同一个结构化 WebSearch result 分别渲染 stream SSE 与 non-stream Anthropic message，事件/blocks/usage 保持同一语义。
- 成功、失败、client drop 均写 UsageRecord；记录实际 MCP/profile attempts、route subtype、public/internal error 和 request-key stable ID，但不存原始 key/query/result。
- 普通 tracing 只保留 request ID、query bytes/chars、单向短 digest、result count、HTTP/error class、latency；删除 raw query、MCP request 和 response body。
- MCP response 在读取前检查 Content-Length，并对 chunked body执行累计字节限制与总 deadline；超限立即取消，不能等待完整聚合。
- handler 在 await MCP 前建立 request-scoped attribution sink；真实 send reserve 后立即登记 pending，完成/失败更新固定 attempt，pre-response RAII guard 在客户端取消时固化 `fail/client_dropped/mcp_client_cancelled` 并写 usage。

该修复不能简单把错误改成空数组；那仍会伪造成功。也不能只新增一条 log，因为当前缺的是 request-scoped 所有权和完整状态机。

Profile discovery、token refresh 与 model catalog 的 production request-scoped attribution 没有被本修复冒充完成，继续作为 auxiliary admission/attribution 独立任务。

## 验收与测试矩阵

| 维度 | 场景 | 轮次 | 验收 |
| --- | --- | ---: | --- |
| 识别 | canonical type/name、普通同名 tool、混合 tools | 每格 5 | 只有 canonical pure server WebSearch 进入特殊路径 |
| query | single turn、20/100 turns、array/string、旧首条/新末条、空/非 text | 每格 5 | 只发送当前最后 user query；非法输入 0 MCP |
| stream | stream true/false、正常/零结果 | 每格 5 | content type/body形态正确；blocks、stop、usage 等价 |
| 错误 | 400/429/500/timeout/disconnect/malformed/JSON-RPC/isError | 每格 5 | 无假成功；规范公开错误 + error ID；内部证据保留 |
| 预算 | 1/20/60 credentials、profile missing、MCP burst | 每格 5 | MCP/profile/refresh 分账；实际 hits 不超过硬预算且不随账号数线性增长 |
| 资源 | Content-Length/chunked over-limit、slow body、client drop | 每格 5 | 有限字节/时间；future/socket/permit释放；随后 5/5 恢复 |
| usage | local success/error/drop、不同 request key | 每格 5 | UsageRecord 有 channel ID、attempts、route/error；无原始 key/query |
| 隐私 | info/debug marker capture | 每格 5 | query/request/response marker 0 命中；保留 bounded metadata |
| CLI | native WebSearch 可用时真实 Claude CLI | 5 session | 实际能力，不以 MCP fixture 冒充；usage/terminal/错误正确 |

## 当前验证与证据

完整命令、轮次、时间与边界见 [WebSearch/MCP 聚焦证据](../evidence/websearch-mcp-protocol-usage-privacy-20260716.md)。当前 focused 结果：

- extractor 红测修复前稳定复用 `stale-query-1`，修复后 WebSearch parser `18/18`。
- handler WebSearch `8/8` tests；其中 13 类 MCP 错误 x 5 + recovery x 5 为 `70/70`。
- canonical/custom/mixed `15/15`；20/100 tool cycle `10/10`；non-text/blank current turn `10/10` 且 0 MCP；stream/non-stream zero result `10/10`。
- full/never-polled/partial stream ownership `15/15`；等待 MCP headers 与读取 body 时取消各 5 轮、合计 `10/10`；每轮均保留稳定 request-key digest、credential 与明确 attempt，且只有 1 个 MCP hit。
- provider MCP `7/7`，覆盖 1/20/60 credentials 各 5 轮、shared hard budget、lease/drop、body limit 和 acquire failure。
- inference attempt budget `13/13`；attribution sink 内部 `5/5`；两套 UI contract 与 production build 通过；独立 target `cargo check --all-targets` 0 warning。
- raw query/result/private marker 在公开 body、usage 与 current-thread DEBUG capture 中均 0 命中。

历史 deep audit 只证明隔离 MCP 工具 `search_fixture` 3/3 成功，并明确说明 Claude CLI 2.1.197 当时没有暴露 native WebSearch；该历史证据和当前 fake MCP 矩阵都不能关闭 native CLI gate。

## 性能边界

正常 WebSearch 只允许一条有界 MCP 工作流；不得因 usage 记录、digest 或 stream/non-stream renderer 对 query/result做重复全量序列化。response 字节在读入时累计，不先 `.text()` 再检查。

新增 attribution sink 只维护 request-scoped 内存状态，不访问 Redis/PG，也不新增 HTTP 调用。attempt vector 受共享上限约束，当前最多 4 项，因此 snapshot/clone 是小常数。1/20/60 账号矩阵证明账号数增长不会让单请求 MCP sends 超过硬预算。

success renderer 已从两次 summary 生成和整段 `Vec<char>` 分块收敛为单次 summary + bounded 增量字符块；优化后 parser `18/18` 与 handler `8/8` 再次通过，stream/non-stream blocks 和 usage 语义未变。

发布前记录 MCP/header/body/first-output/total p50/p95/p99、实际 HTTP hits、RSS/FD起峰终值和错误后恢复。Redis/global request-key 协调若加入热路径，必须有短 timeout、breaker 与本地有界降级，不能重现 scheduler Redis degraded 429 风暴。

## 残余风险、回滚与限制

- Claude CLI 当前环境可能不暴露 native WebSearch；这只能记为环境限制，不能把 MCP fixture 写成 native pass。
- Profile discovery、token refresh、model catalog 尚缺生产 request-scoped channel/usage attribution；需要单独完成，不能因 `mcpAttempts` 已存在而关闭。
- 搜索结果本身是不可信外部内容；协议正确不等于内容可信，摘要还需保留长度/引用/注入边界。
- 删除 raw logs 可能减少临时排障信息；回滚只能启用经过脱敏、配额、到期清理的 break-glass capture，不能恢复普通 tracing 原文。
- 若 non-stream support 无法与官方块语义对齐，应明确本地拒绝而不是返回 SSE；最终选择必须由 direct protocol fixture 和真实客户端证据决定。
- 任何回滚不得恢复 `Err => None => success`、无界 body 或无 usage/attempt 的旧行为。
- 当前共享 dirty tree 没有最终 candidate SHA、release binary SHA-256、HTTP/CLI/load soak；完成这些 gate 前不得据此发版。
