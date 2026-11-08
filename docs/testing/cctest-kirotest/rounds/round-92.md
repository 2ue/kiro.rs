# Round 92：非流再次无 reasoning，402/429 恢复链复核

## 时间与边界

- 时间：2026-11-08 11:59–12:03（北京时间；结果和抓包事件为 UTC 03:59–04:03）。
- 59139 使用 debug 增量开发循环；59138 只读，未重启、未改配置、未写 PostgreSQL/Redis、未改账号状态。
- 模型：`claude-opus-5-5`；路由：`/dfcache/tune-ss6-99`；并发：1。
- 结果：[`20261108T035956-round-92-capture.json`](../results/20261108T035956-round-92-capture.json)。
- 原始抓包唯一目录：`logs/llm-capture/capture-20261108T035947Z-629d5eb9/`。
- 请求矩阵：[`analysis/round-92-request-matrix.json`](../analysis/round-92-request-matrix.json)。

## 结果

| 检测项 | Round 91 | Round 92 |
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

本轮仍为当前正确模型的最高分，稳定区间保持 `73–82/110`。

## 抓包统计

- `events.jsonl`：10,542 行，9,100,072 字节；JSON 错误 0，`dropped_events=0`。
- 23 个客户端入口请求、22 个客户端流、28 个上游请求、25 个 HTTP 200、2 个 HTTP 402、1 个 HTTP 429。
- 22/22 客户端流完整；`message_start/message_delta/message_stop` 均为 22；`content_block_start/stop` 为 59/59。
- MCP 响应 1 次；上游流结束 21 次；非流请求 1 次。

## 非流采样

非流请求 `req_01C9vGkjHfpPp2m9pm5yEvJQ` 的入参仍为 `claude-opus-5-5`、`thinking` 未显式设置、消息 `1+1=?`。上游只返回一个 `assistantResponseEvent(content="2")`，没有 `reasoningContentEvent`；代理返回单一 `text` block、`stop_reason=end_turn`，CCTest `non_stream=10`。

本轮没有捕获到“上游 reasoning → 非流空 thinking block”的新样本，因此没有条件实施删除 thinking block 的 59139-only A/B。Round 88 的唯一 `non_stream=0` 样本仍不能单独作为全局修复依据。

## 重试与公开流

本轮出现 2 次 402 和 1 次 429，均在上游首输出前完成换号或重试并取得 200；22/22 客户端流闭合，没有最终 `stream_error`、502 或重复 block。该轮继续支持现有恢复链，不能据此修改 retry 或账号池。

## WebSearch、签名和代码

WebSearch 公开事件、MCP 回灌、ID/caller 配对和 usage 延续 Round 91 的完整形状，评分仍为 `websearch=0`。8 个 thinking 流均有非空真实 Kiro signature；`signature_proto=5` 没有变化。

本轮未修改源码，59139 继续热更新开发模式，没有 release 冻结编译。59138 `/healthz` 只读检查保持通过；原始 JSONL、ZIP、manifest 和结果文件全部保留。

## 结论

Round 92 没有产生新的可直接改变评分的代码证据。三个长期失败项仍为 `stream_structure=5`、`websearch=0`、`signature_proto=5`；非流继续为 10。下一轮继续完整检测，直到获得可归因的失败样本或满足连续三轮 `110/110`。
