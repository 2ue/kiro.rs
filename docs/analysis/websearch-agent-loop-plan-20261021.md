# WebSearch 改为两轮推理 agent 循环：问题分析与改造方案

日期：2026-10-21

## 结论

原生 `web_search` 请求在 cctest 等检测站上一直不过，原因在架构上：整条链路不经过模型推理。代理把最后一条 user 文本整句当作 query，调用一次上游 MCP，然后返回模板文本。检测站发的是普通 API 请求（例如"搜索今天的 XX，回答并给出来源"），它拿到的是一份结果列表，问题本身没有得到回答，也没有 citations，流式事件和 usage 的形态也和官方不一致。

改造方向是参照官方语义，把这条链路改成服务端 agent 循环：模型决定搜什么，代理执行搜索，结果回灌给模型，最后由模型带引用作答。

## 需要改动的现状与证据

| # | 官方行为 | 当前实现 | 代码位置 |
|---|---|---|---|
| 1 | 模型读完结果后回答问题 | 返回 `Here are the search results for "...":` 加结果列表，没有回答 | `src/anthropic/websearch.rs:1164` `generate_search_summary` |
| 2 | 由模型决定是否搜索、搜什么，必要时多次搜索 | 整句用户原文直接作为 query（截断到 200 字符），只搜一次；`max_uses` 只做规范化，不参与执行 | `websearch.rs:444` `extract_search_query`，`websearch.rs:590` |
| 3 | 文本块始终带 `citations`（`web_search_result_location`） | 没有 citations | `websearch.rs:867`、`websearch.rs:928` |
| 4 | 流式时 `server_tool_use` 的 `content_block_start` 为 `input: {}`，query 通过 `input_json_delta` 推送 | 在 `content_block_start` 里一次性给全 input | `websearch.rs:991-1006` |
| 5 | `encrypted_content` 是不透明串，多轮对话原样回传后可还原 | 填的是明文 snippet | `websearch.rs:809` `websearch_result_content` |
| 6 | usage 是真实推理用量 | `output_tokens` 按字符数除以 4 估算，`input_tokens` 是本地估算；服务端记录为 `UsageSource::RequestEstimate` | `websearch.rs:843`，`src/anthropic/handlers.rs:7504`、`7575` |
| 7 | `usage.server_tool_use.web_search_requests` 记录搜索次数 | 为兼容 sub2api 被刻意删除，并有测试断言字段不存在 | `websearch.rs:2496-2523` |
| 8 | 搜索失败时模型拿到错误结果后继续作答 | 可恢复失败合成 `web_search_tool_result_error` 后直接结束本轮，没有后续推理 | `websearch.rs:1251` |

分流入口在 `handlers.rs:7454`（`has_native_web_search_tool`），改造集中在这个分支和 `websearch.rs`。

## 改造方案

### 1. 两轮（多轮）推理 agent 循环

- 第一轮：把原生 `web_search_*` 工具替换为一个普通工具 `web_search`，`input_schema` 为 `{query: string}`，再按普通对话路径发给上游模型。由模型决定是否搜索、搜什么。
- 模型没有调用 `web_search` 时，直接返回它的文本回答（官方语义：不需要搜索时不搜）。
- 模型调用了 `web_search` 时，代理用模型给出的 query 调 MCP，过滤 `allowed_domains`/`blocked_domains` 的逻辑保持不变，再把结果作为 `tool_result` 回灌，进入下一轮推理。
- 循环上限：有 `max_uses` 时取 `max_uses`；未设置时用一个代理侧的保守上限（作为配置项）。超出上限后再次调用，回灌 `max_uses_exceeded` 错误，让模型据此收尾。
- MCP 可恢复失败（限流、超时、JSON-RPC 错误等）作为 `web_search_tool_result_error` 回灌，模型继续作答。这也解决了第 8 项。协议坏包仍然按硬失败处理，保持现有分界。
- Claude Code 的 WebSearch 子请求（`Perform a web search for the query: X`）走同一条循环，模型会直接用 X 搜索，不需要再单独剥前缀。
- 最终响应的块顺序对齐官方：
  - 第一轮的说明文本（如果有）；
  - 每次搜索一对 `server_tool_use`（id 以 `srvtoolu_` 开头）和 `web_search_tool_result`（带 `tool_use_id` 配对）；
  - 最后是带 citations 的文本块。
- 首输出前的换号重试、推理预算（`InferenceAttemptBudget`）、外部池 fallback 都要覆盖到每一轮推理。

### 2. 合成 citations

