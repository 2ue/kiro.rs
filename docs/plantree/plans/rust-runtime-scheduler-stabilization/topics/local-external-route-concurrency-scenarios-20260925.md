# 本地账号与外部池路由并发场景梳理

- 分析日期：2026-09-25 Asia/Shanghai
- 代码基线：`main`，`f1fcca4`，版本 `0.0.171`
- 分析方式：源代码核对；本次未修改运行时代码，未启动服务，未做生产压测
- 相关主题：[外部池与本地凭证调度剩余项](external-pool-local-first-scheduler.md)

## 结论先行

1. 本地调度器和外部池调度器是两个独立容量域。一个域的候选排序、并发槽和队列，不会直接占用另一个域的槽位或队列。
2. 但所有 `/messages` 路由在进入 handler 前都经过同一个 `RequestAdmissionController`；该准入状态是**每个实例、每个请求 API Key 独立**的，不是整个进程所有租户共享的总闸门。
3. 同一个 API Key 的本地请求如果已经拿到入口准入许可、随后在本地账号调度器排队，那么这些请求会继续持有入口准入并发许可，直到响应 body 结束或失败。因此，本地排队可以间接把同一个 API Key 的外部请求挡在入口准入层。
4. 不同 API Key 之间不会因为上述入口准入计数互相阻塞；它们仍可能分别受到各自的本地池、外部池、Redis、CPU/网络资源影响。
5. “本地容量超过 30 后是否立即转外部”没有一个脱离配置的固定答案。默认配置下，若路由允许外部池、外部池对当前模型可立即承接、且本地预检开启，代码会先做最多 250ms 的本地容量预检，然后把请求送入外部池；否则会继续本地有界排队/失败，或在本地错误被分类为可回退后再转外部。
6. 这里的“30”必须先确认是**单账号容量**、**本地全局并发容量**还是**本地调度等待队列容量**。三者触发条件和结果不同。

## 请求实际经过的层次

```text
认证
  -> RequestAdmission（按 API Key）
  -> 路由策略 / direct policy / local preflight
  -> 本地凭证调度器 或 外部池调度器
  -> 上游请求与响应流
```

每个内置 `/v1`、`/na/v1`、`/cc/v1`、`/ha/v1`、`/dfcache/.../v1` 的 `/messages` 路由都挂载了请求准入 middleware；认证层在外，确保准入按认证后的 API Key 归属。见 `src/anthropic/router.rs:282-294`、`:307-319`、`:332-344`、`:357-369`、`:389-404`。

### Layer A：RequestAdmission

`RequestAdmissionConfig` 的语义是“每个实例、每个请求 API Key”，默认值为 `maxConcurrentRequests=32`、`maxQueuedRequests=64`、`queueTimeoutMs=1000`。它不经过 Redis，多实例时同一 Key 的总上限近似为各实例上限之和。见 `src/model/config.rs:3360-3377`。

准入顺序是先扣 RPM，再获取并发许可。见 `src/anthropic/request_admission.rs:491-517`。

一旦获取并发许可，permit 被包装进 response body；只有 body 正常结束或出错时才释放。也就是说，handler 内部等待本地凭证、外部池或上游响应期间，许可仍然存在。见 `src/anthropic/request_admission.rs:993-1005`、`:1019-1059`、`:1078-1089`。

### Layer B：本地与外部是分离的

- 本地账号使用本地凭证并发、全局本地 dispatch 并发、本地 dispatch 等待队列和本地等待时间。默认本地全局 dispatch 并发为 512，等待队列为 30，凭证调度等待上限为 5 秒。见 `src/model/config.rs:3645-3650`、`:3795-3801`、`:4191-4193`、`:4258-4264`。
- 外部池使用独立的 `externalPoolGlobalMaxConcurrentRequests`、`externalPoolMaxQueuedRequests`、容量模式和等待时间。默认外部全局并发为 512，外部等待队列为 10，外部等待上限为 5 秒。见 `src/model/config.rs:4659-4668`。
- 外部池容量为 `FailFast` 时不会等待外部队列；只有 `Wait` 才会进入外部池队列，队列满或等待超时会返回外部容量错误。见 `src/external_pool.rs:8066-8090`、`:8092-8174`。

