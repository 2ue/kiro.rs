# Business Redis And Observability Redis Fault Domain

Status: `implementation-present / product-runner-current-candidate-pass / broader-release-gates-open`

Severity: P0 availability

Last updated: 2026-07-22

## 问题、现象与影响

本专题覆盖主线业务 Redis 与统计/观测 Redis 的故障域隔离。主线业务 Redis 承载 credential selection、scheduler capacity、sticky/session binding、dispatch queue、lease release 等接口调用热路径；观测 Redis 承载 usage summary materialization、Admin cache、余额 cache 和部分 cleanup 派生缓存。两类工作如果共用同一个 Redis server，即使使用不同 DB 或不同 key prefix，仍共享同一个 Redis 单线程和同一个网络/进程故障域。

用户可见现象不一定带有同一个指纹：

- `No account is ready for this request right now`、`Retry after 1 second`、`routeSubtype=local_error_no_fallback`。
- `SchedulerRedisDegraded`、`Redis 调度协调状态不可用`、`selectionFailure.stage=dispatch_queue`。
- 短窗口内出现 `local_all_disabled`，但数据库 `credentials.disabled=false`，实际是调度层无法确认可调度账号，不是账号被真正禁用。
- 并发很高、下游 RPM 不高时，首字变慢、请求排队、内部 scheduler/Redis attempts 被放大，最终流量转外部池或直接 429。
- 无明显 `No account` 文案时，也可能表现为 usage/Admin 查询慢、cleanup 同窗 Redis 慢、scheduler p95/p99 抖动或 external takeover 延迟。

这不是 Claude Code tool hash、schema key 或 transcript sanitizer 的同类问题；那些协议泄漏由转换/历史处理路径负责。本专题是存储故障域问题：统计/观测负载不能压到业务调度热路径。

## 源码链与根因

直接根因是业务 scheduler 与 usage/Admin/cache/cleanup 这类观测工作负载曾可落到同一个 Redis authority。不同 logical DB 或 key prefix 不能隔离 Redis 单线程、slow command、连接池、网络断连、server restart 或 Redis process pause。因此 usage 高基数 HMGET、cleanup 大范围删除、Admin cache 读写或 Redis writer timeout 都可能与 scheduler 的 capacity acquire/session binding 同窗竞争。

当前工作树的修复链如下：

- `src/model/config.rs` 增加 `observabilityRedis`，并通过 `validate_redis_fault_domains()` 拒绝 `redis.url` 与 `observabilityRedis.url` 使用同一 host/port authority。只改 DB 或 keyPrefix 会启动失败。
- `src/storage/redis_cache.rs` 增加 `RedisStoreRole::Business / Observability`，业务 store 保留 scheduler 多连接，observability store 不创建独立 scheduler 热路径连接，并暴露 `server_run_id()`。
- `src/main.rs` 在启动时把文件/环境 storage endpoint 覆盖回 runtime config；业务 Redis 仍是 readiness authority。observability Redis 是可选依赖，连接失败、超时、无法读取 identity 或 `run_id` 与业务 Redis 相同，都会降级为 PostgreSQL/进程内观测，不回落业务 Redis。
- `src/anthropic/usage.rs` 的生产 `UsageRecorder` 只接受可选 observability Redis；未配置时为 PostgreSQL-only，不再把 scheduler Redis 当 usage materialization store。
- `src/admin/service.rs` 的 Admin cache、余额 cache、usage cleanup Redis 阶段只使用 observability Redis；没有 observability Redis 时不回落业务 Redis。
- `src/storage/redis_cache.rs` 的 Redis usage materialization 专用入口在生产期调用 `ensure_observability_usage_store(...)`；即使未来有新调用绕过上层构造器，业务 scheduler Redis 也不能直接执行 usage summary、usage detail snapshot、dashboard series/top、derived cache invalidation 或 usage cleanup 聚合删除。

