# External Pool Authoritative Dispatch Evidence 2026-07-18

Status: `source-fixed / non-storage-focused-pass / isolated-storage-dynamic-pass / release-NO-GO`

## 证据身份与约束

- Repository HEAD: `401473ca1649997bdeccf4468e3add1bdb187248` (`v0.0.109`)；工作树有大量未提交用户与本轮变更。
- Rust toolchain: `1.92.0`。
- 本轮 2026-07-18 使用当前仓库专属隔离 Docker PostgreSQL/Redis 执行 storage dynamic，不访问/探测/停止 `127.0.0.1:9022`，未读取或暂存 `kiro_idc_users*.txt`。
- 所有 Cargo 命令均由 `feature/tests/run-cargo-scoped.sh` 执行；每个 scope 都要求并观察到 `removed=true` 和 `reservation_released=true`。
- 本机 `KIRO_RS_TEST_POSTGRES_URL`、`KIRO_RS_TEST_REDIS_URL` 及两个 isolated confirmation 均未设置。本文不把 storage test 的正文早退记为动态成功。

## 已确认的修复前源码事实

修复前 `load_authoritative_pool_snapshot` 对每个 external request 调用 `list_external_pools(false)`，timeout 2 秒，selection hard cap 32。请求内重选复用 Arc，但不同请求不共享。`validate_pool_dispatch_fence` 位于 `forward_once` 之前，而 `forward_once` 内仍构造 URL、headers、body、model、request builder 与 timeout 后才 reserve/send。

`list_external_pool_eligibility` 对 `request_body_mode` decode error 默认 `normalized`，对非字符串数组的 `supported_models` 默认空列表；空列表在 eligibility 语义中匹配全部模型。full row decoder 对多个 enum/JSON 也使用相同宽松默认。

## 实现变更

- 新增 manager-owned generation-bound authoritative snapshot：success TTL 250ms、failure retry 100ms、singleflight wait 2250ms、PG query deadline 2s；caller cancellation 不取消正在进行的 authority refresh。
- 新增 strict secret-free eligibility decoder 与 strict dispatch full-row decoder；坏行按 pool 隔离。
- 新增 `(pool_id, revision)` in-flight-only fence flight map；flight 从 map 删除后才通知已有 waiter，完成结果不作为 cache。
- 拆分 `prepare_forward_once` 与 `forward_prepared_once`；prepare 后 fence，`Current` 后 reserve/send。
- 新增 `feature/tests/run-external-dispatch-storage-validation.sh`，默认 3 outer rounds，覆盖 17 个 static/selection/fence/storage filters。

## 静态与编译门禁

`external-dispatch-format-r1`：

```text
cargo fmt --all
size_kib=28
removed=true
reservation_released=true
```

随后 `git diff --check` 通过，scoped validation target 为 0。

`external-dispatch-check-r1`：

```text
env RUSTUP_TOOLCHAIN=1.92.0 cargo check --all-targets
Finished dev profile in 1m44s
warnings=0
size_kib=447300
removed=true
reservation_released=true
```

取消安全重构后的 `external-dispatch-cancel-check-r1` 再次执行 `cargo fmt --all && cargo check --all-targets`，零 warning，`size_kib=446944`，`removed=true / reservation_released=true`。

加入 authoritative/fence c128 timeout、即时第二波 suppression、恢复和 15 类坏行后，`external-dispatch-wave-check-r1` 第三次执行同一 fmt/all-target gate，零 warning，`size_kib=446944`，`removed=true / reservation_released=true`。

## 实际无存储聚焦结果

`external-dispatch-focused-r1` 在一个 scope 内运行 15 个 exact filters。以下 11 个实际进入正文并均为 `running 1 / passed 1`：

```text
persisted_external_pool_enum_parsers_reject_unknown_values_for_five_rounds
external_pool_outbound_body_strips_budget_tokens_for_adaptive_thinking
external_pool_normalized_wire_preserves_omitted_output_effort_for_five_rounds
external_pool_outbound_body_applies_model_mapping_and_thinking_normalization
external_pool_raw_passthrough_keeps_body_byte_for_byte
external_pool_raw_body_mode_does_not_apply_payload_guard
external_pool_normalized_body_mode_applies_payload_guard
external_pool_raw_probe_is_reused_across_modes_and_failover_for_five_rounds
external_pool_outbound_body_strips_budget_tokens_for_disabled_thinking
external_pool_outbound_body_preserves_enabled_budget_tokens
external_pool_outbound_body_require_mapping_match_rejects_miss_before_send
```

严格 parser、omitted effort 与 raw probe 测试各自内部 5 轮；其余本次为单次 exact 执行，最终 release 前还需用统一 outer-round runner重复。所有 raw/normalized/thinking/effort 测试证明本轮 scheduler/selection refactor 没有改变这些已有单点合同。

以下四个 test function 被测试 harness 匹配并编译，但正文打印“未设置 KIRO_RS_TEST_POSTGRES_URL”后返回，耗时 0.00s：

```text
external_pool_authoritative_snapshot_singleflights_c32_c128_for_five_rounds
external_pool_dispatch_fence_coalesces_only_in_flight_c32_c128_for_five_rounds
external_pool_dispatch_prepares_then_fences_before_attempt_and_http_send_for_five_rounds
malformed_external_pool_rows_are_isolated_and_fail_closed_for_five_rounds
```

随后新增 manager-owned refresh cancellation、authoritative c128 timeout/recovery 和 fence c128 timeout/recovery 三项，并由后两次 all-target check 编译。七个 storage fixture 当前都是 `compile-only`，不是 c32/c128、leader cancellation、fault suppression/recovery、TOCTOU 或 malformed row 动态 PASS。

