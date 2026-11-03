# 本地账号池风险保护（全池熔断）误伤

## 现象

报错 `本地账号池风险保护已打开（retry_after_secs=2）`，`source: local_account`，selectionFailure 主因 `risk_circuit_open`，样本 #1594–#1613 全部是 disabled。号池里有大量可用账号，请求却在选号前被直接拒绝。

## 现网数据

- 配置：`localPoolCircuitEnabled=true`，窗口 60 秒，3 次失败，至少 2 个不同账号，打开 30 秒（即默认值）。
- 8 小时内熔断打开过 3 次：16:20:00、16:20:30、16:21:01。
- 推满计数的全部是 16:19–16:20 新导入的 2050–2054（binus.ac.id，0 次成功）。它们首次使用就收到上游 403 temporarily_suspended，都走 ak-* 代理。同批走其他代理的 2055–2064 正常。是账号被封还是代理问题，没有验证。
- 第一次打开时实际只有 2 个账号出问题，其中 2051 被两个并发请求各命中一次，重复计数凑够了 3 次。
- 熔断窗口约 91 秒：本地直接拒绝 584 条（mine-haa 445、onlylocal 90、minea 36、dfcache/cc 13），这些路由不在外部池白名单里，无法回退。另有 14 条回退到外部池。
- 熔断时池里 57 个账号只是被熔断挡住，本身可用（`reasonCounts={disabled:421, risk_circuit_open:57}`，容量 170 以上）。
- 0.0.224 新增的"本请求空回排除全部账号"（RequestExcludedAll）**没有**进入这个熔断：8 小时日志里出现 0 次。

## 原因（设计缺陷）

进程内熔断（实际拒绝来源）：

- 状态是整个进程一把锁（`manager.rs:523-539`），不区分路由、模型、优先级、代理。
- 只由上游风控 403 计数：TemporarilySuspended、AccountSuspended、AccountLocked（`account_state.rs:194`），入口 `record_local_pool_risk_circuit_failure`（`manager.rs:3241`）。
- **同一账号重复计数**：账号已禁用的分支（11833、11960）和账号不存在的分支（11821、11948）仍会再记一次，所以同一账号的并发请求会重复计数。
- **阈值是绝对次数**，与池子大小无关。
- **打开时不清空计数窗口**，没有半开状态（3207-3234）。到期后窗口内再来 1 次失败就立刻重开。
- 从没成功过、还在预热的新账号，风控也计入全池。
- 选号前直接拒绝（6362-6371），打开后当前请求也不再换号（`can_retry_local`）。
- selectionFailure 样本按账号顺序取前 20 个（3183，`selectionFailureSampleLimit=20`），禁用账号 id 靠前，造成"全是 disabled"的误导。
- 默认值不一致：字段缺失时是 false（`#[serde(default)]`，config.rs:4278），整个 `externalPools` 缺失时是 true（4440）。

Redis 熔断（潜在问题，现网不生效）：

- 由 `handlers.rs:2437-2462` 写入，除 `local_request_body_invalid` 外的所有可外部兜底的本地错误都计入，包括请求级的 `local_request_excluded_all`、`no_available_credentials`、`local_capacity_exhausted`。
- 只在 `direct_policy_reason`（`external_pool.rs:6200-6215`）里读取，要求 `directExternalOnLocalMaintenance=true`（现网为 false）。即使打开，它也只改原因标签、不改变是否直连外部，却让每个请求多一次 Redis 读。

## 解决方案

已实现，提交和验收见 [07-implementation.md](07-implementation.md)。

全池熔断要谨慎，不能连续错几次就打开：

1. 按账号去重：窗口内同一账号只计一次；账号已禁用、账号不存在的分支不再计数。
2. 从未成功过的账号（新导入、预热中）被风控时，只禁用它自己，不计入全池。
3. 比例阈值：5 分钟内至少 5 个曾经成功过的账号被风控，**并且**占当时可调度账号的 30% 以上，才打开。打开前读一次真实的可调度数。
4. 打开时清空计数窗口；到期后进入半开状态，放少量探测请求，成功就关闭，失败就退避后再打开。
5. 日志和 selectionFailure 记录真实可调度数；采样优先列出非 disabled 的账号。
6. 统一默认值；旧配置字段兼容迁移。
7. 删除 Redis 熔断：它只改一个默认关闭的标签，口径混杂，还让每个请求多一次 Redis 读。

新增配置 `localPoolCircuitMinRatioPercent`（默认 30）和 `localPoolCircuitHalfOpenProbes`（默认 3），窗口默认 300 秒、账号数默认 5。运行配置迁移 v14 把仍是旧默认值（60 秒、3 次、2 个账号）的配置升级为新默认值，改过的值保留。
