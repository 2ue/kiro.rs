# Round 30：beta thinking 可见 delta 增加 `estimated_tokens: null`

时间：2026-11-07（北京时间 04:20–04:22；证据时间 UTC 20:20–20:22）

## 实验变量

- 目标与后端：`kirotest.okmcode.com` → 59139。
- 代码：工作区 HEAD `2e625e52`。
- 代码改动：beta 请求的可见/native thinking delta 在没有估算值时附带 `estimated_tokens: null`。
- 开发二进制：`/tmp/kirotest-dev/kiro-rs-cctest-thinking-estimate`，SHA-256 `301194a1bf3d3fb1828975b2a07ceca887c8def6238efb35c747005d342e1278`。
- 运行时保持 `discardHistoricalThinking=true`、`syntheticThinkingSignatures=true`、`compatProfile=claude-code`。

## 结果

结果文件：[20261106T202018-round-30-visible-thinking-estimate.json](../results/20261106T202018-round-30-visible-thinking-estimate.json)

总分 `68/110`：

| 流结构 | 非流结构 | WebSearch | 签名 | 结构化输出 | server tool | 模型归因 |
|---:|---:|---:|---:|---:|---:|---:|
| 5 | 10 | 0 | 0 | 10 | 10 | 0 |

## 抓包证据

抓包：[capture-20261106T201838Z-1528e974/events.jsonl](../../../../logs/llm-capture/capture-20261106T201838Z-1528e974/events.jsonl)

- 5927 行，6,996,154 字节；22 个完整流。
- 上游请求 25 个，全部 HTTP 200。
- `client_sse` 中 `message_start=22`、`message_delta=22`、`message_stop=22`。
- thinking delta 均按 beta 路径携带字段；仍存在签名分项为 0 的 `claude-opus-4-8` 样本。

## 结论

增加 `estimated_tokens: null` 没有改善 `stream_structure`、`signature_proto` 或 WebSearch。Round 25–30 的对照已足够排除该字段是当前采样失败的充分根因；保留公开 beta 字段形状，停止继续猜测估算值。

资源：开发模式增量编译 `-j2`，未触发暂停阈值；59138 只读。
