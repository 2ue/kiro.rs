# Round 12：采集 WebSearch 完整成功流并核对评分

时间：2026-11-06（北京时间；cctest UTC 13:22–13:26）

## 代码与开发实例

- 源码 HEAD：`2e625e52`，crate `0.0.240`；使用 scoped `cargo run -j 2`，无 release 冻结。
- 开发监听：`0.0.0.0:59139`，PID `194346`；独立 PostgreSQL 25432、Redis 26379。
- 重启时第一次漏传 `KIRO_RS_HOST=0.0.0.0`，进程绑定 `127.0.0.1`，Caddy 公网短暂返回 502。修正后以 `KIRO_RS_HOST=0.0.0.0` 启动，确认监听 `0.0.0.0:59139`，公网恢复 HTTP 401、TLS 验证通过，响应包含 `via: 1.1 Caddy`。本轮 cctest 只在链路恢复后开始；未修改 Caddy，也未触碰 59138。

## cctest 结果

结果：`results/20261106T132241-round-12-upstream-body-capture.json`，完整 `done`，总分 `73/110`。

| 检测项 | Round 09 | Round 12 |
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

WebSearch 两次内部 Kiro 调用均得到 HTTP 200 并完成；本轮没有 `websearch_model_call_failed`。因此 0 分是完整响应进入 cctest 判分后的协议差异，不是本轮 Caddy 或上游 502 中断。`caller` 字段和去重 input 生命周期同时存在，单轮不能把 caller 的独立效果归因出来。

## 抓包证据

抓包：`logs/llm-capture/capture-20261106T132227Z-6acb8749/events.jsonl`，10,341 行、9,922,567 字节，全部 JSON 可解析，丢弃事件 0；ZIP 一并保留。上游 HTTP `200=27`、`402=4`、`403=2`。402/403 是共享账号池中其他尝试，成功 WebSearch 调用最终由 200 响应完成。

WebSearch 请求 `req_01g5PprU3gSqnNwKaL1TdEPa`：

```text
message_start
content_block_start(server_tool_use, index=0, input.query 已完整, caller=direct)
content_block_stop(index=0)
content_block_start(web_search_tool_result, index=1, 10 个 web_search_result, caller=direct)
content_block_stop(index=1)
content_block_start(text, index=2)
citations_delta × 10
text_delta …
content_block_stop(index=2)
message_delta(stop_reason=end_turn, web_search_requests=1)
message_stop
```

结果与引用均非空，id 配对在摘要请求中保留，stop 和 usage 完整。可见差异是 `server_tool_use` 的 `input.query` 已放进 `content_block_start`，整个流没有 `input_json_delta`。官方 WebSearch 流式规范及本项目已有官方资料记录要求以 `input_json_delta` 逐步传递 server tool 输入（参考 <https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-search-tool>；本地协议整理 `docs/analysis/websearch-agent-loop-plan-20261021.md`）。官方页面访问尝试时间 2026-11-06 13:32:11 UTC，返回 HTTP 403；判定依据是此前核验的官方协议记录和本轮流量形状，不把 cctest 私有判分猜测当作规范。Round 10 移除重复 delta 后留下的“完整 start input、零 delta”仍不符合该流生命周期。下一轮改成 `input={}` 的 start，再发送唯一一条 `input_json_delta`；完整非流响应保留 query。

其他 200 空体/截断情况本轮没有复现，因此新加的完整 body 诊断路径仍需等待下一次真实失败验证；本轮所有上游流均可解码，未产生 `upstream_body`。

## 资源和保护范围

- 监控最低 `MemAvailable≈11.1 GiB`、最低 `SwapFree≈11.9 GiB`，load 最高观测约 `1.98/2.44/2.95`（1/5/15 分钟）；未触发暂停阈值。
- 59138 只读探针每 15 秒返回 HTTP 401，耗时 2–17 ms；编译/检测前后均正常。
- 生产容器、配置、PostgreSQL、Redis 未修改或重启；没有执行 release 编译。
