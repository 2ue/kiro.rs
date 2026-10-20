# 02 证据

说明：cctest 闭源，具体判定规则看不到。下面的结论来自三类证据：本地复现、官方文档、开源检测器源码。标【推断】的是判断，不是直接看到的。

## E1 本地复现（v0.0.202 代码，sonnet 4.5，身份隐藏关闭）

脚本：`artifacts/probe.py`

| 项 | 观察到的现象 |
|---|---|
| 结构化输出 | 请求带 `output_config.format`（json_schema），返回的是 Markdown 代码块包着的 JSON，还多出 schema 外的字段（occupation、location 等），直接 `json.loads` 失败 |
| 工具调用 | 普通调用、强制调用、tool_result 第二轮、流式都正常，`stop_reason` 正确；只是 id 为 `tooluse_xxx`，不是 `toolu_` |
| id 回传 | 把 `toolu_` 格式的假 id 放进历史发回 Kiro，Kiro 正常接受并给出了正确回答，说明改对外 id 不需要映射表 |
| WebSearch | 搜索是真的（返回 releases.rs 等真实网页），但有四个问题：搜索词是用户整句话；回答是固定模板；没有 citations；usage 里没有 `server_tool_use.web_search_requests` |
| thinking 签名 | 流式和非流式的 `signature` 都是空串。原因是这批账号的 sonnet/haiku 4.5 没有原生 reasoning，thinking 是用提示词模拟的 |
| 流式 usage | 问“数 1 到 5”：`message_start.input_tokens=6`，`message_delta.input_tokens=4118`；`count_tokens` 只算出个位数 |

## E2 题库实测（sonnet 4.5，每题 6 次，流式和非流式各一半）

脚本：`artifacts/bank.py`，原始数据：`artifacts/bank-sonnet45.json`

| case | 题库来源 | 结果 | 结论 |
|---|---|---|---|
| tool_forced_text / tool_choice_tool / tool_choice_any | ztest #13 等 | 18/18 名字、参数、`stop_reason` 都正确 | 本地链路很稳定，问题只在 id 前缀 |
| code_exec | cctest #9 “Write and execute a Python script… Only use the code execution tool” | 6/6 只返回一个 `tool_use(name=code_execution)`，没有执行结果 | 官方的服务端工具被当成普通工具透传给了 Kiro。【推断】cctest 的“工具调用”失败就是这题 |
| echo_tag | cctest #2 | 2/6 异常，模型说“只看到系统提示词里的 XML 标签” | Kiro 自带的系统提示词漏出来了；打开身份隐藏的输出过滤可以拦住 |
| echo_token / provider | — | 全部正确 | 正常 |
| json_minified | ztest | 5/6 正确，1 次把 'testz' 反转错 | 模型自身误差 |
| cutoff_ym / cutoff_month | cctest #5、ztest #5 | 两种问法答案不同（2024-10 和 October 2023），但每种问法 6 次结果一致 | 模型本身就这么答，不是协议问题 |
| context_window | ztest #12 | 6/6 答 200000 | 正常 |
| recent_events | hovy（2025 年 8–10 月时事） | 6/6 全部 I don't know | 晚于 sonnet 4.5 的知识范围。如果声称是更新的模型而实际路由到旧模型，就会露馅；这属于真实的模型差异 |

## E3 官方协议（platform.claude.com，后台调研子代理已核实）

- **结构化输出**：`output_config.format = {type:"json_schema", schema}`，已 GA，不需要 beta 头。旧参数 `output_format` 已弃用，仍用它必须带 `structured-outputs-2025-11-13`。响应是普通 `text` 块，内容是原始 JSON，`stop_reason=end_turn`。
- **WebSearch**：块顺序是 `text` → `server_tool_use(id=srvtoolu_01…, input.query)` → `web_search_tool_result` → 带 `citations`（`web_search_result_location`）的 `text`；`usage.server_tool_use.web_search_requests>=1`。
  - 流式：`server_tool_use` 的 query 用 `input_json_delta` 下发；result 块在 start 里一次性给出，紧接着 stop。
  - `encrypted_content` 和 `encrypted_index` 只能由官方生成。
