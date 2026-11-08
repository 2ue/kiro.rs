# Round 91：402 换号成功与 WebSearch opaque 字段复核

## 时间与边界

- 时间：2026-11-08 11:47–11:50（北京时间；结果和抓包事件为 UTC 03:47–03:50）。
- 59139 使用 debug 增量开发循环和正余额开发账号；59138 只读，未重启、未改配置、未写 PostgreSQL/Redis、未改账号状态。
- 模型：`claude-opus-5-5`；路由：`/dfcache/tune-ss6-99`；并发：1。
- 结果：[`20261108T034710-round-91-capture.json`](../results/20261108T034710-round-91-capture.json)。
- 原始抓包唯一目录：`logs/llm-capture/capture-20261108T034657Z-78bc03f4/`。
- 请求矩阵：[`analysis/round-91-request-matrix.json`](../analysis/round-91-request-matrix.json)。

## 结果

| 检测项 | Round 90 | Round 91 |
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

本轮与 Round 89、90 同分；当前正确模型最高分仍为 `82/110`，稳定区间仍为 `73–82/110`。

## 抓包统计与哈希

- `events.jsonl`：10,530 行，9,181,626 字节；JSON 错误 0，`dropped_events=0`。
- 23 个客户端入口请求、22 个客户端流、28 个上游请求、25 个 HTTP 200、3 个 HTTP 402。
- 22/22 客户端流完整；`message_start/message_delta/message_stop` 均为 22；`content_block_start/stop` 为 60/60。
- MCP 响应 1 次；上游流结束 21 次；非流请求 1 次。

```text
events.jsonl  59386828372e1f48d99a55de7a1c1c9711990e9090618e5fd8d3899d6659c493
capture.zip   29c19511203da41765c604b6d917724ed8a8b7fd227735ac39d7bae434537422
manifest.json 3415a7b32e814aaa90f628599610518e42b6039f483336be97794617a18f9ebe
```

## 402 换号链

本轮 3 次 HTTP 402 都是上游 `MONTHLY_REQUEST_COUNT` 额度耗尽，均发生在首输出前；每条链都随后切换到可用账号并取得 HTTP 200。下游没有 `stream_error`、502 或重复内容块，所有客户端流正常闭合。

因此本轮继续证明当前 quota exhausted 换号链能够恢复；不能仅因出现 402 就修改 retry、账号禁用或 sticky 逻辑。后续只有在同一个 request ID 出现“换号后最终错误”时，才具备状态机修复依据。

## 非流请求

非流请求为 `req_01ofPLPWmGcyAQKYGYvS5eLs`：上游 HTTP 200，仅有 `assistantResponseEvent` 文本，代理返回单一 `text` 内容 `2`，`stop_reason=end_turn`，`non_stream=10`。本轮没有复现 Round 88 的“上游 reasoning → 空 thinking block → non_stream=0”。

Round 88、89、90、91 的反向样本继续表明该项与上游是否采样 reasoning 共变；当前实现保留官方 `thinking.display=omitted` 的空 thinking block 语义，不做删除 block 的全局修改。

## Thinking 与签名

本轮 8 个含 thinking 的流请求均满足：

- `thinking_delta.thinking` 文本字符数为 0；
- 每个 thinking block 只有一个非空 `signature_delta`；
- 签名 Base64 解码后均为 Kiro opaque protobuf 形状，前缀为 `08 04 12 ... 0a 12 08`；
- 解码长度约 633–1,922 字节，签名未被代理改写。

`signature_proto` 仍为 `5/10`，`stream_structure` 仍为 `5/10`。这再次排除“签名为空、签名被截断、签名被 synthetic marker 替换”作为本轮可修复根因；没有 CCTest 失败请求 ID 或官方 verifier fixture 前，不改真实签名字节和终止状态机。

## WebSearch

WebSearch 内部请求为 `req_019obHpWeQUMNcN5TQzcbWYp`，API、MCP、结果回灌均为 HTTP 200。对外生命周期完整：

```text
message_start
→ server_tool_use(index=0)
→ input_json_delta
→ content_block_stop(index=0)
→ web_search_tool_result(index=1)
→ content_block_stop(index=1)
→ citations_delta/text_delta
→ message_delta(stop_reason=end_turn)
→ message_stop
```

- `server_tool_use.id` 与 result 的 `tool_use_id` 配对；两个 block 都有 `caller={"type":"direct"}`。
- MCP 返回 10 个结果；最终 usage 为 `server_tool_use.web_search_requests=1`。
- `web_search_result.encrypted_content` 仍是 MCP snippet 明文。
- citation 的 `encrypted_index` 仍可逆为 Base64(`序号:URL`)。
- 没有第二轮请求把 provider-issued opaque 字段原样回传的证据。

完整脱敏字段矩阵见 [`analysis/round-91-websearch-fields.json`](../analysis/round-91-websearch-fields.json)。公开生命周期连续多轮完整，但 `websearch=0` 未变化；没有官方 fixture 或生成算法前，不随机伪造 opaque 字段。

## 代码与资源

- 本轮未修改源码；59139 继续由开发循环增量编译，没有 release 冻结编译。
- 59139、59138 `/healthz` 均为 200；资源门禁未触发暂停。
- 59138 仍为 `0.0.239` / revision `30edc00c12431371e22498298715adc7bea539fd`，只读参照。
- 原始 `events.jsonl`、`capture.zip`、`manifest.json` 和结果文件继续保留，不清理唯一证据源。

## 结论

Round 91 没有产生可直接改变评分的公开协议差异。三个长期失败项仍为 `stream_structure=5`、`websearch=0`、`signature_proto=5`；当前最高分仍为 `82/110`。下一轮继续在 59139 做完整检测和抓包，只在出现可复现的失败请求级证据后修改代码。