这条链解决的是故障域共因，不替代 scheduler breaker、usage writer 原子性、cleanup 一致性、external takeover 和两实例协调的独立修复。相关专题见 [Redis scheduler degraded 与 fallback](redis-scheduler-degraded-and-fallback.md)、[Redis usage writer 原子性](redis-usage-writer-atomicity-cardinality-and-scheduler-isolation.md)、[高并发低 RPM 与 runtime quarantine](high-concurrency-low-rpm-runtime-quarantine.md)。

## 复现方案

### R1：配置伪隔离

把配置设为同一 Redis authority 但不同 DB 或 prefix：

```json
{
  "redis": { "url": "redis://127.0.0.1:26379/0", "keyPrefix": "kiro_rs" },
  "observabilityRedis": { "url": "redis://127.0.0.1:26379/15", "keyPrefix": "kiro_rs:observability" }
}
```

验收：`Config::validate_redis_fault_domains()` 返回错误，说明 DB/prefix 不算故障域隔离。

### R2：端口别名但同一 Redis process

通过 tunnel、proxy 或 DNS alias 让两个 URL 看似不同 host/port，实际指向同一个 Redis server。验收：启动阶段 `server_run_id()` 比对失败，observability Redis 不启用，日志明确降级为 PostgreSQL/进程内观测，不回落业务 Redis。

### R3：观测 Redis 慢/断不影响业务调度

使用两个真实 Redis authority 和两个本地 chaos proxy：

```bash
KIRO_REDIS_FAULT_DOMAIN_BUSINESS_URL=redis://127.0.0.1:26379/15 \
KIRO_REDIS_FAULT_DOMAIN_OBSERVABILITY_URL=redis://127.0.0.1:50892/15 \
KIRO_RS_TEST_REDIS_ISOLATED=1 \
node feature/tests/run-redis-fault-domain-product-validation.mjs
```

该 runner 会强制 `KIRO_RS_REQUIRE_STORAGE_TESTS=1`，通过 `feature/tests/run-cargo-scoped.sh` 运行真实 Rust 产品测试。每次 exact invocation 内部再执行三轮：observability latency `50/150/500 ms`、observability disconnect、business disconnect、recovery。缺 URL、DB0、9022、同 authority、未设置 isolation marker 时必须在 proxy/Cargo 前 fail closed。

### R4：基础物理隔离与 namespace cleanup

`node feature/tests/run-redis-fault-domain-validation.mjs` 只验证两个 Redis 端点、`run_id`、prefix bounded cleanup 和基础 proxy 行为。它不启动 kiro.rs、不调用 Cargo、不证明产品 `RedisStore`/`UsageRecorder` 路径，因此只能作为辅助证据。

### R5：多轮/长会话边界

本专题不是 Claude CLI 长会话协议问题。多轮要求由 R3 的 outer rounds 与 Rust 内部三轮故障注入承担；真实 Claude CLI 长会话的 tool/history 泄漏由 [协议 transcript 与工具历史泄漏](protocol-transcript-and-tool-history-leak.md) 关闭。发布前还需要把本专题的隔离结论带入 L3/L4/L5 负载、两实例和 external takeover 门禁，证明隔离不是只在单个 focused test 下成立。

## 修复方案

选定方案是“业务 Redis 必需、观测 Redis 可选且独立、观测失败降级、不回落业务 Redis”：

- `redis.url` 继续作为 scheduler/readiness authority。
- `observabilityRedis.url` 只有在 host/port authority 与业务 Redis 不同，并且启动时 `INFO server run_id` 不同时才启用。
- 未配置或不可证明独立时，usage/Admin/cleanup 的 Redis 派生层关闭，使用 PostgreSQL/进程内状态；这可能降低部分 dashboard/cache 性能，但不能污染 scheduler 热路径。
- 配置迁移和环境变量只覆盖 storage endpoint，不让 PostgreSQL runtime config 中的旧 URL 重新选择错误 Redis。
- 所有 runtime/validation runner 只使用随机 prefix 或 caller-owned DB；不触碰 `127.0.0.1:9022`，不启动 Docker，不用 `FLUSHDB` 清 shared DB。

