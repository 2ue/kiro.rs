# Round 33：native signature 清洗路径复测

时间：2026-11-07（北京时间 04:54–04:56；证据时间 UTC 20:54–20:56）

## 实验变量

- 目标与后端：`https://kirotest.okmcode.com/dfcache/tune-ss6-99` → 59139。
- 代码：工作区 HEAD `2e625e52`。
- 代码改动：对 native reasoning signature 做污染判断；真实 opaque signature 保留，污染的真实签名不下发；无真实签名时按配置生成 synthetic signature。
- 二进制：`/tmp/kirotest-dev/kiro-rs-cctest-synthetic-native-v2`，SHA-256 `fe3e6ff97cd43d30c826f484f4fb5131dfe83516fd638d66a98df19b9bf4b3ed`。

## 结果

结果文件：[20261106T205425-round-33-synthetic-native-signature-v2.json](../results/20261106T205425-round-33-synthetic-native-signature-v2.json)

总分 `73/110`：流结构 5、非流结构 10、WebSearch 0、签名 0、结构化输出 10、server tool 10、模型归因 5，其余保护项 10。

## 抓包证据

抓包：[capture-20261106T205417Z-4984b328/events.jsonl](../../../../logs/llm-capture/capture-20261106T205417Z-4984b328/events.jsonl)

- 6044 行，7,345,293 字节；23 个客户端请求，26 个上游请求；上游全部 HTTP 200。
- 23 个完整流，`message_start=23`、`message_delta=23`、`message_stop=23`。
- 逐请求重建显示：23 个流均有连续 block index、start/delta/stop 配对、最终 `stop_reason`；没有重复 `message_start`、缺失 `message_stop` 或 error 与成功流混排。
- WebSearch 请求 `req_01w2txi4CRZq3SGPhPcboUsT`：`text → server_tool_use → web_search_tool_result → text`，server/result 的 `id` 配对，两个 block 都有 `caller={"type":"direct"}`，query 通过唯一 `input_json_delta` 发送，10 个结果、10 个 citations，最终 `end_turn` 与 `message_stop` 存在。
- WebSearch 结果中的 `encrypted_content` 是 MCP snippet 明文；引用中的 `encrypted_index` 是代理自定义 Base64(`序号:URL`)。这两个字段不是官方 opaque 值的可验证生成结果。
- thinking 请求存在真实上游 signature；Round 33 的 `claude-opus-4-8` signature 以 `Ep...` 开头，Round 28 的 `claude-opus-5-5` signature 以 `CAQS...` 开头，均原样保留，不能跨模型改写。

## 结论

公开 SSE 生命周期已经排除为 WebSearch 0 分的充分根因；剩余高概率差异是 opaque 搜索字段或 cctest 私有历史闭环检查。`stream_structure=5` 也不能由公开事件缺失解释，当前样本状态机完整，需拿到 cctest 可验证 fixture 才能继续最小化修改。

Round 32–33 没有证据支持继续伪造 signature。下一步只保留真实上游 signature，记录无法离线生成的协议边界，并补齐最终回归文档。

59138 版本仍为镜像 `0.0.239`、revision `30edc00c12431371e22498298715adc7bea539fd`；容器和端口均未重启、未写入。
