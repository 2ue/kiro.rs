# P13 原生 WebSearch 仿真不完整：域名过滤被忽略、无 citations / 模型摘要、`encrypted_content` 装明文、usage 不含 `server_tool_use`

Status: open / documented / not-fixed
Severity: Medium
Area: response
Discovered: 2026-09-28 协议互转审计
Verified-against: a4227c1 (2026-09-29)

## 问题与影响

原生 WebSearch（`type: web_search_YYYYMMDD`，`name: web_search`）在本代理中不是由模型驱动的 server tool loop。它是固定流程：取最后一个 user 文本作为 query，执行一次 Kiro MCP 调用，再用模板拼出 assistant 回复。已有文档 [WebSearch/MCP 协议、错误、usage、attempt 与隐私边界](../websearch-mcp-protocol-usage-and-privacy.md) 已经关闭了"错误伪装成功、stream/non-stream 不分、query 取错轮次、同名工具被劫持、usage 缺失、隐私日志"等问题。本文只记录剩下的**协议保真度**差距：

| # | 差距 | 是否有意为之 | 影响 |
| --- | --- | --- | --- |
| 1 | `allowed_domains` / `blocked_domains` / `user_location` 被 serde 静默丢弃，MCP 只收到 `query` | 否（没有文档说明） | Claude Code `WebSearch` 的 `allowed_domains` / `blocked_domains` 参数（推断：CLI 会把它们写进 tool 定义）完全不生效，结果里可能出现用户明确排除的站点 |
| 2 | `max_uses` 只解析不读取；每个请求固定搜索 1 次 | 部分有意（固定单次） | `max_uses >= 1` 时不影响；`max_uses: 0` 或负数时不报错，照样搜索 |
| 3 | query 由代理从最后一个 user 文本推导（去掉 `Perform a web search for the query:` 前缀），不是模型选择的 | 有意 | 贴合 Claude Code 的 WebSearch 子请求形态；对直接调用 API、希望模型自己拟定 query 或多次搜索的客户端，语义不同 |
| 4 | 最终文本是模板结果列表（标题、200 字符摘要、URL），不是模型生成的摘要，也没有 `citations` | 有意（没有第二轮推理），citations 缺失未说明 | 直接调用 API 的客户端拿不到答案，只拿到链接列表；依赖 `web_search_result_location` 做引用渲染的 UI 没有数据 |
| 5 | `web_search_result.encrypted_content` 放的是明文 snippet | 未说明 | 官方语义是只能原样回传的不透明密文；本代理写入明文，历史转换时又不读取，属于名不副实 |
| 6 | `usage.server_tool_use.web_search_requests` 被刻意移除 | 有意（有测试固化，但没有书面理由） | 客户端和下游计费看不到搜索次数；Claude Code 的费用和搜索计数显示为 0（推断） |
| 7 | 历史回放时 `server_tool_use` / `web_search_tool_result` 被 converter 丢弃 | 隐含设计 | 后续轮次模型只能看到模板摘要文本（这也是模板里保留标题、摘要、URL 的实际作用），看不到结构化结果 |

定级 Medium：Claude Code 的主要路径（WebSearch 子请求）可以正常工作，有真实 CLI 证据；但 #1 会让用户显式设置的域名约束失效，#6 让计费和观测失真，都是可见的行为偏差。

## 官方协议对照

> 方向约定：kiro.rs 基于 Kiro 对外提供 Claude Code（Anthropic）协议接口。客户端（如 Claude Code CLI）发出 Claude Code 协议请求，经转换后变为 Kiro 协议，调度到最终上游 Kiro；Kiro 的响应再转换回 Claude Code 协议返回给客户端。下文引用的"Anthropic 文档/官方"一律指 **Claude Code 协议规范**，即接口侧必须满足的行为；"Kiro 侧"指上游 Kiro 协议的要求。Anthropic 官方 API 不是本项目的上游。thinking 签名均为 Kiro 原生签名，在转换中原样透传。

已验证事实（Anthropic Web search tool 文档）：

- Tool 定义字段：`type`（如 `web_search_20250305`）、`name: "web_search"`、`max_uses`（可选，正整数）、`allowed_domains` / `blocked_domains`（可选，二者互斥）、`user_location`（可选，`{type:"approximate", city, region, country, timezone}`）。
- 模型自己决定何时搜索、搜索什么、搜索几次（受 `max_uses` 限制）。超过次数时，返回 `web_search_tool_result_error`，`error_code` 为 `max_uses_exceeded`。
- 响应块：`server_tool_use`（`id` 以 `srvtoolu_` 开头，`input.query`），然后是 `web_search_tool_result`，`content` 为 `web_search_result[]`，每项含 `url`、`title`、`encrypted_content`、`page_age`。
- 最终 text 块带 `citations`，类型为 `web_search_result_location`，字段包括 `url`、`title`、`encrypted_index`、`cited_text`。
- `encrypted_content` / `encrypted_index` 是不透明密文，多轮对话时必须原样回传，才能继续引用这些结果。
- usage 含 `server_tool_use: { web_search_requests: N }`，按次计费。

