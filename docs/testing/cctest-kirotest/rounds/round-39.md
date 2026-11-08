# Round 39：结构化输出保活状态机验证

## 运行信息

- 时间：2026-11-07（北京时间 10:24–10:27）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 模型：`claude-opus-5-5`
- 59139：开发模式二进制 `/tmp/kirotest-dev/kiro-rs-cctest-round39-structured-keepalive`
- 二进制 SHA-256：`1d2b5aab625162aa0747dcf2a2f55cb97d2981a660d9c8e4f678e556e9469f9f`
- 59138：只读，未重启、未改配置、未写 PostgreSQL/Redis

## 结果

结果文件：`results/20261107T022433-round-39-structured-keepalive.json`

| 项目 | Round 38 | Round 39 |
|---|---:|---:|
| 总分 | 73/110 | 73/110 |
| 流结构 | 5/10 | 5/10 |
| 非流结构 | 10/10 | 10/10 |
| WebSearch | 0/10 | 0/10 |
| 签名 | 5/10 | 5/10 |
| 结构化输出 | 0/10 | 0/10 |
| server tool | 10/10 | 10/10 |

## 代码变更

`StreamContext::claude_code_noop_delta_keepalive_event` 在内部 `structured_output` 工具块打开时不再生成 `input_json_delta`。该工具块随后会被重写为下游 `text` block，保活由外层发送标准 `ping`。

新增单测 `structured_output_tool_keepalive_falls_back_to_ping`，并与现有 keepalive、WebSearch 流测试一起通过。

## 抓包证据

抓包：`logs/llm-capture/capture-20261107T022419Z-a6c7dfaf/events.jsonl`

- 8,526 行，8,157,536 字节，JSON 解析错误 0，丢弃事件 0
- 22 个客户端请求、27 个上游请求；上游 `200=25`、`500=2`
- 2 次上游 500 均未造成客户端流中断，但本轮没有完整上游重试成功证据以外的评分改善
- 结构化输出请求 `req_01BvdTgpkUy53Zt4vQTKY4uU` 的所有 `content_block_delta` 都是 `text_delta`，不再出现 text block 内的空 `input_json_delta`
- 客户端生命周期仍完整：`message_start=22`、`message_delta=22`、`message_stop=22`

## 结论

非法保活事件已修正，但 `output_config` 仍为 0，说明该字段组合不是 cctest 结构化输出评分的充分条件。Round 37–39 三个最小协议变量均未改变 73 分，下一步停止继续微调公开字段，转向对评分采样请求和上游模型行为做请求级矩阵分析。
