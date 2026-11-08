# Round 83：重试状态机证据复核

时间：2026-11-08（北京时间 04:58–05:02；事件时间 UTC 20:58–21:01）
代码：工作树 `2e625e52` 加未提交改动；59139 由 debug 开发循环自动编译并运行。
模型：`claude-opus-5-5`
服务器：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`

## 结果

结果文件：[`20261107T205907-round-83-retry-evidence.json`](../results/20261107T205907-round-83-retry-evidence.json)

| 检测项 | 分数 |
|---|---:|
| tag_check | 10 |
| stream_structure | 5 |
| non_stream | 10 |
| websearch | 0 |
| signature_proto | 5 |
| output_config | 10 |
| server_tool | 10 |
| knowledge | 10 |
| model_attribution | 10 |
| doc_recognition | 10 |
| image_recognition | 10 |
| token usage audit | 通过 |
| **总分** | **82/110** |

与 Round 82 相同。没有新增代码改动，也没有把重试假设直接写入状态机。

## 抓包

原始抓包目录：

```text
logs/llm-capture/capture-20261107T205818Z-f291be2b/
```

保留文件：`events.jsonl`、`capture.zip`、`manifest.json`。本轮未删除任何原始文件。

统计：

- 8,427 个事件；8,193,168 字节；JSON 错误 0；丢弃 0；
- 23 个客户端请求，其中 22 个流式、1 个非流式；
- 上游 27 次响应：25 次 200、2 次 402；
- 22/22 客户端流拥有完整 `message_start → message_delta → message_stop`；
- `content_block_start/stop` 为 50/50；
- WebSearch 有 1 次 MCP 响应；
- 21 个 `upstream_stream_end`，2 个上游错误正文用于 402 诊断。

## 重试状态机证据

本轮出现两条额度失败后的换号链：

- `req_01CAT9SnBeAzX1jjUPnPnaxJ`：`2471` 返回 402，随后 `2473` 返回 200；摘要为 `success/local_success`，终态 `completed`；
- `req_01owu44S5i3JS8jzuCf4vP5J`：`2467` 返回 402，随后 `2468` 返回 200；摘要为 `success/local_success`，终态 `completed`。

两个请求都满足：

- 首输出前切换账号；
- 下游只出现一套完整 SSE 生命周期；
- 没有 `stream_error`、`local_error_no_fallback` 或遗留错误详情；
- `upstreamEofWithoutCompleted=true` 时，本地终止来源仍是 `local_inferred_end_turn` 或 `local_inferred_tool_use`，不是 retry 错误。

其余请求全部为单次 200，唯一非流请求也没有错误。当前抓包没有复现“429/500 后成功 retry 但最终仍是 `stream_error`”的旧生产摘要，因此没有足够证据修改 `retry_stream_before_downstream_commit`、`finish_stream_error` 或 `record_stream_error`。

## 提示词注入复核

本轮 marker 扫描只发现：

- 客户端历史中的 `<system-reminder>`；
- Claude Code reasoning 请求中的 `<thinking_mode>`。

没有发现 tune 路由额外注入的 identity、prompt steering、WebSearch guidance、language reminder、tool choice 或 thinking policy marker。该结果与 [`analysis/prompt-injection-audit.md`](../analysis/prompt-injection-audit.md) 一致。

提示词仍会影响模型行为和评分样本的请求形状，但本轮没有证据表明代理自定义注入导致三个失败项。客户端自带 system/history 不清洗。

## Hidden thinking 形状

Round 83 的 11 个含 thinking 请求都包含 `thinking_delta` 和 1 个 `signature_delta`，但经过当前输出策略后 `thinking_delta.thinking` 字符数全部为 0；`estimated_tokens` 仍是数值或缺失的混合形状。脱敏矩阵见 [`analysis/round-83-thinking-shape.json`](../analysis/round-83-thinking-shape.json)。

这与 Claude Code 的隐藏 thinking 显示策略一致，不能单凭“文本为空”判定为协议错误。此前同类 `estimated_tokens` A/B 没有改变分数；本轮先记录为待验证抽样差异，不直接把隐藏内容改成可见文本。

## 资源与边界

- 59139 `/healthz`：200；
- 59138 `/healthz`：200；
- 59138 未停止、未重启、未写配置、未写 PostgreSQL/Redis、未改账号状态；
- 59139 仅使用开发数据库和开发账号池；
- 本轮没有 release 冻结编译，继续使用 debug 增量开发循环。

## 结论

Round 83 证明当前开发实例的额度重试链在 402 场景能够清理旧尝试并形成成功终态，但没有覆盖旧生产抓包中的 429/500 流协议错误。`stream_structure=5`、`websearch=0`、`signature_proto=5` 仍未解决，最高分仍为 `82/110`，稳定区间仍为 `73–82/110`。原始抓包继续保留，下一步等待可复现的 429/500 失败样本或 CCTest 失败请求标识后再修改状态机。

CCTest 官方 FAQ 的本地存档还明确把 `aws-bedrock` 解释为非 Claude 官方原版渠道，并提到 Kiro 等逆向渠道。Round 83 的 `streamChannel=aws-bedrock`、`verdictKey=unofficial` 与该解释一致；这使 `signature_proto=5` 更像外部渠道归因限制，而不是可通过重编码 Kiro opaque signature 修复的公开协议错误。
