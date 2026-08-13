# WebSearch/MCP 协议、usage、取消与隐私聚焦证据

Date: 2026-07-16

Status: `focused-pass / native-cli-gate-open / auxiliary-attribution-open`

对应问题：[WebSearch/MCP 协议、错误、usage、attempt 与隐私边界](../issues/websearch-mcp-protocol-usage-and-privacy.md)

## 结论

当前共享 dirty tree 上，WebSearch/MCP 的 focused correctness、fail-closed、stream/non-stream、零结果、资源上限、usage、客户端取消、隐私与独立 MCP attempt channel 已通过重复验证：

- 最后一个 user turn 无 text 时不再回退复用旧 query；tool-result-only 与空白 turn 共 `10/10` 本地 400、`0` MCP hit、`0` inference attempt。
- canonical server WebSearch、普通同名 client tool、混合 tools 分流各 5 轮；只有 canonical pure server tool 命中 MCP。
- 20/100 tool cycle 各 5 轮，均捕获当前 query，不复用首条历史 query。
- stream/non-stream 合法零结果共 `10/10` 成功；错误不再伪装成 `No results found`。
- MCP 13 类错误各 5 轮，加正常恢复 5 轮，共 `70/70`；全部为规范错误或真实恢复，错误场景无成功 terminal。
- 完整 stream、未 poll 后 drop、首 chunk 后 drop 各 5 轮；等待 MCP headers 时取消 5 轮、读取 body 时取消 5 轮；usage 所有权均保留且每轮只有 1 个 MCP hit。
- request key 只记录 64 位单向 channel ID；raw query、raw result、private upstream marker 在公开 body、usage 与 DEBUG capture 中均为 0 命中。
- 1/20/60 credential 各 5 轮，MCP 实际 send 始终受共享 4-attempt 上限约束；`localAttempts=0`、`externalAttempts=0`、`mcpAttempts=consumed`。
- 成功响应的 summary 从“clone 结果并生成两次”收敛为一次生成；stream 分块不再先构造整段 `Vec<char>`，协议与 usage 回归保持一致。
- 两套 UI 类型合同、两套 production build 和 Rust all-targets check 均通过。

这不是最终发布证明。Claude Code CLI `2.1.197` 的 native WebSearch 能力尚未在隔离候选服务上完成 5-session C/D gate；MCP fixture 不能冒充 native WebSearch。Profile discovery、token refresh、model catalog 的生产 request-scoped attribution 也仍是独立任务。

## 测试边界

- 基线 commit：`401473ca1649997bdeccf4468e3add1bdb187248`。
- 工作区：多代理共享 dirty tree；本证据不能绑定最终候选 SHA 或 release binary SHA-256。
- Claude Code CLI：`2.1.197`，本轮只记录版本，没有把 MCP fake fixture 写成 native CLI pass。
- 上游：进程内 fake MCP/fake inference，随机监听端口，80 个假 credential。
- 未访问、重启或修改 `127.0.0.1:9022`。
- 未读取或修改任何 `kiro_idc_users*.txt`。
- 无真实 token、cookie、Authorization、账号或生产服务调用。

## 修复前红项

### 最后一个 user turn 回退旧 query

红测：

```bash
cargo test --bin kiro-rs \
  latest_user_turn_without_text_never_reuses_a_stale_query_for_five_rounds \
  -- --nocapture --test-threads=1
```

修复前稳定失败，代表性差异：

```text
left: Some("stale-query-1")
right: None
```

根因是 extractor 遍历全部历史并寻找“最后一个有 text 的 user message”。当当前 user turn 只有 `tool_result` 或空白 text 时，它会越过当前 turn，复用更早查询。修复后先锁定最后一个 user message，只在该 turn 内解析 string/text block。

### MCP body 处理中取消丢 usage

红测：

```bash
cargo test --bin kiro-rs \
  websearch_client_cancel_during_mcp_body_keeps_usage_ownership_for_five_rounds \
  -- --nocapture --test-threads=1
```

修复前第一轮稳定失败：

```text
cancelling during MCP body validation lost the usage record
left: 0
right: 1
```

当 fake MCP 已收到请求、200 headers 已返回、slow body 尚未验证完成时 abort Router future，provider lease 会释放，但 handler 尚未进入 success/failure 分支，导致 request key、真实 send、credential attempt 与 `ClientDropped` usage 全丢。

修复采用 request-scoped attribution sink：真实 reserve 后立即写 pending send；headers/body/error/completion 固化 attempt；pre-response RAII guard 在 drop 时把 pending 固化为 `fail/client_dropped/mcp_client_cancelled` 并写 `ClientDropped` usage。

