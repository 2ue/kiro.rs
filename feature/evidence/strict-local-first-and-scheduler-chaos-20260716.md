# Strict Local-First And Scheduler Chaos Evidence

Date: 2026-07-16

Role: E05 handler-level local/external routing、CapacityFull、Redis degraded 红绿与辅助 RPM 证据

Status: `historical-focused-pass / latest-fixed-binary-revalidation-red / release-blocked`

## 测试合同

脚本：`feature/tests/strict-local-first-routing.mjs`

每个模式至少 3 轮，每轮 5 个独立 Messages 请求。runner 启动当前 binary、隔离 PostgreSQL/Redis、fake Kiro、fake external，并记录每请求 local inference、local auxiliary、external hit、HTTP 状态、延迟、RSS/FD、Git revision、dirty diff hash、binary hash 和清理结果。现有 `127.0.0.1:9022` 不在测试端口集合内。

CapacityFull 使用一个真实 holder 请求占住唯一 local credential 槽，再发送 5 个探测请求。SchedulerRedisDegraded 使用独立 Toxiproxy 在服务 ready 后给 Redis downstream response 注入 150 ms 延迟，超过 75 ms scheduler hot-path timeout；移除 toxic 并等待 breaker 后再发恢复请求。

## 最新固定二进制复核（优先于下方历史绿灯）

runner 现已为每个 case 配置独立 Redis key prefix，避免不同 PostgreSQL authority 共享 Redis 调度状态；同时记录每 case prefix digest，校验运行前后 binary SHA，并在失败时保留脱敏 JSON 报告。单独 CapacityFull 3 轮报告 `target/e05-reports/e05-20260716122157259-94531-07346c.json` 通过，report SHA-256 `e62db5ea60c835ffe59dad134c485a6cd104cf5f189eb6eda33eda8c04733011`，binary SHA-256 `f654f2bb7d2a1872ff90a66faa2d0fb21ab16cb091f48a0f7631f12332d64503`。

固定 binary `dd15a7bf79e5017e4218e8fda6e99656fb826180b0327c8beaa249619a07dbc1` 的 `local_ready_transient` 三次外层复核结果为 1 pass / 2 fail：

| 外层 | 结果 | 首个错误 fallback | 同窗口证据 | 报告 SHA-256 |
| --- | --- | --- | --- | --- |
| 1 | pass，3 内轮 x 5 请求 | 无 | cleanup 全 true | `9183a5767ed8dc5ee8f22397f3d526bc6ced4723250db619c842794c7405d99c` |
| 2 | fail | 首内轮 request 3，local 1 / external 1 | sticky write >75 ms，slot acquire >75 ms；启动 credential PG write 401 ms | `a815a860b507f9f4b974d7c4143250c7634fa2d801fdb2324003e2f29727845c` |
| 3 | fail | 首内轮 request 4，local 2 / external 1 | session soft-failure 118 ms，slot acquire >75 ms；PG stats delta 179 ms | `d3747983b3f0cc233f402c977c4ee01030d3f96f345be9124660f7a1efe9e8cb` |

对应报告依次为 `target/e05-reports/e05-20260716123127641-79445-306886.json`、`e05-20260716123215840-88348-da60dd.json`、`e05-20260716123250625-93136-3b59e2.json`。两份失败报告均确认 `binaryStableDuringRun=true`、cleanup 全 true，且 fixture key/token 扫描为 0。其直接语义与生产问题一致：还有大量 local dispatchable credential，但 Redis scheduler 客户端热路径在错误/持久化压力下超过 75 ms，路由状态转为 `SchedulerRedisDegraded` 后进入 external。故本文件下方的历史 focused pass 不再代表当前候选，E01/E05 保持红灯。

## 历史七状态矩阵

报告：`target/e05-reports/e05-20260715231526471-78076-890ca5.json`

- report SHA-256：`c71b6fe6550f6d8eddba1d7b8ef31bdcc271b2185f854200a12c23a5f001031e`
- binary SHA-256：`0c63cba53981723fce6e991a15060005754c4dd9ac1f675d5504fb67b906746f`
- Git revision：`401473ca1649997bdeccf4468e3add1bdb187248`，dirty diff SHA-256 `ef8ec6c501cacf5b2dccad14211e91ea02ad95123499eddd381755109f1554fe`
- 7 模式 x 3 轮 x 5 请求，共 105 请求。

| 模式 | local inference | startup auxiliary | external | 状态 | 结论 |
| --- | ---: | ---: | ---: | --- | --- |
| NoCredentials | 0 | 0 | 15 | 15 x 200 | 允许 fallback |
| AllDisabled | 0 | 0 | 15 | 15 x 200 | 允许 fallback |
| NoModelCompatible | 0 | 9 | 15 | 15 x 200 | 模型感知 fallback；每轮 3 个账号均参与 discovery |
| AllCoolingDown | 3 | 3 | 15 | 15 x 200 | 每轮首个 local 429 后冷却，其余直接 fallback |
| fallback disabled + NoCredentials | 0 | 0 | 0 | 15 x 503 | 开关关闭时不偷跑 external |
| external 500 / no loop | 0 | 0 | 15 | 15 x 502 | 每请求仅 external 1 hit；1 秒 cooldown 后恢复再测，不回 local 循环 |
| local ready + transient 500 | 45 | 12 | 0 | 15 x 502 | 每请求恰好 3 个有界 local inference；本地仍 dispatchable 时 external 恒为 0 |