## 场景一：部分路由只允许本地，部分路由只允许外部

### 场景一览

| 场景 | 同一 API Key 的入口准入 | 本地调度器 | 外部调度器 | 结论 |
|---|---|---|---|---|
| 本地-only 路由，本地容量正常 | 先取得 Layer A permit | 直接抢本地槽 | 不进入 | 外部池排序不会影响该请求 |
| 本地-only 路由，本地容量满 | 先取得 Layer A permit，然后可能在本地排队 | 本地队列/超时/队列满 | 不进入 | 该请求不会改走外部 |
| external-only/direct 路由，外部容量正常 | 先取得 Layer A permit | 不进入 | 外部候选排序和外部槽位 | 本地队列不会直接影响外部 |
| external-only/direct 路由，外部容量满 | 先取得 Layer A permit | 不进入，严格 direct 不回本地 | `FailFast` 直接失败；`Wait` 进入外部队列 | 不会因为本地空闲而改走本地 |
| 本地-only 请求在本地排队，同时同 Key 发 external-only 请求 | 前者已持有 Layer A permit；后者与其竞争同一 Key 的 Layer A | 本地队列仍独立 | 外部可能健康 | **本地排队可能间接阻塞外部请求，但发生在 Layer A，不是外部池被本地占用** |
| 本地-only 请求在本地排队，同时不同 Key 发 external-only 请求 | 不同 Key 有不同 Layer A gate | 本地队列独立 | 外部独立 | 不会通过 RequestAdmission 互相阻塞 |

### 对问题 1 的直接回答

**“本地账号进入排队，外部池正常，外部请求会不会也排队？”**

- **不会直接排在外部池队列里。** 本地请求不会占用外部池的并发 lease，也不会参与外部池候选排序。
- **可能在入口先排队。** 如果本地排队请求和外部请求使用同一个请求 API Key，并且这些本地请求已经持有 Layer A 的并发许可，那么同 Key 的外部请求会先在 RequestAdmission 排队或被拒绝。此时看起来像“外部池也排队”，实际排的是 API Key 入口准入。
- **不同 API Key 不会发生这种入口级互相阻塞。**
- 如果关闭 RequestAdmission 并发限制，或该 Key 仍有足够入口并发余量，本地队列不会阻止外部请求进入外部池。

因此必须把“排队位置”记录清楚：

```text
本地账号队列       != 外部池队列       != API Key 入口准入队列
```

三者都可能表现为请求变慢，但因果完全不同。

## 场景二：所有路由都允许本地和外部

### 先区分“30”是什么

#### A. 单个本地账号并发上限为 30

- 如果还有其它本地账号可调度，本地调度器可能换到其它账号，未必触发外部 fallback。
- 只有当前模型下所有本地候选都没有可用 dispatchable 槽位，才进入 `CapacityFull` 语义。

#### B. 本地全局并发上限为 30

- 第 31 个请求会看到本地全局容量不足，更容易触发本地容量预检。
- 这不是单账号满，而是整个本地 dispatch 域满。

#### C. 本地等待队列上限为 30

- 这表示已有请求在本地等待容量的数量上限，不等于本地并发槽只有 30。
- 队列满时，本地调度器会直接返回“账号调度等待队列已满”，之后是否外部 fallback 由错误分类和路由配置决定。

### 默认配置下的实际决策

默认情况下：

```text
fallback_on_local_capacity_exhausted = true
local_pool_preflight_enabled = true
external_pool_route_mode = allow_all
external_pool_capacity_mode = fail_fast
```

当本地 fresh route state 变为 `CapacityFull` 且当前请求允许外部时：

1. 先执行本地容量预检；
2. 对容量满，最多等待 250ms，看本地是否自然释放；
3. 如果本地恢复，继续留在本地，不做不必要的外部切换；
4. 如果本地仍满，只有在外部池对当前路由和模型**可立即承接**时，才直接进入外部 fallback；
5. 外部池也没有可立即承接的池时，不会假装外部可用，而是继续本地受控路径，随后可能进入本地 dispatch 队列、等待超时，或按本地错误分类再尝试外部；
6. 外部池自身如果容量满，`FailFast` 返回容量错误；改为 `Wait` 才会进入外部队列，并受 `externalPoolMaxQueuedRequests` 和等待上限约束。

