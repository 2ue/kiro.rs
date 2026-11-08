# Round 20：固定 Opus 5.5

时间：2026-11-06（UTC 17:15–17:18；北京时间 01:15–01:18）

## 环境与边界

- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`，后端 59139 开发实例。
- 模型：`claude-opus-5-5`。
- 抓包：`logs/llm-capture/capture-20261106T171520Z-ca1025dd/events.jsonl`。
- 抓包统计：8315 个事件、8053898 字节、27 个上游响应；上游响应全部 HTTP 200。
- 59138 保持只读，未重启、未改配置、未写生产数据库或 Redis。

## 结果

结果文件：`results/20261106T171520-round-20-opus55.json`

总分：`73/110`。

| 检测项 | 分数 |
|---|---:|
| tag_check | 10 |
| stream_structure | 5 |
| non_stream | 10 |
| websearch | 0 |
| signature_proto | 5 |
| output_config | 0 |
| server_tool | 10 |
| knowledge | 10 |
| model_attribution | 10 |
| doc_recognition | 10 |
| image_recognition | 10 |

## 请求级结论

- WebSearch 请求可以生成 `server_tool_use`、`web_search_tool_result`、caller、结果、引用、最终 usage、`end_turn` 和 `message_stop`，但私有评分仍为 0。
- thinking 流中出现空 `thinking_delta`、`estimated_tokens` 和 `signature_delta` 的组合；这成为 `stream_structure=5` 的最小单变量实验候选。
- 结构化输出的上游请求携带 `additionalModelRequestFields.output_config`，本轮评分为 0；需与固定模型重复轮次比较，不能只看单个合法 JSON 响应。
