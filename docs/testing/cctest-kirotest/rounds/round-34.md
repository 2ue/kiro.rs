# Round 34：固定 `claude-opus-5-5` 复测

时间：2026-11-07（北京时间 05:08–05:16；证据时间 UTC 21:08–21:16）

## 实验变量

- 代码不变，继续使用 59139 开发模式二进制 `kiro-rs-cctest-synthetic-native-v2`。
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`。
- 模型固定：`claude-opus-5-5`。
- 59138 未重启、未改配置、未写 PostgreSQL/Redis。

## 结果

结果文件：[20261106T210853-round-34-fixed-opus55.json](../results/20261106T210853-round-34-fixed-opus55.json)

总分 `73/110`：流结构 5、非流结构 10、WebSearch 0、签名 5、结构化输出 0、server tool 10；保护项全部 10。

Round 33 使用 `claude-opus-4-8` 时签名和模型归因分别为 0/5；固定为 Opus 5.5 后恢复为 5/10。这证明签名和模型归因存在模型采样差异，不能把单轮总分下降归因于 Round 33 代码。

本轮未开启新抓包；Round 33 的逐请求抓包仍保留，后续 Round 35 开始重新采集。
