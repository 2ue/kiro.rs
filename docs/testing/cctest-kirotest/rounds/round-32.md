# Round 32：无 native opaque signature 时生成 synthetic signature

时间：2026-11-07（北京时间 04:43–04:45；证据时间 UTC 20:43–20:45）

## 实验变量

- 目标与后端：`kirotest.okmcode.com` → 59139。
- 代码：工作区 HEAD `2e625e52`。
- 代码改动：native reasoning 没有真实 signature 时，按已下发的清洗 thinking 文本生成 synthetic `signature_delta`；真实上游 signature 仍优先。
- 二进制：`/tmp/kirotest-dev/kiro-rs-cctest-synthetic-native`，SHA-256 `47db7f3dbeb6f3e496426248c694090b6ba9330f073c16c2ff5e6145ac472f06`。

## 结果

结果文件：[20261106T204319-round-32-synthetic-native-signature.json](../results/20261106T204319-round-32-synthetic-native-signature.json)

总分 `68/110`：流结构 5、非流结构 10、WebSearch 0、签名 0、结构化输出 10、server tool 10、模型归因 0，其余保护项 10。

## 抓包证据

抓包：[capture-20261106T204311Z-6fadd2c1/events.jsonl](../../../../logs/llm-capture/capture-20261106T204311Z-6fadd2c1/events.jsonl)

- 6001 行，7,326,122 字节；23 个完整流。
- 上游 `200=26`、`402=1`；402 只影响一个账号尝试，未导致整轮中断。
- WebSearch 请求有完整 server tool/result 生命周期。

## 结论

synthetic signature 没有改善签名分项。cctest 需要的 signature 很可能是 Kiro 返回的 opaque/protobuf 值，离线 marker 只能满足非空形状，不能通过私有验证。保留真实 signature 优先，停止继续调整 marker 编码。

资源与生产边界：开发编译 `-j2`；59138 只读、未重启。
