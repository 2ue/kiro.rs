# Round 90：429 恢复链与非流稳定复测

## 时间与边界

- 时间：2026-11-08 11:38–11:40（北京时间；结果和抓包事件为 UTC 03:38–03:40）。
- 59139 使用 debug 增量开发循环和正余额开发账号；59138 只读，未重启、未改配置、未写 PostgreSQL/Redis、未改账号状态。
- 模型：`claude-opus-5-5`；路由：`/dfcache/tune-ss6-99`；并发：1。
- 结果：[`20261108T033815-round-90-capture.json`](../results/20261108T033815-round-90-capture.json)。
- 原始抓包唯一目录：`logs/llm-capture/capture-20261108T033805Z-6e3de299/`。

## 结果

| 检测项 | Round 89 | Round 90 |
|---|---:|---:|
| tag_check | 10 | 10 |
| stream_structure | 5 | 5 |
| non_stream | 10 | 10 |
| websearch | 0 | 0 |
| signature_proto | 5 | 5 |
| output_config | 10 | 10 |
| server_tool | 10 | 10 |
| 其它保护项 | 40 | 40 |
| **总分** | **82/110** | **82/110** |

本轮与 Round 89 完全同分；当前正确模型最高分仍为 `82/110`，稳定区间仍为 `73–82/110`。

## 抓包统计与哈希

- `events.jsonl`：7,266 行，8,499,265 字节；JSON 错误 0，`dropped_events=0`。
- 23 个客户端入口请求、22 个客户端流、36 个上游请求、25 个 HTTP 200、11 个 HTTP 429。
- 22/22 客户端流完整；`message_start/message_delta/message_stop` 均为 22；`content_block_start/stop` 为 56/56。
- MCP 响应 1 次；上游流结束 21 次；无 402/500。

```text
events.jsonl  e105efe1f5ecb08f8c58e2fcb33f32211ad22e0caa7a4101fc77fc6bf8094265
capture.zip   65f3cbeda59bd97e1155b8b79019c87d9cea20f766c89202eb1dc4398ceb2dfa
manifest.json 5bee2faee6d43876fce8972153a747257965ac7087d0065ee5a4963e3463217d
```

## 429 恢复链

非流请求 `req_01QRp1BupHmLkJvYEKGxeDhT` 在同一请求内经历 5 次上游 429，随后第 6 次 HTTP 200，最终下游响应只有 text，`stop_reason=end_turn`，`non_stream=10`。其它流式请求也出现 429，但所有客户端流最终闭合，request summary 均保持成功终态。

这轮没有复现“429 后成功重试仍记录 `stream_error`”的旧生产问题；不能因为出现 429 就修改 retry 状态机。后续若再次出现最终 `stream_error`，必须保存同一 requestId 的完整 attempt、`upstream_stream_end`、客户端 SSE 和 summary，再做最小修复。

## WebSearch

WebSearch 请求：`req_01edHceixe2fKbNtxF2D4sqS`。

- Kiro API、MCP、回灌 API 均为 200；server tool/result ID 配对，两个 block 均带 `caller=direct`。
- MCP 和下游结果均为 10 项；引用数量随模型回答为本轮实际值；usage 为 `web_search_requests=1`；`end_turn/message_stop` 完整。
- `encrypted_content` 仍是 MCP snippet 明文，`encrypted_index` 仍为可逆 Base64(`number:url`)；`websearch=0` 没有变化。

## 非流与代码边界

- 本轮非流没有上游 reasoning，响应只有 text；Round 88 的 reasoning+空 thinking block 差异没有再次出现。
- 本轮未修改源码；59139 继续由开发循环增量编译，没有 release 冻结编译。
- 59139 RSS 约 183 MiB；59139、59138 `/healthz` 均为 200；资源门禁未触发暂停。
- 59138 仍为 `0.0.239` / revision `30edc00c12431371e22498298715adc7bea539fd`，只读参照；当前开发账号池只剩 2528/2529 正余额 fast 账号，未从 59138 写入或改动账号。

## 结论

Round 90 继续证明公开 WebSearch 结构和 429 恢复链正常，但没有改善三个长期失败项。原始抓包继续保留；完整字段矩阵见 [`analysis/round-90-websearch-fields.json`](../analysis/round-90-websearch-fields.json)。
