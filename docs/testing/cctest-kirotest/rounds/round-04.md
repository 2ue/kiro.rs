# Round 04：精确抓包关联 cctest WebSearch

时间：2026-11-06（北京时间）

## 结果

结果文件：`results/20261106T104056-round-04-capture.json`。

总分仍为 `64/110`：`stream_structure=5`、`signature_proto=5`、`websearch=0`、`output_config=0`、`server_tool=0`，其余保护项保持通过。

本轮抓包：`logs/llm-capture/capture-20261106T104041Z-16fbcc19/events.jsonl`，10264 行、约 9.0 MiB，全部保留。

## cctest WebSearch 请求的真实序列

本轮首次按 requestId 找到 cctest 触发的 WebSearch 子请求：`req_01ANsL8eKchpjQM3JF88CrX9`。它不是普通 `client_request`，而是主请求内部生成的服务端 WebSearch 子请求；抓包中存在完整 Kiro upstream request、MCP request/response 和 client SSE。

客户端事件为：

```text
message_start
content_block_start index=0 type=server_tool_use id=srvtoolu_...
content_block_delta index=0 type=input_json_delta
content_block_stop index=0
content_block_start index=1 type=web_search_tool_result tool_use_id=srvtoolu_...
content_block_stop index=1
content_block_start index=2..12 type=text
text_delta/citations_delta
content_block_stop
message_delta stop_reason=end_turn usage.server_tool_use.web_search_requests=1
message_stop
```

MCP `tools/call` 的 query 为 `AI news 2026-11-06`，响应 HTTP 200，结果数组非空。`server_tool_use.id` 与结果 `tool_use_id` 完全相等，前缀为 `srvtoolu_`；文本 block 有 `web_search_result_location` 引用；最终 usage 搜索次数为 1。

这证明本轮 cctest 已经走到模型驱动 WebSearch，且代理输出满足项目现有 `feature/todo/cctest-protocol-compat-20261020/artifacts/e2e.py` 中的主要协议条件。cctest 仍记 `websearch=0/server_tool=0`，说明私有评分还有额外条件，或将 native server tool 检查与普通 client tool 检查分开计算。

## thinking/signature 证据

- 10 个含 thinking 的流式请求都有一次 `signature_delta`；签名长度约 824–1436，Base64 解码为二进制 opaque payload，不是 synthetic marker。
- 多轮请求历史中的 thinking signature 长度 584，发往 Kiro 的 upstream body 不含 `kirogo-thinking` synthetic marker。
- 本轮没有上游 `THINKING_SIGNATURE_INVALID`、400、429、500。

因此当前 `signature_proto=5` 不是生产旧版本 synthetic signature 400 的直接复现，待继续确认 cctest 是否检查签名可回放、历史签名与 block 位置。

## stream_structure 证据

普通 thinking/tool 流的下游首尾事件完整，且 block index 连续。仍有 `latencyTrace.stopReasonSource=local_inferred_end_turn/local_inferred_tool_use` 与 `upstreamEofWithoutCompleted=true` 的请求；这些请求需要作为采样失败候选单独统计。

## Caddy

本轮前后均已验证：`caddy validate` 成功；`kirotest.okmcode.com/v1/models` 和 `kiro2.okmcode.com/v1/models` 都返回 HTTP 401。公网链路已正常，401 是后端 API key 鉴权结果。

## 结论

不能再把 WebSearch 0 归因于 Caddy 或单纯未开启开关。当前应继续分析 cctest 的额外字段判定和普通工具闭环；在证据前不改 WebSearch 序列。
