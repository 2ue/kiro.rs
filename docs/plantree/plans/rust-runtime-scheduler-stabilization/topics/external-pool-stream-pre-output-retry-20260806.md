# 外部池流式首语义输出前错误恢复

Status: `implemented / focused-fake-http-and-routing-validated / normal-routing-rerun-passed-20260807 / frozen-cli-load-passed-20260807 / production-rollout-observation-pending / release-candidate-v0.0.134`

Last reviewed: 2026-08-07 Asia/Shanghai

Related:

- [Stream terminal errors and precommit retry](../../../../../feature/issues/stream-terminal-errors-and-precommit-retry.md)
- [外部池高可用调度与冷却回归](../../../../../feature/issues/external-pool-ha-scheduler-cooldown-regression-20260805.md)
- [外部池高可用调度执行计划](external-pool-ha-scheduler-execution-plan-20260805.md)
- [统一调度目标契约、状态机与验证方案](scheduler-target-state-machine-and-test-contract.md)
- [Yuenan stream-error sampling 2026-08-06](../../../../../feature/evidence/external-pool-stream-error-yuenan-sampling-20260806.md)
- [External-pool stream pre-output retry focused validation 2026-08-06](../../../../../feature/evidence/external-pool-stream-pre-output-retry-validation-20260806.md)

## 1. 这份记录解决什么

这份记录只覆盖一个新暴露的外部池流式子问题：

> 外部池上游已经返回 HTTP 200 并开始 SSE，但随后在没有任何有效助手内容、thinking、tool_use 或 JSON delta 前发出 error event 或断流，当前请求会直接向下游报 `流错误`，不会切换到其他外部池。

这不是 `v0.0.132` 冷却风暴的同一个根因。`v0.0.133` 已修复外部池 Redis 自事件导致候选快照被清空的问题，并通过多池 failover/recovery、并发、RPM 和发布门禁。本问题发生在外部池已经被选中、上游已经响应 200、`ExternalPoolManager` 已把 `Response` 返回给 handler 之后；现有跨池重试循环已经结束，所以不会再选其他池。

## 2. 用户可见样本

用户在 2026-08-06 给出的页面字段：

| 页面字段 | 值 |
| --- | --- |
| 时间 | `08/06 09:35:35` |
| 请求 ID | `req_01KaWrDY5oZkY13XQqdJB9PH` |
| 入口 | `/cc/v1/messages` |
| 模型（请求） | `claude-sonnet-5` |
| 模型（上游） | `claude-sonnet-5` |
| 外部账号 | `#18 yuenan-1` |
| 路由 | `外部直连 · external_pool · external_direct_policy` |
| 状态与路由 | `流错误 / 外部直连 / stream / 请求估算` |
| 客户端错误类型 | `api_error` |
| 客户端状态码 | `200` |
| 客户端收到的错误 | `The request could not be completed right now...` |
| 错误类型 | `stream_error` |
| 错误阶段 | `external_account_stream` |
| 内部错误信息 | `external upstream emitted an error event` |
| 直连原因 | `explicit_direct` |
| 上游 / 处理错误 | `处理 / 内部错误` |
| 内部错误摘要 | `external upstream emitted an error event` |

用户问题：

- 为什么这种 `流错误` 没有切换账号？
- 这种错误是否会一直等当前池自愈？
- Claude Code CLI 调用时是否只能中断？
- 据观察这类流错误大多是空回，是否能换池恢复？
- 如果可以，开关应该放在外部池全局还是单独号池上？

## 3. 远端采样结果

对 `152.53.243.159` 使用现网配置中的 `yuenan` / `yuenan-1` 做过真实请求采样，结论只代表这两个外部池，不可直接推广到所有外部上游。

| 外部池 | stream 样本 | 正常结束 | 空回 / 协议前置流错误 | non-stream 样本 |
| --- | ---: | ---: | ---: | ---: |
| `yuenan` | 12 | 7 | 5 | 5/5 成功 |
| `yuenan-1` | 12 | 10 | 2 | 5/5 成功 |

观察到的可恢复指纹：

