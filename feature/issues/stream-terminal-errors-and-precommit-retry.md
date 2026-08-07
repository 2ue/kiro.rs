# Stream Terminal Errors And Precommit Retry

Status: `focused-router-and-release-2mib-unit-passed / external-pool-pre-output-stream-retry-implemented / focused-fake-http-and-routing-validated / normal-routing-rerun-passed-20260807 / frozen-cli-load-passed-20260807 / production-rollout-observation-pending / release-candidate-v0.0.134`

Severity: P0 release gate

Last updated: 2026-08-07

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

## 2026-08-06 外部池流式首语义输出前错误子问题

本文件原先主要覆盖本地 Kiro/AWS EventStream handler 的 precommit retry 和 postcommit fail-closed。2026-08-06 新增的现网问题来自外部池 `event_passthrough` / SSE 路径：

- 页面样本：`req_01KaWrDY5oZkY13XQqdJB9PH`，入口 `/cc/v1/messages`，路由 `外部直连 · external_pool · external_direct_policy`，外部账号 `#18 yuenan-1`，状态 `流错误`，错误阶段 `external_account_stream`，内部错误信息 `external upstream emitted an error event`，客户端状态码 `200`。
- 真实采样：`yuenan` stream `12` 次中 `5` 次为空回/协议前置流错误，`7` 次正常；`yuenan-1` stream `12` 次中 `2` 次为空回/协议前置流错误，`10` 次正常。两个池 non-stream 各 `5/5` 成功。
- 可恢复指纹：`HTTP 200 -> message_start -> error`，且没有 `content_block_start`、`content_block_delta`、thinking、tool_use 或 `input_json_delta`。

修复前代码不会切池的原因是外部池流式 `HTTP 2xx` 路径在拿到 response header 后立刻构造并返回 `Response`；后续 SSE body 里的 error/read/idle 由 body stream wrapper 记录为 `stream_error`，此时外部池 failover 主循环已经结束，不能再重新选择池。

正确修复不是“所有流错误都重试”，而是新增外部池流式预读缓冲：

1. 在向下游发送任何旧尝试事件前，预读上游 SSE。
2. 只缓冲 `message_start`、`ping`、usage preview 等 protocol-only 事件。
3. 若首个有效语义输出前遇到 error event、idle timeout、read error 或 EOF without terminal，则丢弃缓冲、释放租约、排除当前池，并按外部池同池/跨池预算换池。
4. 一旦读到有效语义输出，就把缓冲和当前语义事件作为 response body 前缀提交；之后任何错误都不得重放完整请求。
5. 外部直连仍只在外部池内部恢复，不能 fallback 到本地账号。
6. 被丢弃的失败尝试只能进入调度 attempt 诊断，不能混入最终成功 usage；最终成功 usage 仍按最终上游真实 usage 或既有估算 fallback 计算。

本轮实现结果：

- `src/model/config.rs` 增加全局 `external_pool_stream_pre_output_retry_enabled`，默认 `true`。
- `src/external_pool.rs` 增加 `ExternalPoolStreamRetryMode` 和单池 `pre_output_stream_retry_mode`，并在 stream 2xx 分支先预读 SSE：pre-output error event、body read error、idle timeout、EOF without terminal 会转为 retryable external-pool failure，回到既有同请求外部池预算；读到 commit 边界后才把 buffered prefix 发给下游。
- `src/storage/postgres.rs` 增加 `external_upstream_pools.pre_output_stream_retry_mode TEXT NOT NULL DEFAULT 'inherit'`，并接入 create/update/list/get/strict dispatch 解析。
- 主 UI 和 admin-ui 增加全局 `流式首输出前错误换池` toggle、单池 `首输出前流式恢复` select 和列表摘要。
- 真实 loopback Axum SSE fake-upstream 测试覆盖 `message_start -> error`、`message_start -> ping -> error`、EOF、read error、idle timeout、单池禁用、post-commit 不重放，并断言失败尝试不泄漏到下游或最终成功 usage。

