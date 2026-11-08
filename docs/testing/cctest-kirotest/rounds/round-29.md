# Round 29：关闭历史 thinking 丢弃

时间：2026-11-07（北京时间 03:53–03:55；证据时间 UTC 19:53–19:55）

## 实验变量

- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`，后端 59139。
- 代码：工作区 HEAD `2e625e52`，仅运行时单变量配置实验。
- 配置：`payloadShaping.discardHistoricalThinking=false`；其余配置与 Round 28 相同。
- 开发模式：使用增量二进制 `/tmp/kirotest-dev/kiro-rs-cctest-null-estimate`，SHA-256 `24e0ebe3dfe70276bf3ee4d0d62c76decc39066062cc960a4f20023ccbbedab4`。
- 生产 59138：未重启、未改配置、未写 PostgreSQL/Redis、未调用 Admin 写接口。

## 结果

结果文件：[20261106T195353-round-29-shaping-off.json](../results/20261106T195353-round-29-shaping-off.json)

总分 `73/110`，分项：

| 流结构 | 非流结构 | WebSearch | 签名 | 结构化输出 | server tool | 模型归因 |
|---:|---:|---:|---:|---:|---:|---:|
| 5 | 10 | 0 | 0 | 10 | 10 | 5 |

与 Round 28 相比，总分不变；`signature_proto` 从 5 变为 0，`output_config` 从 0 变为 10。该变化同时伴随 cctest 采样模型从 `claude-opus-5-5` 变为 `claude-opus-4-8`，不能归因于配置。

## 抓包证据

抓包：[capture-20261106T195352Z-b215a045/events.jsonl](../../../../logs/llm-capture/capture-20261106T195352Z-b215a045/events.jsonl)

- 5838 行，7,250,624 字节。
- 23 个客户端请求，26 个上游请求；上游 `200=26`。
- 23 个完整流，`message_start`、`message_delta`、`message_stop` 均为 23 次。
- WebSearch 路由 38 个客户端 SSE 事件；本轮仍有完整搜索生命周期。

抓包表明关闭历史 thinking 丢弃会保留更多历史 reasoning/signature，但没有形成稳定分数收益，且签名分项随模型采样下降。

## 结论

`discardHistoricalThinking=false` 不是四个失败项的已证实修复。恢复 `true`，避免把未确认可重放的历史 thinking signature 继续送入 Kiro；后续不再用该配置做随机微调。

资源：开发增量编译限制 `-j2`，可用内存约 9 GiB，未触发暂停阈值。
