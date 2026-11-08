# Round 99：完整抓包复测仍为 82/110

## 时间与边界

- 时间：2026-11-08 13:00–13:04（北京时间；抓包事件 UTC 05:00–05:03）。
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`，后端 59139；模型 `claude-opus-5-5`；并发 1。
- 59139 保持默认 tune 配置：`thinkingTrigger=off`、`channelLeakGuard=false`、`modelDrivenWebSearch=true`。
- 本轮没有源码修改，59139 继续由 debug 增量开发循环管理；59138 只读健康检查通过，未重启、未写生产配置或账号状态。
- 结果：[`20261108T050049-round-99-capture.json`](../results/20261108T050049-round-99-capture.json)。
- 原始抓包唯一目录：`logs/llm-capture/capture-20261108T045852Z-e9969a35/`。
- 请求矩阵：[`analysis/round-99-request-matrix.json`](../analysis/round-99-request-matrix.json)。

## 结果

| 检测项 | Round 98 | Round 99 |
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

- `events.jsonl`：10,325 行，8,916,151 字节；JSON 错误 0；`droppedEvents=0`。
- 22 个客户端入口请求、21/22 请求级公开生命周期完整；23 个 request summary；26 个上游尝试，其中 25 个 HTTP 200、1 个 HTTP 402。
- 1 个 MCP response；未出现最终 502、`stream_error` 或客户端断流。
- 哈希：

```text
events.jsonl  45f52d27f0ac008c1703cf755b102482166eb528699bb8a4fb53aa5e3c308e1f
capture.zip   94903fe049e0801210bc58fae7b1cc3d1788faf8267bf174adb2791b85652cf5
manifest.json 4c9b1d6e4c68dac62d78325ca36e646c05a55ff87eadb84a692c805b42b7c510
```

## 请求级协议证据

- 10 个 thinking 请求下发 `thinking` block 与真实非空 signature；其中 thinking/tool 请求的 block 配对完整。
- 非流请求 `req_01HEpjbSz4JAppJWLf2esFFb` 上游只有一个 `assistantResponseEvent`，无 reasoning；下游保持 `non_stream=10`。
- 另有未声明 thinking 的普通流请求出现 `reasoningContentEvent`，但下游按请求显示策略只输出 text；该组合没有和分数变化共变。
- 大多数上游以 EOF 结束且无 `COMPLETED`，代理按 `local_inferred_end_turn` 或 `local_inferred_tool_use` 完成；下游 message 生命周期仍闭合。
- WebSearch 请求 `req_01oPw1vtrWonZLr9zrUyj9FD` 的 server tool、MCP 200、`tool_use_id` 配对、10 项结果、citations、usage 和 `message_stop` 均存在；`websearch=0` 未改变，仍没有官方 opaque 字段第二轮原样回传 fixture。

## 重试证据

请求 `req_01sMAHWzcTsA3M3xGcwku4BC` 首次使用账号 2535 返回 HTTP 402，随后换号重试返回 HTTP 200；下游只收到一套完整响应，最终 request summary 为完成状态。该链路不支持修改 retry 状态机。

## 结论

Round99 继续确认最高分为 `82/110`，稳定区间为 `73–82/110`。三个长期失败项仍是 `stream_structure=5`、`websearch=0`、`signature_proto=5`。本轮没有新的可直接修改根因，继续不伪造 `COMPLETED`、不随机生成 WebSearch opaque 字段、不改写真实 signature。唯一原始抓包、结果文件和请求矩阵全部保留。
