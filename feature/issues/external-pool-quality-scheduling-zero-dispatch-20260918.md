# 2026-09-18 外部池质量感知调度导致零派发（v0.0.162 / v0.0.163）

Status: `incident-confirmed / compatibility-and-cooldown-fixed / selector-guard-fixed / coordinator-path-unconfirmed / local-focused-validated`

Severity: P0

影响版本：`v0.0.162`、`v0.0.163`（两者二进制行为一致）<br>
现场恢复版本：`v0.0.161`（不等同于已证明该版本在所有环境均不受影响）<br>
引入变更：`4c7790b feat(external-pool): quality-aware scheduling for external accounts`

本轮核对了源码、版本差异、真实 PostgreSQL/Redis 测试和 mock 上游调度链路。
没有连接生产环境，也没有修改生产 Redis；本地测试使用项目指定的
`127.0.0.1:25432` PostgreSQL、`127.0.0.1:26379` Redis 和临时 fake upstream。
现网现象来自用户的生产观察，仍未在本地稳定复现。

## 现象与影响

用户环境有 3 个外部池账号，账号本身可正常调通（偶发报错，偶尔连续报错）。
从 `v0.0.161` 升级到 `v0.0.163` 后：

- 外部池账号一个请求都调度不上，**usage 页面 RPM 为 0，用户观察到使用记录为空**
- 账号被持续标记为冷却
- 持续观察至少 5 分钟不恢复（期间确认一直有下游流量）
- 回退到 `v0.0.162`：冷却经过一段时间消失，但外部池仍无 upstream/usage 流量
- 回退到 `v0.0.161`：**流量立即恢复正常**，全程未修改任何配置

影响是：外部池在质量感知调度开启时可能完全不可用，本地账号不可用时
失去兜底能力，且故障表现为稳定态而非间歇抖动，等待不会自愈。

## 版本差异澄清

`v0.0.162` 与 `v0.0.163` 之间不存在代码差异：

```
git rev-list v0.0.162..v0.0.163   → 仅 5313bc1 chore(release): 0.0.163
git diff  v0.0.162  v0.0.163      → 仅 Cargo.toml / Cargo.lock 版本号
```

因此「163 冷却、162 不冷却」不能直接当作二进制行为差异。更稳妥的解释是：
版本切换过程中 Redis 的 cooldown/probation/transient-failure 状态和 coordinator
熔断状态发生了演化。`v0.0.162` 与 `v0.0.163` 的业务代码相同，真正的业务变化
发生在 `v0.0.161` → `v0.0.162`。

注意：这只能说明 162/163 没有业务代码差异，不能单凭版本号证明故障一定由 Redis
状态引起；还需要现场状态键和 coordinator 日志来闭环。

## 结论校正

### 已确认的代码缺陷与修复

1. 质量感知调度的默认值曾为 `true`，与升级兼容要求不符。当前 Rust、UI、Admin UI
   默认值已统一为 `false`；只有显式配置 `true` 才会改变选择并启动被动采样。
2. 普通瞬态失败原先可按 streak 阈值升级为池级 hard cooldown。该路径已移除：
   `429/5xx/网络/协议` 等普通抖动只保留 transient streak 排序信号，不会在最外层过滤账号。
3. 质量采样和 probation 写入原先可能与正式调度 Redis 资源争用。当前已使用独立
   quality connection manager，并以 32 个 detached task 的有界 semaphore 限制；超限丢弃
   质量样本，不等待、不阻塞 coordinator/lease 正式链路。
4. selector 调用方原先在 `available_pools > 0 && selected_pool == None` 时无条件继续；
   当前改为记录 invariant 错误并返回有界 503，避免潜在无限空转。
5. probation 已移到候选扫描阶段，基线使用本次请求已经通过路径、模型、cooldown、并发
   和 runtime 准入的整个候选集合。全体候选一起变差时，中位数同步变化，不会按单账号
   固定红线把整个池打入 probation；probation 仍是软降权，不是 hard cooldown。

上述修复不等于已证明现网唯一根因。现网 Redis coordinator 延迟/熔断路径仍需生产证据
闭环。

### 已证伪：`selector 返回 None` 不是当前实现的主根因