没有选择“不同 DB/prefix 也允许”的方案，因为它不能隔离 Redis 单线程和 slow command。也没有选择“observability Redis 故障时临时回落业务 Redis”，因为这会在异常时把观测压力重新打回 scheduler。

## 验证与证据

已具备的静态/源码证据：

- `observabilityRedis` 配置和环境变量已加入 `config.example.json` 与 `README.md`。
- `Config::validate_redis_fault_domains()` 覆盖 optional、same authority/different DB、loopback alias、distinct authority 和 invalid business authority。
- startup 有 business/observability `run_id` 比对；readiness 不依赖 observability Redis。
- `UsageRecorder` 与 `AdminService` 有 debug assertion，防止传入 business Redis store。

新增验证程序：

- `feature/tests/run-redis-fault-domain-product-validation.mjs`：产品级动态 runner，启动两个 loopback proxy 并强制执行 Rust exact test。
- `feature/tests/run-redis-fault-domain-product-validation.contract.test.mjs`：纯 Node 合同测试，覆盖缺 URL、未确认隔离、DB0、9022、同 authority、非 loopback、非法 rounds/scope 的三轮早拒绝，并检查 runner 不调用 Docker、不使用 `FLUSHDB`、不探测 9022。若显式提供两个 live Redis URL，可额外验证 HUP/INT/TERM 时自有 proxy、端口、temp root 清理。

当前补充证据：

- 纯 Node contract 默认运行初始为 37 tests，28 passed，9 skipped（live signal cases 未给 Redis URL），0 failed。2026-07-21 后续新增生产源码合同、RedisStore role guard 和主/观测 Redis 路径隔离合同后，默认合同为 46 tests，37 passed，9 skipped，0 failed；新增合同锁定 `main.rs`、`UsageRecorder`、`AdminService`、`RedisStoreRole`、Redis usage materialization entrypoint guard、config env/authority guard、scheduler/external/runtime-event/health 的 business Redis 专用路径、observability Redis 启动失败不回落 business Redis、以及 UsageRecorder 主请求路径只入队观测 writer 且压力下丢弃 summary 以避免阻塞。
- 新增底层 production guard 后，`feature/tests/run-cargo-scoped.sh redis-observability-role-guard-20260721 -- cargo +1.92.0 check --bin kiro-rs` 通过，wrapper cleanup `size_kib=446876 removed=true reservation_released=true`。最新 scheduler chaos + fault-domain 合批合同为 74 tests：53 passed，21 live-fixture skips，0 failed。随后只删除无引用 root `target/debug`/`target/flycheck0` 可再生产物，inventory 复核 `targets=0 reservations=0 target_processes=0 blockers=0`。
- 纯 Node contract 使用当前项目两个 loopback Redis URL 后：37/37 passed，覆盖 HUP/INT/TERM 各三轮 proxy/temp/port cleanup。
- 基础 Redis fault-domain runner：3 outer rounds passed，确认两个 Redis `run_id` 不同、observability 250ms latency/disconnect 不影响 business 基础操作、business fault fail closed、bounded cleanup 不用 `FLUSHDB`。
- 产品 runner `redis-fault-domain-product-r1` 先红于 business Redis fault 后立即 recovery acquire；该红项是测试合同过严。business Redis fault 后保留 `retry_after` 退避是防 spin 保护，已改为使用 `recover_capacity_breaker_five_times()` 验证 5/5 恢复。
- 产品 runner `redis-fault-domain-product-r2` 通过：3 outer rounds × 1 exact test × 内部 3 轮，exact invocations 3/3 passed，`protected9022ProbeSkipped=true`、`flushDbUsed=false`、`dockerUsed=false`、`cargoThroughScopedWrapper=true`，scoped cleanup `size_kib=1714572 removed=true reservation_released=true`。
- 2026-07-21 在 scheduler Redis failure classification 修复后又重跑产品 gate：`redis-fault-domain-product-20260721-r4` 使用业务 `redis://127.0.0.1:26379/8` 与观测 `redis://127.0.0.1:50892/2`，3 outer × 1 exact × 内部 3 轮通过；`dockerUsed=false`、`flushDbUsed=false`、`protected9022ProbeSkipped=true`、端口/temp 清理完成，scoped cleanup `size_kib=1708364 removed=true reservation_released=true`。
- 2026-07-22 当前候选再次复跑：
  - `node --test feature/tests/run-redis-fault-domain-product-validation.contract.test.mjs`：46 tests，37 pass / 9 live-signal skips / 0 fail；
  - `feature/tests/run-redis-fault-domain-product-validation.mjs`：业务 `redis://127.0.0.1:26379/8`，观测 `redis://127.0.0.1:50892/2`，3 outer × 1 exact × 内部 3 轮通过；
  - runner 明确记录 `dockerUsed=false`、`flushDbUsed=false`、`protected9022ProbeSkipped=true`，child groups、ports、temp root 和 scoped target 均清理完成；
  - 该轮验证 observability latency/disconnect 不影响 business scheduler；business Redis fault fail closed 且不伪装 `AllDisabled`；observability failure 不回落到 business Redis。
