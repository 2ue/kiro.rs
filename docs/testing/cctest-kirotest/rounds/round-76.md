# Round 76：完整抓包复测与逐请求 signature 对照

## 时间与边界

- 时间：2026-11-07 19:16–19:19 UTC。
- 59139 使用 debug 热更新实例；59138 只读健康探针，未停止、未重启、未写生产配置或数据。
- 模型：`claude-opus-5-5`；路由：`/dfcache/tune-ss6-99`。
- 抓包在 59139 开发实例进程内启动，任务完成后停止；不清理原始 JSONL。

## 结果

结果文件：[`20261107T191606-round-76-signature-field-capture.json`](../results/20261107T191606-round-76-signature-field-capture.json)

任务 UUID：`2ca309d5-5a2d-40e1-84a7-98ccaa28b805`。

总分 **82/110**，失败项仍为 `stream_structure=5`、`websearch=0`、`signature_proto=5`。

## 抓包完整性

抓包目录：[`capture-20261107T191558Z-019305d6`](../../../logs/llm-capture/capture-20261107T191558Z-019305d6/)

- `eventCount=8997`；`events.jsonl` 约 8.97 MiB。
- `droppedEvents=0`。
- `capture.zip` 与 `manifest.json` 已生成。
- 客户端流均闭合，存在 `message_start`、`message_delta`、`message_stop`。
- `content_block_start` 与 `content_block_stop` 成对，block index 连续。
- WebSearch 公开生命周期完整：`server_tool_use → input_json_delta → web_search_tool_result → citations_delta → text_delta → end_turn → message_stop`。

## signature 逐请求对照

本轮共有 13 个带 thinking 的上游请求。逐请求比较上游 `reasoningContentEvent.signature` 与下游 Anthropic `signature_delta.signature`：

```text
13/13 完全一致
```

样本签名为 Kiro opaque Base64，长度约 876–2628 字符，解码前缀为 protobuf 形状（`08 04 12 ...`），不是代理生成的 synthetic marker。当前没有证据支持删除、随机改写或重新编码真实签名。

## 结论

`signature_proto=5` 不能由代理改写真实签名解释，更可能与 CCTest 的私有通道判定、抽样请求无 thinking 或 AWS Bedrock 通道规则有关。`websearch=0` 仍优先排查 `encrypted_content` / `encrypted_index` opaque 语义；`stream_structure=5` 继续排查 EOF 无 `COMPLETED` 与私有抽样规则。原始 JSONL、ZIP、manifest 和结果文件全部保留。
