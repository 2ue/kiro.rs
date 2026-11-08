# Round 101：429 重试恢复后仍为 82/110

## 时间与边界

- 时间：2026-11-08 13:18–13:21（北京时间；抓包事件 UTC 05:18–05:20）。
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`，后端 59139；模型 `claude-opus-5-5`；并发 1。
- 59139 默认 tune 配置未改变；本轮无源码修改，59139 继续 debug 增量开发循环；59138 仅只读健康检查。
- 结果：[`20261108T051857-round-101-capture.json`](../results/20261108T051857-round-101-capture.json)。
- 原始抓包：`logs/llm-capture/capture-20261108T051857Z-8136f530/`。
- 请求矩阵：[`analysis/round-101-request-matrix.json`](../analysis/round-101-request-matrix.json)。

## 结果

| 检测项 | Round 100 | Round 101 |
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

## 抓包完整性

- `events.jsonl`：8,643 行，8,959,206 字节；JSON 错误 0；`droppedEvents=0`。
- 22 个客户端入口请求、22/22 客户端流结束、23 个 request summary。
- 上游共 34 次尝试：25 个 HTTP 200、9 个 HTTP 429；9 次 429 均在首输出前恢复，未出现最终 502、`stream_error`、重复响应或客户端断流。
- MCP response 1 次；公开 message 生命周期有 22 个 start/delta/stop。
- 哈希：

```text
events.jsonl  e024ac0d8b79dd4ade94cba107b41c1534067fbb341353fbf1cb7f3313f02caa
capture.zip   f94ed7aa04cb1d4c5759eaa6636edefb8e10c37e3721a0b45230798b40b47ff5
manifest.json e1f9963ea81e357a8bfdab595a993423af361cb488c90a40daf3f039d1a9c3ea
```

## 请求级证据

- 12 个 thinking 请求均有真实非空 signature；非流请求保持 `assistantResponseEvent` 文本路径，`non_stream=10`。
- 21 个入口请求的上游 EOF 无显式 `COMPLETED`，本地按 `local_inferred_end_turn` 或 `local_inferred_tool_use` 完成；下游公开生命周期闭合。
- WebSearch 原始 `client_sse` 路由事件和 MCP 200 仍存在，公开 server/result、citation、usage、终止事件完整；`websearch=0` 未变化。

## 429 恢复

本轮 9 个 429 均出现在首输出前，随后同一 request 的后续尝试返回 200 并完成。没有出现 429 后向客户端泄漏内部错误、重复 block 或错误终止，因此当前重试状态机有成功恢复证据，不改 retry、账号冷却或 sticky 策略。

## 结论

Round101 仍为 `82/110`，长期失败项保持 `stream_structure=5`、`websearch=0`、`signature_proto=5`。429 恢复不是本轮失分原因；继续保持原始抓包和开发模式，不做 release 冻结编译。
