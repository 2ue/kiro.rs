# Round 07：多项矩阵基线

时间：2026-11-06（北京时间）

## 结果

结果文件：`results/20261106T112359-round-07-matrix-baseline.json`，总分 `82/110`。

| 检测项 | Round 06 | Round 07 |
|---|---:|---:|
| tag_check | 10 | 10 |
| stream_structure | 5 | 5 |
| non_stream | 10 | 10 |
| websearch | 0 | 0 |
| signature_proto | 5 | 5 |
| output_config | 0 | 10 |
| server_tool | 10 | 10 |
| knowledge | 10 | 10 |
| model_attribution | 10 | 10 |
| doc_recognition | 10 | 10 |
| image_recognition | 10 | 10 |

唯一波动是 `output_config` 从 0 变为 10；其请求/响应内容来自模型生成，现阶段按采样随机性记录，不以单轮通过认定实现已稳定。

## 抓包与结论

抓包：`logs/llm-capture/capture-20261106T112311Z-dfbdd12e/events.jsonl`，8444 行、约 8.1 MiB，JSON 均可解析，上游 HTTP 200 共 25 次。

WebSearch 请求 `req_01mhBYrmDYvoNV9TgMGSV4KT` 的对外事件为 `server_tool_use → web_search_tool_result → 10 个 text block → message_delta(end_turn) → message_stop`。搜索结果和 citation 都存在，评分仍为 0。

这一轮没有代码改动，用于观察代码 schema 修复后的基线与 `output_config` 波动。完整请求及响应保留在原始抓包中，文档不复制用户正文。

## 资源与保护范围

- 使用 59139 开发实例；59138 未重启、未改配置或写生产存储。
- 资源监控峰值未形成连续采样，本轮只保留 cctest 运行记录；后续记录运行前后样本。
- 未执行 release 编译。
