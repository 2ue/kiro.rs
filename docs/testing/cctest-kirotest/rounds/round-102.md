# Round 102：默认 tune 配置复测仍为 82/110

## 时间与边界

- 时间：2026-11-08 13:28–13:32（北京时间；抓包事件 UTC 05:28–05:31）。
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`，后端 59139；模型 `claude-opus-5-5`；并发 1。
- 59139 使用开发热更新实例；本轮没有源码或运行时配置改动。
- 59138 只做健康检查，未重启、未写配置、未写数据库/Redis、未修改账号状态。
- 路由实际配置：`thinkingTrigger=off`、`channelLeakGuard=false`、`modelDrivenWebSearch=true`；路由注入项保持关闭。
- 结果：[`20261108T052839-round-102-capture.json`](../results/20261108T052839-round-102-capture.json)。
- 原始抓包：`logs/llm-capture/capture-20261108T052832Z-9611b44d/`。
- 请求矩阵：[`analysis/round-102-request-matrix.json`](../analysis/round-102-request-matrix.json)。

## 结果

| 检测项 | Round 101 | Round 102 |
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

- `events.jsonl`：10,597 行，9,233,887 字节；JSON 错误 0；`droppedEvents=0`。
- 22 个客户端入口、22/22 客户端流结束、23 个 request summary。
- 上游共 27 次尝试：25 个 HTTP 200、2 个 HTTP 402；两次 402 均发生在首输出前并完成换号恢复。
- MCP response 1 次；公开 SSE 包含 22 个 `message_start`、22 个 `message_delta`、22 个 `message_stop`。
- client SSE 路由：local 4,969 个事件，websearch 53 个事件；WebSearch 独立链路仍被捕获。
- 哈希：

```text
events.jsonl  32ab850237d245ba30f2b939e33bca03d5a4935cbb97e76455df3141e3fca005
capture.zip   538044e311756d96f253f0173fb1819acd438f621335ddd764a8a22b5c680864
manifest.json 185349edd9973c450418281236969e8e53e6dc089e94713967246c5d54fa5eb4
```

## 请求级证据

- 22 个请求中 8 个声明 thinking；这些请求均有真实非空 Kiro signature，签名没有被代理改写、截断或跨请求复制。
- 1 个非流请求只收到 `assistantResponseEvent`，代理返回纯文本路径，`non_stream=10`；本轮没有新增“上游 reasoning + 非流空 thinking block”失败样本。
- 4 个未声明 thinking 的普通流出现上游 `reasoningContentEvent`，下游按请求显示策略只下发 text；这与 Round 96/99 的观察一致，尚无分数共变证据支持改变过滤策略。
- 21 个流式上游请求没有显式 `COMPLETED`，本地按 `local_inferred_end_turn` 或 `local_inferred_tool_use` 收束；下游公开生命周期均闭合。没有失败请求 ID或官方 verifier fixture 支持伪造 `COMPLETED`。
- WebSearch 的 `server_tool_use → MCP → web_search_tool_result → citation/text → message_delta/end_turn → message_stop` 公开链路完整；`websearch=0` 未变化。`encrypted_content`/`encrypted_index` 仍没有 provider-issued opaque fixture，不能继续随机改写。

## 402 恢复

本轮 2 次 HTTP 402 都出现在首输出前，随后同一 request 由其它账号取得 200 并完成。22/22 客户端流闭合，没有 `stream_error`、502、重复 block 或错误终态。该轮继续增加换号恢复证据，不能把账号配额或 retry 归因到三个长期失分项。

## 结论

Round 102 仍为 `82/110`，长期失败项保持 `stream_structure=5`、`websearch=0`、`signature_proto=5`。本轮没有支持修改 EOF、retry、WebSearch opaque 字段、真实 signature 或未声明 thinking 隐藏策略的新因果证据；唯一原始抓包、矩阵、结果和哈希全部保留。
