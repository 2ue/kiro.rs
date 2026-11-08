# Round 61：高余额账号 2462 主池复测

## 运行信息

- 时间：2026-11-08（北京时间 00:26–00:29；UTC 16:26–16:29）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 代码：与 Round 60 相同，无源码修改
- 开发调度：59139 只将余额最高的 PRO 账号 2462 设为 Fast=10；其他高余额 PRO 账号暂时关闭 Fast，生产 59138 未改
- 结果：[`20261107T162616-round-61-high-balance-2462.json`](../results/20261107T162616-round-61-high-balance-2462.json)
- 抓包：`logs/llm-capture/capture-20261107T162559Z-09c344cd/`
- 抓包 session：`capture-20261107T162559Z-09c344cd`

## 结果

总分 `82/110`：

- `stream_structure=5`
- `websearch=0`
- `signature_proto=5`
- `output_config=10`
- 其余保护项全部 10

## 抓包统计

- 事件 10,176；JSON 错误 0；丢弃 0
- 客户端请求摘要 23；对外流式请求 22；客户端完整流 22/22
- 上游请求/响应 29；HTTP 200 为 25，HTTP 402 为 4
- `message_start/message_delta/message_stop` 为 22/22/22
- `content_block_start/content_block_stop` 为 49/49
- WebSearch 对外闭环：`server_tool_use(index=0)` → `web_search_tool_result(index=1)` → `text(index=2)` → `message_delta(end_turn)` → `message_stop`

## 账号消耗与 402 证据

主账号 2462 承担 17 个最终请求摘要；同一 conversation 的 sticky 请求集中到该账号。外围请求使用 2489、2491、2493、2494、2496、2507 等账号。

4 次 HTTP 402 都是明确月额度耗尽：

| credential | 状态 | 证据 | 处理 |
|---:|---:|---|---|
| 2490 | 402 | `QuotaExceeded`、`MONTHLY_REQUEST_COUNT` | 59139 自动禁用 |
| 2499 | 402 | `QuotaExceeded`、`MONTHLY_REQUEST_COUNT` | 59139 自动禁用 |
| 2503 | 402 | `QuotaExceeded`、`MONTHLY_REQUEST_COUNT` | 59139 自动禁用 |
| 2504 | 402 | `QuotaExceeded`、`MONTHLY_REQUEST_COUNT` | 59139 自动禁用 |

这些 402 请求均在首输出前切换到后续账号并最终成功；没有客户端 502。它们是开发账号池容量噪声，不能直接作为协议评分失败样本。59138 对应账号未修改。

## 流结构与终止状态证据

23 个请求摘要中，21 个记录 `upstreamEofWithoutCompleted=true`；其中 8 个最终由 `local_inferred_end_turn`，12 个由 `local_inferred_tool_use`，1 个由显式本地 stop reason 终止。公开下游仍全部拥有完整 message 生命周期。

这给出 `stream_structure=5` 的高价值候选：上游 Kiro EventStream 经常只 EOF，不发送显式 `messageStatus=COMPLETED`；本地根据 assistant/tool 内容推断 `end_turn/tool_use`。当前不能直接把 EOF 改成伪造 `COMPLETED`，因为上游实际行为和公开下游事件都没有出现缺帧，仍需要 cctest 失败样本或官方 fixture 证明评分器检查上游终止状态。

## WebSearch 证据

本轮 WebSearch 对外事件顺序、caller、server/result ID、10 个结果、引用、usage、`end_turn` 和 `message_stop` 均完整。当前实现仍使用：

- `encrypted_content =` 搜索 snippet 明文；
- `encrypted_index = Base64(number:url)`。

两者是官方要求原样回传的不透明字段，Round 61 没有真实官方 fixture，不能随机替换或伪造。

## 隔离核对

- 59138 `/healthz=200`；59139 `/healthz=200`
- 59138 镜像仍为 `0.0.239`、revision `30edc00c12431371e22498298715adc7bea539fd`
- 59139 继续由 debug 热更新循环管理
- 没有停止、重启或写入 59138；没有写生产 PostgreSQL/Redis；没有修改生产账号
