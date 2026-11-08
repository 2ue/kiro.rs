# Round 80：WebSearch 完整抓包复测

## 时间与版本

- 时间：2026-11-07（北京时间）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 模型：`claude-opus-5-5`
- 59139：开发循环热更新实例，版本来自当前工作树 `v0.0.240`
- 结果文件：[`20261107T195739-round-80-capture.json`](../results/20261107T195739-round-80-capture.json)
- 原始抓包：`logs/llm-capture/capture-20261107T195734Z-0c5fe1ef/events.jsonl`
- 抓包归档：`logs/llm-capture/capture-20261107T195734Z-0c5fe1ef/capture.zip`
- 抓包清单：`logs/llm-capture/capture-20261107T195734Z-0c5fe1ef/manifest.json`

## 结果

总分 **82/110**，与 Round 79 相同。

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

## 请求级抓包证据

抓包清单记录：

- `eventCount=10037`
- `bytesWritten=8810664`
- `droppedEvents=0`
- 版本 `0.0.240`

请求矩阵：

- 22 个客户端请求；其中 21 个流式、1 个非流式
- `thinking_text=3`、`thinking_tool=7`、`tool_continuation=4`
- `plain_stream=6`、`server_tool=1`
- 公开流式请求均有 `message_start → content_block* → message_delta → message_stop`
- 含 thinking 的请求均有 `signature_delta`
- 上游流仍普遍为 `upstreamEofWithoutCompleted=true`，代理按内容推断 `end_turn` 或 `tool_use`

## WebSearch 链路

本轮 WebSearch 请求已完整完成：

```text
server_tool_use(web_search)
→ input_json_delta
→ content_block_stop
→ MCP tools/call HTTP 200
→ web_search_tool_result
→ citations_delta
→ text_delta
→ message_delta(stop_reason=end_turn, web_search_requests=1)
→ message_stop
```

已确认：

- MCP 结果回灌当前模型轮次；
- `tool_use_id` 配对正确；
- 结果块和 citation 均存在；
- 最终文本引用结果；
- `server_tool_use.web_search_requests=1`；
- 终止事件完整。

仍存在的公开字段差异：

- `web_search_result.encrypted_content` 由 MCP `snippet` 明文直接填充；
- `web_search_result_location.encrypted_index` 为可逆的 `Base64(序号:URL)`；
- 官方 Anthropic 文档要求这两个字段作为 opaque 值原样回传。

Round 77/78 已用本地确定性 protobuf 外层做过单变量 A/B，`websearch` 仍为 0，因此本轮不继续随机伪造字段。没有完整官方 fixture、生成密钥或 verifier 错误样本前，不能把外层形状仿造当成兼容实现。

## 提示词注入复核

Round 80 客户端和上游 marker 统计：

- 客户端 `<system-reminder>`：17 个
- 上游保留 `<system-reminder>`：17 个
- 上游 `<thinking_mode>`：16 个
- 上游代理自定义 identity、prompt steering、WebSearch guidance、language reminder、tool choice 文本：0 个

Admin preview（`/dfcache/tune-ss6-99`，本地上游）显示：

- `thinkingControls`：`not_triggered`
- `modelDrivenWebSearch=true`
- `webSearchGuidance`：路由显式关闭
- `identityPrompt`、`identityTurnReminder`、`toolChoicePrefix`、`toolChoiceTurnReminder`、`emptyTurnNudge`：路由显式关闭

这说明提示词注入会改变模型行为，但当前三项失败不能归因于 tune 路由额外注入；客户端自带的 `<system-reminder>` 和 `<thinking_mode>` 仍需保留。

## 操作与保留

- 59138 仅执行 `/healthz` 只读检查
- 59139 通过开发循环自动编译和重启
- Round 80 的 JSONL、ZIP、manifest、结果文件和矩阵继续保留
- 未实时清理任何原始抓包
