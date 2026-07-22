# Business/Observability Redis Fault Domain Product Validation 2026-07-21

Role: 记录主线业务 Redis 与观测/统计 Redis 故障域隔离的当前产品级验证证据

Status: `product-focused-pass / broader-release-gates-open / NO-GO`

## 结论

当前工作树已经通过本专题的产品级单实例验证：业务 Redis 与观测 Redis 使用两个独立 loopback Redis authority 时，observability Redis 的延迟和断连不会把 business scheduler 判成 degraded；business Redis 故障会按 scheduler 热路径 fail closed，且不会伪装成 `AllDisabled` 或持久账号禁用；observability usage 写入不回落到 business Redis。2026-07-21 scheduler Redis deterministic response-error classification 修复后，本专题又用独立空 DB 重跑产品 runner 并通过，证明该修复没有破坏业务/观测故障域合同。

该结论只关闭 E09 的 focused/product gate。2026-07-21 后续又补充了纯 Node 源码合同和产品源码 guard，锁定生产注入链不能把业务 Redis 传给 usage/Admin/cleanup，并让 Redis usage materialization 专用入口在生产期 fail closed 拒绝业务 scheduler Redis。最新补证还锁定 scheduler、external pool、runtime event listener 和 health readiness 只使用 business Redis，observability Redis 启动失败时只降级到 PostgreSQL/进程内观测且绝不回落 business Redis，UsageRecorder 主请求路径只入队观测 Redis writer、压力下丢弃观测记录而不阻塞请求。它仍不替代两实例真实服务进程、external takeover、生产高基数、真实 Claude CLI/native upstream、UI、upgrade、final frozen release 与 inventory gate。

## 环境与源码身份

- 日期：2026-07-21（Asia/Shanghai）。
- Git revision：`401473c`，dirty tree。
- Rust 有效门禁工具链：`rustc 1.92.0 (ded5c06cf 2025-12-08)` / `cargo 1.92.0 (344c4567c 2025-10-21)`。
- 默认本机工具链事实：`rustc 1.86.0` / `cargo 1.86.0`；一次无效 C0 尝试因此失败，见下方“无效红项”。
- 初始业务 Redis：`redis://127.0.0.1:26379/15`。
- 初始观测 Redis：`redis://127.0.0.1:50892/15`。
- 修复后回归业务 Redis：`redis://127.0.0.1:26379/8`。
- 修复后回归观测 Redis：`redis://127.0.0.1:50892/2`。
- 本轮未启动 Docker，未探测或触碰 `127.0.0.1:9022`，未读取 `kiro_idc_users*.txt`。

## 验证程序

新增/使用的验证程序：

- `feature/tests/run-redis-fault-domain-validation.mjs`：基础端点与 namespace 级验证，只证明两个 Redis endpoint、`run_id` 和 bounded cleanup 行为，不证明产品 `RedisStore`/scheduler/usage 路径。
- `feature/tests/run-redis-fault-domain-product-validation.mjs`：产品级 runner，要求两个 Redis URL 和 `KIRO_RS_TEST_REDIS_ISOLATED=1`，启动两个自有 loopback proxy，通过 `feature/tests/run-cargo-scoped.sh` 运行 Rust exact test。
- `feature/tests/run-redis-fault-domain-product-validation.contract.test.mjs`：纯 Node 合同测试，默认不连接 Redis、不运行 Cargo；显式 live Redis URL 时覆盖 HUP/INT/TERM 清理合同。
- Rust exact test：`kiro::token_manager::manager::tests::redis_business_and_observability_fault_domains_are_independent_for_three_rounds`。

产品 runner 固定保护：

- 缺 URL、DB0、同 authority、未确认隔离、非法 scope/rounds 在 proxy/Cargo 前拒绝。
- 非 loopback Redis 需要显式 opt-in。
- 不使用 `FLUSHDB`。
- 不把 Redis URL 放到 wrapper argv。
- 不调用 Docker。
- 不探测 9022。
- 强制 `KIRO_RS_REQUIRE_STORAGE_TESTS=1`，避免集成体 skip 被算作 pass。

## 执行结果

### 1. 纯 Node 合同，默认不连 Redis

命令：

```bash
node --test feature/tests/run-redis-fault-domain-product-validation.contract.test.mjs
```

结果：

- 37 tests。
- 28 passed。
- 9 skipped（live signal cases 未提供 Redis URL）。
- 0 failed。

### 1b. 纯 Node 源码合同补证：生产路径只注入 observability Redis

命令：

```bash
node --test feature/tests/run-redis-fault-domain-product-validation.contract.test.mjs
```

结果：