对应实现：

- `local_pool_preflight_enabled` 和 `external_pool_enabled_for_endpoint` 是 raw/parsed preflight 的前置条件：`src/anthropic/handlers.rs:1338-1347`、`:1861-1884`。
- 容量预检等待上限被硬限制为 250ms：`src/anthropic/handlers.rs:2299-2309`。
- 本地容量满会分类为 `local_capacity_full`，但只有 `fallback_on_local_capacity_exhausted=true` 才进入外部回退语义：`src/anthropic/handlers.rs:2241-2273`。
- 对容量类原因，外部必须是“立即可用”而不是仅仅“存在候选池”：`src/anthropic/handlers.rs:1784-1790`。

### 对问题 2 的直接回答

**“本地账号并发容量只有 30，超过 30 后会直接打外部，还是等待本地排队？”**

正确答案是：**两者都可能，当前实现按状态和配置决定；默认不是无条件立即外部，也不是无条件先排本地。**

在默认配置且外部池可立即承接时，典型行为是：

```text
本地 30 槽占满
  -> 最多等待 250ms 等本地自然释放
  -> 仍满 + 外部当前模型/路由可立即承接
  -> 进入外部池
```

以下任一条件成立时，不能假设会立即转外部：

- 路由的 `external_pool_route_mode/rules` 不允许该入口进入外部；
- 外部池未启用，或没有支持当前模型的候选；
- 外部池有候选但当前也不可立即承接；
- `local_pool_preflight_enabled=false`；
- `fallback_on_local_capacity_exhausted=false`；
- 请求已经进入本地 provider 调用阶段，当前本地 acquire mode 选择等待；
- 本地调度等待队列已满、Redis 协调降级或本地错误不满足外部 fallback 分类；
- 同 API Key 的 Layer A 已满，请求尚未走到本地/外部二选一。

## 场景三：外部池排序、容量和本地容量的边界

### 外部池排序不会“卡住本地”

外部池候选排序只发生在外部候选集合内部。它可能改变：

- 选哪个外部池；
- 是否因质量/优先级/负载暂时跳过某个外部池；
- 外部池内部 failover 的顺序。

它不会：

- 占用本地凭证槽；
- 把本地请求放进本地队列；
- 让 strict external direct 请求回到本地。

### 本地全局容量会“卡住本地所有入口”

所有允许本地的路由共享本地凭证调度器的全局容量和等待队列。两个不同 endpoint 即使路由规则不同，只要最终进入本地调度域，就会竞争本地全局容量。

### 外部全局容量会“卡住外部所有入口”

所有允许进入同一外部池管理器的路由共享外部池容量和外部队列。一个 external-only 路由的外部积压会影响其它 external-eligible 路由，但不影响纯本地请求的本地槽位。

## 场景四：多 API Key 能否实现路由并发隔离

### 当前已经支持的部分

当前 `RequestAdmissionController` 按“实例 + 请求 API Key”保存并发、排队和 RPM 状态。不同 API Key 不共享这一层的 `active`、`queued` 和 RPM bucket，因此：

- `key-local-a` 的入口排队不会直接占用 `key-external-a` 的入口并发；
- external-only 请求使用独立 Key 后，可以绕过 local-only 请求造成的同 Key Layer A 阻塞；
- 不同 Key 的入口队列长度、入口超时和入口拒绝计数互相独立。

### 当前没有支持的部分

多 Key **不会**自动创建下面这些独立资源：

1. **本地账号调度池不会按 Key 拆分。**
   - 所有允许本地调度的路由最终使用同一个 `KiroProvider` / 本地凭证管理器。
   - 本地账号的全局并发、单账号槽位、dispatch queue 和 Redis 调度协调状态仍然是共享的。
   - 因此，`key-local-a` 的本地排队仍可能占用本地全局 dispatch queue，影响 `key-local-b` 的本地请求。

2. **外部池管理器不会按 Key 拆分。**
   - 所有 external-eligible 路由进入同一外部池管理器和外部容量协调域。
   - `key-external-a` 的外部积压仍可能占用外部全局容量或外部等待队列，影响 `key-external-b`。
   - 外部池的 route rules 只负责候选是否允许该 endpoint，不等于为每个 endpoint 创建独立容量池。

