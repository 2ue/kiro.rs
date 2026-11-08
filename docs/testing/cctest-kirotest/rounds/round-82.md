# Round 82：Kiro 原生边界清理后的完整复测

时间：2026-11-08（北京时间 04:41–04:44；事件时间 UTC 20:41–20:44）
代码：工作树 `2e625e52` 基线加未提交改动；59139 由 debug 开发循环自动编译并重启。
模型：`claude-opus-5-5`  服务器：`https://kirotest.okmcode.com/dfcache/tune-ss6-99`

## 结果

结果文件：[`20261107T204117-round-82-synthetic-boundary.json`](../results/20261107T204117-round-82-synthetic-boundary.json)

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

修复没有改变 CCTest 分数。`stream_structure=5`、`websearch=0`、`signature_proto=5` 仍稳定复现；保护项没有退化。

## 本轮代码变化

`src/anthropic/payload_guard_runtime.rs` 在所有 Kiro request 序列化前扫描历史 assistant 的 native reasoning：

- 已识别的代理 synthetic signature 被移除；
- 真实 Kiro opaque signature 原样保留；
- redacted reasoning 原样保留；
- 增加边界单测，验证 synthetic 被清除、真实签名仍发送。

这修复的是历史 synthetic signature 可能进入 Kiro `reasoningContent` 的明确协议缺陷，不伪造签名，也不改变当前对外 Anthropic SSE 的 synthetic 兼容输出。

## 抓包证据

原始抓包目录：

```text
logs/llm-capture/capture-20261107T204110Z-fdbcf638/events.jsonl
logs/llm-capture/capture-20261107T204110Z-fdbcf638/capture.zip
logs/llm-capture/capture-20261107T204110Z-fdbcf638/manifest.json
```

统计：

- 10,321 个事件；9,000,046 字节；JSON 错误 0；丢弃 0；
- 23 个客户端请求，22 个流式请求，1 个非流式请求；
- 26 个上游响应：25 次 200、1 次 402；402 在首输出前切换到 2464 后成功；
- 22/22 客户端流都有 `message_start`、`message_delta`、`message_stop`；
- `content_block_start/stop` 为 53/53；
- WebSearch 产生 MCP 200、结果回灌和最终响应；
- 抓包内没有 `kirogo-thinking-signature-block`、`kiro-rs-compat-thinking-signature` 或 `THINKING_SIGNATURE_INVALID` marker；
- 客户端 `<system-reminder>` 与 `<thinking_mode>` 仍按原样保留，代理自定义提示 marker 未出现。

这证明边界清理没有引入公开 SSE 回归，也没有伪造上游签名。它没有解决 CCTest 私有的签名抽样判定。

## 账号处理

本轮唯一明确额度失败为凭据 `2463` 的上游 `402 ServiceQuotaExceededException`，正文为 `MONTHLY_REQUEST_COUNT`。已只在 59139 开发数据库禁用 `2463`，没有修改 59138。高余额账号 `2462`、`2464` 继续保留用于后续抓包；生产账号池未触碰。

## 验证

通过：

```text
feature/tests/run-cargo-scoped.sh cctest-protocol-regression -- \
  bash -lc 'cargo fmt --check && cargo test -q websearch --bin kiro-rs && cargo test -q payload_guard_runtime --bin kiro-rs'
64 websearch tests passed
4 payload_guard_runtime tests passed
```

59138 与 59139 `/healthz` 均为 200。59139 仍由开发循环管理；没有 release 冻结编译，没有重启或写入 59138。

## 结论

本轮固定了“synthetic 签名不跨越 Kiro 原生边界”的实现改进和证据，但没有评分提升。下一步不能再盲目改签名字节、WebSearch opaque 字段或上游 `COMPLETED`；需要新的 CCTest 失败样本、官方 fixture 或可归因的评分共变证据。
