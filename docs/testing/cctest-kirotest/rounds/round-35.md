# Round 35：固定 Opus 5.5 并开启抓包

时间：2026-11-07（北京时间 05:13–05:17；证据时间 UTC 21:13–21:16）

## 实验变量

- 代码不变，后端 59139，开发模式。
- 模型固定：`claude-opus-5-5`。
- LLM 抓包会话：`capture-20261106T211344Z-e4ada322`。
- 二进制沿用 Round 33：`fe3e6ff97cd43d30c826f484f4fb5131dfe83516fd638d66a98df19b9bf4b3ed`。

## 结果

结果文件：[20261106T211354-round-35-fixed-opus55-capture.json](../results/20261106T211354-round-35-fixed-opus55-capture.json)

总分仍为 `73/110`：流结构 5、非流结构 10、WebSearch 0、签名 5、结构化输出 0、server tool 10；保护项全部 10。

## 抓包统计

抓包：[capture-20261106T211344Z-e4ada322/events.jsonl](../../../../logs/llm-capture/capture-20261106T211344Z-e4ada322/events.jsonl)

- 9758 行，8,673,604 字节；22 个客户端请求，25 个上游请求，全部 HTTP 200。
- `message_start=22`、`message_delta=22`、`message_stop=22`；`content_block_start/stop` 各 51 次。
- thinking 请求的签名为 Kiro 原生 opaque 值，Round 35 的 Opus 5.5 前缀为 `CAQS...`，无 synthetic marker 混入上游。
- WebSearch 请求重建为完整 server tool/result 生命周期；仍有 10 个结果和 citations，最终 usage 含 `server_tool_use.web_search_requests`。

## 结构化输出请求对照

抓包中有两个相关请求：

1. `output_config={"effort":"medium"}`、`thinking={"type":"adaptive"}`：响应为带 thinking 的自然语言解题文本，最终 `output_tokens_details.thinking_tokens=77`。
2. `output_config.format.type=json_schema`：响应为单一 text block，拼接后是合法 JSON，顶层键为 `identity_platform`、`desc`，`stop_reason=end_turn`。

尽管第二个请求满足公开 JSON 形状，本轮 `output_config=0`。这说明 cctest 还检查 effort 语义、请求组合或模型行为，不能只按 JSON 可解析性判定。

本轮无代码修改；抓包已停止并永久保留，59138 全程只读。
