# Round 66：thinking token 字段单变量复测

## 结果

- 时间：2026-11-08 01:27–01:31（北京时间；抓包 UTC 17:27–17:30）
- 路由：`/dfcache/tune-ss6-99`
- 模型：`claude-opus-5-5`
- 单变量：59139 环境启用 `KIRO_RS_CCTEST_NO_PLAIN_EMPTY_THINKING_DELTA=1` 与 `KIRO_RS_CCTEST_NULL_THINKING_ESTIMATED_TOKENS=1`
- 结果：`82/110`
- 失败项：`stream_structure=5`、`websearch=0`、`signature_proto=5`
- 结果文件：`results/20261107T172732-round-66-thinking-estimates.json`

## 抓包

- 原始事件：`logs/llm-capture/capture-20261107T172726Z-07bca188/events.jsonl`
- 原始归档：`/tmp/kirotest-dev/artifacts/round-66-capture.zip`
- 事件行：10101；大小：8,840,350 字节；JSON 错误：0；丢弃：0
- 上游请求：27；HTTP `200=25`、`402=2`
- 客户端请求：22；完整客户端流：22/22
- `message_start/message_delta/message_stop`：22/22/22
- `content_block_start/content_block_stop`：54/54

## thinking 字段统计

- `thinking_delta`：30 个
- `estimated_tokens` 数值：21 个
- `estimated_tokens:null`：0 个
- 缺失 `estimated_tokens`：9 个

缺失字段来自本轮实际输出路径中的非过滤器事件，不能靠继续给所有空帧补 `null` 消除。Round 25–28 与本轮共同表明该字段不是 `stream_structure` 或 `signature_proto` 的充分根因。

## 结论

停止该字段方向的微调；保留环境开关和两份原始抓包，后续只在出现新的评分共变证据时再改变 thinking wire。
