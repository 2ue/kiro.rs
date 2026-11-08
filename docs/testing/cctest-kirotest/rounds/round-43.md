# Round 43：identityTurnReminder 回归检查

## 运行信息

- 时间：2026-11-07（北京时间 12:13–12:17）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 模型：`claude-opus-5-5`
- 开发实例：59139；与 Round 42 相同开发二进制
- 59138：只读，未重启、未改配置、未写 PostgreSQL/Redis

## 实验变量

只在 59139 测试路由开启 `identityTurnReminder`，保持 `identityPrompt=false`。preview 证实 reminder 只追加到当前轮消息。完整 CCTest 结束后自动恢复；GET 确认两个 identity 开关均为 false，59139 `/healthz` 为 200。

## 结果

结果文件：`results/20261107T041355-round-43-identity-turn-reminder.json`

| 项目 | Round 42 | Round 43 |
|---|---:|---:|
| tag_check | 10 | 10 |
| stream_structure | 5 | 5 |
| non_stream | 10 | **0** |
| websearch | 0 | 0 |
| signature_proto | 5 | 5 |
| output_config | 0 | 0 |
| server_tool | 10 | 10 |
| knowledge | 10 | 10 |
| model_attribution | 10 | 10 |
| doc_recognition | 10 | 10 |
| image_recognition | 10 | 10 |
| 总分 | 73/110 | 64/110 |

完整抓包：`logs/llm-capture/capture-20261107T041355Z-155375eb/events.jsonl`，另保留 manifest 与 ZIP。共有 9,743 个事件、22 个客户端请求、25 个上游请求；上游全部 HTTP 200，JSON 错误和丢弃事件均为 0。22 个流均有完整 message start/delta/stop。

## non_stream 请求级复现

Round 43 的非流请求 `req_016bx122B9G6RrPomwDyK5V7` 内容为 `1+1=?`。从该请求原样提取 body，在 59139 用同一模型对比 identityTurnReminder 关闭和开启：

- 关闭：HTTP 200，`stop_reason=end_turn`，响应 block 只有 `text`，文本 `1+1=2`。
- 开启：HTTP 200，`stop_reason=end_turn`，响应 block 变成 `thinking + text`，可见文本为 `2`。

脱敏 A/B 摘要：`results/20261107T042024-round-43-nonstream-replay.json`。这为 Round 43 `non_stream` 从 10 降到 0 提供了直接候选解释：提醒改变了非流响应 block 类型。私有评分条件仍未知，因此记录为高相关证据，不声称完全等同评分器判据。

资源监控 17 次：最低可用内存约 9.01 GiB、最低 swap 约 10.99 GiB、最高 1 分钟 load 0.60；59138 健康探针全部 HTTP 200，未触发暂停阈值。

## 结论

不保留 identityTurnReminder 配置；它没有改善目标项，并伴随 `non_stream` 退化。配置已恢复。下一步在原始路由配置下重跑一次完整基线，确认 `non_stream` 恢复，再继续其他协议差异分析。