质量感知调度主开关在受影响版本中默认开启，但这本身不能证明 selector 会返回非法池。
以下结论是基于调用链和测试重新核验后的校正。

`scan_pool_availability_from_filtered_pools_and_runtimes` 会先执行 `skip_reason`；
只有已通过 cooldown、并发和 enabled 准入的池才会进入 `ExternalPoolCandidate`。

`probe_candidates` 虽然在优先级分层前构造，但来源仍是这批已通过准入过滤的
candidates；它不会把不可派发池重新加入候选。

调用方确实有 `available_pools > 0` 时的 `continue` 防御分支：

```rust
let Some(pool) = selection.selected_pool else {
    let snapshot = selection.availability;
    if snapshot.has_temporary_unavailable_pool() { ... }
    if snapshot.available_pools > 0 {
        continue;
    }
    break;
};
```

这个分支未来仍可能在 selector 不变量被破坏时空转，但当前实现中非空 candidates
的 selector 路径有明确的 `Some` 返回保障。并且 `v0.0.161` 也存在同样的调用方
分支，不能把它作为 162/163 新增的根因。

### 已确认：正式 dispatch 失败可以发生在 upstream HTTP 之前

正式路径在发出上游请求前还要经过：

```text
runtime snapshot
-> coordinator breaker
-> Redis lease acquire
-> PostgreSQL dispatch fence
-> external HTTP dispatch
```

runtime snapshot 或 lease coordinator 失败时，显示直连、无本地兜底的请求会得到合成容量错误，
不会命中 fake/upstream。由此可以同时出现：下游请求一直有流量、外部 upstream/RPM 为 0、
账号看起来仍可用但没有正式外部 HTTP 派发。

raw preflight 看到 eligible pool 也不能证明正式 dispatch 一定能拿到 lease，因为正式路径
会重新读取运行态并重新经过 coordinator。

## 尚未闭环的现网候选根因

### 质量采样 detached task 可能拖慢 coordinator/lease Redis 连接

受影响版本质量感知默认开启；每次成功或质量相关失败都会创建 detached `tokio::spawn`，
执行带 500ms timeout 的 Redis Lua `EVAL`。

该任务具有以下风险：

- 受影响版本每个请求都创建 detached task，没有 semaphore、bounded channel 或 coalescing；
- 每次质量样本都是 Redis Lua `EVAL`；
- 受影响版本质量采样与 coordinator snapshot、lease acquire、dispatch fence 共用
  `scheduler_capacity_manager` 对应的 Redis `ConnectionManager`；
- `ConnectionManager` clone 共享同一底层 multiplexed connection。超时只取消等待响应，
  不会取消已经发往 Redis 的命令。

在持续下游流量下，可能形成：

```text
质量 EVAL detached task 累积
-> scheduler multiplexed connection 排队/延迟
-> runtime snapshot / lease acquire 超过 2 秒
-> coordinator breaker 打开或 fail-fast
-> 正式 external dispatch 为 0
-> 下游持续收到请求但 upstream/RPM 为 0
```

这是当前最符合用户现象的候选解释，但还不是已确认根因。需要生产日志中的
coordinator timeout/breaker、质量采样超时/排队指标，以及 Redis `SLOWLOG` 或命令延迟
证据才能闭环。

### 为什么 161 可能立即恢复、162 可能延迟恢复

161 不读取质量键，也不创建质量采样任务；切回 161 后，旧质量状态不再参与选择，
同时新的质量 EVAL 洪峰停止，因而可能很快恢复。

162 虽然和 163 代码相同，但降级到 162 后已产生的 Redis 状态和连接队列不会立即消失，
所以可能出现“冷却稍后消失，但仍无 upstream 流量”的过渡期。这个解释仍需现场证据
确认，不能把版本切换本身当成清理 Redis 状态的操作。

### 放大器：probation 与瞬态失败连击的双重降权

用户账号「偶发报错、偶尔连续报错」会同时触发两套独立降权：

- `transient_failure_streak` 抬高**有效优先级**：
  `effective_priority = priority + streak × transient_failure_priority_penalty`，
  penalty 默认 **20**（`src/model/config.rs:4757`）。连击 1 次即等于优先级 +20。
- 质量系统将其打入 probation。

