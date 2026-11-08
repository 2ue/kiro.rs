# Round 09：server_tool_use 首事件携带完整 input

时间：2026-11-06（北京时间）

## 变更与版本

工作树包含：原生代码执行 schema 修复、WebSearch 最终连续文本块合并，以及 `server_tool_use` 在首个 `content_block_start` 中携带完整 `input`。开发实例运行时 API 已确认 `/dfcache/tune-ss6-99` 的代码执行与模型驱动 WebSearch 开关开启。

## cctest 结果

结果文件：`results/20261106T115132-round-09-server-tool-input.json`，总分 `73/110`。

| 检测项 | Round 08 | Round 09 |
|---|---:|---:|
| tag_check | 10 | 10 |
| stream_structure | 5 | 5 |
| non_stream | 10 | 10 |
| websearch | 0 | 0 |
| signature_proto | 5 | 5 |
| output_config | 0 | 0 |
| server_tool | 10 | 10 |
| knowledge | 10 | 10 |
| model_attribution | 10 | 10 |
| doc_recognition | 10 | 10 |
| image_recognition | 10 | 10 |

## 抓包证据

抓包：`logs/llm-capture/capture-20261106T115125Z-7a33e1cf/events.jsonl`，9805 行、8,702,715 字节（约 8.3 MiB），全部 JSON 可解析；没有丢弃事件。ZIP 为 1,751,871 字节，均保留。

WebSearch 请求 `req_01ywQjCszeZ3LLVSgoXiw2P2` 的序列摘要：

```text
message_start
content_block_start(server_tool_use, index=0, input.query 已完整)
content_block_delta(input_json_delta, index=0, 同一 query 再次发送)
content_block_stop(index=0)
content_block_start(web_search_tool_result, index=1, 结果非空)
content_block_stop(index=1)
content_block_start(text, index=2, citations_delta + 多个 text_delta)
content_block_stop(index=2)
message_delta(stop_reason=end_turn, web_search_requests=1)
message_stop
```

该观察说明此前补齐 `server_tool_use.input` 后仍留下了 `input_json_delta`，同一输入被发送两次。Round 10 已移除 server tool 的增量事件，只保留客户端 `tool_use` 的 `input_json_delta`。本轮 WebSearch 仍为 0，说明 query 首事件字段不构成评分差异的充分原因。

请求 summary 显示 WebSearch MCP 成功，Kiro upstream 返回 200；cctest 全部保护项没有退化。

## 资源与保护范围

- 运行前：可用内存约 9.8 GiB，load average `1.83/2.99/2.93`，8 个 CPU；未触发暂停阈值。
- 本轮完整检测约 3 分钟；没有连续采样峰值数据，所以上述是启动前样本，不冒充峰值。
- 仅访问 59139；生产 59138 未重启、未改配置或写数据库/Redis。
- 没有执行 release 编译。

## cctest 调用环境修正

本轮最初通过 `source` 加载 `/root/code/kiro.rs/.env.local` 和 cctest 环境文件，zsh 对该 env 文件的 `-i` 标志给出错误；脚本仍运行并成功提交任务，因此 Round 09 结果已正常保存。本轮后续改用 `set -a; . <file>; set +a`，只留下该 shell 选项提示，不影响 Round 10 API 调用。
