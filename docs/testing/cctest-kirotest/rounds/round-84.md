# Round 84：thinkingOutputPolicy 非触发边界

## 运行信息

- 时间：2026-11-08（北京时间 05:15–05:19；事件时间 UTC 21:15–21:18）
- 代码：工作树 `2e625e52` 加未提交改动；59139 由 debug 开发循环自动编译并运行
- 模型：`claude-opus-5-5`
- 目标：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`
- 59138：只读，未重启、未改配置、未写 PostgreSQL/Redis、未改账号状态

## 实验变量

只修改 59139 开发运行配置：

```text
promptInjection.pathOverrides["/dfcache/tune-ss6-99"].thinkingOutputPolicy.enabled
false → true
```

其它提示词注入项、`routeFeatures`、账号池、代理绑定和请求 API Key 保持不变。完整配置在实验前保存到 `/tmp/kirotest-dev/runtime-round84-thinking-policy-before.json`，结束后使用 `PUT /api/admin/config/runtime` 恢复，恢复后再次读取确认值为 `false`。

## 结果

结果文件：[`20261107T211537-round-84-thinking-policy-on.json`](../results/20261107T211537-round-84-thinking-policy-on.json)

| 检测项 | Round 83（关闭） | Round 84（开启） |
|---|---:|---:|
| tag_check | 10 | 10 |
| stream_structure | 5 | 5 |
| non_stream | 10 | 10 |
| websearch | 0 | 0 |
| signature_proto | 5 | 5 |
| output_config | 10 | 10 |
| server_tool | 10 | 10 |
| knowledge | 10 | 10 |
| model_attribution | 10 | 10 |
| doc_recognition | 10 | 10 |
| image_recognition | 10 | 10 |
| **总分** | **82/110** | **82/110** |

实验抓包：`logs/llm-capture/capture-20261107T211537Z-d5880da8/`，保留 `events.jsonl`、`capture.zip`、`manifest.json`。

- 9,860 个事件，JSON 行错误 0，丢弃 0；
- 22/22 客户端流均有完整 `message_start → content_block* → message_delta → message_stop`；
- 上游 27 次响应，其中 25 次 HTTP 200、2 次 HTTP 402 后换号成功；
- `content_block_start/stop = 54/54`；
- MCP 响应 1 次；
- Sse 中 33 个 `thinking_delta` 的文本字符数为 0，11 个 `signature_delta` 均非空；
- 代理自定义 identity、prompt steering、WebSearch guidance、language reminder、tool choice、thinking policy marker 均为 0；仅看到 Claude Code 自带 `<system-reminder>` 与 `<thinking_mode>`。

## EOF 与终止信号

本轮 22 个客户端请求中，21 个有上游流结束记录；这 21 个请求的上游均没有显式 `messageStatus=COMPLETED`，但都由下游正常形成 `message_delta` 和 `message_stop`。请求级分布为：8 个 `local_inferred_end_turn`、12 个 `local_inferred_tool_use`、1 个 `local_explicit_stop_reason`；另有 1 个非流请求和 1 个流请求的摘要没有该字段。完整脱敏矩阵见 [`analysis/round-84-eof-shape.json`](../analysis/round-84-eof-shape.json)。

这仍是 `stream_structure=5` 的高价值候选，但 Round 84 与 Round 83 的评分完全相同，不能仅凭“EOF 无 COMPLETED”修改终止状态机。当前代码已经把本地推断终止记录为 `terminalReason=completed`，并正确闭合公开 SSE；下一步需要拿到 CCTest 失败请求 ID 或复现 429/500 后的异常流，才能证明评分器把该差异判为失败。

恢复后的 Admin preview 再次确认：`taskQuality`、`languageConstraint`、`languageTurnReminder`、`custom`、`identityPrompt`、`identityTurnReminder`、`chunkedWritePolicy`、`chunkedWriteToolSuffix`、`thinkingOutputPolicy`、`toolChoicePrefix`、`toolChoiceTurnReminder`、`webSearchGuidance`、`emptyTurnNudge` 均为 `blocked`；`thinkingControls` 为 `not_triggered`。`routeFeatures` 仍为 `thinkingTrigger=off`、`channelLeakGuard=false`、`modelDrivenWebSearch=true`。

## 结论

本轮不是有效的“提示词已注入”A/B：恢复后的 Admin preview 显示 `thinkingOutputPolicy=blocked`，实验前开启时也仅为 `not_triggered`；代码目录说明该项对 `-thinking` 模型和强制可见 thinking 生效，而 Opus 5 系列自动跳过。因此本轮只能证明“在 `claude-opus-5-5`、`thinkingTrigger=off` 的 tune 路由上，打开配置没有实际注入文本”，不能证明该提示词在其它模型或触发条件下不会影响评分。

只读 preview 草稿进一步验证触发矩阵：将草稿中的 `thinkingTrigger` 改为 `real_request` 并启用策略后，`claude-sonnet-4-5-thinking`、`claude-sonnet-5-5-thinking`、`claude-opus-4-8-thinking` 均为 `applied`，而 `claude-opus-5-5` 仍为 `not_triggered`。因此当前 CCTest 固定模型无法用该开关做有效得分 A/B。

本轮完整 CCTest 仍为 `82/110`，没有改善 `stream_structure`、`websearch` 或 `signature_proto`；配置已恢复关闭。后续若验证该策略，必须换用满足触发条件的 thinking 模型或使用 Admin preview 草稿确认 `applied` 后再跑评分，不能把本轮当作有效因果排除。

这轮只证明“开启该项没有得分收益”，不能证明所有提示词都与评分无关。提示词仍可能改变 thinking、工具调用、WebSearch 查询、结构化输出和非流 block 形状；后续若继续实验，必须一次只改一个开关，并保留上游 marker 与请求级 SSE。

## 证据

完整字段矩阵见 [`analysis/round-84-thinking-policy-ab.json`](../analysis/round-84-thinking-policy-ab.json)。清理规则见 [`analysis/evidence-retention.md`](../analysis/evidence-retention.md)。

## 回归检查

使用 `feature/tests/run-cargo-scoped.sh` 执行协议回归：

- `cargo fmt --check`：通过；
- WebSearch 64 个测试并发运行：63 个通过，1 个日志隐私矩阵测试因并发日志观察未看到 `query_bytes` 失败；
- 将该测试单独串行重跑：通过（1/1）；
- `payload_guard_runtime` 与 `stream` 测试未因本轮配置实验新增失败；
- 59138、59139 `/healthz` 均为 200，开发循环仍在运行。

随后按串行方式执行 `cargo test -q websearch --bin kiro-rs -- --test-threads=1`，64/64 通过。并发运行的同一隐私日志测试偶发未观察到 `query_bytes`，串行重跑稳定通过，未修改源码。

CCTest 任务接口复查只返回汇总分数和 usage，没有失败请求 ID 或事件级判定；因此本轮没有新增可直接修改状态机、WebSearch 或签名的证据。

该失败是测试日志观察的并发竞态，未修改源码掩盖；后续回归按串行方式复核该用例。
