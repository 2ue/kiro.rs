# Stream Terminal Errors And Precommit Retry

Status: `focused-router-and-release-2mib-unit-passed / final-cli-http-load-gates-pending`

Severity: P0 release gate

Last updated: 2026-07-16

## 问题、现象与影响

修复前，malformed/不完整 AWS EventStream 可能被合成为 HTTP/SSE success：unknown-only 会产生空 `end_turn + message_stop`，正文后缺 completion 会在可见文本后伪造正常结束，postcommit 断流也可能丢失明确错误。另一类风险是 provider 与 handler 各自重试，导致一次下游请求绕过共享 attempt budget；错误原文还可能进入公开响应、usage 或 DEBUG 日志。

这些缺陷不要求出现 `bashHashxxxxxxxx` 才成立。空成功、假的 `message_stop`、thinking/tool 已提交后重放、usage 错误分类、attempt 放大或私有错误正文泄漏都属于同一发布门禁。

## 当前协议所有权

标准 `HTTP 200 + application/json` exception 现在由 `KiroProvider` 在把 body 交给 EventStream handler 前识别、分类和有限换号：

- 双账号 fixture 使用 `credential_retry_max_attempts=2`，第一次 typed JSON exception 后换另一账号，第二次成功。
- 单账号 fixture 使用 `credential_retry_max_attempts=1`，只发一次并返回 typed 429。
- 这两条路径不进入 handler `JsonStreamErrorSniffer`，因此 handler 的 `streamRetryAttempts`、`streamRetryDispatchFailures` 和 reasons 均应为空。

真正进入 handler precommit retry 的当前矩阵是六类 EventStream 故障：body read error、idle timeout、bad CRC、truncated frame、显式 incomplete status、protocol contamination。另测 `application/vnd.amazon.eventstream` Content-Type 配 JSON bytes；生产可达分类是 decoder `protocol_error`，不是 handler `status_error`。

因此不再把 handler `status_error` 写成当前 Router 可达的生产路径。provider typed retry 和 handler stream retry 必须分别计账，但共享同一个 inference attempt budget。

## 根因与修复

- decoder/feed/EOF 过去只关注是否读到任意 frame，没有要求可信 terminal；现在 success 需要显式 `messageStatus=COMPLETED`、有意义输出后的 metadata、`stop=true` 完整 tool use，或明确受信任的 legacy terminal。
- unknown-only、正文后缺 terminal、partial tool 和 decoder dirty 必须 fail closed；非流式返回规范 502，流式首输出前可在共享预算内换号，已提交后只发 SSE error 且不伪造 `message_stop`。
- provider 在标准 JSON content type 上先完成 typed exception 分类，避免 JSON bytes 被 EventStream decoder 当作普通流继续处理。
- `streamRetryAttempts` 只记录重试阶段新增的真实 send delta；没有可调度账号、重试未发送时单列 `streamRetryDispatchFailures`。
- 公开错误只保留规范 type/status、request/error ID；上游 message、query、result、credential、调度和 pool 细节不得进入 downstream、usage JSON 或 DEBUG log。

## 聚焦复现矩阵

使用真实 Axum Router、reqwest provider、临时 fake upstream 和假 credential，不连接真实上游：

| 分类 | 轮次 | 关键断言 |
| --- | ---: | --- |
| handler precommit 六类 | 30 | 每轮 2 hits；恢复成功；`streamRetryAttempts=1`；固定 reason；无 dispatch failure |
| provider JSON 双/单账号 | 10 | 双账号 2 hits/换号成功；单账号 1 hit/typed 429；handler retry telemetry 全空 |
| EventStream Content-Type + JSON bytes | 5 | 2 hits；按 `protocol_error:sends=1` 恢复 |
| 单账号 handler bad CRC | 5 | 1 hit；SSE error；无 `message_stop`；`dispatchFailures=1`；0 重发 |
| postcommit text/thinking/tool read error | 15 | 1 hit；0 retry；保留已提交 block；SSE error；无伪正常结束 |
| unknown-only stream | 5 | 首输出前 2 hits 后恢复；未知私有 marker 不泄漏 |
| visible text 后缺 completion | 5 | 1 hit；可见正文后 SSE error；无 `message_stop` |
| non-stream unknown/missing | 10 | 502；1 hit；usage Error；不复制原始 marker/text |
| legacy text+metadata / complete tool 正控 | 20 | stream/non-stream 均 success；1 hit；正确 `end_turn/tool_use` |
| 16 MiB non-stream limit/recovery | 20 | Content-Length/chunked over-limit 拒绝并恢复；exact/small 正控成功 |
| JSON exception secret marker | 5 | downstream/usage/DEBUG log 均 0 命中；429 分类一致 |

历史修复前红测：unknown-only 稳定返回空 200 success；正文后缺 completion 稳定合成正常 `message_stop`。上述两项在当前聚焦矩阵均已红转绿。

## 正常能力防回归

流式与非流式的 legacy text+metadata、完整 tool-use 正控用于防止 fail-closed 过严。postcommit 矩阵分别覆盖 text、thinking、tool，确认已经向客户端提交的内容不会触发上游重放。完整协议防回归还需联动 transcript 原子 trim、signed/redacted thinking、20/100 tool cycles、GIF/WebP、WebSearch/MCP usage/privacy 和真实 Claude CLI；不能只凭本文件的 fault fixture 宣称所有协议能力通过。

## 性能与异常流量

handler retry 不能独立创造第二套无界预算。每轮实际上游 hits 必须等于 usage 的 inference sends，并始终不超过共享上限。首输出后固定 0 retry，单账号 dispatch failure 固定 0 新 send；这防止异常响应或不可用账号在系统内部形成高 RPM。

聚焦单元矩阵只证明次数与状态机有界，尚不证明高并发下的 CPU/RSS/FD 和 tail latency。最终 release 必须执行 normal/malformed 混合 burst、断流/idle/JSON exception 连续故障和恢复流量，记录上游放大系数、p95/p99、RSS/FD 与进程存活。

## 当前证据与验收

聚焦结果见 [Handler EventStream 与 runtime stack 矩阵证据](../evidence/handler-eventstream-runtime-matrix-20260716.md)。冻结候选仍须完成：

1. `cargo fmt --check`、`git diff --check`、`cargo check --all-targets` 和相关全量测试。
2. 当前 checkpoint 的 release-only 2 MiB worker 已通过；最终 tag binary 需绑定同一结果，并完成隔离 HTTP fault matrix。
3. Claude Code CLI 2.1.197 的 C1-C4：normal、thinking、tool、MCP、长会话、resume 和错误路径。
4. 至少 1,000 次 release HTTP 请求、三轮 burst、异常后 normal `5/5` 恢复及资源回落。

任一 success 缺可信 terminal/final usage，任一 failure 伪造 `message_stop`，任一已提交请求发生上游重放，任一 usage/hit 超共享预算，或任一私有 marker 出现在公开面，都阻止发布。

## 残余边界

local non-stream contamination 当前没有 response-level 跨账号重试；external SSE 在 Response 已建立后也不能跨 pool 重试。这些路径必须 fail closed 并有明确观测，不能通过伪造 success 补齐。当前结果不能承诺未来未知 upstream event 一定兼容，只能保证已列未知/缺终止模型会明确失败或在首输出前有界恢复。
