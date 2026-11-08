# Round 75：thinking 起始块恢复空 signature

## 时间与边界

- 时间：2026-11-07 19:12–19:15 UTC。
- 59139 使用 debug 热更新实例；59138 只读健康探针，未停止、未重启、未写生产配置或数据。
- 模型：`claude-opus-5-5`；路由：`/dfcache/tune-ss6-99`。
- 单变量改动：恢复 Anthropic 官方流式 thinking `content_block_start` 中的空 `signature` 字段。

## 结果

结果文件：[`20261107T191228-round-75-thinking-signature-start.json`](../results/20261107T191228-round-75-thinking-signature-start.json)

总分 **82/110**。分项仍为：

- `stream_structure=5`
- `websearch=0`
- `signature_proto=5`
- 其余分项通过，包括 `output_config=10`、`server_tool=10`、`model_attribution=10`。

## 协议证据

修改后所有 thinking 起始块均为：

```json
{"type":"thinking","thinking":"","signature":""}
```

定向 Rust 测试 `thinking_block_starts_include_empty_signature_field` 通过。该字段与 Anthropic 官方流式示例一致，但本轮评分没有变化，说明它不是三个剩余失败项的充分根因。

## 结论

保留该协议修复；不再把起始块空 `signature` 作为 `signature_proto` 或 `stream_structure` 的主因。原始抓包和结果文件继续保留。