```text
HTTP 200
  -> event: message_start
  -> event/data: error
  -> 没有 content_block_start
  -> 没有 content_block_delta
  -> 没有 thinking
  -> 没有 tool_use
  -> 没有 input_json_delta
```

解释：

- non-stream 在同一时间窗口内成功，说明这不是请求体必然非法。
- stream 失败集中在首个有效语义输出前，体感是“空回”或“刚开始就错误”。
- 该样本可以支持“对这两个池，首语义输出前重放到另一个外部池可能提升成功率”的方向。
- 原始采样命令 transcript 尚未在仓库中找到；当前 durable 生产证据文件仍是汇总记录。
- 本轮实现已补可复跑 fake-upstream focused validation；真实远端 `yuenan` / `yuenan-1` 复核和生产 rollout 观察仍是后续 gate。

## 4. 当前代码为什么不会切换

当前外部池 failover 主循环只处理 `forward_prepared_once` 在返回 `Err` 时的换池：

```text
选择池
  -> 获取租约
  -> 发送 HTTP
  -> 如果 HTTP status 非 2xx 或发送前/读非流式 body 失败：返回 Err，进入同池/跨池重试
  -> 如果是 stream 且 HTTP 2xx：立刻构造 Response，返回 Ok
  -> handler 开始向下游输出 Response body
  -> 后续 SSE body 内出现 error/read/idle：只在 body stream wrapper 中记录 stream_error
```

因此：

- `外部池最多尝试`、`跨池重试状态码`、`网络错误跨池重试`、`协议错误跨池重试` 当前只覆盖 `Response` 建立前的失败。
- `external_account_stream` 阶段的错误发生在 `Response` 返回后，不再回到外部池选择循环。
- 页面显示 `客户端状态码 200` 是因为 HTTP response 已经建立；错误在流里发生。
- 外部直连边界是正确的：它不应该 fallback 到本地账号。本问题需要在外部池内部重试/换池，而不是回本地。

## 5. 安全边界

不能把“流式错误都重试”作为方案。必须先判断下游是否已经收到会被客户端持久化或执行的语义输出。

### 5.1 可以考虑重试的窗口

只有同时满足以下条件，才可在同一请求内换外部池：

1. 原上游失败发生在首个有效语义输出前；
2. 原尝试的 protocol-only 事件还没有被发送给下游，或实现能够保证不会让下游看到旧尝试的 `message_start` 后再看到新尝试的第二个 `message_start`；
3. 请求级 attempt budget、外部池尝试预算和截止时间仍有剩余；
4. 路线计划允许外部池内部重试；
5. 外部直连仍只在外部池范围内重试，不回本地；
6. local-first fallback 到外部的请求若要回本地，仍必须满足既有 local rescue 来源和 fresh 本地容量条件，本子问题默认优先换外部池。

### 5.2 必须禁止重放的窗口

以下情况不能重放完整请求：

- 已向下游发送 `content_block_start`；
- 已向下游发送任意 `content_block_delta`；
- 已向下游发送 thinking / redacted thinking / signature 相关事件；
- 已向下游发送 `tool_use` 或 `input_json_delta`；
- 已经输出可见文本；
- 已经输出客户端会持久化为 assistant 内容或工具调用的事件；
- 客户端已经主动断开；
- 错误是请求体非法、工具 schema 非法、输入过长等确定的下游请求错误。

### 5.3 `message_start` 的特殊点

`message_start` 本身通常不包含助手正文，但它是 Anthropic/Claude Code 流协议状态。如果旧尝试的 `message_start` 已经发给客户端，再切到新池输出第二个 `message_start`，会导致协议重复、usage 拼接和客户端状态机混乱。

因此可行实现不是“看到 error 后直接重新发送一个新流”，而是：

```text
先从上游预读并缓冲 protocol-only 事件
  -> 如果在首语义输出前遇到 error/idle/EOF/read error：丢弃缓冲，释放租约，排除该池，换外部池
  -> 如果读到首个有效语义输出：一次性下发已缓冲事件 + 当前语义事件，并标记 downstream committed
  -> 后续任何错误都只作为流中断，不重放
```

## 6. 配置目标

