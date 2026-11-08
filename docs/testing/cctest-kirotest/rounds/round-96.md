# Round 96：未声明 thinking 的上游 reasoning 对照仍未改变评分

## 时间与边界

- 时间：2026-11-08 12:30–12:33（北京时间；结果和抓包事件为 UTC 04:30–04:33）。
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`，后端 59139；模型 `claude-opus-5-5`；并发 1。
- 代码：`HEAD=2e625e52`、`v0.0.240`；本轮没有源码修改，59139 继续使用 debug 增量开发循环。
- 59138 仅做只读健康检查，未重启、未写配置、未写 PostgreSQL/Redis、未改账号状态。
- 结果：[`20261108T043024-round-96-capture.json`](../results/20261108T043024-round-96-capture.json)。
- 原始抓包唯一目录：`logs/llm-capture/capture-20261108T043015Z-b377f71d/`。
- 请求矩阵：[`analysis/round-96-request-matrix.json`](../analysis/round-96-request-matrix.json)。

## 结果

| 检测项 | Round 95 | Round 96 |
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

## 抓包与资源

- `events.jsonl`：7,690 行，7,795,858 字节；JSON 错误 0；`droppedEvents=0`。
- 22 个客户端入口请求、22/22 客户端流结束、25 个上游请求和 25 个 HTTP 200 响应。
- MCP 响应 1 次；没有最终 502、`stream_error` 或客户端断流。
- 文件哈希：

```text
events.jsonl  f0f1434aa61a0270aac53c60389793e8e4b25f7a952bbad8562af86871aaf585
capture.zip   6f94ee9d3ec7d8b0e33c8dac11ef034b8f3f2b3aee39c58a91d5226aec0f35e3
manifest.json 5326511f765f72d0498ffec77780a2e47b87a970d0b1e40654810dc076e81a0e
```

开始抓包前资源门禁正常，开发循环没有暂停；59139 与 59138 `/healthz` 均为 200。

## 请求级对照

Round 96 的请求矩阵显示：

- `thinking=true` 的 thinking/tool 请求都带真实非空 `signature_delta`，下游 `thinking` 与 `tool_use` block 成对闭合。
- 有一类 `thinking=false` 的普通流请求，上游同时出现 `reasoningContentEvent` 与 `toolUseEvent`，但下游只输出单一 `text` block，没有 `signature_delta`。这是请求没有声明 thinking 时的当前过滤策略；本轮未看到它单独改变 `stream_structure` 或 `signature_proto`。
- 其它普通流只含 `assistantResponseEvent`，下游 text 生命周期完整。
- 多数上游流仍以 EOF 结束、没有显式 `COMPLETED`，代理使用 `local_inferred_end_turn` 或 `local_inferred_tool_use`；公开下游 `message_start → block → message_delta → message_stop` 均完整。
- 非流请求没有上游 reasoning，`non_stream=10`；因此没有新的空 thinking block 失败样本。

WebSearch 的 API/MCP/回灌公开生命周期继续完整，评分仍为 0。真实 Kiro signature 继续原样透传，评分仍为 `signature_proto=5`。

## 结论

Round 96 是恢复默认配置后的第二轮完整复测，得分和 Round95 完全一致。上游在未声明 thinking 的请求中偶尔产生 reasoning，而下游不发送 thinking/signature，这条证据值得与官方 `thinking.display=omitted` 语义继续对照，但尚不足以直接修改输出过滤器；强行暴露 reasoning 会同时改变签名绑定和 Claude Code 行为。

当前仍不改 EOF 状态机、WebSearch opaque 字段或真实签名；继续保留所有唯一抓包源文件，等待评分器失败请求 ID或官方 fixture。
