# Round 68：当前运行时配置复测与 WebSearch 回灌取证

## 时间与运行边界

- 时间：2026-11-08（北京时间；抓包时间 2026-11-07 17:54–17:56 UTC）
- 59139：`/root/.cache/kirotest-dev/kiro-rs-dev`，debug 热更新实例
- 59138：只读健康探针，未停止、未重启、未写配置、未写 PostgreSQL/Redis、未修改账号
- 59138/59139 `/healthz`：均为 HTTP 200
- 代码：工作树 `2e625e52`，保留既有未提交改动；本轮未改 Rust 源码
- 运行时路由：`/dfcache/tune-ss6-99`
- Admin 预览：`modelDrivenWebSearch=true`；路由显式关闭 identity、language、tool choice、WebSearch guidance、empty-turn 等注入；`thinkingControls` 为协议兼容前置项

## CCTest 结果

结果文件：[`20261107T175425-round-68-live-config.json`](../results/20261107T175425-round-68-live-config.json)

总分：**73/110**。

| 项目 | 分数 |
|---|---:|
| tag_check | 10 |
| stream_structure | 5 |
| non_stream | 10 |
| websearch | 0 |
| signature_proto | 0 |
| output_config | 10 |
| server_tool | 10 |
| knowledge | 10 |
| model_attribution | 5 |
| doc_recognition | 10 |
| image_recognition | 10 |

本轮没有把 `73` 当作代码回归结论：`signature_proto=0`、`model_attribution=5` 与此前轮次的抽样结果不同，说明这两项受评分采样和模型行为影响。当前最高分仍是 82/110。

## 抓包统计

原始文件（长期保留）：

- [`events.jsonl`](../../../logs/llm-capture/capture-20261107T175416Z-e3353afd/events.jsonl)
- [`capture.zip`](../../../logs/llm-capture/capture-20261107T175416Z-e3353afd/capture.zip)
- [`manifest.json`](../../../logs/llm-capture/capture-20261107T175416Z-e3353afd/manifest.json)

本轮 6159 行、JSON 错误 0、丢弃 0、7,089,674 字节；22 个客户端请求、25 个上游请求，25/25 上游 HTTP 200。客户端 22/22 具备 `message_start → content_block* → message_delta → message_stop` 生命周期，35/35 content block 成对。

## WebSearch 请求级证据

关键请求：`req_01dtfK6GnMYfkrbLsh1wQQhw`。

抓包显示完整闭环：

1. 上游 Kiro 首轮返回 `assistantResponseEvent` 和 `toolUseEvent(name=web_search)`；
2. 下游发送 `server_tool_use`，`id=srvtoolu_01p91AwoRHDa2LByDnZz92mX`，`caller={"type":"direct"}`；
3. 下游发送 `input_json_delta`，完整 query 为 `AI news 2026-11-07`；
4. MCP `tools/call` HTTP 200，返回 10 个结果；
5. 下游 `web_search_tool_result.tool_use_id` 与 `server_tool_use.id` 完全相同；
6. 结果后模型继续调用 Kiro，当前消息正文为 `Tool result received.`，历史 tool result 通过内部 `tool_use_id` 回灌；
7. 下游发送 10 条 `web_search_result_location` citation，随后继续发送文本；
8. 最终 usage 含 `server_tool_use.web_search_requests=1`，`stop_reason=end_turn`，随后 `message_stop`。

因此，`websearch=0` 不能由缺少 `tool_use_id`、缺少 MCP 回灌、缺少 citation、缺少最终回答或 SSE 终止事件解释。

当前仍存在的高价值差异是字段语义：

- `web_search_result.encrypted_content` 直接使用 MCP snippet 明文；
- `web_search_result_location.encrypted_index` 使用本代理的可逆 Base64(`序号:URL`)；
- 本轮没有取得官方生成的 opaque fixture，也没有评分器失败样本 ID。

本轮不伪造加密字段，不把 Base64 外形当成官方语义；下一步只有在拿到可验证 fixture 或设计明确的单变量 A/B 后才改代码。

## 提示词注入结论

本轮 Admin preview 与 Round 63 一致：tune 路由的可配置注入基本关闭，客户端自带 `<system-reminder>`、`<thinking_mode>` 仍会随历史进入上游。已有单变量证据仍有效：

- `identityTurnReminder` 会让非流响应增加 thinking block，并使 `non_stream` 从 10 降到 0；
- `thinkingControls` 关闭会在本地 preflight 直接返回 400，不能当作普通提示删除；
- 客户端 system/history 不能直接清洗；
- 当前没有证据表明 tune 路由的剩余注入是 `stream_structure`、`websearch` 或 `signature_proto` 的直接根因。

## 证据保留

Round 68 的 JSONL、ZIP、manifest、结果 JSON 和运行时预览均保留。关键失败项尚未固定根因，不清理任何原始抓包；后续只在确认重复展开副本不再需要后清理副本，并至少保留一份可恢复归档。
