# Round 13：WebSearch 输入增量生命周期对照

时间：2026-11-06（北京时间；cctest UTC 13:41–13:44）

## 代码与实例

- 使用当前工作区开发代码，crate 版本 `0.0.240`，未执行 release 编译。
- 开发实例继续监听 `0.0.0.0:59139`，独立 PostgreSQL `25432`、Redis `26379`。
- 59138 仅做监听和健康只读检查，没有重启、配置修改、数据库/Redis 写入。
- `kirotest.okmcode.com` 继续由 Caddy 代理到 59139；本轮链路正常。
- 本轮代码差异：`server_tool_use` 的流式 `content_block_start` 使用空 `input`，随后发送唯一一条 `input_json_delta`；同时保留 `caller: {"type":"direct"}`。普通 `tool_use` 的增量行为不变。

## cctest 结果

结果文件：`results/20261106T134159-round-13-input-json-delta.json`

总分仍为 **73/110**：

| 检测项 | Round 12 | Round 13 |
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

本轮只改变 server tool 输入事件生命周期，分数没有变化。因此“缺少 `input_json_delta`”不是 WebSearch 0 分的唯一原因；也不能把当前流形状视为已被 cctest 接受。

## 抓包证据

抓包目录：`logs/llm-capture/capture-20261106T134142Z-43dcb476/`

- `events.jsonl`：8,093 行，8,043,022 字节，全部 JSON 可解析。
- ZIP：同目录的 `capture.zip`，原始抓包保留。
- 丢弃事件：0。
- 上游响应：HTTP 200 共 25 次，403 共 2 次。
- WebSearch 请求：`req_01gRFXbBxBGKdNcDMXtLE6ZY`。

该请求的客户端事件顺序为：

```text
message_start
content_block_start(server_tool_use, index=0, input={}, caller=direct)
content_block_delta(input_json_delta, index=0, partial_json={"query":"AI news 2026-11-06"})
content_block_stop(index=0)
content_block_start(web_search_tool_result, index=1, 10 个 web_search_result, caller=direct)
content_block_stop(index=1)
content_block_start(text, index=2)
citations_delta × 10
text_delta × 17
content_block_stop(index=2)
message_delta(stop_reason=end_turn, server_tool_use.web_search_requests=1)
message_stop
```

结果 block 的每项都含 `type`、`title`、`url`、`page_age`、`encrypted_content`；引用含 `type`、`url`、`title`、`encrypted_index`、`cited_text`。当前 `encrypted_content` 是搜索 snippet 明文，`encrypted_index` 是代理生成的 Base64 字符串；两者仍是高价值协议差异候选。

## 账号清理与生产边界

为避免失效账号干扰开发测试，只在 59139 的独立数据库上删除了 32 个明确自动禁用账号：24 个 `QuotaExceeded`、8 个 `TemporarilySuspended`，批量删除结果 `32/32` 成功。剩余手工禁用账号保留；唯一 `rate_limit/token_refresh_rate_limited` 账号未因一次限流而删除。

本轮及此前清理均没有访问或写入 59138 Admin API，没有修改 59138 的 PostgreSQL、Redis、配置、进程或容器。

## 结论与下一步

1. Caddy、公网入口、上游 WebSearch 成功响应、server tool/result 配对、caller、输入增量、引用、usage 和终止事件均已在请求级抓包中确认。
2. WebSearch 仍为 0，下一轮不再同时修改事件生命周期；优先对照 `encrypted_content`、`encrypted_index` 的官方 opaque 字段语义，以及非流响应的实际字段集合。
3. 继续使用开发模式 `cargo run` 迭代；在 110/110 连续三轮之前，不执行 release 冻结收尾。

## 资源监控

- 编译和检测使用 scoped Cargo，开发模式运行，未执行 release 编译。
- 59138 监听仍由原生产容器提供；59139 由开发进程提供。
- 本轮没有触发资源暂停阈值；没有停止或重启生产服务。
