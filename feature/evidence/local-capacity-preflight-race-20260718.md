# Local Capacity Preflight Race Evidence

Status: `focused-policy-and-manager-pass / storage-and-load-pending / NO-GO`

Date: 2026-07-18

Issue authority: [Local Capacity Preflight Race And External Fallback Latency](../issues/local-capacity-preflight-race-and-external-fallback-latency.md)

## Evidence Boundary

本证据针对当前未提交候选中的 preflight/acquire 竞态，不把它反向声明为既有生产事故的唯一根因。没有访问生产服务、`127.0.0.1:9022`、Docker、凭据文件、API key、token 或请求正文。

## Static Reproduction

修复前候选链路为：external 静态 eligibility 已确认；runtime availability hint absent/expired；`local_pool_acquire_mode` 返回 `WaitForCapacity`；manager 使用默认 `credentialDispatchMaxWaitSecs=120`。hint 只在 external runtime scan 后写入且 TTL 250 ms，因此 first request、TTL miss 和新 model/body mode 是确定性 absent 分支。

修复删除 hint authority。capacity fallback 与 preflight 均开启且 external 静态 eligible 时返回 `FailFastOnCapacity`；external 动态状态留给真正 selection/acquire。该变化不新增本地成功路径 Redis RTT。

## Executed Scoped Results

无效命令轮：

```text
scope=queue-preflight-race-r1
cargo test --lib ... -> exit 101 before compile: package has no library target
size_kib=32
removed=true
reservation_released=true
```

该轮不计格式、编译或行为证据。

有效轮：

```text
scope=queue-preflight-race-r2
toolchain=1.92.0
wall=358.6s
policy test: running 1 / passed 1 / five internal rounds
cargo fmt --all -- --check: pass
cargo check --all-targets: pass
size_kib=2017340
removed=true
reservation_released=true
```

有效 policy filter：

```text
preflight_ready_acquire_full_race_never_enters_default_local_queue_for_five_rounds
```

它同时验证 capacity fallback OFF 与 local preflight OFF 仍为 `WaitForCapacity`。

## Skipped Storage Filters

以下三个 filter 均编译并进入 `running 1`，但 fixture 输出“未设置 `KIRO_RS_TEST_POSTGRES_URL`”后提前返回：

```text
external_pool_manager_distinguishes_global_capacity_from_no_pool
external_pool_model_unavailable_cooldown_is_model_scoped_and_does_not_queue
external_pool_coordinator_failure_fails_closed_without_queue_admission
```

因此它们不得记为动态 PASS。旧 fixture 的 skip-as-ok 行为本身是证据风险；最终由显式环境检查、缺依赖 exit 64 的 runner 执行。用户已豁免 Docker 动态执行，所以 runner 只能使用明确提供的非 Docker 隔离 PostgreSQL/Redis。

## Manager And Refresh Integration Batch

```text
scope=queue-refresh-integration-r3
toolchain=1.92.0
wall=231.2s
nine filters: each running 1 / passed 1
each filter: five internal rounds
cargo fmt --all -- --check: pass
cargo check --all-targets: pass
size_kib=2016724
removed=true
reservation_released=true
```

本专题直接采用的三条 manager 证据是：

```text
test_fail_fast_global_capacity_full_returns_without_queueing_for_five_rounds
test_local_pool_route_state_reports_capacity_full_without_queueing_for_five_rounds
test_fail_fast_slot_race_reselects_other_available_credential_for_five_rounds
```

五轮内 global/per-account full 都保持 queue 0；alternate credential 有槽时先重选，两个 lease 释放后 global in-flight 回到 0。

## Remaining Evidence

- 用非 Docker 隔离 PG/Redis 执行 external available/full/cooling/model-cooling/coordinator 五轮矩阵。
- 40x15、60 RPM、global 500、100 秒 holder 的 c1/c32/c128/c500 burst，记录 local/external hits、queue wait、TTFB 和恢复。
- Redis latency/disconnect/restart、PgSQL pool timeout、usage writer/cleanup 联合压力及两实例。
- 同一仓库外冻结 release binary 的 L3-L5、RSS/FD/lease/queue 零残留。

## Current Decision

聚焦 policy 修复通过，完整 scheduler/external 性能和可用性仍未证明。发布判定保持 `NO-GO`。
