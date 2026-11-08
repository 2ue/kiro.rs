# Round 86：WebSearch 字段逐项抓包复核

## 时间与边界

- 时间：2026-11-08 10:29–10:31（北京时间；结果文件时间字段为 UTC 02:29–02:31）。
- 59139 使用 debug 增量开发循环和高余额开发账号池；59138 只读，未重启、未改配置、未写 PostgreSQL/Redis、未改账号状态。
- 模型：`claude-opus-5-5`；路由：`/dfcache/tune-ss6-99`。
- 原始抓包唯一目录：`logs/llm-capture/capture-20261108T022901Z-a17b60b7/`。

## 结果

结果文件：[`20261108T022918-round-86-websearch-capture.json`](../results/20261108T022918-round-86-websearch-capture.json)

| 检测项 | 分数 |
|---|---:|
| stream_structure | 5 |
| non_stream | 10 |
| websearch | 0 |
| signature_proto | 5 |
| output_config | 0 |
| server_tool | 10 |
| 其它保护项 | 40 |
| **总分** | **73/110** |

`output_config` 从 Round 85 的 10 变为 0；三个长期失败项没有变化。因此总分下降不能归因于本次 WebSearch block 修改。

## 抓包统计

- `events.jsonl`：6,896 行，7,559,976 字节，JSON 错误 0；
- 23 个客户端请求，22 个客户端流，30 个上游请求，25 个上游 HTTP 200、5 个上游 HTTP 402；
- 22/22 客户端流完整；`message_start`、`message_delta`、`message_stop` 均为 22；
- `content_block_start/stop` 为 54/54；
- MCP 响应 1 次；丢弃事件 0；
- WebSearch 请求：`req_01o7XRtJopuMfhZhjwf9CeT9`。

字段矩阵：[`analysis/round-86-websearch-fields.json`](../analysis/round-86-websearch-fields.json)

## WebSearch 公开生命周期

该请求的下游顺序为：

```text
message_start
→ server_tool_use(index=0)
→ input_json_delta
→ content_block_stop(index=0)
→ web_search_tool_result(index=1)
→ ping
→ text block(index=2..11)
→ citations_delta/text_delta
→ message_delta(stop_reason=end_turn)
→ message_stop
```

已确认：

- `server_tool_use.id` 与 `web_search_tool_result.tool_use_id` 完全相等；
- 两个 server block 都有 `caller={"type":"direct"}`；
- 结果数组有 10 项；
- 有 10 个带引用的 text block，最后还有 1 个无引用文本 block；
- 每个引用 block 都是独立的 `content_block_start → citations_delta → text_delta → content_block_stop`；
- 最终 usage 的 `server_tool_use.web_search_requests=1`；
- 没有重复 block index、缺失 stop 或错误事件后继续正常输出。

## opaque 字段对照

抓包仍显示两个直接的官方形状差异：

1. `web_search_result.encrypted_content`
   - 当前：MCP 返回的 snippet 明文，共 10 项；
   - 官方文档：不透明加密内容，后续多轮必须逐字节回传；
   - 当前 Kiro MCP 没有提供官方 opaque 值。

2. `web_search_result_location.encrypted_index`
   - 当前：普通 Base64(`序号:URL`)，可直接解码；
   - 官方文档：多轮回传用的不透明引用值；
   - 当前没有完整官方 fixture、生成算法或密钥。

Round 77/78 已用本地伪 protobuf 外形替换 `encrypted_content`，两轮 `websearch=0`，因此不能再通过随机伪造外层字节推进。当前结论是：WebSearch 0 分仍可能来自 opaque 字段语义、跨轮原样回传检查，或 CCTest 私有结果内容判定；公开生命周期已经不是充分解释。

## 代码与验证

- 保留 Round 85 的引用 block 边界修复；
- 不修改 `encrypted_content`、`encrypted_index`、retry、usage 或真实 Kiro signature；
- 定向 `model_driven_websearch` 测试：13/13 通过；
- 59139 与 59138 `/healthz` 均为 200；
- 未执行 release 冻结编译；
- 原始 `events.jsonl`、`capture.zip`、`manifest.json` 全部保留。

## 结论

Round 86 把 WebSearch 失败进一步收敛到字段语义或私有评分条件：公开 block 生命周期、caller、ID、引用、usage 和终止事件均完整，但 `websearch` 仍为 0。`output_config` 的 0/10 波动独立存在。下一轮继续使用高余额账号和完整 capture；只有拿到真实官方 opaque fixture 或新的可共变失败证据，才修改这两个字段。