## Focused 矩阵

### 解析与 query 归属

命令：

```bash
cargo test --bin kiro-rs anthropic::websearch::tests:: \
  -- --nocapture --test-threads=1
```

结果：`18/18` tests，`0.01s` test time，`1.2s` wall time。

单次 summary/增量字符分块优化后再次运行：`18/18`，`0.10s` test time；wall `98.9s` 中约 `96s` 为共享树编译。

覆盖：

- current last-user query 与旧首条 query；
- 最后 user turn 为 tool-result-only/空白；
- string/array 与固定搜索前缀；
- 20/100 tool cycle；
- canonical server tool 与同名 client tool；
- JSON-RPC version/request ID、malformed、RPC error、`isError`、非 text；
- stream/non-stream success shape 与公开错误脱敏。

### Handler C07 矩阵

命令：

```bash
cargo test --bin kiro-rs anthropic::handlers::tests::websearch_ \
  -- --nocapture --test-threads=1
```

结果：`8/8` tests；测试体 `12.81s`，wall `60.9s`，其中约 `46.72s` 为共享树编译。

单次 summary/增量字符分块优化后再次运行：`8/8`，测试体 `12.65s`，wall `22.1s`。

header/body cancel 扩展后的最终运行：`8/8`，测试体 `12.06s`，wall `13.5s`。

| 分组 | 场景 | 结果 |
| --- | --- | ---: |
| 路由 | canonical、同名 custom、mixed tools | `15/15` |
| 长历史 | 20/100 tool cycle current query | `10/10` |
| 当前 turn 无 query | tool-result-only、blank text | `10/10`，400、0 MCP |
| 合法零结果 | stream、non-stream | `10/10` success |
| stream 所有权 | full、never-polled drop、partial drop | `15/15` |
| MCP 请求处理中取消 | headers 前等待、headers 后读 body | `10/10` ClientDropped、每轮 1 hit |
| non-stream | JSON message、stop、usage、stable key | `5/5` |
| privacy success | stream/non-stream 交替，query/result marker | `5/5`，日志/usage 0 命中 |
| MCP 错误 | 13 类 x 5 | `65/65` fail closed |
| 错误后恢复 | normal success | `5/5` |

13 类错误为：HTTP 400、429、500、header timeout、body timeout、disconnect、malformed JSON、JSON-RPC error、`isError=true`、non-text content、mismatched JSON-RPC ID、Content-Length over-limit、chunked over-limit。

逐请求断言包括：

- error response 为 JSON、公开 status/type 符合分类、含 request/error ID；
- raw query 和 private upstream body marker 不进入公开 response；
- 每个请求恰好一条 usage；success/error/client-drop 状态与下游行为一致；
- stable request-key channel ID 为 64 位十六进制 digest，不等于原始 key；
- credential/attempt 存在且 attempt taxonomy 为固定小写 marker；
- `consumed == credentialAttempts.len()`，且不超过共享硬预算；
- `localAttempts=0`、`externalAttempts=0`、`mcpAttempts=consumed`；
- 错误发生在首输出前时 `downstreamCommitted=false`，不会生成假 success terminal。

### 取消与 attribution sink

命令与结果：

```text
cargo test --bin kiro-rs websearch_client_cancel_during_mcp_body_keeps_usage_ownership_for_five_rounds -- --nocapture --test-threads=1
初次 body-only：PASS 1/1；内部 5/5；test 0.71s；wall 76.2s，其中约 74s 为 build lock/编译
扩展 header/body 后：PASS 1/1；内部 10/10；test 0.56s；wall 66.4s，其中约 64s 为编译

cargo test --bin kiro-rs mcp_attribution_sink -- --nocapture --test-threads=1
PASS 1/1；内部 5/5；test <0.01s；wall 1.3s
```

每轮取消断言：fake MCP hit 只增加 1；`consumed=1`、`local=0`、`external=0`、`mcp=1`、credential 非空、恰好一个 attempt，且 `action=fail`、`error_type=client_dropped`、`error_message=mcp_client_cancelled`。

### 共享预算与 provider 资源

命令：

```bash
cargo test --bin kiro-rs anthropic::inference_attempt_budget::tests:: \
  -- --nocapture --test-threads=1
cargo test --bin kiro-rs kiro::provider::tests::mcp_ \
  -- --nocapture --test-threads=1
```

结果：