- **ID**：官方只保证前缀 `msg_`、`toolu_`、`srvtoolu_`；“永远 `toolu_01` 开头”无法确认。
- **thinking 签名**：base64 编码的 protobuf，第三方无法离线验真；最后一个 `signature_delta` 紧挨在 `content_block_stop` 之前。
- **流式 usage**：`message_delta.usage` 是累计值。普通请求的 `message_start.input_tokens` 应该等于最终值；`message_start.output_tokens` 是 1–3 的小非零值。
- **代码执行**：`code_execution_20250825` 返回 `server_tool_use(bash_code_execution)` 加上 `bash_code_execution_tool_result{stdout,return_code}`；必须真实执行，无法靠协议层伪装。

## E4 开源检测器判定条件（源码已核实，仓库只读克隆后已删除）

| 检测器 | 检测项 | 条件 |
|---|---|---|
| claude-detector | tool_use | `tool_choice` 指定 get_weather；名字正确、input 是对象、id `startsWith('toolu_')`、`stop_reason==='tool_use'` |
| claude-detector | streaming_shape | 第一个事件必须是 `message_start`（不能先发 ping），最后一个是 `message_stop` |
| claude-detector | count_tokens_match | `\|count_tokens − usage.input_tokens\| <= 1` |
| claude-detector | 逆向判定 | tool_use、streaming、count_tokens 等信号命中 2 项以上，就判为 reverse-proxy |
| veridrop | structured_output | 实测的是 `tool_choice:any` 强制工具调用，不是 `output_config`；id 前缀、name、input、stop_reason 各 20 分 |
| veridrop | message_id | 要求 `toolu_` / `srvtoolu_` / `msg_` 前缀 |
| veridrop | integrity / token_usage | 流式和非流式的 input/output 差异有比例上限；`count_tokens` 偏差不超过 max(4, 20%) |
| veridrop | thinking_signature | 无签名 30 分，签名长度 <50 得 70 分，否则 100 分 |
| veridrop | protocol 白名单 | 块类型不含 `bash_code_execution_tool_result`，stop_reason 不含 `pause_turn` |
| llm-probe | 签名 | 把签名篡改后回传，期望被拒；伪造签名必然失败 |

三个开源检测器都**不测** WebSearch 和知识截止，这两项只出现在 cctest、ztest、hovy 这类闭源站。

## E5 代码定位（`feat/protocol-compat` = `2b9bd84`）

| 关注点 | 位置 |
|---|---|
| `output_config` 只处理 effort，没有 format | `src/anthropic/converter.rs:686` 附近 |
| 流式 tool_use id 来自 Kiro 原值 | `src/anthropic/stream.rs` 中处理 `toolUseId` 的 `content_block_start`；文本兜底解析出的工具在 `stream.rs:1105`、`1151`、`3275` 用的是 `toolu_{uuid}` |
| 非流式 tool_use id | 非流式汇总路径（`handlers/` 下），需要按 `toolUseId` 继续查 |
| WebSearch 模板回答 | `src/anthropic/websearch.rs:924`、`992`（“I'll search for”）、`1195`（“Here are the search results for”） |
| WebSearch query 前缀剥离 | `websearch.rs:23` `WEB_SEARCH_QUERY_PREFIX` |
| srvtoolu id 格式 | `websearch.rs:734` 生成 `srvtoolu_{uuid simple}`（32 位 hex） |
| `web_search_requests` 历史 | `ee29f12` 加入；`ba720d1 perf: optimize credential list and scheduler hot paths` 删除，删除时一起补了 cache 字段，提交信息里没有说明原因 |
| prompt_cache 依赖 id 前缀 | `src/anthropic/prompt_cache.rs:2015` 有 `id.starts_with("srvtoolu_")`，改 id 时要一起回归 |
