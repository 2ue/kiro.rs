# 03 分析结论与待实施方案

## 逐项根因

| cctest 项 | 结果 | 根因 | 本次是否处理 |
|---|---|---|---|
| 工具调用 | 失败 | ① id 前缀是 `tooluse_`；② 【推断】主要失败点是代码执行题（cctest #9），代理没有沙箱 | ① 处理；② 不改（用户要求） |
| 结构化输出 | 失败 | `output_config.format` 完全没有处理 | 处理 |
| WebSearch | 失败 | 搜索词不是模型生成的，回答是模板，没有 citations，没有 `web_search_requests` | 处理，做成开关 |
| 签名校验 | 5/10 | 上游没有签名，thinking 是用提示词模拟的 | 不伪造 |
| 流结构校验 | 5/10 | start 和 delta 的 input_tokens 不一致，count_tokens 偏差大 | 不处理（不改 usage 的约束） |
| 知识库检测 | 通过 | 不是 RAG，是用只有真 Claude 才答对的题（截止时间、Anthropic 事实）鉴别模型真伪 | 无需处理 |

## 对用户问题的回答

- **代码执行是不是伪造的？** 没有伪造结果，但也没有执行。官方的 `code_execution` 工具被当成普通工具透传给 Kiro，模型吐出一个 `tool_use` 就结束了，客户端拿不到 stdout。真实执行需要 Anthropic 的沙箱，协议层补不出来。按用户要求不修改。
- **WebSearch 和测试通过有没有关系？** 有直接关系。WebSearch 那一项判失败，就是因为回答是模板、缺 citations 和搜索次数。开关打开后才有机会通过。cctest 的判定规则看不到，能不能通过需要上线后实测。
- **是否只影响对外协议层？** 大部分是。id 改写只动协议层，结构化输出只往 Kiro 请求里多加一个工具，仍然是一次请求对应一次调用。WebSearch 是例外，一个用户请求会对应多次 Kiro 调用，会多消耗额度。

## 方案（已获用户确认的范围）

### P1 工具调用 id 改为官方格式（只动协议层）

- 返回给客户端的 `tool_use.id` 统一成 `toolu_01` + base62（约 24 位）；服务端工具 id 统一成 `srvtoolu_01` + base62。
- 用**确定性映射**：由 Kiro 原 id 哈希得到对外 id。原因是 Kiro 可能对同一个 `toolUseId` 分多个 chunk 下发，重试或去重时得到的 id 也要一致。
- 在接入点一次性规范化，让内部状态（`tool_block_indices`、`tool_input_buffers`、`response_tool_gate`、tool_name_map、AskUserQuestion 等）只使用一个 id 空间。不要在输出边界再做二次改写。
- 已验证 Kiro 接受 `toolu_` id 在历史中回传，所以不需要反向映射。
- 要排查的点：`stream.rs` 的 Kiro 原 id 路径、`stream.rs:1105/1151/3275` 的 uuid 路径、非流式汇总路径、`websearch.rs:734`、`prompt_cache.rs:2015`、外部池透传路径（外部池返回的 id 是否已经合规）。

### P2 结构化输出

- 解析 `output_config.format`（json_schema）和旧版 `output_format`（需要带 beta 头）。
- 转换时把 schema 注入为一个内部工具（例如 `__structured_output`），设置 `tool_choice` 强制调用。
- 返回时把这个工具的 input 还原成一个纯 JSON 的 `text` 块，`stop_reason` 改为 `end_turn`；非流式和流式都要处理，流式里把 `input_json_delta` 改成 `text_delta`。
- 和用户自带的 tools 同时出现时：官方允许模型先调用工具，所以不能强制调用内部工具，要允许模型二选一（细节待定，见 04）。
- usage 不做额外处理，按现有链路原样计算。

### P3 WebSearch 开关

- 新增开关（全局默认值加路由级覆盖，具体形式见 04），关闭时保持现有模板行为，Claude Code 照常使用。
- 打开后的流程：
  1. 模型生成搜索词。
  2. 调用 Kiro MCP 执行真实搜索。
  3. 把结果交给模型写回答，并带上 citations。
  4. 输出 `server_tool_use` → `web_search_tool_result` → 带 citations 的 `text`。
- citations 的 `encrypted_index` 和 result 的 `encrypted_content` 无法合法生成，保持现状（现有实现填什么就填什么），不伪造官方加密串。
- 几次调用都在同一个账号、同一次调度里完成，只占一个调度名额。
- `usage.server_tool_use.web_search_requests` 只作为协议字段追加，不改 token 计算。这个字段曾在 `ba720d1` 被删掉，加回前要先查清删除原因（见 04）。

### 明确不做

- 代码执行沙箱、伪造执行结果。
- 伪造 thinking 签名或 `encrypted_*`。
- 修改 usage 数值和口径（流结构校验、count_tokens 偏差因此保持现状）。

## 测试计划

1. 每项补单元测试：id 格式和确定性、结构化输出非流式/流式还原、WebSearch 开关两种状态。
2. 隔离实例用真实账号回归：`artifacts/probe.py`、`artifacts/bank.py` 多轮重复，对比 `bank-sonnet45.json` 基线。
3. 本地 Claude Code 实际跑多轮工具调用，验证新 id 被回传后不会出错（参见记忆“本地 CC 真实测试环境坑”）。
4. 单跑一次全量测试，复用共享编译目录，结束后清理资源。
5. 合并发布后在 cctest 复测，确认实际通过情况。

## 风险

- cctest 闭源：即使三项都改完，“工具调用”仍可能因为代码执行题失败，不能承诺通过。
- WebSearch 开关打开后，每次搜索多 1–2 次 Kiro 调用，账号额度消耗会上升。
- 改 id 涉及流式状态机里的多个 map，容易漏掉某条路径，导致 tool_result 对不上。
