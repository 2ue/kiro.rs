# External fallback auxiliary attribution 与 PostgreSQL usage 复核

Date: 2026-07-17

Status: `focused-pass / frozen-candidate-and-load-gates-pending`

Git baseline: `401473ca1649997bdeccf4468e3add1bdb187248`

Worktree: dirty；本证据对应演进中的源码快照，不是冻结 release candidate，也不构成发版结论。

## 问题、影响与根因

本地 credential 在 token refresh 或 `ListAvailableProfiles` profile discovery 失败后转入
external fallback 时，最终 usage 原来只从 request-scoped ledger 复制 inference channel：

```rust
latency_trace.inference_attempts =
    Some(route.inference_attempt_budget.snapshot());
```

因此真实发生的 refresh/profile auxiliary HTTP 不会出现在最终 external usage。请求本身可能成功，
但 usage 会低报内部 outbound RPM，无法从最终记录核对“下游 1 次请求为何产生多次内部调用”，也会让
错误 burst、账号切换和外部池 fallback 的放大分析失真。

修复在 `src/external_pool.rs` 中集中通过 `external_usage_latency_trace(route)` 写入两个独立快照：

```rust
latency_trace.inference_attempts =
    Some(route.inference_attempt_budget.snapshot());
latency_trace.auxiliary_attempts =
    Some(route.inference_attempt_budget.auxiliary_snapshot());
```

该改动不读取或改写 request/response body，不增加 HTTP 请求、重试、Redis 命令或 PostgreSQL
写入次数；热路径新增的是固定大小 ledger snapshot。它修复的是归因缺失，不是靠清理、压缩或
关键词过滤掩盖内部调用。

测试时相关源码 SHA-256：

| 文件 | SHA-256 |
| --- | --- |
| `src/external_pool.rs` | `bae1252e25b86424c445dfa1a41df7ce1b338ff090697b0861564debee3bf02f` |
| `src/external_pool/tests.rs` | `9e954b4447d460fbf33a6a0392f7dcba357ccedfc2eb4b945b22fb78a1af5113` |
| `src/storage/postgres.rs` | `89afd5c016fb3223e194b9322f55337cfe020639b972faffb5dcb11294b2d2ec` |
| `src/anthropic/usage.rs` | `c06d6da624fe2f41000558ecf36516cd040e7919b6da45e57d608ab25ee5f0b4` |

## 隔离环境与安全边界

- PostgreSQL：临时 Docker 容器，host port `47542`。
- Redis：临时 Docker 容器，host port `47492`。
- external、refresh、profile：本地 fake HTTP upstream。
- 未向现有 `127.0.0.1:9022` 发请求，也未修改该服务。
- 容器字段只在进程内构造测试 URL；日志和本文不包含用户名、密码、API key、credential、
  Authorization、cookie 或 refresh token。
- 所有 Cargo 调用均经 `feature/tests/run-cargo-scoped.sh`，Rust `+1.92.0`，wrapper SHA-256
  `2a6f219857197c702d7e4c5f89fb1b66467789c0d51781a9dc728327065c431f`。

## 真实链路与归因矩阵

前置聚焦批执行以下 8 项，结果 `8/8 passed`：

| 测试 | 真实断言 |
| --- | --- |
| `external_usage_trace_preserves_local_auxiliary_attempts_for_five_rounds` | 5 轮保留 local inference、external inference 和 auxiliary snapshot |
| `external_fallback_usage_matches_real_refresh_profile_and_inference_hits_for_five_rounds` | refresh/profile 两类各 5 轮，最终 usage 与 fake upstream 实际 HTTP hit 完全相等 |
| `external_pool_error_response_masks_raw_error_body_with_trace_id` | external 私有错误 body 不进入公开响应，保留 trace id |
| `external_error_classification_attempt_usage_and_final_error_never_retain_raw_bodies` | `400/403/429/500` 私有 marker 每类 5 轮均不进入 classification、attempt、usage 或 final error |
| `postgres_dashboard_duration_p95_uses_weighted_histogram_and_negative_deltas` | nearest-rank weighted P95 与 same-ID 负 delta 更新正确 |
| `postgres_dashboard_uses_exact_utc_boundary_population_for_every_window_metric` | `[from,to)` 首尾 partial hour 的 summary/series/breakdown population 一致 |
| `postgres_duration_write_and_dashboard_histogram_saturate_without_signed_wrap` | `u64::MAX` 写入饱和为 `i64::MAX`，histogram 不发生 signed wrap |
| `production_postgres_only_usage_never_materializes_redis_for_five_rounds` | 每轮 128 条、共 5 轮；PostgreSQL-only writer 的 Redis channel 始终 disabled，Redis usage key 为 0 |

真实 fallback 链不是手工伪造最终计数：

1. Refresh 场景由本地 fake OAuth endpoint 返回 HTTP 500，本地 inference hit 为 0，随后 external
   `/v1/messages` 返回 200。