本轮已实现目标：

- 全局默认开启；
- 单个外部池可覆盖；
- 单池默认继承全局；
- 关闭后恢复当前行为：流内错误只记录并向下游返回流错误，不跨池重放；
- 该配置只控制“首语义输出前的外部池流式恢复”，不改变非流式、HTTP 发送前错误、usage 计算、本地救援和直连边界。

已实现字段：

| 作用域 | 字段 | 默认 | 含义 |
| --- | --- | --- | --- |
| 全局外部池配置 | `external_pool_stream_pre_output_retry_enabled` | `true` | 是否允许外部池流式首语义输出前错误换池恢复 |
| 单外部池 | `pre_output_stream_retry_mode` | `inherit` | `inherit` / `enabled` / `disabled` |

字段已接入 PostgreSQL storage、runtime config、Admin API 类型、主 UI 和 admin-ui。页面中文：

- 全局：`流式首输出前错误换池`
- 单池：`首输出前流式恢复`
- 选项：`继承全局`、`启用`、`禁用`

## 7. 当前工作树进展

当前工作树已完成 focused implementation，仍不是完整 release gate：

| 范围 | 状态 |
| --- | --- |
| `src/model/config.rs` | 全局 `external_pool_stream_pre_output_retry_enabled` 已增加并默认 `true` |
| `src/external_pool.rs` | 单池 `ExternalPoolStreamRetryMode`、create/update 字段、有效模式解析、stream 2xx 预读缓冲、pre-output error/read/idle/EOF 转 retryable external error、prefix/downstream commit 边界和最终 stream wrapper 已实现 |
| `src/storage/postgres.rs` | `external_upstream_pools.pre_output_stream_retry_mode TEXT NOT NULL DEFAULT 'inherit'` 已接入 migration、select/create/update/list/get；strict dispatch 下未知值会失败，非 strict 旧库缺列默认 `inherit` |
| `ui` / `admin-ui` | runtime config 类型/default、全局 toggle、单池 select、列表摘要和保存 normalize 已接入 |
| `src/external_pool/tests.rs` | 新增真实 loopback Axum SSE fake upstream，覆盖 pre-output error、protocol-only error、EOF/read/idle、单池禁用和 post-commit 不重放 |

重要状态：

- focused fake-upstream、storage、正常 stream/non-stream、direct/fallback/rescue 调度回归、Rust check/fmt、UI check/build、admin-ui build、artifact inventory 和 diff hygiene 已通过。
- 2026-08-07 按用户要求复跑正常输出和调度矩阵：`cargo +1.92.0` scoped focused scheduler/output batch、external normal output/usage batch、Rust fmt/check、UI/admin-ui build、文档链接、diff hygiene 和 artifact inventory 均通过；结果已追加到 focused evidence。
- 2026-08-07 最终冻结候选已通过真实 Claude Code CLI fake-upstream gate 和 L3-L5 load/chaos：`kiro-rs` SHA-256 `eec71c67ce49ee9003d2cd70fae0d8ebfef1d44f72ee56bda8bb7c7ee592b688`，`kiro_loadtest` SHA-256 `023f3e961cdbc56e32f46f896ac66494b1a92d0e182728ddaddbeb5b8ed90e4d`；CLI bare `20/20`、long-session `110 turns`、thinking-wire rerun `60/60`，L3 `9/9`、L4 `12/12`、L5 `900s` soak `6820/6820` 成功并在 `300s` idle 后 RSS/FD 回落。
- 生产 rollout 后观察和真实 `yuenan` / `yuenan-1` 复核仍是发布后的观察 gate。
- 不能把本轮写成已完成生产观察；发版前最后一次 docs/diff/artifact/UI gate 已通过。

## 8. 已采用实现路线

### 8.1 外部流 pre-read 结果类型

把 `forward_prepared_once` 的 stream 分支拆成三种结果：