- budget `13/13`，包括三通道并发守恒、reserve 后取消不退款、commit 后拒绝、max=1/zero clamp、独立 MCP counter；test `0.02s`。
- provider MCP `7/7`；包括成功/失败/drop lease、200 headers 不提前 success、错误 body Content-Length/chunked 限制、本地 acquire failure 不跑满 retry、1/20/60 credential 各 5 轮共享硬预算；test `25.81s`。

共享树 checkpoint 恢复后的 provider 最终复跑仍为 `7/7`，test `26.21s`、wall `137.6s`；约 `110s` 为并行改动后的重新编译。该次编译另报告 external-pool 两个 dead-code warning 与 endpoint 一个 dead-code warning，均不在 C07 所有权范围；因此它证明 MCP tests 通过，但全树最终 zero-warning gate 仍应在对应所有者清理后重跑。

provider 矩阵明确验证：账号数从 1 增长到 60 不会让单请求发送数突破 4，且：

```text
consumed = localAttempts + externalAttempts + mcpAttempts
localAttempts = 0
externalAttempts = 0
mcpAttempts = actual MCP sends
```

### UI 与静态 gate

```text
node feature/tests/mcp-attempt-channel-contract.mjs
PASS: both UI contracts expose the explicit MCP attempt channel

(cd ui && npm run build)
PASS；Vite build 7.60s；存在既有 >500 kB chunk warning

(cd admin-ui && npm run build)
PASS；Vite build 7.08s

CARGO_TARGET_DIR=/tmp/kiro-rs-protocol-matrix-target cargo check --all-targets
PASS；111.6s clean compile；0 warning
```

默认 target 上的首次 `cargo check --all-targets` 因共享 build lock 等待 120 秒后超时，期间没有开始编译。独立 `CARGO_TARGET_DIR` 的 clean check 用于区分锁竞争与代码失败。

## 性能与放大边界

新增 sink 是 request-scoped 内存状态，不访问 Redis/PG，不发额外 HTTP 请求。每次更新克隆 bounded attempt vector；当前共享上限最多 4 项，因此时间与内存为小常数。客户端取消路径只在 RAII drop 时进行一次 bounded snapshot 和 usage 写入。

成功 renderer 原来为了先估算 usage 而 clone 全部 `WebSearchResults` 并生成 summary，随后 stream/non-stream renderer 再生成同一 summary；stream 还把 summary 全量收集为 `Vec<char>` 后才切块。当前实现先持有 `Option<WebSearchResults>`，只生成一次 summary，用同一字符串计算 output tokens 和渲染；分块通过 `chars().take(100)` 增量构造 bounded chunk。该优化减少一次结果 clone、一次 summary 遍历和一份整段字符数组，不改变查询、结果 block、chunk 字符边界或 usage。

本轮证明了请求数/账号数不会引起 MCP send 线性放大，但不是 release load/soak 证明。仍需在冻结候选上记录 burst/错误 burst 的 MCP hits、p50/p95/p99、RSS、FD、恢复时间，并与 request admission 和 auxiliary call 计数联合验收。

## 未关闭项

1. Claude Code CLI `2.1.197` native WebSearch：需要隔离 HOME/CLAUDE_CONFIG_DIR、随机临时服务端口、5 个独立 session，分别验证 stream、non-stream、错误、恢复、usage 和 terminal。若 CLI/模型不暴露该能力，应记录 capability unavailable，不能用 MCP fixture 替代。
2. Profile discovery、token refresh、model catalog：当前 shared inference budget 与 usage 已有显式 MCP channel，但这些 auxiliary calls 的生产 request-scoped attribution 尚未完整接入。应另设 channel/snapshot 与 admission，不得混入 `localAttempts` 或冒充 MCP 已覆盖。
3. 最终候选：当前是共享 dirty tree，没有 release binary SHA、Docker/HTTP/CLI 证据，也没有长时间 soak；不能据此发版。
4. UI 主站已有单个压缩前 chunk 超过 500 kB 的 Vite warning。本次类型字段未引入可见 chunk 增量结论，但最终性能专题仍应处理或接受基线。

## 发布门禁

- 冻结 candidate SHA 后重跑本文件所有命令，并保存脱敏 JSONL/stats/usage 摘要和 binary SHA-256。
- 完成 native CLI D04/D06 或记录可重复的 capability-unavailable 证据。
- 完成 auxiliary attribution/admission 专项，证明错误 burst 下 profile/refresh/catalog 不形成内部 RPM 放大。
- 完成 release HTTP/CLI/load、RSS/FD、错误恢复、两套 UI browser gate。
- `cargo fmt --all -- --check`、`git diff --check`、all-targets、全量测试、release build 和敏感信息扫描全部通过后，才可关闭本专题的发布阻断状态。