本代理已对齐的部分（不在本 issue 范围）：`srvtoolu_` 前缀、`server_tool_use` 与 `web_search_tool_result.tool_use_id` 配对、`page_age` 的日期格式、错误码枚举（`src/anthropic/websearch.rs:300-320`）、可恢复失败时返回 HTTP 200 加 `web_search_tool_result_error`（依据见 `docs/analysis/websearch-tool-error-stability-fix-20260927.md`）。

经验推断（未验证）：

- Claude Code 的 `WebSearch` 工具会单独发起一个子请求（tools 里只有 `web_search_*`，user 文本是 `Perform a web search for the query: …`），然后从响应的 `web_search_tool_result` 和 text 块中整理出链接和摘要，交给主模型。所以对 Claude Code 来说，"代理推导 query + 模板摘要"基本够用。
- Kiro MCP `web_search` 的 `arguments` 是否支持域名过滤参数，未知。

## 源码链与根因

检测与路由：

- `src/anthropic/websearch.rs:351-364`：`is_versioned_web_search_tool_type` / `is_native_web_search_tool` 要求 `web_search_` 加 8 位数字，且 `name == "web_search"`。
- `src/anthropic/websearch.rs:401-405` `has_web_search_tool`：纯 native 的条件是只有一个 tool。
- `src/anthropic/websearch.rs:411-415` `has_mixed_native_web_search_tool`：tool 数大于 1 且包含 native。
- `src/anthropic/handlers.rs:6482-6507`：只要包含 native WebSearch（`has_native_web_search_tool`），不管是纯 native 还是混合，都进入 MCP 分支。混合时，普通 client tools 在这一轮被忽略，这是已有文档记录的边界（"not yet implement a complete Anthropic mixed native/client tool state machine"）。审计里说"只在恰好一个 tool 时激活"不准确：2026-07-31 之后，混合场景也会激活。

请求字段丢失：

- `src/anthropic/types.rs:390-409` `Tool`：只有 `tool_type`、`name`、`description`、`input_schema`、`max_uses`、`cache_control`。没有 `allowed_domains`、`blocked_domains`、`user_location`，也没有设置 `deny_unknown_fields`，所以这三个字段在反序列化时被静默丢弃。
- 全仓库中 `max_uses` 只在 `src/anthropic/prompt_cache.rs:796`（cache key）、`src/anthropic/converter/tools.rs:379`（工具去重比较）、`src/anthropic/converter.rs:2251`（测试）出现，WebSearch 执行路径从不读取。
- `src/anthropic/websearch.rs:473-499` `create_mcp_request`：`arguments` 只有 `query`（第 492-494 行）。

query 推导：

- `src/anthropic/websearch.rs:420-441` `extract_search_query`：取最后一个 `role == "user"` 消息里最后一个非空 text block，去掉 `WEB_SEARCH_QUERY_PREFIX`（第 23 行）。有意的设计，依据见 [WebSearch/MCP 协议文档](../websearch-mcp-protocol-usage-and-privacy.md)"query 只从最后一个 user turn 内提取"。

响应生成：

- `src/anthropic/websearch.rs:604-629` `websearch_result_content`：第 621 行 `"encrypted_content": result.snippet.clone().unwrap_or_default()`。
- `src/anthropic/websearch.rs:662-702`（non-stream）和 `723-905`（stream）：固定四个 block，依次是决策文本 `I'll search for "…".`、`server_tool_use`、`web_search_tool_result`、摘要文本。摘要文本没有 `citations` 字段。
- `src/anthropic/websearch.rs:959-985` `generate_search_summary`：模板列表，snippet 截断到 200 字符，末尾加免责声明。
- usage：`src/anthropic/websearch.rs:695-700`、`740-747`、`887-892` 只有 `input_tokens`、`output_tokens`、两个 cache 字段。

`server_tool_use` usage 被移除的来历：

- `git log -S test_websearch_usage_is_sub2api_compatible` 指向提交 `ba720d1`（2026-06-18，标题是 `perf: optimize credential list and scheduler hot paths`）。这个提交删除了 `message_delta.usage.server_tool_use.web_search_requests: 1`，同时新增测试 `test_websearch_usage_is_sub2api_compatible`（现位于 `src/anthropic/websearch.rs:2023-2054`，第 2038 行和第 2051 行断言 `server_tool_use` 不存在）。
- 提交标题和仓库文档都没有书面理由，只能从测试名推断：为了兼容下游中转平台 sub2api 的 usage 解析或计费。这是有意的选择，本文不推翻，只要求把它变成可配置并补上书面理由。

