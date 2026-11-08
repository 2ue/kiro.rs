# Round 94：59139 thinkingTrigger=always 单变量实验无评分收益

## 时间与边界

- 时间：2026-11-08 12:14–12:17（北京时间；结果和抓包事件为 UTC 04:14–04:17）。
- 只修改 59139 开发实例的运行时路由配置；59138 全程只读，未重启、未改配置、未写 PostgreSQL/Redis、未修改账号状态。
- 模型：`claude-opus-5-5`；路由：`/dfcache/tune-ss6-99`；并发：1。
- 代码未修改，59139 继续由 debug 增量开发循环管理；没有 release 冻结编译。
- 结果：[`20261108T041402-round-94-thinking-always.json`](../results/20261108T041402-round-94-thinking-always.json)。
- 原始抓包唯一目录：`logs/llm-capture/capture-20261108T041355Z-653a74d7/`。
- 请求矩阵：[`analysis/round-94-request-matrix.json`](../analysis/round-94-request-matrix.json)。

## 实验变量

实验前保存了 59139 路由快照 `/tmp/kirotest-dev/runtime-round93-thinking-always-before.json`，仅把：

```text
/dfcache/tune-ss6-99.routeFeatures.thinkingTrigger
off → always
```

实验结束后通过 `PUT /api/admin/config/runtime` 恢复，并再次 GET 确认：

```json
{
  "thinkingTrigger": "off",
  "channelLeakGuard": false,
  "modelDrivenWebSearch": true
}
```

没有把 `always` 留在开发实例，也没有触碰 59138。

## 结果

| 检测项 | Round 93 | Round 94 |
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

完整检测没有任何分项变化。`thinkingTrigger=always` 不能作为全局默认修复，也没有理由保留在运行时配置中。

## 抓包完整性

- `events.jsonl`：10,767 行，9,710,315 字节；JSON 错误 0；`droppedEvents=0`。
- 24 个客户端入口请求、24 个客户端流、27 个上游请求；27 个上游响应均为 HTTP 200。
- 24/24 客户端流完整；`message_start/message_delta/message_stop` 为 24/24/24。
- `content_block_start/content_block_stop` 为 74/74；MCP 响应 1 次。
- `capture.zip`、`manifest.json` 和唯一 `events.jsonl` 全部保留。

文件哈希：

```text
events.jsonl  8eae64e8d27ebef070a1953099a1c861ad9743507936f24720b25e206379895a
capture.zip   5300e71899054ae53b7d4306540df67c75dab3f4be9f5ba1e85f8af1cd9f8698
manifest.json 88d5bf1824c017be76060f115d3df986554a75316cbb4ac8b199a42286fae51f
```

## 非流与 direct 对照

完整 CCTest 的非流样本仍带：

```json
{
  "thinking": {"type": "adaptive"},
  "stream": false
}
```

本轮上游没有 `reasoningContentEvent`，下游返回单一 `text` block，`non_stream=10`。因此本轮没有产生新的“上游 reasoning + 空 thinking block + non_stream=0”样本，不能启动删除 thinking block 的请求级 A/B。

在 `thinkingTrigger=always` 生效期间，另做了一个独立的本机 direct localhost 最小请求：`claude-opus-5-5`、`stream=false`、`thinking.type=adaptive`、消息 `1+1=?`。该请求确实返回了 `thinking` block 加 `text` block，证明 `always` 在 direct 最小路径能触发非流 thinking；但同一配置下的完整 CCTest 仍返回纯文本非流样本，且总分保持 82。

这说明 CCTest 的完整 Claude Code system/history、请求路径或上游采样条件会影响是否出现 reasoning；不能把 direct 最小请求的行为外推为评分修复。

## 结论与后续

1. `thinkingTrigger=always` 在 59139 direct 最小请求中有效，但完整 CCTest 无评分收益。
2. `stream_structure=5`、`websearch=0`、`signature_proto=5` 均未变化。
3. 保持 `thinkingTrigger=off`；不改源码，不删除非流 thinking block，不伪造 signature 或 WebSearch opaque 字段。
4. 下一轮继续使用 59139 开发模式完整抓包；若再次捕获“非流上游 reasoning + `non_stream=0`”，再做保留空 thinking block与删除整个 block 的请求级 A/B。
5. 当前最高分仍为 `82/110`，稳定区间为 `73–82/110`；尚未满足连续三轮 `110/110` 的完成门禁。