scope 结果：

```text
wall=149.6s
size_kib=1695244
removed=true
reservation_released=true
```

## Storage Runner 负向门禁

`bash -n feature/tests/run-external-dispatch-storage-validation.sh` 通过。

缺 URL 直接执行：

```text
exit=64
KIRO_RS_TEST_POSTGRES_URL is required; no storage test was run
```

以 PG URL 端口值 9022、Redis dummy URL 和两个 isolated flag 执行：

```text
exit=64
KIRO_RS_TEST_POSTGRES_URL cannot use protected port 9022
```

runner 先解析所有 target 并检查端口，再进行任何 TCP probe；因此上述 protected-port case 没有访问 9022。两次负向运行后 `.validation-build-*` 为 0，未进入 Cargo。

## 隔离存储动态矩阵结果

2026-07-18 在当前仓库专属隔离 PostgreSQL/Redis 上重跑 `feature/tests/run-external-dispatch-storage-validation.sh`。依赖边界：

- PostgreSQL 容器：`kiro-final-20260718-pg`；
- Redis 容器：`kiro-final-20260718-redis`；
- 每轮测试使用独立临时 database；失败/通过后 drop database；
- Redis 使用当前隔离 DB，并在本批开始前清空；
- 未使用、探测或影响 `127.0.0.1:9022`；
- 未清理另一个本机同名项目 `/Users/yuanfeijie/Desktop/project/kiro.rs` 正在使用的 `kiro-rs-scheduler-gate-*-a1` 容器。

有效最终门禁：

```text
scope=external-dispatch-storage-real-20260718-r3
outer_rounds=3
filters_per_round=17
result=51/51 passed
cleanup=size_kib=1691008 removed=true reservation_released=true
```

覆盖：

- static eligibility model/body；
- SWR PG lock c32/c128；
- cross-instance TTL/invalidation；
- PG failure negative cache；
- authoritative timeout/singleflight；
- leader caller cancellation；
- authoritative/fence c128 timeout、即时第二波 suppression 与 recovery；
- request-scoped reselection；
- revision TOCTOU；
- in-flight-only fence；
- prepare-before-fence 0 HTTP hit；
- 15 类 malformed row；
- Redis runtime snapshot c128 coalescing。

本轮发现并修正两个测试合同问题，均为 `#[cfg(test)]` 测试语义修正，未改产品路径：

1. `external_pool_authoritative_selection_pg_lock_is_typed_bounded_and_recovers` 原断言“PG lock timeout 后首次恢复 selection 只能 1 个 Redis RTT”。真实路径在 coordinator cold bootstrap 时需要固定小常数 RTT：run_id、reconcile 内 run_id、install guard、confirm run_id、batch snapshot，实测为 5。修正后的合同区分 cold bootstrap 与 warm path：cold bootstrap 必须有上限，warm selection 仍为 1 RTT，warm selection + acquire 为 2 RTT。
2. `external_pool_selection_runtime_snapshot_coalesces_128_waiters_for_five_rounds` 原第 0 轮把 coordinator cold bootstrap 误算进 128-waiter runtime snapshot coalescing。修正后先显式 bootstrap 并断言固定上限，再清 runtime snapshot 测 5 轮 warm 128 并发，每轮保持 1 RTT。

无效/中间运行不计入 pass：

- `runtime-external-storage-real-20260718`：runtime quarantine 部分已通过，但 external 部分在旧 cold-bootstrap 断言处红，wrapper 已清理；
- `external-authoritative-pg-lock-fix-20260718-r2`：单红项修正后通过；
- `external-dispatch-storage-real-20260718-r2`：第 1 轮最后一个旧 coalescing 断言红，wrapper 已清理；
- `external-selection-coalescing-fix-20260718`：单红项修正后通过。

这些红项证明旧测试没有区分 coordinator cold bootstrap 与 steady-state hot path；最终 r3 证明产品 hot path 未出现随 waiter 或 c128 fanout 的 Redis/PG 放大。

显式提供 caller-owned、非生产、隔离 PG/Redis 后运行：

```bash
KIRO_RS_TEST_POSTGRES_URL=<isolated-postgres> \
KIRO_RS_TEST_REDIS_URL=<isolated-redis> \
KIRO_RS_TEST_POSTGRES_ISOLATED=1 \
KIRO_RS_TEST_REDIS_ISOLATED=1 \
feature/tests/run-external-dispatch-storage-validation.sh
```

runner 默认 3 outer rounds，每轮串行 17 个 exact filters。本轮已按上述矩阵真实执行；后续冻结 release/load 仍需记录同一候选二进制下的 RSS/FD、TTFB、队列、attempt 和错误恢复。

## 性能与清理事实

- authoritative list 的正常算法上限从每 caller 一次降为每 process/generation/250ms 一次；r3 storage dynamic 已验证 c32/c128 与 timeout/recovery。
- fence 只合并同时进行的同 revision query，顺序调用不缓存；r3 storage dynamic 已验证 c32/c128、timeout/recovery 与 TOCTOU。
- local-success eligibility 仍不读 external Redis；body preparation逻辑的聚焦 identity tests 已通过。
- format/check/focused 三个本轮 scope 均清理。最后检查未发现 `.validation-build-*`；根 `target/` 属于编辑器状态，不由本轮删除。

## 当前判定

源码缺陷已有针对性实现，non-storage body/parser 合同通过，隔离 PostgreSQL/Redis dynamic 三轮通过。该专题的开发/隔离存储证据可标记为 pass；完整默认树、frozen release、两实例、真实负载/chaos 和发布零残留仍未完成，整体发布仍为 `NO-GO`。