- Rust 1.92.0 scoped C0 子集 `redis-fault-domain-c0-r2` 通过：`cargo fmt --all -- --check`、`git diff --check`、`cargo test observability_redis` 4/4、`cargo check --all-targets`。scope `size_kib=2051072 removed=true reservation_released=true`。
- 一次 `redis-fault-domain-c0-r1` 使用默认 Rust 1.86.0 触发 `if let` chain `E0658`，按用户“必须 Rust 1.92.0”约束判为无效门禁，不作为源码红项；该批同样完成 `removed=true / reservation_released=true`。
- 证据文件：[业务/观测 Redis 故障域产品验证 2026-07-21](../evidence/business-observability-redis-fault-domain-20260721.md)。

## 发布验收

- 配置层拒绝同 Redis authority，即使 DB/prefix 不同。
- 启动层拒绝看似不同但 `run_id` 相同的 Redis server。
- Observability Redis latency/disconnect 期间，business scheduler acquire 不增加 scheduler breaker failure，不返回 `AllDisabled`，恢复后 5/5 normal。当前产品 runner 已通过。
- Business Redis latency/disconnect 期间，scheduler fail closed 且分类不能伪装为 credential disabled；observability usage write 仍可用。当前产品 runner 已通过。
- 未配置 observability Redis 时，usage/Admin/cleanup 不使用业务 Redis materialization；readiness 仍只依赖业务 Redis/PostgreSQL/event bus。当前源码链和 focused tests 已覆盖，最终仍需 frozen candidate 复绑。
- Runner 不启动 Docker、不探测 9022、不把 secret URL 放到 argv、不执行全库 flush；所有临时 proxy、target、reservation 在成功、失败和信号路径下清理。当前 contract/product/C0 均已通过。
- 该专题通过后仍不能替代两实例真实服务、external takeover、生产高基数、UI/browser、upgrade 和最终 frozen release gates。

## 残余风险、限制与回滚

隔离观测 Redis 会降低共因风险，但不能消除业务 Redis 自身的 scheduler hot-path 问题。业务 Redis 仍需要 breaker、bounded queue、lease release、two-instance fencing 和 L3/L5 负载门禁证明。

如果 observability Redis 配置错误，系统会降级到 PostgreSQL/进程内观测，可能导致 Admin cache 命中率下降或 usage dashboard Redis materialization 缺失；这是可接受的可用性取舍。回滚只能删除或修正 `observabilityRedis`，不得把 observability fallback 指向业务 Redis。

跨实例总并发仍取决于所有进程写入同一个 observability Redis 的 aggregate 负载；当前产品 runner 只验证单进程 `RedisStore`、两个独立 Redis fault domains 和产品注入路径。多进程真实服务 SIGKILL/restart、external takeover、生产高基数和最终 release soak 仍是发布阻断。