- 46 tests。
- 37 passed。
- 9 skipped（live signal cases 未提供 Redis URL）。
- 0 failed。

新增源码合同覆盖：

- `src/main.rs` 中 `UsageRecorder::with_postgres_and_observability_redis(...)` 和 `AdminServiceDependencies` 只接收 `observability_redis_store.clone()`，不接收业务 `redis_store.clone()`。
- `src/main.rs` 中 `MultiTokenManager`、`ExternalPoolManager`、runtime event listener 和 health readiness 只接收业务 `redis_store.clone()`，不依赖 observability Redis；observability Redis 启动失败只降级到 PostgreSQL/进程内观测，不会 `Some(redis_store.clone())` 回落。
- `src/anthropic/usage.rs` 的生产构造函数断言 Redis store 必须是 observability role；历史 `with_postgres_and_redis` 保持 `#[cfg(test)]`。
- `src/anthropic/usage.rs` 的 `UsageRecorder::record(...)` 主请求路径只 enqueue PostgreSQL/Redis writer；`record_usage_redis(...)` 不直接执行 Redis IO，队列满或 writer 关闭时丢弃 Redis summary 记录以避免阻塞主请求。
- `src/admin/service.rs` 的 Admin cache、余额 cache 和 usage cleanup 只从 `observability_redis_store` 取 Redis；cleanup 缺观测 Redis 时不回落业务 scheduler Redis。
- `src/storage/redis_cache.rs` 明确保留 `RedisStoreRole::Business / Observability` 两条连接路径。
- `src/storage/redis_cache.rs` 的 usage materialization 专用入口有 `ensure_observability_usage_store(...)` 生产 guard；覆盖 cleanup watermark、derived-cache invalidation、summary write/read、usage record snapshots、dashboard series/top 和 usage summary cleanup，不依赖调用方“传对 Redis”的约定。
- `src/model/config.rs` 继续拒绝仅靠 DB 或 keyPrefix 的伪隔离，并保留 `KIRO_RS_OBSERVABILITY_REDIS_URL` / `KIRO_RS_OBSERVABILITY_REDIS_KEY_PREFIX` 环境覆盖。

补证过程中的两次红项均为新测试正则过宽或截取窗口过短，分别误把 `postgres_store.clone()`/`observability_redis_store` 识别成 `redis_store`、以及未截到 cleanup 注释；收窄断言后通过。没有发现产品代码把业务 Redis 注入 usage/Admin/cleanup。

### 1c. 底层 production role guard 编译与合同复核

新增生产 guard 后执行：

```bash
node --test feature/tests/run-redis-fault-domain-product-validation.contract.test.mjs
feature/tests/run-cargo-scoped.sh redis-observability-role-guard-20260721 -- cargo +1.92.0 check --bin kiro-rs
node --test feature/tests/run-redis-fault-domain-product-validation.contract.test.mjs feature/tests/run-scheduler-redis-chaos-validation.contract.test.mjs
```

结果：

- fault-domain 合同：46 tests，37 passed，9 skipped，0 failed。
- scoped `cargo +1.92.0 check --bin kiro-rs`：passed；wrapper cleanup `size_kib=446876 available_kib=74694132 removed=true reservation_released=true`。
- scheduler chaos + fault-domain 合批：74 tests，53 passed，21 explicit live-fixture skips，0 failed。
- scoped check 后 root `target/debug`/`target/flycheck0` 曾由外部 rustc/flycheck 重新出现约 709 MiB；`lsof +D target` 为空后只删除无引用可再生产物，复核 `node feature/tests/inventory-build-artifacts.mjs --gate` 为 `targets=0 reservations=0 target_processes=0 blockers=0`。

### 2. 纯 Node 合同，live signal 清理

命令：

```bash
KIRO_REDIS_FAULT_DOMAIN_CONTRACT_BUSINESS_URL=redis://127.0.0.1:26379/15 \
KIRO_REDIS_FAULT_DOMAIN_CONTRACT_OBSERVABILITY_URL=redis://127.0.0.1:50892/15 \
node --test feature/tests/run-redis-fault-domain-product-validation.contract.test.mjs
```

结果：

- 37/37 passed。
- 覆盖 HUP/INT/TERM 各三轮。
- 无 proxy、端口、temp root 残留。

### 3. 基础 Redis fault-domain runner

命令：

```bash
KIRO_REDIS_FAULT_DOMAIN_OUTER_ROUNDS=3 \
node feature/tests/run-redis-fault-domain-validation.mjs
```

结果：