3. **不同 Key 目前不能配置不同的入口上限。**
   - `RequestAdmissionConfig` 是一份运行时配置，`maxConcurrentRequests`、`maxQueuedRequests`、`queueTimeoutMs` 和 `rpm` 对所有 Key 使用同一组数值。
   - 状态按 Key 分开，但策略参数不是按 Key 分开。
   - 使用 10 个 Key 会把这层“每 Key 上限”复制 10 份；如果没有额外总量限制，入口总并发可能约放大到 10 倍。

### 对“10 个路由”的直接判断

假设：

```text
路由 R1、R2：只允许本地账号
路由 R3、R4、R5：只允许外部池
路由 R6-R10：允许本地和外部
```

| 做法 | 能否隔离入口准入 | 能否隔离本地/外部调度容量 | 是否足以保证队列互不影响 |
|---|---:|---:|---:|
| 10 个路由共用 1 个 API Key | 否 | 否 | 不足 |
| 本地路由和外部路由各用 1 个 API Key | 是，按两类隔离 | 否，底层调度器仍共享 | 只能部分满足 |
| 每个路由 1 个 API Key | 是，按路由隔离入口 | 否，底层调度器仍共享 | 只能部分满足 |
| API Key + 独立本地/外部调度 lane | 是 | 是 | 可以满足设计目标 |
| API Key + 完全独立凭据池/外部池实例 | 是 | 是，隔离最强 | 可以满足，但资源碎片和运维成本最高 |

因此，**多 Key 不是完整方案，只是第一层保护**。

## 针对你的 10 个路由，推荐的实际方案

### 方案 A：立即可用的低风险配置方案

在不修改运行时代码的前提下，建议至少按流量职责拆成 3 个 API Key：

```text
key-local-only     -> R1、R2
key-external-only  -> R3、R4、R5
key-mixed          -> R6-R10
```

这样可以保证：

- local-only 长请求不会直接吃光 external-only 的入口准入；
- external-only 的入口排队不会直接挡住 mixed 路由；
- 三类流量在 Layer A 有独立排队计数。

但必须明确：

- R1/R2 仍共享本地账号调度容量；
- R3/R4/R5 仍共享外部池容量和外部队列；
- R6-R10 仍会和 R1/R2 竞争本地容量，也会和 R3-R5 竞争外部容量；
- 这只能减少入口级互相阻塞，不能保证底层账号调度完全不受影响。

如果 R1 和 R2 也必须完全互不影响，则应使用两个不同 Key；不过这会进一步复制入口并发上限，必须同步设置总量保护。

### 方案 B：满足“排队不影响正常账号调度”的正式改造

建议增加显式的 `schedulerDomain` / `routeConcurrencyProfile`，按路由组建立调度 lane，而不是把 API Key 当成调度池：

```text
local-only-lane
  routes: R1、R2
  local: enabled
  external: disabled
  local max inflight / max queued: 独立配置

external-only-lane
  routes: R3、R4、R5
  local: disabled
  external: enabled
  external max inflight / max queued: 独立配置

mixed-lane
  routes: R6-R10
  local: preferred
  external: fallback
  local/external budgets: 独立配置
```

实现上需要同时具备两层：

1. **入口 lane**
   - 每个 lane 有独立的 `active`、`queued`、`queue_timeout`；
   - 仍保留每 API Key 总上限，防止单个 Key 无限放大总并发；
   - lane 入口队列不能直接占用其它 lane 的 reserved 名额。

2. **调度容量 ledger**
   - 本地账号池增加按 lane 的并发预算或保留配额；
   - 外部池增加按 lane 的并发预算或保留配额；
   - 一个 lane 的排队不能消耗另一个 lane 的保留容量；
   - 空闲 lane 的容量可以按明确规则借给其它 lane，但借用不能抢占已经运行的正常请求。

只有做到第二层，才可以真正保证：

> R1/R2 本地-only 排队时，不会把 R6-R10 的正常本地账号调度全部拖住；R3-R5 外部-only 排队时，也不会把 mixed 路由的外部 fallback 全部拖住。