| 结果 | 条件 | 调度动作 |
| --- | --- | --- |
| `ReadyToCommitStream` | 已读到首个有效语义输出或确认正常可输出 | 构造 response，先发 buffered events，再接剩余 body stream |
| `RetryablePreOutputStreamError` | 首语义输出前 error event / read error / idle / EOF without terminal | 返回 `ExternalForwardError`，当前请求排除该池并按既有外部池 retry/failover 继续 |
| `TerminalPreOutputFailure` | 请求错误、输入过长、schema 错、预算耗尽、无候选 | 返回最终错误，不污染健康池 |

### 8.2 预读缓冲规则

预读需要解析 SSE event，而不是按字节字符串猜：

- 缓冲 `message_start`、`ping`、纯 usage preview 等 protocol-only 事件；
- 捕获 usage 但不把失败尝试的 usage 写成最终成功 usage；
- 读到首个 semantic event 时停止预读，把 buffered events 和该 semantic event 作为 response body 的前缀；
- 预读阶段遇到上游 error event 时，记录 attempt 失败但不向下游写该 error；
- 预读阶段达到 buffer 上限、idle timeout 或 read error 时按 retryable stream protocol/transport failure 处理；
- 如果上游返回合法 `message_stop` 但没有内容，这是“空成功”还是“可重试空回”需要单独产品决策；本问题先只处理 error/断流/idle/EOF without terminal。

### 8.3 usage 与 attempt 规则

- 最终成功的 usage 仍来自最终成功外部池上游返回的 usage；缺失时才走本地估算 fallback。
- 本地整形 usage 仍按 usage 配置执行，和调度重试无关。
- 被丢弃的 pre-output 失败尝试只能进入调度 attempt 诊断，不得累加进最终成功 usage。
- 如果所有外部池都失败，最终 usage 状态是错误，不伪造成功。
- `message_start` 中的 usage preview 只能在该尝试被 commit 后对下游可见；失败并重试时必须丢弃。

### 8.4 直连与 fallback 边界

- 外部直连：只能在外部池内部同池/跨池恢复，最终仍是外部错误；本地发送数必须为 0。
- 本地优先进入外部 fallback：优先外部池内部恢复；若最终外部失败，再沿既有 local rescue 规则判断，不新增回环。
- 已经 local rescue 的请求不能再次外部 fallback。

## 9. 验证矩阵

本轮 focused validation 已覆盖本地真实 HTTP fake-upstream 的核心恢复场景，并追加正常输出/调度回归；不能等同于最终 release/load/production gate：

| 场景 | 拓扑 | 预期 |
| --- | --- | --- |
| 首语义前 error event | A 返回 `message_start -> error`，B 正常 | 已通过：客户端只看到 B 的一条完整流；A/B 各 1 次；最终 success；usage 来自 B |
| protocol-only 后 error | A 返回 `message_start -> ping -> error`，B 正常 | 已通过：不向下游泄漏 A 的 error 或重复 start |
| 首语义前 idle timeout | A 200 后不出语义直到 idle，B 正常 | 已通过：A 释放租约，B 接管 |
| 首语义前 read error / 截断 | A header 后断流，B 正常 | 已通过：B 接管；A 失败只进 attempt 诊断 |
| EOF without terminal | A 只发 `message_start` 后 EOF，B 正常 | 已通过：按 protocol failure 重试；无空成功 |
| 合法空 message_stop | A 合法 stop 但无内容 | 已通过 classifier 单元：`message_stop` 被视作 commit，不因本问题重试 |
| 语义输出后 error | A 已发 `content_block_start` 后 error，B 正常 | 已通过：不重放；B 0 hit；客户端收到流错误；不伪造 `message_stop` |
| thinking/tool 后 error | A 已发 thinking 或 tool_use 后 error | 间接由 conservative commit classifier 保护；完整 thinking/tool CLI gate pending |
| 外部直连边界 | A pre-output error，B 正常，本地 fake 可用 | focused 测试断言 `local_attempted=false` 且 `credential_attempts` empty；完整 CLI/direct-boundary gate pending |
| 多池优先级 | A 优先级 1 间歇 pre-output error，B/C 健康 | 基础两池 request-level failover 已通过；持续多池优先级/load gate pending |
| 并发突增 | 50/100/500 并发混合 pre-output error 和成功 | pending：需 L3-L5 load/chaos |
| 真实 yuenan 复核 | 远端或本地复刻 yuenan/yuenan-1 配置 | pending：rollout 后复核 stream 空回率；non-stream 不受影响 |

