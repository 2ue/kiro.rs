# Round 98：恢复配置后的基线复测仍为 82/110

## 时间与边界

- 时间：2026-11-08 12:46–12:49（北京时间；结果和抓包事件为 UTC 04:46–04:49）。
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`，后端 59139；模型 `claude-opus-5-5`；并发 1。
- 运行时已恢复 Round97 前的 tune 配置：`thinkingTrigger=off`、`channelLeakGuard=false`、`modelDrivenWebSearch=true`。
- 代码没有修改，59139 继续由 debug 增量开发循环管理；59138 只读，未重启、未写配置、未写 PostgreSQL/Redis、未改账号状态。
- 结果：[`20261108T044604-round-98-capture.json`](../results/20261108T044604-round-98-capture.json)。
- 抓包：`logs/llm-capture/capture-20261108T044554Z-59d09b8f/`。
- 请求矩阵：[`analysis/round-98-request-matrix.json`](../analysis/round-98-request-matrix.json)。

## 结果

| 检测项 | Round 97 | Round 98 |
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

- `events.jsonl`：9,546 行，8,998,554 字节；JSON 错误 0；`droppedEvents=0`。
- 23 个客户端入口请求、23/23 客户端流结束、27 个上游请求；其中 26 个首尝试成功，1 个请求出现 402 后换号成功。
- MCP 响应 1 次；未出现最终 502、`stream_error` 或客户端断流。
- 哈希：

```text
events.jsonl  f797d40f2c926b836bc31e51331b016e2671a5dceb55f90230bf66ccbc46e96a
capture.zip   cf69d4d16247eb8b6ce3c3aca744dfc412b8ef5d7a835e98fdf034a7d0a41a92
manifest.json fe3973d40da80922f24fb7893d03a407217dc19b25ebe64557f556637cd18ab1
```

## 重试证据

请求 `req_01jAY7E7NnUAviHhvegFKk4q` 首次使用账号 2532，Kiro 返回 HTTP 402，错误为 `MONTHLY_REQUEST_COUNT`；代理执行 `disable_and_retry`，第二次使用账号 2534 返回 HTTP 200。最终 `request_summary` 为 `success/local_success/completed`，客户端只收到一套完整响应，没有 `stream_error` 或重复内容。

## 请求级协议证据

- 含 thinking 的流继续输出 `thinking` block 和真实非空 Kiro signature；未见 synthetic marker、空 signature 或截断。
- 非流请求 `req_01bVqqu3xdZsnGGVu8YJwGWN` 上游只有 1 个 `assistantResponseEvent`，没有 reasoning；`non_stream=10`。
- 多数流仍是上游 EOF 无 `COMPLETED`，代理按 `local_inferred_end_turn` 或 `local_inferred_tool_use` 完成；下游公开 message/block 生命周期闭合。
- WebSearch API/MCP/回灌公开生命周期完整，`websearch=0` 未改变；本轮没有新的 opaque 第二轮回传 fixture。

## 结论

Round98 确认 Round97 的运行时实验已经完全回滚，基线分数仍为 82。账号 2532 的 402→2534 的 200 换号链成功完成，不修改 retry 状态机。继续保持当前配置和抓包保留策略，不伪造 `COMPLETED`、WebSearch opaque 值或 signature。
