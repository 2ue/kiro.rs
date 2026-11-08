# Round 31：恢复历史 thinking 丢弃并复测

时间：2026-11-07（北京时间 04:24–04:27；证据时间 UTC 20:24–20:26）

## 实验变量

- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`，后端 59139。
- 代码与二进制：同 Round 30，SHA-256 `301194a1bf3d3fb1828975b2a07ceca887c8def6238efb35c747005d342e1278`。
- 配置：恢复 `payloadShaping.discardHistoricalThinking=true`；保留 Round 30 的 thinking delta 字段改动。

## 结果

结果文件：[20261106T202454-round-31-shaping-on-estimate.json](../results/20261106T202454-round-31-shaping-on-estimate.json)

总分 `68/110`，分项与 Round 30 完全一致：流结构 5、非流结构 10、WebSearch 0、签名 0、结构化输出 10、server tool 10、模型归因 0，其余保护项 10。

## 抓包证据

抓包：[capture-20261106T202447Z-85162d9a/events.jsonl](../../../../logs/llm-capture/capture-20261106T202447Z-85162d9a/events.jsonl)

- 6019 行，7,029,635 字节；22 个完整流。
- 上游请求 25 个，全部 HTTP 200。
- `message_start=22`、`message_delta=22`、`message_stop=22`，没有流终止缺失。

## 结论

Round 30 的分数下降不是 `discardHistoricalThinking` 配置造成的；Round 29–31 也显示模型采样会改变签名、结构化输出和模型归因分项。当前应停止这两个配置之间的来回切换，转向请求级协议字段和上游模型格式证据。

59138 全程未修改。
