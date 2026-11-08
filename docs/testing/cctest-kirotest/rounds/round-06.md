# Round 06：补齐代码执行工具 schema 后复测

时间：2026-11-06（北京时间）

## 变更

唯一 Rust 改动位于 `src/anthropic/converter/tools.rs`：

- 识别 `code_execution_20250825` 和 `bash_code_execution_20250825`；
- 原生工具缺少输入 schema 时补充明确对象 schema；
- `code_execution` 要求 `code`，可选 `language`（`python` / `python3`）；
- `bash_code_execution` 要求 `command`；
- 同时补充描述，帮助 Kiro 生成实际输入。

开发实例通过 `cargo check --bin kiro-rs` 验证后，以 `cargo run` 方式继续运行。没有执行 release 编译，也没有改动 59138。

## cctest 结果

结果文件：`results/20261106T111026-round-06-code-schema.json`。

| 检测项 | Round 05 | Round 06 |
|---|---:|---:|
| tag_check | 10 | 10 |
| stream_structure | 5 | 5 |
| non_stream | 10 | 10 |
| websearch | 0 | 0 |
| signature_proto | 5 | 5 |
| output_config | 0 | 0 |
| server_tool | 0 | 10 |
| knowledge | 10 | 10 |
| model_attribution | 10 | 10 |
| doc_recognition | 10 | 10 |
| image_recognition | 10 | 10 |

总分从 `64/110` 提升到 `73/110`，唯一变化是 `server_tool` 从 0 变为 10。

## 代码执行请求级证据

抓包：`logs/llm-capture/capture-20261106T111016Z-7d98710b/events.jsonl`，约 7.6 MiB、8078 行，全部 JSON 可解析；上游请求均为 HTTP 200。

cctest 请求 `req_01FvsaydBLHGFxmA8zeuk17y` 的客户端事件顺序为：

```text
message_start
content_block_start(type=server_tool_use)
content_block_delta(type=input_json_delta)
content_block_stop
content_block_start(type=bash_code_execution_tool_result)
content_block_stop
message_delta(stop_reason=end_turn)
message_stop
```

核心输入和执行结果已脱敏为：

```json
{
  "server_tool_use": {
    "name": "bash_code_execution",
    "input": {"code": "print('HELLO_CHECK')", "language": "python"}
  },
  "result": {
    "stdout": "HELLO_CHECK\\n",
    "stderr": "",
    "return_code": 0
  }
}
```

`tool_use_id` 与结果 block 配对，执行返回码为 0。该证据与 `server_tool=10` 同步变化，确认 Round 05 的根因是工具输入契约缺失，而不是 Caddy 或执行器不可达。

## WebSearch 复核

本轮 cctest WebSearch 子请求 `req_01mjMPZXgmqaYg761dfGNjJa` 的公开响应结构完整：

- `server_tool_use.id` 为 `srvtoolu_` 前缀；
- `web_search_tool_result.tool_use_id` 完全匹配；
- 结果内容非空，包含 `web_search_result_location` 引用；
- `usage.server_tool_use.web_search_requests=1`；
- 最终 `stop_reason=end_turn`，存在 `message_stop`。

但私有评分仍为 `websearch=0`。因此当前差异不在已验证的 ID、结果非空、引用、usage 或尾事件，候选范围收窄到评分器对文本 block 拆分、历史回传、请求模式或额外字段的要求。本轮没有盲改 WebSearch 序列。

## output_config 复核

结构化输出请求 `req_019m2NKVf1kFCujVKoRpKHSW` 使用 `stream=true` 和 `output_config.format.type=json_schema`。响应只有一个 text block，拼接文本可严格 `json.loads`，顶层键为 `identity_platform`、`desc`，枚举值为 `kiro`，无尾随文本，`stop_reason=end_turn`，存在 `message_stop`。

但私有评分仍为 `output_config=0`。这排除了“响应不是合法 JSON”这一解释，下一步需要用非流/流、schema/旧版字段的最小矩阵确认评分器实际请求和实现路径差异。

## 资源与结论

- 使用开发模式迭代，未执行 release 冻结。
- 本轮没有重启生产 59138，也没有写生产 PostgreSQL/Redis。
- 抓包 JSONL、ZIP 和停止记录全部保留。
- schema 改动已验证有效；下一轮优先处理 WebSearch、output_config、stream/signature 的剩余差异。