60 账号模式每轮 startup model discovery 严格 4 hit，而不是 60 hit；并发第二次同步的 single-flight 由 provider 单测另行证明。

## CapacityFull

报告：`target/e05-reports/e05-20260715232408676-39344-a85d97.json`

- report SHA-256：`4e248bec96f1d9486ba656427d43cf1de73798a87a365d939f197f0d8641ef16`
- binary SHA-256：`1ab25d6443b80c12076112d570c5e64a5fb24be7d43884b4fc10dcb998b33f2b`
- 3 轮，每轮 1 个 holder + 5 个 capacity probe。
- 15/15 probe 均为 local inference 0、external 1、HTTP 200。
- 每轮 holder 恰好 local inference 1；受控 local 500 后独立按 transient 原因 external 1。该 hit 不计入 capacity probe。
- probe TTFB：p50 `19.87 ms`，p95/p99/max `72.79 ms`。

## SchedulerRedisDegraded 红灯

修复前同一 Toxiproxy fixture 稳定得到：

```text
local_state=SchedulerRedisDegraded
local_dispatchable=1
external fallback suppressed because the fresh local pool remains dispatchable...
HTTP 429 No account is ready... Retry after 2 seconds
local inference hits=0, external hits=0
```

根因是 `local_pool_fallback_reason_for_fresh_state()` 对所有 `dispatchable > 0` 一律禁止 external。`SchedulerRedisDegraded` 的 `dispatchable` 只来自本地内存容量估计，不能证明 Redis 分布式 concurrency lease 可安全取得；因此即使开关为 true，仍复现生产 `local_error_no_fallback` 语义。

修复保留普通 Ready/不一致状态的 strict local-first 保护，只让 `SchedulerRedisDegraded` 按其独立开关决策。开关 false 仍返回本地规范错误。

## SchedulerRedisDegraded 绿灯

报告：`target/e05-reports/e05-20260715233242556-89838-cc4c9a.json`

- report SHA-256：`b31d1bba6d86e5e3a0a3e9ab7ef591352f37b970cde8af7bbea701d4be05bd29`
- binary SHA-256：`54e0cefbff26c1ef2ad7e7f2f275485f888211275e6c3e84b78d05de92aa11ed`
- dirty diff SHA-256：`58d059849a6012024373414b226b8616d6a4afa9e5e287f80fbd5eadf27e616d`
- 150 ms Redis latency，3 轮 x 5 请求：15/15 HTTP 200；每请求 local inference 0、external 1。
- 每轮移除 toxic、等待 5 秒后，恢复请求先命中 local 1；受控 local 500 后再 external 1，3/3 HTTP 200。
- 故障窗口 TTFB：min `1420.43 ms`、p50 `1507.82 ms`、p95/p99/max `1991.59 ms`。
- RSS 每个独立进程 start/peak/end 分别约 43-45/55-58/55-58 MiB；FD start 28-29、peak/end 29-30。该短测试在响应后立即停止进程，不代替 idle recovery 或 L5。

## 清理与安全

三个 pass 报告均为：`containersRemoved=true`、`tempSecretsRemoved=true`、`portsReleased=true`。早期报告曾用现有 `9022` PID 不变作为隔离证明；该探针已按当前安全合同作废，当前 runner 不应读取既有 9022 listener，只按数值排除该端口并报告 `protectedPortProbeSkipped:true`。runner 只使用 fixture key/token；不读取 `kiro_idc_users*.txt`。

## 未关闭项

- 三个报告来自连续演进的 dirty build，不是最终统一 release candidate；最终 SHA 必须全模式重跑。
- Scheduler degraded 成功路径 p50 约 1.51 秒、p95 约 1.99 秒，因为 external pool 的 capacity snapshot、cooldown、lease ID 和 atomic lease 同样串行访问慢 Redis。可用性已恢复，但异常流量下排队/吞吐性能仍需 L3-L5 评估和 round-trip 优化。
- 仍缺 external pool full/disabled/coordinator unavailable 组合、双实例 handler-level routing、E01 分布和 E02 lease race/reselect。
- 本证据不替代生产只读 recurrence，也不证明 usage cleanup 压力已经与 scheduler 隔离。

## 重跑

```bash
cargo +1.92.0 build
node feature/tests/strict-local-first-routing.mjs

# 聚焦模式仍强制至少 3 轮、每轮至少 5 请求
KIRO_E05_MODES=local_capacity_full node feature/tests/strict-local-first-routing.mjs
KIRO_E05_MODES=scheduler_redis_degraded node feature/tests/strict-local-first-routing.mjs
```
