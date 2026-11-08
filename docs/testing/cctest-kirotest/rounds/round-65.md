# Round 65：稳定复测抓包

## 结果

- 时间：2026-11-08 01:20–01:23（北京时间；抓包 UTC 17:20–17:23）
- 路由：`/dfcache/tune-ss6-99`
- 模型：`claude-opus-5-5`
- 代码：无源码改动，59139 debug 热更新实例
- 结果：`82/110`
- 失败项：`stream_structure=5`、`websearch=0`、`signature_proto=5`
- `output_config=10`，`server_tool=10`，保护项全部通过
- 结果文件：`results/20261107T172022-round-65-capture.json`

## 抓包

- 原始事件：`logs/llm-capture/capture-20261107T172014Z-3378f334/events.jsonl`
- manifest：同目录 `manifest.json`
- 原始归档：`/tmp/kirotest-dev/artifacts/round-65-capture.zip`
- 事件行：9000；大小：8,443,061 字节；JSON 错误：0；丢弃：0
- 上游请求：27；HTTP `200=25`、`402=2`
- 客户端请求：22；完整客户端流：22/22
- `message_start/message_delta/message_stop`：22/22/22
- `content_block_start/content_block_stop`：51/51

## 请求级结论

- 23 个请求摘要中 21 个记录 `upstreamEofWithoutCompleted=true`；下游仍闭合为完整 Anthropic 流。
- WebSearch 请求的 `server_tool_use → web_search_tool_result → text → end_turn → message_stop` 结构完整，`caller=direct` 和 usage 均存在。
- 上游正文 marker 仅见客户端 `<system-reminder>` 18 次、`<thinking_mode>` 17 次；没有代理 identity、WebSearch guidance、tool-choice 或语言注入。
- 未发现 synthetic thinking signature 进入本轮上游请求；该轮没有新增签名 400。

## 结论

评分没有因账号 2462 主选或完整公开事件结构而改善。原始抓包及归档保留，继续用于 EOF、WebSearch opaque 字段和抽样请求关联。
