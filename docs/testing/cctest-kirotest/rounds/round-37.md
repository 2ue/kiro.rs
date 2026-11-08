# Round 37：标准 usage 字段单变量验证

## 运行信息

- 时间：2026-11-07（北京时间 09:48–09:51）
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 模型：`claude-opus-5-5`
- 59139：开发模式二进制 `/tmp/kirotest-dev/kiro-rs-cctest-round37-usage-fields`
- 二进制 SHA-256：`f9b70aebc1b1cc915a0a459327138f78a8f7dcc96d8718bf84a257bd1d77d110`
- 59138：只读，未重启、未改配置、未写 PostgreSQL/Redis

## 结果

结果文件：`results/20261107T014855-round-37-usage-fields.json`

| 项目 | Round 36 | Round 37 |
|---|---:|---:|
| 总分 | 73/110 | 73/110 |
| 流结构 | 5/10 | 5/10 |
| 非流结构 | 10/10 | 10/10 |
| WebSearch | 0/10 | 0/10 |
| 签名 | 5/10 | 5/10 |
| 结构化输出 | 0/10 | 0/10 |
| server tool | 10/10 | 10/10 |
| 其他保护项 | 全部通过 | 全部通过 |

## 代码变更

`src/anthropic/cache.rs` 和 `src/anthropic/websearch.rs` 的标准最终 usage 默认增加：

```json
{
  "output_tokens_details": null,
  "server_tool_use": null
}
```

thinking 或 WebSearch 请求仍分别覆盖这两个字段。缓存 usage 单测同步增加字段断言。

## 抓包证据

抓包：`logs/llm-capture/capture-20261107T014843Z-e30c7f4b/events.jsonl`

- 9,345 行，8,505,843 字节，JSON 解析错误 0，丢弃事件 0
- 22 个客户端请求、25 个上游请求；上游 `200=25`
- 客户端 `message_start=22`、`message_delta=22`、`message_stop=22`
- 22 个客户端流的初始和最终 usage 都包含四个标准 token 字段以及 `output_tokens_details`、`server_tool_use`
- 其中 1 个 WebSearch 内部请求产生 3 次上游调用，最终客户端流完整结束

## 结论

补齐 SDK 类型中的可选 usage 字段没有改变任何评分项。`message_start.usage` 与最终 usage 的数值差异仍符合 Anthropic 官方关于初始 usage 可为估算值、最终 usage 为权威值的约定。停止继续猜 usage 字段。