### 方案 C：最强隔离

为不同路由组配置完全独立的本地凭据集合、外部池集合和调度器实例。

优点是边界最清楚，缺点是：

- 账号和外部池不能共享空闲容量；
- 容量碎片化；
- 配置、监控、故障切换和发布复杂度明显增加。

除非 R1/R2、R3/R5 是强租户或强 SLA 隔离场景，否则不建议一开始就采用方案 C。

## 需要避免的误区

### 误区 1：每个路由配一个 Key，就等于每个路由有独立账号并发

不等于。它只隔离了 RequestAdmission；本地账号 manager 和外部 pool manager 仍是共享对象。

### 误区 2：把 `local_pool_route_rules` 当成本地账号池分组

不等于。它只决定某个 endpoint 是否允许进入本地池，不会自动给该 endpoint 分配独立账号集合或独立并发预算。

### 误区 3：把 external pool 的 `route_rules` 当成外部队列隔离

不等于。它主要影响候选资格；如果多个路由仍命中同一外部池管理器和容量协调域，容量竞争仍然存在。

### 误区 4：用多个 Key 解决总容量问题，却不设置总量保护

多个 Key 会复制每 Key 入口并发额度。若当前值为 32，10 个 Key 理论上可能把入口侧放大到约 320 个并发，而本地账号和外部池底层容量并不会同步放大。

## 补充：单个账号设置为 5 并发，是否会超过 5

### 正常情况下的结论

如果“5 并发”指的是该凭据的 `max_concurrent_requests=5`，并且：

- 所有请求都由同一个服务实例调度，或多实例共享同一个健康 Redis；
- 没有启用会改变容量口径的特殊权重配置；
- 没有人工清理仍在运行的 lease；
- 没有发生 lease 过期回收误判；

那么调度器不会故意让该账号同时持有超过 5 个**容量单位**。

第 6 个请求的正常动作是：

```text
换其它可调度账号
或进入本地 dispatch queue
或按 local/external fallback 策略转外部
或在等待队列/等待时间耗尽后失败
```

它不会因为 API Key 不同，就给同一个账号再增加一份“5 并发”。

单账号限制和全局本地限制同时生效：

```text
单账号有效上限 = 凭据自身 max_concurrent_requests
               或全局 credential_max_concurrent_requests

整个本地池还受 dispatch_global_max_concurrent_requests 限制
```

实现上，账号 lease 建立时同时检查账号容量和本地全局容量；账号自身容量判断是 `entry.in_flight_requests + request_weight <= max`。见 `src/kiro/token_manager/capacity.rs:118-150`、`src/kiro/token_manager/manager.rs:4081-4115`。

### 多 API Key 不会把同一个账号的上限复制

下面这个理解是错误的：

```text
key-a -> 账号 A 5 并发
key-b -> 账号 A 再 5 并发
```

正确行为是：

```text
key-a + key-b + 其它 Key
    -> 竞争同一个账号 A 的同一组 in-flight leases
    -> 总容量仍按账号 A 的有效上限判断
```

因此，在单实例或健康 Redis 协调下，5 是账号级上限，不是每 Key 上限。

### 多实例但没有 Redis：可能出现聚合超 5

如果部署了多个服务实例，但没有使用 Redis 做跨实例账号调度协调：

```text
实例 1：账号 A 最多 5
实例 2：账号 A 最多 5
```

每个实例只知道自己的本地 lease，聚合到上游时可能出现约 10 个实际并发。此时“5”只是**每实例上限**，不是集群上限。

如果启用 Redis，调度器会在确认本地 provisional slot 后调用 Redis 的 `acquire_dispatch_lease`，由 Redis 协调同一凭据的跨实例 lease；Redis 正常时，目标是维持账号级集群上限。见 `src/kiro/token_manager/manager.rs:4433-4462`、`:4488-4549`。

Redis 调度不可用时，代码不会把本地 provisional slot 当成永久成功的跨实例 lease；通常会等待、失败或触发已配置的外部 fallback。因此 Redis 故障更可能造成不必要等待或容量不足，而不是安全地放大账号并发。

### “5 并发”可能不是 5 个请求

默认 `weighted_capacity.enabled=false` 时：

