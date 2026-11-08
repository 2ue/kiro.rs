# Round 103：默认 tune 配置复测仍为 82/110

## 时间与边界

- 时间：2026-11-08 13:49–13:54（北京时间；抓包事件 UTC 05:49–05:54）。
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`，后端 59139；模型 `claude-opus-5-5`；并发 1。
- 59139 使用开发热更新实例；本轮没有源码或运行时配置改动。
- 59138 只做健康检查，未重启、未写配置、未写数据库/Redis、未修改账号状态。
- 路由实际配置：`thinkingTrigger=off`、`channelLeakGuard=false`、`modelDrivenWebSearch=true`；路由注入项保持关闭。
- 结果：[`20261108T054954-round-103-capture.json`](../results/20261108T054954-round-103-capture.json)。
- 原始抓包：`logs/llm-capture/capture-20261108T054953Z-fd2ddefb/`。
- 请求矩阵：[`analysis/round-103-request-matrix.json`](../analysis/round-103-request-matrix.json)。

## 结果

| 检测项 | Round 102 | Round 103 |
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

`streamChannel=aws-bedrock`，结果判定为 `unofficial`。

## 抓包完整性

- `events.jsonl`：7,390 行、7,669,511 字节；JSON 错误 0，`droppedEvents=0`。
- 22 个客户端入口、22 个客户端结束、23 个 request summary。
- 上游共 26 次尝试：25 个 HTTP 200、1 个 HTTP 402。402 发生在首输出前，随后同一请求换号取得 200 并完成。
- MCP response 1 次、HTTP 200；客户端公开 SSE 包含 22 个 `message_start`、22 个 `message_delta`、22 个 `message_stop`，56 个 content block 成对闭合。
- `client_sse` 路由统计为 local 3,395、websearch 57；WebSearch 独立链路仍被捕获。
- 资源门禁抽样：可用内存约 5.6 GiB、1 分钟负载约 1.07；59138 与 59139 健康检查均 HTTP 200，未触发暂停。

哈希：

```text
events.jsonl  72e35a04aae83ac4e6c96fc0005e1c9dadd120242f3258aa4dac0250d0fb856
capture.zip   f6ed91c6c175dd1eabe3bd8584c3264d05b82bd5535c04b5ad5b6eae35c1f925
manifest.json b72b3464779d55b7c912b8afea02f3fe57f47a49f538f1a7061d8dd7a1950980
request-matrix 051f4adc9d7ced09bf6c37c7ec7a1839bd8a5e54f120bec44d207f35167fedab
```

## 请求级证据

- 22 个请求中 11 个公开 thinking；这 11 个均有真实非空 Kiro signature，下游没有截断、复制或重编码。
- 唯一非流请求上游只有 `assistantResponseEvent`、`contextUsageEvent` 和 `meteringEvent`，代理返回纯文本路径，`non_stream=10`。
- 21 个流式请求的上游 EOF 没有显式 `COMPLETED`，本地按 `local_inferred_end_turn`、`local_inferred_tool_use` 或一次 `local_explicit_stop_reason` 收束；下游公开生命周期均闭合。
- WebSearch 请求独立出现 `message_start`、11 个 block start/stop、31 个 delta、`message_delta`、`message_stop`，MCP 返回 HTTP 200；评分仍为 0。没有新的 provider-issued `encrypted_content` / `encrypted_index` fixture。
- 402 请求在首输出前恢复为 200，客户端只收到一套完整响应，没有 502、`stream_error`、重复 block 或错误终态。

## 结论

Round 103 与 Round 99–102 的分数和请求级形状一致，当前 Kiro/Bedrock 链路稳定复现 `82/110`。本轮没有支持修改 EOF、retry、WebSearch opaque 字段、真实 signature 或未声明 thinking 隐藏策略的新因果证据；唯一原始抓包、矩阵、结果和哈希全部保留。