- 回灌结果时给每条结果编号，并在提示里要求模型用 `[n]` 标注所引用的来源。
- 后处理最终文本：在标注处切分文本块，去掉标注，把对应结果映射成 `web_search_result_location`：
  - `url`、`title` 取结果本身；
  - `cited_text` 从 snippet 截取，最多 150 字符；
  - `encrypted_index` 写入可还原的结果定位信息（编码方式同第 3 节）。
- 编号越界或格式不对的标注按普通文本保留，不能产生悬空引用。

### 3. 流式事件、encrypted_content 与 usage

- 流式：
  - `server_tool_use` 的 `content_block_start` 改为 `input: {}`，随后用 `input_json_delta` 推送 query，再 `content_block_stop`；
  - 文本块的引用用 `citations_delta` 下发；
  - 搜索执行期间保持心跳；
  - `content_block` 的 index 在多轮之间连续递增。
- `encrypted_content`：改为带版本前缀的可逆编码，内容包含 url、title、snippet、page_age，必要时加 HMAC 防篡改。历史回放时在 transcript 渲染阶段解码成文本，继续沿用现有上限（单块最多 8000 字符、最多 20 条结果）。这只是编码，不是保密手段。
- usage：对外返回和服务端记录都改为累加各轮上游推理的真实用量，不再用字符数估算，记录来源也不再是 `RequestEstimate`。

### 4. `web_search_requests`（待定）

这一项和之前"兼容 sub2api / 不动 usage 口径"的决定冲突，建议做成运行配置开关：
- 关闭（默认）：保持现状，不输出该字段。
- 开启：在 `message_delta.usage` 和非流式 `usage` 中输出 `server_tool_use.web_search_requests`，取值为成功执行的搜索次数，出错的搜索不计入。

## 验证要点

- 单元测试：citations 标注解析与越界、`encrypted_content` 编解码往返、`max_uses` 上限、流式事件序列（`input_json_delta`、`citations_delta`、index 连续）。
- handler 层（fake 上游）：
  - 模型不搜索、搜索一次、搜索多次、超出上限；
  - MCP 可恢复失败后模型继续作答；
  - 硬失败；
  - stream 和 non-stream 都要覆盖。
- 真实账号：只用 haiku-4.5 / sonnet-4.5，分别跑 API 直调和 Claude Code CLI 的 WebSearch 子请求。
- 检测站：改完后在 cctest 复测，记录结果。

## 成本与预期

- 每次搜索至少多一次模型推理，推理成本和延迟都会上升。
- cctest 的 FAQ 写明会解析 thinking 签名（Protobuf）来识别渠道，并点名了若干 IDE 逆向渠道。所以即使 WebSearch 这一项通过，整体判定大概率仍是"疑似逆向"。这次改造的价值在于直接调用原生 web_search 的 API 客户端能拿到正确语义（回答问题，带引用），而不是让检测站整体判定为官方。

## 分析来源

官方文档：
- Web search tool：<https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-search-tool>。依据：块结构与顺序、流式 `input_json_delta`、citations 字段（`cited_text` 最多 150 字符、`encrypted_index`）、`encrypted_content` 须原样回传、错误码与 HTTP 200 语义、`usage.server_tool_use.web_search_requests`。

检测站：
- CCTest FAQ：<https://cctest.ai/zh/faq>。依据：检测项包括流结构、非流结构和签名渠道识别，并点名 IDE 逆向渠道。网站闭源，WebSearch 检测项的具体请求和判定未公开。
- canarybyte/veridrop：<https://github.com/canarybyte/veridrop>。它会被动检查 `server_tool_use.id` 是否以 `srvtoolu_` 开头，以及 content 块类型白名单。

同类项目：
- BerriAI/litellm（websearch_interception）：<https://github.com/BerriAI/litellm/issues/26252>。唯一完整实现服务端 agent 循环的项目：把原生工具改写为普通工具，模型给出 query，代理执行搜索，回灌 tool_result，模型作答，再回填 `server_tool_use` 和 `web_search_tool_result`。本方案的主要参考。
- router-for-me/CLIProxyAPI（Antigravity 执行器）：上游 Gemini grounding 自带引用，能输出 `web_search_result_location` 和 `citations_delta`。citations 形态的参考。
- hank9999/kiro.rs：<https://github.com/hank9999/kiro.rs>。本项目上游，同样是"取整句 query、调一次 MCP、套模板"，说明问题来自原始设计。
- Wei-Shaw/sub2api、jwadow/kiro-gateway：同样采用模板式模拟，不经过模型推理。

本地取证：
- 读代码核对上表各位置。未在本地实例上实际请求：测试实例 19023 当前使用运行时 key，配置文件中的 key 被拒绝（401），未改动凭据。
