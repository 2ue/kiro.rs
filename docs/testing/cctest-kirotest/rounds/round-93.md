# Round 93：非流 adaptive 仍无 reasoning，重试恢复稳定

## 时间与边界

- 时间：2026-11-08 12:05–12:08（北京时间；结果和抓包事件为 UTC 04:05–04:08）。
- 59139 使用 debug 增量开发循环；59138 只读，未重启、未改配置、未写 PostgreSQL/Redis、未改账号状态。
- 模型：`claude-opus-5-5`；路由：`/dfcache/tune-ss6-99`；并发：1。
- 结果：[`20261108T040526-round-93-capture.json`](../results/20261108T040526-round-93-capture.json)。
- 原始抓包唯一目录：`logs/llm-capture/capture-20261108T040518Z-8e0b65a1/`。
- 请求矩阵：[`analysis/round-93-request-matrix.json`](../analysis/round-93-request-matrix.json)。

## 结果

| 检测项 | Round 92 | Round 93 |
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

## 抓包与非流样本

- `events.jsonl`：9,511 行，8,568,129 字节；JSON 错误 0，`dropped_events=0`。
- 23 个客户端入口请求、22 个客户端流、26 个上游请求、25 个 HTTP 200、1 个 HTTP 429。
- 22/22 客户端流完整；`content_block_start/stop` 为 60/60；MCP 响应 1 次。
- 非流请求 `req_01ACMVWdTXZ4pe7GA48ZysYY` 使用 `thinking={"type":"adaptive"}`，上游产生 3 个 `assistantResponseEvent`，没有 `reasoningContentEvent`；代理返回单一 `text` block，`non_stream=10`。

Round 93 仍没有“上游 reasoning → 非流空 thinking block”的新样本，继续不启动删除 thinking block A/B，不修改官方 `thinking.display=omitted` 语义。

## 重试、WebSearch 与签名

本轮只有 1 个上游 429，首输出前重试得到 200；无最终 `stream_error`、502 或重复 block。WebSearch API/MCP/回灌生命周期与前轮一致，`websearch=0` 未变化。含 thinking 的流仍输出非空真实 Kiro signature，`signature_proto=5` 未变化。

## 代码与证据

本轮未修改源码，59139 继续热更新开发模式；59138 只读健康检查通过。原始 JSONL、ZIP、manifest 和结果文件全部保留。

## 结论

Round 93 没有新增可直接改变评分的代码证据。当前最高分仍为 `82/110`，三个长期失败项仍为 `stream_structure=5`、`websearch=0`、`signature_proto=5`。