2. Profile 场景由 fake `ListAvailableProfiles` 返回 HTTP 500，本地 inference 返回 400，随后
   external `/v1/messages` 返回 200。
3. 每轮分别读取 fake upstream hit 和最终 usage 的 `local_attempts`、`external_attempts`、
   `token_refresh_attempts`、`profile_discovery_attempts`；任一不相等即失败。
4. 初版 fixture 只注册 `/cc/v1/messages`，而 external normalizer 的目标是 `/v1/messages`，真实
   请求得到 404。夹具增加规范路径后通过；生产路由没有为测试改变。

## PostgreSQL 单轮用例补至三轮

前置批中的三个 storage case 各执行过 outer 1。2026-07-17 在同一个新 scoped target 内连续补
outer 2、3，复用一次编译缓存；6 次均强制检查“恰好 1 passed、0 failed、无 skip 文案”。

| Case | Outer | 运行结果 | 测试耗时 | 原始日志 SHA-256 |
| --- | ---: | --- | ---: | --- |
| weighted P95 / negative delta | 2 | `1 passed` | 1.71 s | `3246e6f95449b30ee7511c43761448eb7a45aa78d22058663202d02cb0786e4b` |
| exact UTC window population | 2 | `1 passed` | 1.00 s | `6461790c253f7df0d9c9000d31ce3a881337817523bfb2d2d5ba169d77f8c869` |
| duration `u64::MAX` saturation | 2 | `1 passed` | 0.77 s | `2d42c8d74388c9647c85f46920b839a1f013003eeb4e3c25133a4362b5ef72a8` |
| weighted P95 / negative delta | 3 | `1 passed` | 1.95 s | `18234a40baa274571e38d35b6908d6f81ba9cbe82d7e829f8044188c15c7092e` |
| exact UTC window population | 3 | `1 passed` | 1.32 s | `082f1a99a64f6b294fa961d233e9bb76b5639c1583281e0545e6ce070375a30d` |
| duration `u64::MAX` saturation | 3 | `1 passed` | 0.92 s | `d596bb9b0a46a476f0dcbe79977fb4bdd0977c7a4dc73599519aed7be8ecd3d1` |

原始日志位于本批拥有的临时目录，只保留上表脱敏摘要和 hash，退出时确认
`raw_logs_removed_on_exit=true`。内置独立写入/断言 5 轮的 external attribution 和
PostgreSQL-only 用例没有机械重复成 3 个 outer。

## 无效夹具轮次与构建清理

第一次补轮 scope `external-pg-dashboard-outer-2-3` 没有形成有效产品测试。测试脚本的 `jq`
程序被 shell 提前展开为空字符串，构造出的 URL 缺少 user/password/database；SQLx 因而回退本机
用户名并在第一个测试连接阶段报 password authentication failed。该次结果为 `0 passed / 1 failed`，
失败日志 SHA-256 为
`2fd435f52a30ee67533d0e25d0d03e423c568ca5b06e48bff3480f4a1d25a06b`。
它是测试 harness 缺陷，不是产品源码缺陷，不计入 outer 轮次，也不能被最终绿灯覆盖或省略。

两个 scope 分别给出独立清理证明：

| Scope | 结果 | Scoped target | Reservation |
| --- | --- | --- | --- |
| `external-pg-dashboard-outer-2-3` | harness failure | `removed=true` | `reservation_released=true` |
| `external-pg-dashboard-outer-2-3-retry` | 6/6 pass | `removed=true` | `reservation_released=true` |

首 scope 退出后、retry scope 启动前，定向检查其 `.validation-build-*` 和 reservation 均为 0；
retry scope 退出后再次检查 scoped target 和 reservation files 均为 0。retry target 峰值
`1,625,556 KiB`。没有保留本批 Cargo/rustc 子进程。

全部断言和文档校验结束后，已删除 `kiro-extattr-pg-20260717` 与
`kiro-extattr-redis-20260717`。`docker inspect` 确认两个容器均不存在，host ports `47542`、
`47492` 均无 listener；本批临时原始日志目录计数为 0。

## 结论与未关闭项

本组证据支持以下窄结论：external fallback 的最终 usage 现在保留本地 auxiliary channel；真实
refresh/profile/outbound HTTP hit 可与 usage 逐轮对账；external 私有错误 body 没有进入公开错误或
usage；PostgreSQL weighted P95、精确 UTC 边界和极值饱和在三个 outer 均通过；生产
PostgreSQL-only usage 不会实例化 Redis usage writer 或物化 Redis usage keys。

本组没有证明整个项目可发版。以下仍需在同一个冻结 candidate SHA 上完成：真实 Claude Code CLI
长会话/tool/thinking/image/search/agent；external/local 完整 fallback 矩阵；429/500/partial/client-drop
burst 与恢复；Redis latency/disconnect/two-instance；PostgreSQL writer 负载；RSS/FD/task/TTFB 与
三轮 soak；两套 UI、upgrade、release build、Docker 和最终 artifact inventory。上述门禁完成前，
总体状态仍为 `NO-GO`。
