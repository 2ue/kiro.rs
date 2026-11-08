# Round 38：WebSearch 非流 caller 字段验证

## 运行信息

- 时间：2026-11-07（北京时间 10:06–10:09）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 模型：`claude-opus-5-5`
- 59139：开发模式二进制 `/tmp/kirotest-dev/kiro-rs-cctest-round38-websearch-caller`
- 二进制 SHA-256：`f965dfcd66ca3a78ba0bccd16284751af30a0b3dc1cebc6afd19257e0d740d8a`
- 59138：只读，未重启、未改配置、未写 PostgreSQL/Redis

## 结果

结果文件：`results/20261107T020647-round-38-websearch-caller.json`

| 项目 | Round 37 | Round 38 |
|---|---:|---:|
| 总分 | 73/110 | 73/110 |
| 流结构 | 5/10 | 5/10 |
| 非流结构 | 10/10 | 10/10 |
| WebSearch | 0/10 | 0/10 |
| 签名 | 5/10 | 5/10 |
| 结构化输出 | 0/10 | 0/10 |
| server tool | 10/10 | 10/10 |

## 代码变更

依据 Anthropic 官方 SDK `WebSearchToolResultBlock` 与 `ServerToolUseBlock` 类型，给 `generate_websearch_message_with_content` 的非流响应补齐：

```json
"caller": {"type": "direct"}
```

该字段同时出现在非流 `server_tool_use` 和 `web_search_tool_result` block。已有流式路径保持不变；WebSearch 形状单测通过。

## 抓包证据

抓包：`logs/llm-capture/capture-20261107T020634Z-2b78f498/events.jsonl`

- 9,337 行，8,501,463 字节，JSON 解析错误 0，丢弃事件 0
- 22 个客户端请求、25 个上游请求；上游 `200=25`
- 客户端 `message_start/message_delta/message_stop` 均为 22 次
- 本轮 WebSearch 公开流仍完整，`server_tool_use`、结果 block、引用与终止事件均存在

## 结论

cctest 本轮未命中非流 WebSearch caller 字段（WebSearch 分数仍为 0），说明当前评分请求主要走流式路径，或 `caller` 不是私有评分的充分条件。保留该协议修复，不继续围绕 caller 猜测。