选择逻辑第一步按有效优先级**硬分层**，只取最优层。3 个账号若连击次数不同
（偶发报错下几乎必然），有效优先级会被 20 的步长拉开，**每层仅剩一个账号**。
此时：

- 质量分只在层内重排，层内单成员，重排无意义
- 「全体劣化不降级任何人」的保护依赖同层多候选互比中位数，层内单成员时失效
- 该唯一成员一旦越过失败率阈值即进入 180s 避让，而它是当前唯一最优层候选

代码确实为“被连击挤出最优层的 probation 池”提供探测流量补偿；这会改变流量分布，
但不会把已通过准入过滤的 candidate 变成不可派发池，也不会单独解释 upstream 为 0。

### 已修复的设计缺陷：probation 判定使用绝对阈值

`evaluate_external_pool_degrade`（`src/external_pool.rs:10313`）判定 probation
使用**绝对失败率阈值**：

- `degrade_window_secs` 默认 120 秒
- `degrade_error_rate_threshold` 默认 **0.5**（窗口内失败率 ≥ 50%）
- `quality_min_samples` 默认 5
- `degrade_probation_secs` 默认 180 秒，指数退避，`max_probation_secs` 上限 900 秒

这与同文件内延迟评分的处理**自相矛盾**：延迟部分已经是相对候选集中位数的
（`ExternalPoolQualityBaseline::from_candidates`，`src/external_pool.rs:10152`，
注释明确指出绝对阈值会系统性偏袒跑小模型的账号），但 probation 判定退回了绝对值。

旧实现的采样回调确实拿不到全局候选集，因此不能在单个样本返回时直接按同伴比较；
当前已将 probation 判定移到候选扫描阶段，使用当前请求的完整准入 cohort。

用户指出的原则正确 —— **账号质量不应自己和自己比较，应与其他可派发账号比较**。
上游整体抖动、所有账号都失败多/都慢时，绝对阈值会让 3 个账号**全部**越过
50% 线、**全部**进入 probation，即所谓「设一个硬门槛，所有账号一起炸」。

## Usage / RPM 语义澄清

初稿把“没有 upstream 请求”与“usage 一条都没有”直接等同，这个结论过于绝对。

`record_external_failure()` 在多条合成失败路径中都会调用 `record_external()`，
并以 `UsageRecordStatus::Error` 写入 recorder；`UsageRecorder::record()` 也接受错误记录。
因此：

- 外部 upstream/RPM 为 0，可以表示正式 external HTTP 没有发出；
- usage 明细是否显示，还取决于请求是否走到合成失败记录、记录是否被持久化，以及
  dashboard 是否只统计成功记录；
- 现网“usage 为空”应通过 PostgreSQL 原始记录和 dashboard 查询条件区分验证，
  不能从 selector 结论推导。

## 排除项

以下 `v0.0.161` → `v0.0.162` 的变更已确认与本问题无关：

- **本地账号狂暴轮换 / region 轮换**（`e6d6397`…`5c6375f`）：
  `kiro_upstream_region_rotation_enabled` 与 `local_berserk_mode_enabled`
  默认均为 `false`（`src/model/config.rs:3755` 附近 `#[serde(default)]`），未开启不生效。
- **`60eb117 fix: preserve credential warmup across capacity updates`**：
  修复「改账号容量会重置 warmup」，方向正确，不会导致不可调度。
- **SSE 保活改造**（`src/anthropic/handlers.rs`）：仅影响已建立的流，与路由准入无关。
- **热路径 Lua ARGV / KEYS 索引**：已逐项核对
  `src/storage/redis_cache.rs:4928` 脚本的 ARGV 布局、`redis_key_count`
  与 `expected_values` 算术，三者自洽，不存在错位。
- **`decode_pool_runtime_snapshot`**（`src/external_pool.rs:11457`）：与 161 一致。

## 立即止血

无需改代码，仅调整 runtime config：

```
externalPoolQualityAwareSchedulingEnabled = false
```

关闭后 `select_external_pool_candidate` 第一行直接走
`select_external_pool_candidate_legacy`（`src/external_pool.rs:10428`），
与 `v0.0.161` 逐字节等价。

重新开启前应在确认影响范围后清理残留状态，否则会继承旧的 `probation_level`：

```
DEL external_pool:*:quality
```

