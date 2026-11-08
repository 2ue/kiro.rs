# Round 87：完整抓包复核 WebSearch 与评分波动

## 时间与边界

- 时间：2026-11-08 10:56–10:59（北京时间；结果文件和事件时间为 UTC 02:56–02:59）。
- 59139 使用 debug 增量开发循环和高余额开发账号；59138 只读，未重启、未改配置、未写 PostgreSQL/Redis、未改账号状态。
- 模型：`claude-opus-5-5`；路由：`/dfcache/tune-ss6-99`。
- 结果：[`20261108T025604-round-87-capture.json`](../results/20261108T025604-round-87-capture.json)。
- 原始抓包唯一目录：`logs/llm-capture/capture-20261108T025405Z-29d0590d/`。

## 结果

| 检测项 | Round 86 | Round 87 |
|---|---:|---:|
| tag_check | 10 | 10 |
| stream_structure | 5 | 5 |
| non_stream | 10 | 10 |
| websearch | 0 | 0 |
| signature_proto | 5 | 5 |
| output_config | 0 | 10 |
| server_tool | 10 | 10 |
| 其它保护项 | 40 | 40 |
| **总分** | **73/110** | **82/110** |

Round 87 与 Round 86 的三个长期失败项完全相同；唯一变化是 `output_config` 从 0 恢复为 10。因此 73/82 波动仍来自结构化输出采样或上游语义，不是本轮 WebSearch 代码变化。

## 抓包统计

- `events.jsonl`：10,547 行，9,190,770 字节，JSON 错误 0；`dropped_events=0`。
- 23 个客户端入口请求、22 个客户端流、27 个上游请求、25 个 HTTP 200、2 个 HTTP 402。
- 22/22 客户端流完整；`message_start/message_delta/message_stop` 均为 22。
- `content_block_start/content_block_stop` 为 61/61；MCP 响应 1 次。
- 上游流结束记录 21 次；抓包停止原因 `admin_stop`，停止后没有继续写入。
- 账号主请求集中在高余额开发账号 `2462`；59139 之外没有修改账号状态。

文件哈希（用于证据链校验）：

```text
events.jsonl  4a588bcacdd83d2c5c1275a384da1e61c6b31660daf67a7f63556791ed8ead9b
capture.zip   90f7155e3e75ddfd33b57fc270c9a53d0e0ae05161fc5520e46a03ff9ef8da8e
manifest.json 69140c99d312bca6f6d188a411d2b725ac6686ee7caac1ceca03193f6d2bdbc0
```

## WebSearch 请求级证据

请求 ID：`req_01EDHQKQFKnuEkcSmkZ61UNQ`。

公开生命周期为：

```text
message_start
→ server_tool_use(index=0)
→ input_json_delta
→ content_block_stop(index=0)
→ web_search_tool_result(index=1)
→ content_block_stop(index=1)
→ ping
→ text block(index=2..10)
→ citations_delta/text_delta
→ message_delta(stop_reason=end_turn)
→ message_stop
```

已确认：

- `server_tool_use.id` 与 `web_search_tool_result.tool_use_id` 配对；两个 block 都有 `caller={"type":"direct"}`。
- MCP `tools/call` 和后续 Kiro `generateAssistantResponse` 均返回 HTTP 200；MCP JSON-RPC envelope 完整。
- MCP 返回 10 个结果，客户端 result block 也有 10 项；最终 usage 为 `server_tool_use.web_search_requests=1`。
- 产生 9 个带 `web_search_result_location` 的引用 delta，`cited_text` 均不超过 150 字符；末尾 block、`message_delta` 和 `message_stop` 均存在。
- 完整字段矩阵见 [`analysis/round-87-websearch-fields.json`](../analysis/round-87-websearch-fields.json)。

## 与 Round 86 对比

Round 86 和 Round 87 的公开 WebSearch 形状一致：两轮都是连续 block index、显式 caller、ID 配对、10 个搜索结果、引用 delta、最终 usage 和完整终止事件。Round 87 的引用数量由模型实际回答决定为 9，不是代理丢失结果；result 数量仍为 10。

两轮都没有发现：

- 第二轮携带 `encrypted_content` / `encrypted_index` 的客户端原样回传；
- 402/重试影响 WebSearch 子请求；
- WebSearch 之后重复 `content_block_stop`、缺失 `message_stop` 或错误事件后继续输出；
- 上游 MCP 失败、空体或解析错误。

仍可直接观察到的字段差异：

1. `web_search_result.encrypted_content` 是 MCP snippet 明文，长度为 112–302 字符，不是可验证的 opaque 值。
2. citation 的 `encrypted_index` 是普通 Base64(`number:url`)，可直接解码；MCP result 本身没有 provider-issued `encrypted_index`。

Anthropic 官方资料只规定这些字段在多轮中作为 opaque 字符串原样回传，没有公开生成算法或密钥。Round 77/78 的伪 protobuf 外形 A/B 仍为 `websearch=0`，所以 Round 87 不继续猜测或改写字段。

## 代码、提示词与版本边界

- 本轮未修改源码；59139 由开发循环自动使用当前工作树增量构建。
- 59138 镜像仍为 `0.0.239` / revision `30edc00c12431371e22498298715adc7bea539fd`；仓库当前 HEAD 为 `2e625e52` / tag `v0.0.240`，生产不是最新工作树。
- 请求正文中继续看到 Claude Code 自带 `<system-reminder>` / `<thinking_mode>`；本轮没有新增代理提示词注入 marker。该提示词层可能改变模型文本、工具选择和引用数量，但不能解释已固定的字段差异，必须与协议字段单独归因。

## 资源与生产边界

- 59139 PID `379231`；记录时 RSS 约 156 MiB，系统可用内存约 7.6 GiB，swap 可用约 10 GiB，load average 约 `0.71/1.08/1.32`。
- 资源门禁未触发暂停；59139、59138 `/healthz` 均为 200。
- 未执行 release 冻结编译；未提交、未推送；未清理任何原始抓包、结果或唯一 manifest。

## 结论与下一步

Round 87 把 `output_config` 的 0/10 波动与 WebSearch 公开结构再次分离。当前最高分仍为 `82/110`，稳定区间为 `73–82/110`。没有新的可证实差异支持修改 WebSearch、retry、EOF、签名或提示词注入代码；继续保持当前实现，启动下一轮完整检测并保留抓包。