- 3 outer rounds passed。
- business `run_id` hash：`e9b5fdefeb926aee`。
- observability `run_id` hash：`d50ea39959da6a41`。
- `serverIdentityDistinct=true`。
- observability 250ms latency 下 business 8/8 成功，p95 约 `15/21/15 ms`。
- observability disconnect 下 business 8/8 成功。
- business fault 下 scheduler-like ops 6/6 fail closed，observability remained available。
- cleanup：randomPrefixesOnly true，flushDbUsed false，proxy stopped。

### 4. 产品 runner 初始红项：测试合同过严

初始命令：

```bash
KIRO_REDIS_FAULT_DOMAIN_BUSINESS_URL=redis://127.0.0.1:26379/15 \
KIRO_REDIS_FAULT_DOMAIN_OBSERVABILITY_URL=redis://127.0.0.1:50892/15 \
KIRO_RS_TEST_REDIS_ISOLATED=1 \
KIRO_REDIS_FAULT_DOMAIN_OUTER_ROUNDS=3 \
KIRO_REDIS_FAULT_DOMAIN_SCOPE=redis-fault-domain-product-r1 \
node feature/tests/run-redis-fault-domain-product-validation.mjs
```

结果：

- Rust exact test panic 于 business Redis fault 后立即 recovery acquire。
- 错误为 `本地账号调度容量暂不可用（Redis 调度协调状态不可用，retry_after_secs=1）`。
- scoped cleanup 正常：`validation-build-cleanup scope=redis-fault-domain-product-r1 size_kib=1715232 available_kib=72485956 removed=true reservation_released=true`。
- runner cleanup 正常：childGroupsStopped、portsReleased、tempRemoved 均 true。

判断：

- 这是测试合同问题，不是业务逻辑问题。business Redis fault 后 capacity breaker 保留 `retry_after` 退避是防 spin 保护；已有 helper `recover_capacity_breaker_five_times()` 明确会先等待 retry-after 再要求 5/5 recovery。
- 修正方式是让产品 exact test 使用 `recover_capacity_breaker_five_times()` 验证恢复，而不是要求故障刚解除后的下一次 acquire 立即成功。

### 5. 产品 runner 修正后通过

命令：

```bash
KIRO_REDIS_FAULT_DOMAIN_BUSINESS_URL=redis://127.0.0.1:26379/15 \
KIRO_REDIS_FAULT_DOMAIN_OBSERVABILITY_URL=redis://127.0.0.1:50892/15 \
KIRO_RS_TEST_REDIS_ISOLATED=1 \
KIRO_REDIS_FAULT_DOMAIN_OUTER_ROUNDS=3 \
KIRO_REDIS_FAULT_DOMAIN_SCOPE=redis-fault-domain-product-r2 \
node feature/tests/run-redis-fault-domain-product-validation.mjs
```

结果：

- `result=pass`。
- `outerRounds=3`。
- `exactTests=1`。
- `exactInvocations=3`。
- `internalRoundsPerInvocation=3`。
- 每次 exact invocation 的 Rust test 都是 `1 passed / 0 failed / 0 ignored`。
- business authority redacted 为 `<loopback>:26379`，database 15。
- observability authority redacted 为 `<loopback>:50892`，database 15。
- `protected9022ProbeSkipped=true`。
- `flushDbUsed=false`。
- `dockerUsed=false`。
- `cargoThroughScopedWrapper=true`。
- runner cleanup：`childGroupsStopped=true`、`portsReleased=true`、`tempRemoved=true`。
- scoped cleanup：`validation-build-cleanup scope=redis-fault-domain-product-r2 size_kib=1714572 available_kib=73715612 removed=true reservation_released=true`。

临时原始日志：

- SHA-256：`6df981390efa144677cf87d78f72cf900a103fd6bae72c19986e381822952998`。
- 行数：328。
- 大小：16 KiB。
- 提取本摘要后删除外层临时目录；不提交原始日志。

### 5b. scheduler Redis failure classification 修复后回归通过

在 `scheduler-redis-joint-chaos-20260721-r5` 暴露并修复 deterministic
Redis response/type error 不应按 commit-unknown 处理之后，重跑产品级
fault-domain runner：

```bash
KIRO_REDIS_FAULT_DOMAIN_BUSINESS_URL=redis://127.0.0.1:26379/8 \
KIRO_REDIS_FAULT_DOMAIN_OBSERVABILITY_URL=redis://127.0.0.1:50892/2 \
KIRO_RS_TEST_REDIS_ISOLATED=1 \
KIRO_REDIS_FAULT_DOMAIN_OUTER_ROUNDS=3 \
KIRO_REDIS_FAULT_DOMAIN_SCOPE=redis-fault-domain-product-20260721-r4 \
node feature/tests/run-redis-fault-domain-product-validation.mjs
```

结果：