管理端「清除冷却」亦可达成同样效果（`clear_pool_cooldowns`
已包含该键，`src/external_pool.rs:8760`）。

## 已落地修复与后续工作

### 1. 给 selector 调用方增加有界防御（已落地）

`available_pools > 0 && selected_pool == None` 当前不是正常可达状态，调用方现在记录
selector invariant 并返回合成容量错误，不再无限 `continue`。

风险：低。需要确认错误类型与 existing capacity-wait 语义，避免把 coordinator unavailable
误判为 selector invariant。

### 2. 质量采样与 coordinator/lease 资源隔离（已落地）

质量采样使用独立 Redis connection manager，并有 32 个后台任务硬上限；达到上限丢弃
样本。质量采样是优化数据，不能占满正式调度协调资源。

必须新增 `accepted / dropped / in_flight / timeout / latency` 指标，并验证采样洪峰后
coordinator snapshot、lease acquire、dispatch fence 的 p95/p99 和错误率。

### 3. 对质量采样做合并或降采样（P1）

保留 EWMA/窗口语义的前提下，可按池做时间桶聚合、固定比例采样或只保留最新状态，
避免每个请求一个 Lua EVAL。聚合必须有明确的失败率统计偏差说明。

### 4. probation 判定改为相对同伴（已落地）

与延迟评分一致，改用当前请求候选集中位数而非单账号绝对红线；全体一起劣化时不
降级任何账号。判定从采样回调移到候选扫描路径，天然持有准入后的全量候选。

风险：中。改动面较大，需要重新审视 `degrade_window` 语义与
`src/external_pool/tests.rs` 中既有的质量调度回归。

`transient_failure_priority_penalty` = 20 仍会影响选择层级，但 probation 已不再只依赖
单一有效优先级层；质量 cohort 先按当前请求准入集合建立，选择重排仍保持优先级硬分层。

## 已执行验证

### 代码与版本核验

- `v0.0.162` → `v0.0.163`：只有版本号变更，无业务代码差异。
- `v0.0.161` → `v0.0.162`：包含质量感知调度提交
  `4c7790b` 和真实调度测试提交 `5d49a6e`。
- 161 调用方同样存在 `available_pools > 0` 分支；初稿的版本定位错误。

### 本地测试

通过：

- 质量 selector 聚焦测试：`15 passed / 0 failed`；
- coordinator breaker 定向测试：`3 passed / 0 failed`；
- L2 质量调度：`6 passed / 1 failed`。失败项是恢复阶段随机份额断言，
  末轮份额 `0.234` 低于测试下限 `0.25`，不是零派发，也没有 upstream 全部为 0 的现象。

未通过/未完成：

- 尚未完成“持续外部流量 + 质量采样洪峰 + coordinator 延迟/断连”的服务级复现；
- 尚未从生产 PostgreSQL/Redis 取得故障窗口原始记录和状态键。

## 待补证据

- 生产环境 `externalPoolTransientFailureCooldownThreshold` 与
  `externalPoolTransientFailurePriorityPenalty` 实际值
  （默认分别为 0 与 20，`src/model/config.rs:4761`、`4757`）
- 故障期间 `external_pool:*:quality` 键内容与 `probation_level`
- 故障期间的 `dispatch_deadline`、`coordinator_unavailable`、`lease_acquire_timeout`
  和 coordinator breaker 日志；
- 本地最小复现：3 池 + 不同 `transient_failure_streak` + 已写入 probation 状态
- 质量采样任务 pending/in-flight/dropped/timeout 计数；
- Redis `SLOWLOG GET`、命令延迟、连接事件和 `external_pool:*:quality` 键内容；
- PostgreSQL usage 原始行（包括 `status=error`）与 dashboard RPM 查询口径。

## 残余风险

- 本文档已排除 selector `None` 作为主根因，但 coordinator/Redis 资源争用仍未完成
  服务级复现。修复前应先补一条能稳定复现“upstream hit=0、下游持续有流量”的回归测试。
- 当前实现已对质量采样任务设置 32 个并发上限并与正式调度连接隔离；仍缺少现网
  coordinator 延迟、采样 dropped/timeout 计数的生产证据。
- `v0.0.162` / `v0.0.163` 已发布，使用默认配置的部署均可能受影响。