focused validation 已通过，但 final release gate 仍未关闭：

- `feature/tests/run-cargo-scoped.sh external-stream-preoutput-unit-final -- cargo test external_pool_pre_output_stream --locked`：`2 passed`
- `feature/tests/run-cargo-scoped.sh external-stream-preoutput-http-matrix -- cargo test external_pool_stream_ --locked`：`6 passed`
- `feature/tests/run-cargo-scoped.sh external-stream-storage -- cargo test postgres_external_pool_list_and_get_preserve_body_modes --locked`：`1 passed`
- `feature/tests/run-cargo-scoped.sh external-stream-preoutput-check -- cargo check --all-targets --locked`：pass
- `feature/tests/run-cargo-scoped.sh external-stream-preoutput-fmt-check -- cargo fmt --all -- --check`：pass
- `git diff --check`、`pnpm --dir ui check/build`、`pnpm --dir admin-ui build`：pass；UI build 只有既有 chunk-size warning

用户追加要求验证“是否影响其他正常逻辑调度，特别是正常流式/非流式输出，以及本地账号和外部池 direct 在各种配置下的调度”。本轮已按本地 plan/issue 文档抽取 focused 回归矩阵并通过，并在 2026-08-07 使用 `cargo +1.92.0` 重新复跑核心调度和输出矩阵：

- 正常外部 stream：`external_pool_stream_` 在显式注入本地 Docker PgSQL/Redis 后 `6 passed`，覆盖 pre-output 恢复、禁用开关和 post-commit 不重放。
- 正常外部 non-stream / stream usage：OpenAI-compatible non-stream usage、stream billing、raw-vs-shaped usage separation、clean non-stream byte identity、missing usage estimate 共 6 个 targeted 测试通过。
- 正常本地 stream/non-stream：`stream_success_records_requested_max_tokens_and_downstream_stop_reason` 与 `local_non_stream_success_commits_shared_attempt_budget_before_usage_for_five_rounds` 通过。
- 外部 direct 正常 stream/non-stream：`normalized_external_direct_policy_skips_raw_preparse_without_raw_pool` 已扩展为同时覆盖 `stream=false` / `stream=true`，断言 route subtype 为 `external_direct_policy`、模型发出值为 `claude-opus-4.6`、外部尝试为 1、本地 Kiro upstream hit 为 0。
- 本地优先/外部 fallback/rescue 配置：`external_fallback`、`direct_external`、`local_pool_preflight_reason`、`local_external_fallback_capacity_gate`、`fresh_local_pool_state`、`classified_scheduler_degraded`、`external_local_rescue`、`local_rescue_requires` 等 focused 组合通过。
- Router 级正常路径：normalized external preflight stream/non-stream、scheduler failure 后 external fallback、WebSearch stream/non-stream success、stream completion/client drop usage ownership、route config authority 矩阵均通过。
- 最终 `cargo check --all-targets --locked`、`cargo fmt --all -- --check`、`git diff --check` 和 build artifact inventory 均通过。
- 2026-08-07 scoped rerun：
  - `normal-routing-scheduler-matrix-20260807`：16 个 focused Cargo filter 命令通过，覆盖 external stream/pre-output/storage、fallback/direct、本地 preflight/fresh Ready/Redis degraded、local rescue、external direct stream/non-stream、本地 stream/non-stream 和 route config authority。
  - `normal-output-external-usage-matrix-20260807`：8 个 external normal output/usage 精确测试通过，覆盖 normal non-stream body identity、OpenAI-compatible usage、stream billing、raw/shaped 分离、missing usage estimate、pricing model match。
  - `normal-routing-static-final-20260807`：`cargo +1.92.0 fmt --all -- --check` 与 `cargo +1.92.0 check --all-targets --locked` 通过。
  - `git diff --check`、feature docs、artifact inventory、`pnpm --dir ui check/build`、`pnpm --dir admin-ui build` 通过；本机 pnpm 为 `10.33.4`，因此该前端结果是本地复验，不替代 baseline pnpm `11.11.0` CI gate。