```text
1 请求 = 1 容量单位
5 容量单位 ≈ 5 个同时执行请求
```

如果显式启用 weighted capacity，则一个大请求可能占用多个容量单位，因此：

```text
账号上限 = 5 容量单位
实际请求数可能少于 5
```

权重模式不会让调度器合法地超过 5 个容量单位，但会让“请求数”和“容量数”不再相等。

### 仍可能造成实际观测超过 5 的异常边界

以下情况需要单独排查，不能把它们误认为正常调度策略：

1. **多实例未接入 Redis**：每个实例都各自允许 5。
2. **人工清理 in-flight leases**：如果 Admin 清理时请求仍在上游执行，旧请求可能仍占用真实上游并发，但调度器已经释放名额。
3. **lease 超时回收**：默认 `credential_in_flight_lease_max_secs=900`。如果请求长时间没有任何可观测活动，lease 可能被当成 stale 清理；原请求若实际仍未结束，短窗口内可能出现真实并发高于调度快照。
4. **Redis 数据被外部删除或协调存储发生异常**：需要结合 Redis lease、服务日志和上游时间线判断，不能只看 Admin 快照。
5. **把入口 RequestAdmission active 数当成账号并发**：入口 active 只表示 API Key 持有 response body，不代表请求已经拿到某个账号 lease。

### 如何验证“账号 A 是否真的超过 5”

不能只看 API Key 并发或请求总数，至少要同时采集：

```text
credential_id = A
in_flight_leases 数量与 weight_units 总和
Redis 中 credential A 的 dispatch lease
服务实例 ID
upstream send / headers / terminal 时间线
```

验收条件应写成：

```text
单实例无 Redis：A 的本地有效容量 <= 5
多实例有健康 Redis：A 的集群 lease 容量 <= 5
weighted capacity 开启：A 的 weight_units 总和 <= 5
```

## 场景五：共享 API Key 造成的隐形耦合示例

假设：

```text
requestAdmission.maxConcurrentRequests = 32
requestAdmission.maxQueuedRequests = 64
本地容量 = 30
本地-only 长请求 = 32 个
external-only 短请求 = 1 个
```

时间线：

1. 前 32 个本地请求先通过 Layer A；其中 30 个占本地槽，剩余请求可能在本地调度等待。
2. 这些请求仍然持有 Layer A permit，因为 permit 要等 response body 结束才释放。
3. 第 33 个 external-only 请求即使外部池完全健康，也要先争抢同一 API Key 的 Layer A。
4. 如果 Layer A 队列还有位置，它会在入口排队；如果入口队列也满，直接得到 `429`。
5. 外部池本身可能从未看到第 33 个请求。

如果把 external-only 流量换成另一个 API Key，则不会经过同一个 Layer A gate，通常可以直接进入外部池；此时仍需考虑外部池自己的容量和队列。

## 场景六：不能误用的结论

### 错误结论 1：本地排队一定会拖住外部

错误。只有同一 API Key 的 Layer A 被本地请求占满，或系统其它共享资源受压时，才会间接影响外部。

### 错误结论 2：本地满了就一定立即外部

错误。外部必须路由允许、模型兼容、容量可承接；本地预检和 fallback 开关也必须打开。

### 错误结论 3：外部池有候选就等于外部能接

错误。容量类本地原因要求外部“立即可用”；只有普通 eligible 不足以证明可以安全接管。

### 错误结论 4：把所有等待都叫“并发排队”

错误。至少要区分：

1. API Key 入口准入排队；
2. 本地 dispatch queue；
3. 单账号/本地全局容量等待；
4. 外部池 queue；
5. 外部池候选排序或冷却导致的重选等待；
6. 上游响应头/流式 body 等待。

## 建议的产品策略

当前代码已经有“本地优先 + 容量不足时有条件外部接管”的实现，但策略仍由多个开关拼接，容易让运营误以为存在统一的“本地满后动作”。

建议后续明确为一个显式策略字段，例如：

```text
localCapacityOverflowPolicy =
  queue_first
  external_first_when_cached_ready
  external_first_with_bounded_wait
```

本次不直接改运行时代码，原因是该字段会改变正常请求的等待顺序和外部用量，必须先完成：

