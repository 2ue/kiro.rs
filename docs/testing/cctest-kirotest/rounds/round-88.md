# Round 88：非流 thinking 波动与 WebSearch 字段复核

## 时间与边界

- 时间：2026-11-08 11:10–11:13（北京时间；结果和抓包事件为 UTC 03:10–03:13）。
- 59139 使用 debug 增量开发循环和高余额开发账号；59138 只读，未重启、未改配置、未写 PostgreSQL/Redis、未改账号状态。
- 模型：`claude-opus-5-5`；路由：`/dfcache/tune-ss6-99`；并发：1。
- 结果：[`20261108T031052-round-88-capture.json`](../results/20261108T031052-round-88-capture.json)。
- 原始抓包唯一目录：`logs/llm-capture/capture-20261108T031042Z-6baf9b7a/`。
- 抓包停止原因为 `admin_stop`；原始 `events.jsonl`、`capture.zip`、`manifest.json` 全部保留。

## 结果

| 检测项 | Round 87 | Round 88 |
|---|---:|---:|
| tag_check | 10 | 10 |
| stream_structure | 5 | 5 |
| non_stream | 10 | 0 |
| websearch | 0 | 0 |
| signature_proto | 5 | 5 |
| output_config | 10 | 10 |
| server_tool | 10 | 10 |
| knowledge | 10 | 10 |
| model_attribution | 10 | 10 |
| doc_recognition | 10 | 10 |
| image_recognition | 10 | 10 |
| **总分** | **82/110** | **73/110** |

本轮唯一评分变化是 `non_stream: 10→0`；`output_config` 保持 10，三个长期失败项没有变化。

## 抓包统计

- `events.jsonl`：8,639 行，8,428,562 字节；JSON 错误 0，`dropped_events=0`。
- 23 个客户端入口请求、22 个客户端流、30 个上游请求、25 个 HTTP 200、2 个 HTTP 402、3 个 HTTP 429。
- 22/22 客户端流完整；`message_start/message_delta/message_stop` 均为 22；`content_block_start/stop` 为 57/57。
- 上游流结束记录 21 次；MCP 响应 1 次；上游错误体 5 个。
- 402/429 均发生在其它检测请求，后续换号成功，最终没有形成客户端失败响应；不要把这些成功恢复链改成失败。

抓包哈希：

```text
events.jsonl  2910d12b67b4270c082c36c35e8f52760dd8beec2dfe05ff08ec98e7071e5d94
capture.zip   7ffa4f9bf836b0517efb122d6030ecbbfe38a5260ddb039928d79f9ee5c4413
manifest.json fb85ef9ebdadac132ce955f0df7a18cd7b74a836045ad9dff95fdac86f44322d
```

## 新发现：non_stream=0

非流请求：`req_01f2m85h3CQkHM1fG9aAFiKB`。

请求特征：

- `stream` 省略，等价于非流；`model=claude-opus-5-5`；`thinking={"type":"adaptive"}`；未设置 `output_config`。
- 用户消息为 `1+1=?`；tools 数量为 28。
- 上游 HTTP 200，先发 10 个 `reasoningContentEvent`（含真实 signature），随后发 `assistantResponseEvent(content="2")`。
- 下游 HTTP 200，响应 `content` 顺序为 `thinking → text`；thinking 文本为空字符串，signature 保留为真实 Kiro opaque 值；text 为 `2`；`stop_reason=end_turn`。

这与 Round 87 的同类请求形成直接对照：

- Round 87：`req_01LrjuyvKoHJdxGdCwaPsYJD`，同样为 `claude-opus-5-5 + thinking.adaptive + 1+1=?`，上游没有 reasoning，响应只有 `text`，`non_stream=10`。
- Round 88：上游产生 reasoning，代理按当前实现保留空 thinking block 和真实 signature，响应为 `thinking + text`，`non_stream=0`。

当前证据支持“上游是否生成 reasoning 与非流分数共变”，但还不能证明 CCTest 规则是“非流禁止 thinking block”。官方 `thinking.display=omitted` 语义允许保留空 thinking block 与 signature；本地 `src/anthropic/thinking_display.rs` 也有对应单测。因此暂不删除非流 thinking block，先做重复轮次和请求级 A/B。

## WebSearch 请求级证据

请求 ID：`req_01xtEPhTEqusktvLZjHoa79L`。

公开生命周期：

```text
message_start
→ server_tool_use(index=0)
→ input_json_delta(index=0)
→ content_block_stop(index=0)
→ web_search_tool_result(index=1)
→ content_block_stop(index=1)
→ ping
→ text blocks(index=2..9)
→ citations_delta/text_delta
→ message_delta(stop_reason=end_turn)
→ message_stop
```

已确认：

- Kiro API、MCP、回灌 API 均为 HTTP 200；MCP JSON-RPC envelope 完整。
- `server_tool_use.id` 与 `web_search_tool_result.tool_use_id` 配对；两个 block 都有 `caller={"type":"direct"}`。
- MCP 返回 10 个结果；下游 result block 也有 10 项；最终 usage 为 `server_tool_use.web_search_requests=1`。
- 产生 9 个 `web_search_result_location` 引用 delta；每个 `cited_text` 不超过 150 字符；最终 `end_turn/message_stop` 存在。
- 直接字段差异仍未变化：`encrypted_content` 是 MCP snippet 明文，citation `encrypted_index` 是可逆 Base64(`number:url`)；没有第二轮原样回传或官方 opaque fixture。

完整脱敏矩阵见 [`analysis/round-88-websearch-fields.json`](../analysis/round-88-websearch-fields.json)。

## 代码、提示词与版本边界

- 本轮未修改源码；59139 由开发循环自动使用当前工作树增量构建，没有 release 冻结编译。
- 59138 生产镜像仍为 `0.0.239` / revision `30edc00c12431371e22498298715adc7bea539fd`；仓库 HEAD 为 `2e625e52` / tag `v0.0.240`，不能宣称生产已部署当前工作树。
- 请求正文继续出现 Claude Code 自带 `<system-reminder>` / `<thinking_mode>`；本轮没有新的代理自定义提示词 marker。提示词层可能影响模型是否产生 reasoning，但不能单独解释 WebSearch opaque 字段差异。

## 资源监控

- Round 88 记录时 59139 PID `379231`，RSS 约 185 MiB；系统可用内存约 7.5 GiB，swap 可用约 10 GiB，load average 约 `0.78/0.85/1.11`。
- 资源门禁未触发暂停；59139、59138 `/healthz` 均为 200。

## 结论与下一步

本轮没有新的可证实 WebSearch、retry、EOF、真实 signature 或提示词注入根因。`non_stream=0` 需要至少一次“同类请求再次产生 reasoning 后仍失败”的复现，随后再用只针对 59139 的 A/B 判断是否应隐藏整个 thinking block。当前正确模型下最高分仍为 `82/110`，稳定区间为 `73–82/110`。
