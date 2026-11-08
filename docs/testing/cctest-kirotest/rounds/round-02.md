# Round 02：公网 cctest 第二轮完整抓包

时间：2026-11-06（北京时间）

## 代码与链路

- 代码：仓库 HEAD `2e625e52`，`v0.0.240`。
- 开发实例：59139，`cargo run` 开发模式，未执行 release 冻结。
- 生产实例：59138 保持运行，镜像仍为 `0.0.239`，未重启、未改配置。
- 公网入口：`https://kirotest.okmcode.com`；Caddy 已加载站点，后端为 `host.docker.internal:59139`。
- 证据抓包：`logs/llm-capture/capture-20261106T101535Z-0d7cead5/events.jsonl`，原始文件及 `capture.zip` 保留在仓库外。

## cctest 结果

结果文件：`results/20261106T101601-round-02-capture.json`。

| 检测项 | 分数 |
|---|---:|
| tag_check | 10 |
| stream_structure | 5 |
| non_stream | 10 |
| websearch | 0 |
| signature_proto | 5 |
| output_config | 0 |
| server_tool | 0 |
| knowledge | 10 |
| model_attribution | 10 |
| doc_recognition | 10 |
| image_recognition | 10 |

总分 `64/110`，`73` 个检查，判定 `reversed`。与 Round 01 相同；`output_config` 仍为 0，未出现新的分数退化。

## 抓包统计

- 10433 个事件，约 8.95 MiB。
- 24 个 requestId，23 个 upstream request/response，全部 HTTP 200。
- 客户端 SSE 4986 个，MCP response 1 个。
- 本轮没有 upstream 400、429、500；生产抓包中的 synthetic thinking signature 400 未在开发实例复现。

## 已确认的事件证据

### thinking 与普通工具

thinking 请求均出现完整的 `message_start → thinking block → signature_delta → text/tool_use → message_delta → message_stop` 结构，最终 usage 含非零 `output_tokens_details.thinking_tokens`。普通工具请求的 `tool_use` block 具有 `id/name`，最终 `stop_reason=tool_use`。

但完整工具闭环尚未确认：需继续按 requestId 检查下一轮 `tool_result.tool_use_id` 是否逐一匹配，以及空 input 是否被检查器视为无效。

### output_config

请求 `req_01mYVjzpTMyY4wdNULamUFu8`（`stream=true`）携带 `output_config.format.type=json_schema`，schema 要求 `identity_platform` 与 `desc`。

响应事件序列为：

```text
message_start
content_block_start index=0 type=text
145 个 text_delta
content_block_stop index=0
message_delta stop_reason=end_turn
message_stop
```

拼接后的 text 是合法 JSON，顶层键正好为 `identity_platform`、`desc`，没有尾随内容；因此“响应不是 JSON”已排除。上游抓包同时显示内部 `structured_output` toolUseEvent，代理把它还原成了 text block。当前 `output_config=0` 的根因仍待确认，下一步要对比 cctest 对流式响应的具体判定，重点检查是否要求非流响应、`output_config` schema 的严格类型校验，或请求是否因内部 tool 转换丢失了原始 format 语义。

### WebSearch

本轮没有把 route=websearch 的所有 SSE 事件误判成成功。已确认开发冒烟请求存在完整的 native server tool 形状，但 cctest 请求级结果仍为 `websearch=0/server_tool=0`。下一步按 cctest 请求单独提取 `server_tool_use`、`web_search_tool_result`、usage 与后续历史回传，不能只凭 block 外形修改代码。

### stream_structure

本轮成功请求的下游 SSE 大多闭合，但 `request_summary.latencyTrace` 仍出现 `upstreamEofWithoutCompleted=true`、`sawUpstreamCompleted=false`、`stopReasonSource=local_inferred_tool_use/end_turn`。这些字段说明代理在 Kiro 没有显式 completed 事件时本地推断终止，可能正是 5/10 的采样失败来源，需先建立请求级状态表再改 retry 逻辑。

## 结论

1. Caddy、公网 HTTPS、Cloudflare、开发后端链路已经打通；`/v1/models` 返回 401 是鉴权层响应，不是连接失败。
2. HEAD 已消除生产抓包里的首次 synthetic signature 上游 400，但 signature 采样仍只有 5/10，说明还存在历史回放或事件位置层面的协议差异。
3. `output_config` 响应文本本身已验证为合法 JSON，不能再按“普通文本未结构化”处理；必须继续确认 cctest 的请求模式和严格 schema 判定。
4. 本轮没有足够证据直接修改 WebSearch、普通工具或 stream 状态机代码，下一轮先完成请求级事件重建。

## 资源与变更

- 本轮未执行 release 编译。
- 未修改生产 59138。
- 未删除或复制生产 4 GiB 抓包。
- 代码无改动，仅新增本轮记录。
