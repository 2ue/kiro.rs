# Local Capacity Preflight Race And External Fallback Latency

Status: `focused-policy-and-manager-pass / storage-burst-and-frozen-candidate-pending / NO-GO`

Severity: P0

## 影响、用户现象与边界

请求开始时本地预检可能看到 `Ready`，但并发请求在真正取得 credential slot 前已经把容量占满。若 external pool 已配置且匹配当前 model/body mode，后到请求应回到 external fallback 层；旧候选逻辑却可能进入本地默认 `credentialDispatchMaxWaitSecs=120` 队列，表现为首字很慢、并发高但 RPM 低，最后才 external 或返回容量错误。

这与持久化账号禁用是两个问题：`disabled=true`、运行态 mutation quarantine、Redis scheduler degraded、普通并发/RPM 饱和和 preflight/acquire 竞态必须分别归因。300 RPM 与约 100 秒平均占用可按 Little's Law 维持约 500 个 in-flight，请求排队本身不证明账号被禁用。

本问题没有 tool hash 或固定文本指纹。可见变体包括长 TTFB、`local_capacity_exhausted`、external fallback 延后、本地 queue depth 增长、低 upstream RPM、高 global in-flight，以及没有明显错误但成本路由晚切换。当前 dirty candidate 的竞态已由源码和聚焦合同确认；它能解释同类症状，但没有 request-level 生产 route/queue 证据时，不能宣称它就是既有生产事件的唯一根因。

## 源码链与根因

1. raw/parsed handler 在请求入口读取 fresh local route state；预检看到 `Ready` 时继续本地。
2. burst 中多个请求可在同一窗口都通过预检，随后前面的请求占满 global 或 per-credential slot。
3. 当前候选的 `ExternalFallbackContext::local_attempt_policy()` 已经完成 external 静态 eligibility 检查，但旧实现还要求此前 selection 写入的 250 ms runtime availability hint 才返回 `FailFastOnCapacity`。
4. 第一次请求、hint TTL 过期、新 model/body-mode key 或 coordinator 异常后都没有正 hint，旧实现因此返回 `WaitForCapacity`。
5. `MultiTokenManager` 对 `WaitForCapacity` 使用全局默认 `credentialDispatchMaxWaitSecs=120`；只有 `FailFastOnCapacity` 才在容量竞争后保持 queue depth 为零并把控制权交回 fallback。

这个 hint 是为避免本地健康路径每次读取 external Redis 而加入的优化，但它把“尚未扫描”误当成“不允许 fallback”，形成确定性冷启动/TTL 竞态。它不是账号健康状态，也不应拥有本地等待策略。

## 复现方法

最小合同：启用 external、local preflight 和 capacity fallback，先确认 model/body 静态 eligibility；令 preflight 时 local 为 `Ready`，在 acquire 前由 holder 占满最后一个 slot。旧逻辑在 hint absent 时返回 `WaitForCapacity`，修复后必须返回 `FailFastOnCapacity`。

多轮与并发复现：40 个账号、每账号 15 并发、global 500、60 RPM；用 100 秒慢流保持 holder，随后 c1/c32/c128/c500 burst。每档至少 5 轮，记录 local preflight state、acquire mode、queue wait、queue peak、local inference hits、external selection/acquire hits、TTFB、route subtype 和释放恢复。

异常复现必须覆盖 external available、full、cooling、model cooling、coordinator unavailable、pool disabled/deleted 和 static snapshot stale。再组合 Redis 50/74/75/90/150/500 ms、disconnect/restart、PgSQL pool timeout、usage writer/cleanup 压力和两实例；不能只用 happy path available pool 证明正确。

## 候选方案与选定修复

- 恢复每个本地请求的 external 动态 availability 查询：动态信息最新，但重新把 Redis/PG selection 热读放回本地成功路径，增加正常 TTFB 和故障共因。
- absent hint 时短等待：需要新隐藏常量或配置，仍保留 stale-negative 竞态，并给所有 burst 人为增加等待。
- 静态 eligible 后直接使用 `FailFastOnCapacity`：本地仍先尝试并重选全部可用 credential；只有容量确实抢不到时才进入 external 的权威 selection/acquire。

选定第三种。删除 runtime availability hint、相关 map/mutex 和仅为该路径保留的生产 cache API。配置开关仍保持权威：`localPoolPreflightEnabled=false` 或 `fallbackOnLocalCapacityExhausted=false` 时继续 `WaitForCapacity`；没有匹配 external pool 时也不改变本地等待。external available/full/cooling/coordinator 的最终判断仍由 selection/acquire 状态机完成，不由静态 eligibility 伪造。

