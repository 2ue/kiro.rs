# Round 97：channelLeakGuard 单变量实验无评分收益

## 时间与边界

- 时间：2026-11-08 12:39–12:43（北京时间；结果和抓包事件为 UTC 04:39–04:43）。
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`，后端 59139；模型 `claude-opus-5-5`；并发 1。
- 59139 唯一变量：`routeFeatures.pathOverrides["/dfcache/tune-ss6-99"].channelLeakGuard` 从 `false` 改为 `true`；`thinkingTrigger=off`、`modelDrivenWebSearch=true` 及其它配置保持不变。
- 实验后已恢复 `channelLeakGuard=false`；59138 只读，未重启、未写配置、未写 PostgreSQL/Redis、未改账号状态。
- 代码没有修改，仍由 debug 增量开发循环管理 59139；未执行 release 冻结编译。
- 结果：[`20261108T043953-round-97-channel-guard.json`](../results/20261108T043953-round-97-channel-guard.json)。
- 抓包：`logs/llm-capture/capture-20261108T043944Z-cc11fd9f/`。
- 请求矩阵：[`analysis/round-97-request-matrix.json`](../analysis/round-97-request-matrix.json)。

## 结果

| 检测项 | Round 96 | Round 97 |
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

开启渠道泄漏过滤没有改变任何分项。该变量不是 `stream_structure`、`websearch` 或 `signature_proto` 的充分修复。

## 抓包完整性

- `events.jsonl`：8,678 行，8,269,759 字节；JSON 错误 0；`droppedEvents=0`。
- 22 个客户端入口请求、22/22 客户端流结束、25 个上游请求和 25 个 HTTP 200 响应。
- MCP 响应 1 次；未出现最终 502、`stream_error` 或客户端断流。
- 哈希：

```text
events.jsonl  da3a8343f099165c6dc6272b40cb886c47677057d021cc9001cf1e4a27ebc02d
capture.zip   393845d29d89d7a60199bc6b4289c4284148ae428e1bb2bb651b636c8551a500
manifest.json 2b593f7041f5d88647723a207b28a40cb358b3de287c0bfa5d2be3ab5438b416
```

唯一原始 JSONL、ZIP、manifest 全部保留。

## 分析

`channelLeakGuard` 只过滤回复文本中的渠道身份泄漏句子，并不改变上游 EventStream、Kiro opaque signature、WebSearch tool/result 或 EOF 状态机。本轮公开生命周期仍完整，三个长期失败项未共变，因此没有理由把该过滤器扩大到 thinking、signature 或工具事件。

实验结束后的 59139 runtime GET 已确认：

```json
{
  "thinkingTrigger": "off",
  "channelLeakGuard": false,
  "channelLeakGuardStream": null,
  "modelDrivenWebSearch": true
}
```

## 结论

继续保持 tune 路由的 `channelLeakGuard=false` 原配置；不把它当作评分修复。下一步仍聚焦失败请求级证据：上游 EOF 无 `COMPLETED`、WebSearch opaque 字段的官方 fixture/跨轮回传、以及 CCTest signature verifier 的真实样本。