- 每个状态的 route plan 和最终动作；
- 同 API Key / 不同 API Key 的准入隔离验收；
- “不掐掉正常长请求”的 L3/L4/L5 验证；
- 本地容量恢复、外部池容量恢复、双边同时满、Redis 降级的矩阵；
- usage 中明确记录 `admission_wait_ms`、`local_dispatch_wait_ms`、`external_dispatch_wait_ms` 和 route subtype。

### 推荐的落地方向

如果目标是“不能掐掉正常请求，同时让健康外部池真正承担溢出流量”，推荐采用以下组合，而不是简单把本地调度改成无条件 fail-fast：

1. **入口准入按路由域隔离，但保留每 Key 总上限。**
   - 保留 `api_key_total_limit` 作为硬总上限，防止一个 Key 无限放大总并发。
   - 在总上限之下增加 `local_lane`、`external_lane` 两个逻辑车道，至少为 external lane 保留可用名额。
   - 最小实现可以按 `api_key + route_domain` 分桶；更稳妥的实现是“总闸门 + 两个子闸门”，避免本地长请求吃光所有外部名额。
   - 这能解决“同一 API Key 的本地队列把 external-only 请求挡在 Layer A”的结构性耦合，同时不改变不同 API Key 的隔离语义。

2. **本地容量溢出采用 `external_first_when_cached_ready`，并保留短暂释放宽限。**
   - 本地容量满后最多等待当前 250ms 宽限，让刚好要释放的本地槽位优先被本地复用。
   - 宽限结束且外部对当前模型/路由有立即可用容量时，转外部。
   - 外部只有 eligible、没有立即容量时，不把请求“甩”到一个马上还会排队或失败的外部池。
   - 外部池自身满时遵循其 `FailFast/Wait` 配置，不通过取消或抢占其它正常请求来制造容量。

3. **只做选择，不做抢占。**
   - 已经拿到本地 lease、已经向上游发送、或已经向下游提交有效语义输出的请求，不因为后来出现容量压力而被切到外部。
   - 本地到外部的 fallback 只发生在本地 lease 尚未建立、或明确的本地可回退错误边界内。
   - 不允许通过缩短 stream idle、response 或 lease 保护时间来“清理容量”；那会直接伤害正常长流。

4. **把等待和容量决策分开计量。**
   - 分别记录入口准入、本地 dispatch、外部 dispatch、上游响应头、首个 chunk、首个可见输出。
   - 每次路由转换记录原因、是否立即可用、是否进入队列、是否发生 local rescue。
   - 只有这样才能证明优化减少的是排队，而不是把正常请求变成更早失败。

5. **验证以“正常请求不被掐掉”为第一门槛。**
   - 长流在本地槽位释放前必须继续完整返回；
   - 外部 fallback 只增加新承接，不取消已接受的本地请求；
   - 同 Key 本地/外部混流、双边同时满、容量瞬时恢复、Redis degraded 和客户端断连都必须有 fake-upstream 并发测试。

## 下一步验证矩阵

| 编号 | 场景 | 必须观测 |
|---|---|---|
| A1 | 同 Key，本地-only 长请求占满 Layer A，external-only 健康 | external 请求是否卡在 admission，而不是 external queue |
| A2 | 不同 Key，同上 | external 请求是否绕过本地 Key 的 admission |
| A3 | 本地全局容量满，外部立即可用 | 是否最多等待 250ms 后 external fallback |
| A4 | 本地容量满，外部只有 eligible 无 immediate capacity | 是否不误切到一个接不住的外部池 |
| A5 | 本地容量满，外部 `FailFast` | 是否返回外部容量错误，不无限等待 |
| A6 | 本地容量满，外部 `Wait` 且队列有位 | 是否进入外部队列并在释放后完成 |
| A7 | 本地容量恢复发生在 250ms 预检内 | 是否保留本地，不产生无谓外部请求 |
| A8 | external-only direct + 本地有空闲槽 | 是否始终不回本地 |
| A9 | 本地等待队列满 | 是否记录本地队列满并按策略 fallback/失败 |
| A10 | 同 Key 的本地和外部混流 + 长流 | 是否没有正常流被提前取消，入口准入释放是否与 body 结束一致 |