## 兼容性、性能风险与回滚

正常本地成功路径仍只做已存在的静态 eligibility snapshot，不新增 Redis RTT；删除 hint 后少一个 4096-entry map、一次 mutex read 和 stale-state 分支。容量满时不再先等本地 120 秒，external 自身仍受 `externalPoolDispatchMaxWaitSecs` 默认 30 秒、queue cap、cooldown 和 coordinator fail-closed 约束。

风险是静态 snapshot 跨实例失效或 pool 刚被删除/禁用时，请求会比旧逻辑更早进入 external selection；selection 会拒绝而不是盲发，并可按既有 local rescue 策略有界回本地。cross-instance pool revision 和 selection-to-send TOCTOU 尚未最终关闭，因此不能把聚焦 policy pass 外推为完整可用性结论。

回滚可以恢复动态 availability 查询，但不得恢复 hint absent 进入 120 秒队列，也不得用 local-memory fail-open 绕过 Redis lease。若回滚，需要同时保留 bounded external wait、queue cap、attempt budget 和公开错误脱敏。

## 验收矩阵

| ID | 场景 | 轮次 | 必须结果 | 当前状态 |
| --- | --- | ---: | --- | --- |
| LQ01 | preflight Ready，acquire 时 CapacityFull，external 静态 eligible | 5 | `FailFastOnCapacity`，不得进入 120 秒队列 | 聚焦 policy pass |
| LQ02 | capacity fallback 或 preflight 关闭 | 各 5 | 保持 `WaitForCapacity` | 聚焦 policy pass |
| LQ03 | fail-fast global/per-account full 与 alternate reselect | 各 5 | queue peak 0；有空账号先重选 | 聚焦 manager pass |
| LQ04 | external available/full/cooling/model-cooling/coordinator unavailable | 各 5 | available 可选；full 按 external 有界队列；cooling/model/coordinator 规范失败 | fixture 编译，动态因缺隔离 PG/Redis pending |
| LQ05 | 40x15、60 RPM、global 500、100 秒 holder，c1-c500 | 每档 5 | 无假禁用；local+external attempts 有界；TTFB/queue 符合配置 | pending |
| LQ06 | Redis/PG/usage 联合故障与恢复 | 每档 5 | 不形成 120 秒反馈队列；恢复 5/5；无 lease/queue 残留 | pending |
| LQ07 | 两实例、static revision 与 selection-to-send race | 每档 5 | 不超卖、不盲发删除池、bounded recovery | pending |
| LQ08 | frozen release L3/L4/L5 | 3 outer/soak | p50/p95/p99 TTFB、RSS、FD、queue 和 attempts 达门槛 | pending |

## 修复后结果与证据限制

`queue-preflight-race-r2` 在 Rust 1.92.0 下执行格式检查、聚焦 policy、三个 external filter 和 `cargo check --all-targets`。policy 测试实际为 `running 1 / passed 1`，内部连续 5 轮；all-targets check 通过。scope 峰值 `size_kib=2017340`，结束时 `removed=true`、`reservation_released=true`。

`queue-refresh-integration-r3` 又执行三条 manager 合同，各自 `running 1 / passed 1` 且内部 5 轮：global capacity full 立即返回并保持 queue 0；单账号从 Ready 到 CapacityFull 时 queue 0、release 后恢复 Ready；slot race 重选另一健康账号且 queue 0。该 scope 同时通过 policy、refresh/API/MCP 和 all-targets check，`size_kib=2016724`，结束时 `removed=true`、`reservation_released=true`。

三个 external storage filter 虽显示 test function `ok`，正文明确输出缺少 `KIRO_RS_TEST_POSTGRES_URL` 并提前返回；它们只计编译/入口证据，不计 available/full/cooling/coordinator 动态 PASS。第一次 `queue-preflight-race-r1` 因错误使用 `cargo test --lib` 在编译前退出 101，`size_kib=32` 且完整清理，也不计行为证据。详细边界见 [专项证据](../evidence/local-capacity-preflight-race-20260718.md)。

## 残余风险

真实隔离 PG/Redis、40x15 慢流 burst、两实例、Redis/usage 联合 chaos、frozen release 和 L3-L5 尚未执行。当前只可以声明删除了一个确定性的 hint-absence 120 秒排队分支并证明 manager fail-fast/reselect 不排队，不能承诺所有慢首字、external 路由或“账号不可用”现象已经根治，发布保持 `NO-GO`。