- `result=pass`。
- `outerRounds=3`。
- `exactTests=1`。
- `exactInvocations=3`。
- `internalRoundsPerInvocation=3`。
- `protected9022ProbeSkipped=true`。
- `flushDbUsed=false`。
- `dockerUsed=false`。
- `cargoThroughScopedWrapper=true`。
- runner cleanup：`childGroupsStopped=true`、`portsReleased=true`、`tempRemoved=true`。
- scoped cleanup：`validation-build-cleanup scope=redis-fault-domain-product-20260721-r4 size_kib=1708364 removed=true reservation_released=true`。

### 6. scoped C0 子集：第一次无效红项

第一次命令未显式使用 Rust 1.92.0：

```bash
feature/tests/run-cargo-scoped.sh redis-fault-domain-c0-r1 -- bash -lc '
set -euo pipefail
cargo fmt --all -- --check
git diff --check
cargo test observability_redis -- --nocapture --test-threads=1
cargo test redis_business_and_observability_fault_domains_are_independent_for_three_rounds -- --exact --nocapture --test-threads=1
cargo check --all-targets
'
```

结果：

- 使用默认 Rust 1.86.0，触发 `if let` chain 的 `E0658` unstable 报错。
- 该结果按用户约束判为无效门禁，不作为代码红项。
- cleanup 正常：`validation-build-cleanup scope=redis-fault-domain-c0-r1 size_kib=1119860 available_kib=73579332 removed=true reservation_released=true`。

### 7. scoped C0 子集：Rust 1.92.0 有效通过

有效命令：

```bash
feature/tests/run-cargo-scoped.sh redis-fault-domain-c0-r2 -- bash -lc '
set -euo pipefail
rustup run 1.92.0 cargo fmt --all -- --check
git diff --check
rustup run 1.92.0 cargo test observability_redis -- --nocapture --test-threads=1
rustup run 1.92.0 cargo test redis_business_and_observability_fault_domains_are_independent_for_three_rounds -- --exact --nocapture --test-threads=1
rustup run 1.92.0 cargo check --all-targets
'
```

结果：

- `cargo fmt --all -- --check` passed。
- `git diff --check` passed。
- `cargo test observability_redis`：4/4 passed，0 failed，0 ignored。
- `cargo test redis_business_and_observability_fault_domains_are_independent_for_three_rounds -- --exact`：该 filter 在本地未设置 `KIRO_REDIS_FAULT_DOMAIN_*` URL 时只作为 compile coverage，显示 `0 tests`；真实动态执行由产品 runner 第 5 节承担。
- `cargo check --all-targets` passed。
- cleanup：`validation-build-cleanup scope=redis-fault-domain-c0-r2 size_kib=2051072 available_kib=73541112 removed=true reservation_released=true`。

### 8. 零残留复核

产品 runner 和 C0 子集结束后复核：

- `find target -maxdepth 1 -type d -name '.validation-build-*'`：无输出。
- `find .git/kiro-validation-build-state -maxdepth 2`：只有 `.git/kiro-validation-build-state`。
- scoped Cargo/rustc 进程：无。
- 根 `target/` 约 `708-711 MiB`，不包含本轮 scoped target；当前根 target 受用户已有服务/编辑器相关产物影响，未清理。

后续低产物补证又清理了无引用的 `target/debug`、`target/flycheck0` 和
`target/.rustc_info.json`，并复核 `node feature/tests/inventory-build-artifacts.mjs --gate`
为 `targets=0 reservations=0 target_processes=0 blockers=0`。该 inventory pass 只说明当前文件系统无验证残留，不替代最终冻结候选后的 release inventory。

## 验收口径

本证据支持以下结论：

- DB 或 key prefix 不算 Redis 故障域隔离；同 authority 必须拒绝。
- 产品路径中 usage/Admin/cleanup 的 Redis 派生层只接收 observability Redis，不回落 business Redis。
- Observability Redis latency/disconnect 不应拖垮 business scheduler。
- Business Redis fault 必须 fail closed，且故障原因不能表现为凭据被持久禁用。
- Business fault recovery 必须尊重 capacity breaker retry-after，不能为了测试把保护退避改成 spin。
- 所有验证构建产物按 scoped wrapper 清理，成功、失败和无效红项均释放 reservation。

仍不支持以下结论：

- 不能声明最终发版通过。
- 不能声明两实例真实服务进程 SIGKILL/restart 已通过。
- 不能声明 external takeover、真实 upstream/Claude CLI、多能力 native tool/search/image/MCP/agent、UI/browser、upgrade、final inventory 已通过。
- 不能声明生产高基数 Redis p95/p99 已消失；生产 recurrence 仍需只读证据。
