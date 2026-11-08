# Round 100：完整抓包复测仍为 82/110

## 时间与边界

- 时间：2026-11-08 13:08–13:11（北京时间；抓包事件 UTC 05:08–05:10）。
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`，后端 59139；模型 `claude-opus-5-5`；并发 1。
- 59139 保持默认 tune 配置：`thinkingTrigger=off`、`channelLeakGuard=false`、`modelDrivenWebSearch=true`。
- 本轮无源码修改，继续使用 debug 增量开发循环；59138 只读健康检查通过，未重启、未写生产配置或账号状态。
- 结果：[`20261108T050827-round-100-capture.json`](../results/20261108T050827-round-100-capture.json)。
- 原始抓包唯一目录：`logs/llm-capture/capture-20261108T050827Z-cf5654a2/`。
- 请求矩阵：[`analysis/round-100-request-matrix.json`](../analysis/round-100-request-matrix.json)。

## 结果

| 检测项 | Round 99 | Round 100 |
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

- `events.jsonl`：7,285 行，7,633,858 字节；JSON 错误 0；`droppedEvents=0`。
- 22 个客户端入口请求、22/22 客户端流结束、25 个上游请求且全部 HTTP 200；MCP response 1 次。
- 公开下游有 22 个 `message_start`、22 个 `message_delta`、22 个 `message_stop`；未出现最终 502、`stream_error` 或客户端断流。
- 哈希：

```text
events.jsonl  dc380aed8e73921fa4ac8af9bf124ddc601edca65161a232578ab116d884da08
capture.zip   d7f22a0121e0025f401437432a8640e7224133daf5d277ba5083d663ccdd7f0f
manifest.json 4d35fa2f6648d27eaa7ef09f3912ee44fbcce2fedcace9e54d48ebdfec8eaf5f
```

## 请求级协议证据

- 请求矩阵包含 22 个入口请求：9 个 thinking/tool、7 个普通流、2 个 thinking/text、2 个工具续接、1 个 server tool、1 个非流。11 个 thinking 请求均有真实非空 signature。
- 非流请求 `req_01Vj6DkgCbav3aEup9xjEc8z` 上游只有 `assistantResponseEvent`，下游仍为非流 text 结果，`non_stream=10`；矩阵对非流入口不包含公开 SSE 生命周期计数。
- 公开 WebSearch 路径在原始抓包中有独立 `client_sse` 事件，路由为 `websearch`，并包含 `server_tool_use`、MCP 200、结果回灌、citation、usage 和 `message_stop`；脱敏矩阵按入口请求分类仍将它归在 server tool 之外，不能据此判断 WebSearch 缺失。
- 21 个可归属入口请求的上游流以 EOF 无 `COMPLETED` 结束，代理按本地推断终止；下游 message 生命周期闭合。

## 结论

Round100 仍为 `82/110`，三个长期失败项没有变化：`stream_structure=5`、`websearch=0`、`signature_proto=5`。本轮所有上游请求均 200，未复现账号错误或最终流错误，不能据此修改 retry。继续不伪造 `COMPLETED`、不随机生成 WebSearch opaque 字段、不改写真实 signature。唯一原始抓包、结果文件和请求矩阵全部保留。
