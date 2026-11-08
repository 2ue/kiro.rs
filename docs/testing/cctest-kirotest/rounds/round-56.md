# Round 56：剔除空响应 social 账号后的完整复测

## 运行信息

- 时间：2026-11-07（北京时间 21:28–21:32；UTC 13:28–13:32）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 代码：与 Round 51–55 相同；没有源码修改，59139 由 debug 热更新循环管理
- 结果：[`20261107T132854-round-56-disable-social-empty.json`](../results/20261107T132854-round-56-disable-social-empty.json)
- 抓包：`logs/llm-capture/capture-20261107T132854Z-61898d0f/`

## 结果

总分 `82/110`：

| 检测项 | 分数 |
|---|---:|
| tag_check | 10 |
| stream_structure | 5 |
| non_stream | 10 |
| websearch | 0 |
| signature_proto | 5 |
| output_config | 10 |
| server_tool | 10 |
| knowledge | 10 |
| model_attribution | 10 |
| doc_recognition | 10 |
| image_recognition | 10 |

## 抓包统计

- 客户端请求 22；上游请求 25；上游 HTTP 200 为 25/25
- 客户端完整流 22/22
- `message_start/message_delta/message_stop` 为 22/22/22
- `content_block_start/content_block_stop` 为 55/55
- 抓包 JSON 错误 0，丢弃事件 0
- WebSearch 502 已消失；`websearch` 仍为 0，属于协议/私有评分差异，不再是账号 2530 空响应

## 结论

禁用 2530 解决了可复现的空响应故障，但没有改变三个协议评分项：`stream_structure=5`、`websearch=0`、`signature_proto=5`。59138 全程只读。