历史回放丢弃：

- `src/anthropic/converter/history.rs:285-346`：assistant block 只处理 `thinking`、`redacted_thinking`、`text`、`tool_use`，其余走 `_ => {}`（第 345 行）。`server_tool_use` 和 `web_search_tool_result` 都被丢弃，所以 `encrypted_content` 从不会被回读。

根因：WebSearch 是"单次 MCP 加合成响应"的适配器，不是代理内的 agent loop。已有工作优先保证正确性和隐私，协议里的可选字段（域名、位置、次数）以及引用和计费元数据，没有纳入这份合同。

## 复现

### 最小复现（单测）

放进 `src/anthropic/websearch.rs` 的 `mod tests`（沿用现有 `Tool` / `MessagesRequest` 构造方式）：

```rust
#[test]
fn native_websearch_domain_filters_survive_request_parsing() {
    let tool: Tool = serde_json::from_value(serde_json::json!({
        "type": "web_search_20250305",
        "name": "web_search",
        "max_uses": 5,
        "allowed_domains": ["docs.rs"],
        "user_location": {"type": "approximate", "country": "US"}
    }))
    .expect("tool parses");
    let round_trip = serde_json::to_value(&tool).unwrap();
    // 当前失败：字段在反序列化时被静默丢弃
    assert_eq!(round_trip["allowed_domains"], serde_json::json!(["docs.rs"]));
    assert_eq!(round_trip["user_location"]["country"], "US");
}

#[test]
fn websearch_results_respect_blocked_domains() {
    // 修复后应存在的纯函数：按 host 过滤结果（blocked 命中子域也排除）
    let results = WebSearchResults {
        results: vec![
            WebSearchResult {
                title: "keep".into(),
                url: "https://docs.rs/tokio".into(),
                snippet: None, published_date: None, id: None, domain: None,
                max_verbatim_word_limit: None, public_domain: None,
            },
            WebSearchResult {
                title: "drop".into(),
                url: "https://www.example.com/x".into(),
                snippet: None, published_date: None, id: None, domain: None,
                max_verbatim_word_limit: None, public_domain: None,
            },
        ],
        total_results: Some(2),
        query: None,
        error: None,
    };
    let filtered = apply_domain_filters(results, &[], &["example.com".to_string()]);
    assert_eq!(filtered.results.len(), 1);
    assert_eq!(filtered.results[0].title, "keep");
}
```

`server_tool_use` usage：修复时要**有意识地**改写现有测试 `test_websearch_usage_is_sub2api_compatible`（`src/anthropic/websearch.rs:2023-2054`）：在新配置项开启时，断言 `message_delta.usage.server_tool_use.web_search_requests == 1`；关闭时保持现有断言。不要直接删掉旧断言。

### 端到端复现

```bash
curl -s http://127.0.0.1:19023/cc/v1/messages -H 'x-api-key: <key>' \
  -H 'content-type: application/json' -d '{
  "model":"claude-sonnet-4-6","max_tokens":1024,"stream":false,
  "tools":[{"type":"web_search_20250305","name":"web_search","max_uses":1,
            "blocked_domains":["github.com"]}],
  "messages":[{"role":"user","content":"Perform a web search for the query: tokio runtime github"}]
}' | jq '{urls: [.content[] | select(.type=="web_search_tool_result") | .content[].url],
         citations: [.content[] | select(.type=="text") | .citations],
         enc: [.content[] | select(.type=="web_search_tool_result") | .content[0].encrypted_content],
         usage}'
```

预期观察：结果中仍有 `github.com` 的 URL；`citations` 全为 null；`encrypted_content` 是可读的英文摘要；`usage` 中没有 `server_tool_use`。

真实 Claude Code：在隔离的 `HOME` / `CLAUDE_CONFIG_DIR` 下，指向 `/cc`，提示"只在 docs.rs 上搜索 tokio spawn_blocking"，让 CLI 带上 `allowed_domains` 调用 `WebSearch`。在 usage 记录和 MCP 捕获中确认：MCP 请求只有 `query`，返回的链接里有非 docs.rs 的域名。

## 修复方案

### 候选方案

A. 只补文档，承认这些是仿真边界。成本为零，但 #1 域名过滤的实际偏差仍然存在。

B. 分级补齐（推荐）：先做确定性、低风险的字段（域名过滤、`max_uses` 校验、可配置的 `server_tool_use`），再考虑 citations，最后才是可选的模型摘要。