## 10. 已跑验证

详见 [External-pool stream pre-output retry focused validation 2026-08-06](../../../../../feature/evidence/external-pool-stream-pre-output-retry-validation-20260806.md)。

首语义输出前恢复实现验证已通过：

- `feature/tests/run-cargo-scoped.sh external-stream-preoutput-unit-final -- cargo test external_pool_pre_output_stream --locked`：`2 passed`
- `feature/tests/run-cargo-scoped.sh external-stream-preoutput-http-matrix -- cargo test external_pool_stream_ --locked`：`6 passed`
- `feature/tests/run-cargo-scoped.sh external-stream-storage -- cargo test postgres_external_pool_list_and_get_preserve_body_modes --locked`：`1 passed`
- `feature/tests/run-cargo-scoped.sh external-stream-preoutput-check -- cargo check --all-targets --locked`：pass
- `feature/tests/run-cargo-scoped.sh external-stream-preoutput-fmt-check -- cargo fmt --all -- --check`：pass
- `git diff --check`：pass
- `pnpm --dir ui check`：pass
- `pnpm --dir ui build`：pass，只有既有 chunk-size warning
- `pnpm --dir admin-ui build`：pass

用户追加的正常输出和调度回归已通过，并在 2026-08-07 使用 `cargo +1.92.0` 复跑：

- `feature/tests/run-cargo-scoped.sh normal-routing-external-stream-db-rerun -- cargo test external_pool_stream_ --locked`，显式注入本地 Docker PgSQL/Redis 测试环境：`6 passed`
- `feature/tests/run-cargo-scoped.sh normal-output-external-usage-rerun -- bash -lc 'cargo test ...'`：8 个 targeted 正常输出/usage 测试均通过，覆盖正常 external stream/non-stream usage、clean non-stream byte identity、missing usage billing estimate、local stream success 和 local non-stream success
- `feature/tests/run-cargo-scoped.sh routing-config-classifiers-db -- bash -lc 'cargo test ...'`：11 个 targeted 调度分类命令通过，覆盖 external fallback、direct external、local preflight toggles、fresh local Ready、Redis degraded、external local rescue、normalized external stream/non-stream fallback
- `feature/tests/run-cargo-scoped.sh normal-direct-stream-nonstream-router -- bash -lc 'cargo test ...'`：4 个 targeted Router 测试通过，覆盖 external direct stream/non-stream 正常输出、本地 WebSearch stream/non-stream、client drop ownership 和 route config authority
- `feature/tests/run-cargo-scoped.sh normal-routing-cargo-check-final -- cargo check --all-targets --locked`：pass
- `feature/tests/run-cargo-scoped.sh normal-routing-fmt-check-final -- cargo fmt --all -- --check`：pass
- `node feature/tests/inventory-build-artifacts.mjs --gate`：`targets=0 reservations=0 target_processes=0 blockers=0`
- `feature/tests/run-cargo-scoped.sh normal-routing-scheduler-matrix-20260807 -- bash -lc 'cargo +1.92.0 test ...'`：16 个 focused filter 命令通过，覆盖 external stream/pre-output/storage、external fallback/direct、本地 preflight/fresh Ready/Redis degraded、local rescue、direct stream/non-stream、本地 stream/non-stream 和 route config authority。
- `feature/tests/run-cargo-scoped.sh normal-output-external-usage-matrix-20260807 -- bash -lc 'cargo +1.92.0 test ...'`：8 个 external normal output/usage 精确测试均通过，覆盖 non-stream body identity、OpenAI-compatible usage、stream billing、raw/shaped 分离、missing usage estimate 和 pricing model match。
- `feature/tests/run-cargo-scoped.sh normal-routing-static-final-20260807 -- bash -lc 'cargo +1.92.0 fmt --all -- --check && cargo +1.92.0 check --all-targets --locked'`：pass
- 2026-08-07 `git diff --check`、`node feature/tests/check-feature-docs.mjs`、`node feature/tests/inventory-build-artifacts.mjs --gate`、`pnpm --dir ui check/build`、`pnpm --dir admin-ui build` 均通过；UI build 仍只有既有 chunk-size warning。当前本机 pnpm 为 `10.33.4`，不是 baseline `11.11.0`，因此前端结果只作为本地复验。