2026-08-07 最终冻结候选动态 gate 也已通过：`kiro-rs` SHA-256 `eec71c67ce49ee9003d2cd70fae0d8ebfef1d44f72ee56bda8bb7c7ee592b688`，`kiro_loadtest` SHA-256 `023f3e961cdbc56e32f46f896ac66494b1a92d0e182728ddaddbeb5b8ed90e4d`；真实 Claude Code CLI `2.1.221` fake-upstream gate 为 bare `20/20`、long-session `5 sessions / 110 turns / 100 tool pairs / leakMatches=0`、thinking-wire rerun `60/60`；load/chaos 为 L3 `9/9`、L4 `12/12`、L5 `900s` soak `6820/6820` success，`300s` idle 后 RSS/FD 回落并通过 post-soak recovery。生产 rollout 观察和必要的 `yuenan` / `yuenan-1` 复核仍留给发布后执行；本状态不冒领生产观察。详细实现/验证状态见 [外部池流式首语义输出前错误恢复](../../docs/plantree/plans/rust-runtime-scheduler-stabilization/topics/external-pool-stream-pre-output-retry-20260806.md)。采样摘要见 [Yuenan stream-error sampling 2026-08-06](../evidence/external-pool-stream-error-yuenan-sampling-20260806.md)，focused/final candidate evidence 见 [External-pool stream pre-output retry focused validation 2026-08-06](../evidence/external-pool-stream-pre-output-retry-validation-20260806.md)。

首次远端 `v0.0.134` tag 发布触发的 GitHub Actions `Publish Docker Images #165` 在 `quality / Frontend and Rust quality gate` 失败，原因是新增代码触发 Clippy baseline bucket：`ExternalPoolStreamRetryMode` 的手写 `Default` 和 SSE commit classifier 的单分支 `match`。本轮已用无行为变化的 lint 修复恢复 release-quality baseline，`feature/tests/run-cargo-scoped.sh release-clippy-baseline-fix-20260807 -- rustup run 1.92.0 node scripts/ci/check-clippy-baseline.mjs` 通过，当前 `813 <= 849`。

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

聚焦结果见 [Handler EventStream 与 runtime stack 矩阵证据](../evidence/handler-eventstream-runtime-matrix-20260716.md)。外部池 stream pre-output focused/final-candidate 结果见 [External-pool stream pre-output retry focused validation 2026-08-06](../evidence/external-pool-stream-pre-output-retry-validation-20260806.md)。冻结候选本地 gate 状态：

1. `cargo fmt --check`、`git diff --check`、`cargo check --all-targets` 和相关全量测试。
2. 当前 checkpoint 的 release-only 2 MiB worker 已通过；最终 tag binary 需绑定同一结果，并完成隔离 HTTP fault matrix。
3. Claude Code CLI `2.1.221` frozen fake-upstream gate 已通过：bare、long-session、thinking-wire。
4. L3/L4/L5 fake-upstream load/chaos 已通过，其中 L5 为 `900s` soak + `300s` idle。

任一 success 缺可信 terminal/final usage，任一 failure 伪造 `message_stop`，任一已提交请求发生上游重放，任一 usage/hit 超共享预算，或任一私有 marker 出现在公开面，都阻止发布。

## 残余边界

local non-stream contamination 当前没有 response-level 跨账号重试；external SSE 在语义输出已经提交后也不能跨 pool 重放。这些路径必须 fail closed 并有明确观测，不能通过伪造 success 补齐。当前结果不能承诺未来未知 upstream event 一定兼容，只能保证已列未知/缺终止模型会明确失败或在首输出前有界恢复。
