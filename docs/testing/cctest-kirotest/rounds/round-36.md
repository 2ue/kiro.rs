# Round 36：固定 Opus 5.5 连续两次复测

时间：2026-11-07（北京时间 05:20–05:28；证据时间 UTC 21:20–21:28）

## 实验变量

- 代码不变，59139 开发模式，模型固定 `claude-opus-5-5`。
- 连续两次完整检测，抓包会话统一为 `capture-20261106T212015Z-ca2efad5`。
- 生产 59138 未重启、未改配置、未写库。

## 结果

结果文件：

- [20261106T212015-round-36-fixed-opus55-repeat2.json](../results/20261106T212015-round-36-fixed-opus55-repeat2.json)
- [20261106T212513-round-36-fixed-opus55-repeat2.json](../results/20261106T212513-round-36-fixed-opus55-repeat2.json)

两次结果均为 `73/110`，分项完全一致：流结构 5、非流结构 10、WebSearch 0、签名 5、结构化输出 0、server tool 10，保护项全部 10。固定模型后签名项稳定为 5，但未突破流结构和 WebSearch。

## 抓包统计

抓包：[capture-20261106T212015Z-ca2efad5/events.jsonl](../../../../logs/llm-capture/capture-20261106T212015Z-ca2efad5/events.jsonl)

- 20291 行，17,754,615 字节；44 个客户端请求，51 个上游请求。
- 上游状态 `200=50`、`500=1`；500 请求在首输出前自动重试，最终 `request_summary.status=success`，没有客户端 502。
- `message_start=44`、`message_delta=44`、`message_stop=44`；没有流终止缺失。
- 2 个 WebSearch 路径均有完整 MCP/Anthropic 事件。

## 结论

两次固定模型复测仍 73，说明当前四个失败项不是单纯模型抖动：`stream_structure=5` 与 `websearch=0` 稳定存在；`output_config=0` 在同一代码下也连续出现。下一步应基于请求级证据修改已证实的状态机或输出语义，不再做无证据的签名编码实验。

本轮抓包停止后原始 JSONL 和 ZIP 均保留；没有触碰 59138 账号或配置。