测试代码补强：

- `normalized_external_direct_policy_skips_raw_preparse_without_raw_pool` 已从只覆盖 non-stream 扩展为同时覆盖 `stream=false` 和 `stream=true`；两种模式都断言 route subtype 为 `external_direct_policy`、外部池 hit、模型重写、attempt 计数和本地 Kiro upstream hit 为 0。

最终候选动态 gate：

- 真实 Claude Code CLI frozen-binary fake-upstream gate 已通过：bare `20/20`，long-session `5 sessions / 110 turns / 100 tool pairs / leakMatches=0`，thinking-wire rerun `60/60`。
- L3-L5 load/chaos 已通过：L3 `9/9`，L4 `12/12`，L5 `900s` soak `6820/6820` success，`300s` idle 后 `rssReturnedWithin32MiB=true`、`idleRssSettled=true`、`fdReturnedWithin5=true`。
- 发布后生产观察仍未执行。
- 真实 `yuenan` / `yuenan-1` 复核仍留给 rollout 后观察。

后续若重新绑定 CLI/load 候选，或发布后做 production 观察，仍要记录：

- 请求入口、路由、外部池尝试列表、是否 external direct；
- 每个 fake upstream 的 hit 数、事件序列、首字时间；
- 客户端看到的 SSE 事件序列；
- usage 原始字段、整形字段、Dashboard/usage 写入是否符合原规则；
- 资源清理：临时服务、端口、PgSQL、Redis、临时文件。

## 11. 发版门禁

本子问题已经完成本地冻结候选的真实 Claude Code CLI 与 L3-L5 load/chaos gate；生产观察仍必须在发布后执行。

不能发版的条件：

- `cargo check` 或初始化字段失败；
- 只靠单元测试，没有真实 HTTP fake-upstream；
- 没证明外部直连不回本地；
- 没证明语义输出后不重放；
- 没证明 usage 不混用失败尝试；
- 没清理临时资源；
- 没更新 issue、当前索引、plan-tree 和 evidence。

当前已满足的最低门禁：

1. 所有 config/storage/admin/UI 字段完整或明确暂不暴露 UI；
2. pre-read 缓冲和 retry 逻辑完成；
3. scoped Rust tests 通过；
4. 本地 fake-upstream 真实 HTTP 矩阵通过；
5. `cargo fmt --check`、`cargo check --all-targets --locked` 和差异检查通过；
6. issue、当前索引、plan-tree 和 evidence 均记录实际 focused 结果与剩余 gate。

发版前最后门禁：

1. 确认没有本轮残留进程、数据库、Redis prefix 或大 raw artifact；
2. 提交并打 `v0.0.134` tag。

发布后观察：

1. 生产 rollout 后只读观察 stream-error recurrence；
2. 必要时复核 `yuenan` / `yuenan-1` stream 空回率；
3. 更新 issue/evidence/plan-tree 状态，不把生产观察写成发布前已完成。

## 12. 接续指令

新会话接手时按以下顺序继续：

1. 先读本文件、[Stream terminal errors and precommit retry](../../../../../feature/issues/stream-terminal-errors-and-precommit-retry.md) 和 [focused validation evidence](../../../../../feature/evidence/external-pool-stream-pre-output-retry-validation-20260806.md)。
2. 检查当前工作树 diff，确认后续改动没有和 `v0.0.133` 外部池 HA 冷却回归混在一起。
3. 若要重新绑定发版候选，仍按 `kiro-claude-cli-validation` 和 `kiro-load-chaos-validation` 重新冻结二进制并重跑 CLI/load gate；不要复用旧 SHA。
4. 发布后再做只读生产观察和真实 `yuenan` / `yuenan-1` 复核。
5. 不要把本问题改成本地 rescue；外部直连必须保持 external-only。