C. 实现完整的 server tool loop：模型选 query，然后多次搜索，把结果注入上下文再推理，输出带 citations 的答案。最接近官方，但需要新的受预算和 deadline 约束的 inference loop 架构（`docs/analysis/websearch-tool-error-stability-fix-20260927.md` 已明确不在当时的范围内），成本和风险都高。

### 推荐方案

采用 B，按优先级排列：

1. **域名过滤（P0）**：`Tool` 增加 `allowed_domains`、`blocked_domains`、`user_location`（`Option<…>`），serde 原样保留。
   - 两个字段同时出现时，返回本地 `400 invalid_request_error`，与官方的互斥规则一致。
   - MCP 返回后，按 URL host 做后过滤：allowed 要求 host 等于该域名或是其子域；blocked 命中该域名或其子域就排除。全部被过滤掉时，按"合法零结果"处理，不算失败。
   - 如果确认 Kiro MCP 支持 `site:` 语法，可以额外改写 query，但后过滤始终作为兜底。
   - `user_location` 暂不下发（上游能力未知），在 `x-kiro-rs-warnings` 或 debug 观测中标记"已忽略"。
2. **`max_uses` 校验（P1）**：`max_uses <= 0` 时返回本地 400，与官方"正整数"要求一致；`>= 1` 时维持单次搜索。
3. **`server_tool_use` usage（P1）**：新增配置项（例如 `webSearch.reportServerToolUse`），按路由策略解析，默认值与官方一致（开启）。sub2api 这类下游如果确实不兼容，在对应路由上关闭。开启时，stream 的 `message_delta.usage` 和 non-stream 的 `usage` 都输出 `server_tool_use: {web_search_requests: 1}`；工具错误路径输出 0。同时补上当初移除它的书面理由，作为配置项的说明。
4. **`encrypted_content`（P2）**：明文写入这一点在 README 或合同文档中明确说明。如果以后要支持回放引用，改成带版本前缀的可逆编码（例如 `kiro-ws-v1:` + base64），converter 识别后还原成文本注入。当前不建议伪造"加密"。
5. **citations（P2）**：模板摘要没有真实的 `cited_text` 定位，伪造 `encrypted_index` 会误导客户端，所以默认不生成。等方案 C 的模型摘要落地后，再用真实的引用片段生成 `web_search_result_location`。
6. **模型摘要（P3，可选）**：作为按路由启用的 opt-in。MCP 成功后，把结果作为 context 发起一次受预算和 deadline 约束的 Kiro 推理，用它替换模板摘要；推理失败时回退到模板。必须复用 shared attempt ledger 和 usage 记录。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)，只使用 `127.0.0.1:19023` 指定测试实例和隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，Cargo 通过 `feature/tests/run-cargo-scoped.sh` 运行，不得影响本机正在运行的 Claude Code CLI 与服务。

- 请求解析：`allowed_domains`、`blocked_domains`、`user_location` 反序列化后能原样保留；二者同时出现时返回 400，且 0 次 MCP 调用。
- 过滤：fake MCP 返回 `a.com`、`sub.a.com`、`b.com` 三条结果。`allowed=[a.com]` 剩 2 条；`blocked=[a.com]` 剩 1 条；全部过滤后得到合法零结果，stream 和 non-stream 各 5 轮。
- `max_uses`：0 和 -1 返回本地 400；1 和 8 正常搜索，MCP 恰好调用 1 次。
- usage：配置开启时，stream 与 non-stream 都有 `server_tool_use.web_search_requests == 1`；关闭时和现状一致；工具错误路径为 0。usage 记录的 MCP attempt 与之一致。
- 回归：[WebSearch/MCP 协议文档](../websearch-mcp-protocol-usage-and-privacy.md) 的验收矩阵（识别、query、stream、错误、预算、隐私）全部重跑，结果不变。
- 真实 Claude Code：隔离环境下带 `allowed_domains` 的 `WebSearch`，返回的链接全部落在允许的域名内，CLI 正常完成（`terminalReason=completed`）。

## 兼容性与风险

- 恢复 `server_tool_use` 可能破坏 sub2api 一类下游。当初移除正是因为兼容性，所以必须按路由可配置，并且先在目标下游上验证。
- 域名后过滤会减少结果数，可能从"有结果"变成"零结果"。这符合用户意图，但需要在摘要中说明"结果已按域名过滤"。
- 新增 `Tool` 字段会影响 `prompt_cache.rs:796` 的 cache key 和 `converter/tools.rs:379` 的去重比较。需要确认序列化字段顺序稳定，并确认同一请求的 cache 命中率不会下降。
- 普通（非 native）工具如果恰好有同名字段，serde 也会保留下来；只有在 WebSearch 分支里才读取这些字段，对普通工具转换没有影响。
- 方案 C 和第 6 步会引入额外的推理成本和延迟，只能 opt-in。
