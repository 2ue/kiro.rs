# Round 89：非流对照复测恢复

## 时间与边界

- 时间：2026-11-08 11:29–11:32（北京时间；结果和抓包事件为 UTC 03:29–03:32）。
- 59139 使用 debug 增量开发循环和高余额开发账号；59138 只读，未重启、未改配置、未写 PostgreSQL/Redis、未改账号状态。
- 模型：`claude-opus-5-5`；路由：`/dfcache/tune-ss6-99`；并发：1。
- 结果：[`20261108T032954-round-89-capture.json`](../results/20261108T032954-round-89-capture.json)。
- 原始抓包唯一目录：`logs/llm-capture/capture-20261108T032943Z-7b2c6207/`。

## 结果

| 检测项 | Round 88 | Round 89 |
|---|---:|---:|
| tag_check | 10 | 10 |
| stream_structure | 5 | 5 |
| non_stream | 0 | 10 |
| websearch | 0 | 0 |
| signature_proto | 5 | 5 |
| output_config | 10 | 10 |
| server_tool | 10 | 10 |
| 其它保护项 | 38 | 40 |
| **总分** | **73/110** | **82/110** |

本轮 `non_stream` 恢复 10，其他目标项和 Round 88 相同。正确模型下最高分仍为 `82/110`。

## 抓包统计与哈希

- `events.jsonl`：9,295 行，8,584,028 字节；JSON 错误 0，`dropped_events=0`。
- 23 个客户端入口请求、22 个客户端流、27 个上游请求、25 个 HTTP 200、2 个 HTTP 402；402 均成功换号。
- 22/22 客户端流完整；`message_start/message_delta/message_stop` 均为 22；`content_block_start/stop` 为 59/59。
- MCP 响应 1 次；上游流结束 21 次；无 429/500。

```text
events.jsonl  bfbff84b703d52e247a99de09d0c7388e23b7a120d525778aec07633b09e534e
capture.zip   0131816ca5238d7bdd1afb971c9441d96ee7092f1f38b29c235ad065645fb1cd
manifest.json d4fb263ff97ac6477d89c29f05fe54940b8d59b1ca6c532c39ade95024ae2f50
```

## non_stream 对照

非流请求：`req_017Ey65jki5mweDWPtYbbbA1`。

- 请求仍为 `model=claude-opus-5-5`、`thinking={"type":"adaptive"}`、用户消息 `1+1=?`，tools 数量为 28。
- 上游 HTTP 200，只返回 `assistantResponseEvent` 文本片段 `1`、` `、`+ 1 = 2`，没有 `reasoningContentEvent`。
- 下游 JSON `content` 只有一个 `text` block，文本为 `1 + 1 = 2`，`stop_reason=end_turn`，`non_stream=10`。

与 Round 88 组成最小采样对照：同类请求在上游产生 reasoning 时，Round 88 返回空 thinking block 并得 0；本轮上游没有 reasoning，返回 text 并得 10。当前证据更支持“上游 reasoning 采样决定 CCTest 是否看到空 thinking block”，而不是代码每次稳定地产生错误 block。仍不能删除官方 omitted thinking 语义，需要后续重复样本或 verifier 证据。

## WebSearch

WebSearch 请求：`req_01K1hJhhJw5Q4iy3nmnpW2dy`。

- Kiro API、MCP、回灌 API 均为 200；server tool/result ID 配对，两个 block 都带 `caller=direct`。
- MCP 和下游结果均为 10 项；引用 9 项；usage 为 `web_search_requests=1`；`end_turn/message_stop` 完整。
- `encrypted_content` 仍是 MCP snippet 明文，`encrypted_index` 仍是可逆 Base64(`number:url`)；`websearch=0` 没有变化。

## 代码、资源与生产边界

- 本轮未修改源码；59139 继续由开发循环自动增量编译，没有 release 冻结编译。
- Round 89 记录时 59139 RSS 约 183 MiB；59139、59138 `/healthz` 均为 200；资源门禁未触发暂停。
- 59138 仍为 `0.0.239` / revision `30edc00c12431371e22498298715adc7bea539fd`，只读参照；未向其执行任何写操作。
- 原始抓包不能删除；完整字段矩阵见 [`analysis/round-89-websearch-fields.json`](../analysis/round-89-websearch-fields.json)。

## 结论

Round 89 没有为三个长期失败项提供新的可证实根因，但完成了 Round 88 非流差异的首次反向对照。下一轮继续完整检测；若再次出现 reasoning+空 thinking 且 `non_stream=0`，再在 59139 做最小 A/B，不触碰 59138。
