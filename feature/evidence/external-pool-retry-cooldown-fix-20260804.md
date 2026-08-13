# 外部池同池/跨池重试与冷却恢复 focused 验证（2026-08-04）

Status: `superseded-by-20260805-ha-target / retry-mechanics-still-useful / cooldown-policy-rejected`

Scope: 外部池请求失败后的同池重试、跨池继续尝试、可配置跨池状态码、网络/协议
错误跨池开关、池级/模型级冷却、连续瞬态失败冷却上浮，以及 Admin “清除冷却”
后的运行态恢复。这里关于“连续失败上浮为长池级冷却”的内容现在只作为历史证据。

本轮使用本地隔离 PostgreSQL/Redis（`127.0.0.1:25432`、`127.0.0.1:26379`）和
测试用 fake HTTP upstream，未读取、修改或重启现网三台机器，也未改变生产配置。

2026-08-05 结论更新：

- 本证据中的“同池重试、跨池尝试、状态码/网络/协议开关、清除冷却”仍是有效基础能力；
- 本证据中的“普通连续瞬态失败上浮为池级长冷却”被新的高可用目标覆盖，不再作为正确策略；
- 当前目标是所有上游错误默认按临时抖动处理，先请求级排除、换池和软降权；临时不可调度必须有严格重复失败证据或人工禁用，并且必须自动恢复；
- 不能再用本文件证明 `v0.0.132` 的冷却策略满足发版标准。

## 已验证的页面行为

### 1. “外部池最多尝试”与“同池重试次数”是两套预算

- “外部池最多尝试”限制的是同一请求最多放弃多少个外部池并继续选择其他池；
- “同池重试次数”只在当前外部池返回可重试状态码时生效；
- 同池重试不消耗“外部池最多尝试”，只有当前池最终被放弃后才进入下一个池；
- 请求的“推理尝试预算”仍是总硬上限，且下游响应已提交后不会透明重试。

### 2. “同池重试状态码”决定哪些 HTTP 状态码允许先重试当前池

当前实现同时要求上游错误本身被分类为“可重试”，再检查状态码是否位于
“同池重试状态码”中。未配置的状态码不会在同一外部池上重复发送，而是直接
按现有跨池/最终错误策略处理。

### 3. “清除冷却”会立即影响当前进程

Admin 端点：

```text
POST /external-pools/{id}/cooldown/clear
```

同时删除池级冷却和模型级冷却，并使当前进程的外部池容量/运行态快照失效，
随后通过状态变更信号唤醒等待中的调度请求。无需等待旧快照自然过期或重启服务。

### 4. “跨池重试状态码”只控制外部池内部换池

- “跨池重试状态码”默认包含 `408,425,429,500,502,503,504,529`；
- “网络错误跨池重试”默认开启，覆盖连接失败、DNS、超时等无 HTTP 状态码错误；
- “协议错误跨池重试”默认开启，覆盖成功状态码但响应体污染、错误信封或协议不兼容；
- 普通不可重试 `400` 仍不进入默认跨池重试；
- 这些设置只决定外部池之间是否继续尝试，不会把“外部直连”改成本地凭证 fallback。

### 5. 历史策略：连续瞬态失败会拉长池级冷却

以下是 2026-08-04 的旧策略验证，已被 2026-08-05 高可用调度目标覆盖，不能作为当前正确策略。

在 5 分钟窗口内，同一外部池连续出现 `429`、`5xx`、网络错误、协议污染、
数据库忙或端点错误等瞬态失败时，池级冷却会随失败次数上浮，并加入 20% 抖动。
默认上限为 300 秒；如果上游返回更长且合法的 `Retry-After`，则按
`Retry-After` 保留更长冷却。成功请求会清除连续失败计数。

这解决的是“高优先级坏池短冷却后反复抢占流量”的稳定性问题。它不是自动永久
禁用外部池；泛化 `5xx` 自动硬禁用可能误伤临时过载的上游，仍应作为单独的
阈值/窗口/手动恢复配置决策。

## 真实验证结果

### 同池重试与跨池顺序

```text
KIRO_RS_TEST_POSTGRES_URL=postgres://kiro_rs:kiro_rs_dev_password@127.0.0.1:25432/kiro_rs
KIRO_RS_TEST_REDIS_URL=redis://127.0.0.1:26379/0
feature/tests/run-cargo-scoped.sh external-pool-same-pool-retry-real-infra \
  -- cargo test --locked external_pool_same_pool_retry -- --nocapture
```

结果：

```text
3 passed / 0 failed
```

覆盖：

- “外部池最多尝试=1”时，“同池重试次数=1”仍会对同一 fake upstream 发送两次；
- 502 位于“同池重试状态码”时，当前池重试耗尽后才切换到第二池并成功；
- 502 不在“同池重试状态码”时，当前池只发送一次，然后切换第二池；
- fake upstream 的请求计数与“外部尝试”轨迹一致。

### 冷却、坏状态与清除恢复

```text
KIRO_RS_TEST_POSTGRES_URL=postgres://kiro_rs:kiro_rs_dev_password@127.0.0.1:25432/kiro_rs
KIRO_RS_TEST_REDIS_URL=redis://127.0.0.1:26379/0
feature/tests/run-cargo-scoped.sh external-pool-cooldown-clear-real-infra \
  -- cargo test --locked \
  external_pool_atomic_acquire_honors_pool_cooldown_and_fails_closed_on_bad_state \
  -- --nocapture
```

结果：

```text
1 passed / 0 failed
```

该测试真实覆盖：

- 池级冷却存在时，原子获取会拒绝发送；
- Redis 中冷却状态损坏时按失败关闭处理，不把坏状态当成可调度；
- 清除“冷却”后，池级冷却和模型级冷却都被删除；
- 清除后重新读取运行态快照，冷却剩余时间为 0，模型冷却为空；
- 清除动作不会留下未释放的并发租约。

### 历史策略：等待提示进入池级冷却

以下测试只证明旧实现会按上游等待提示写池级冷却；新策略不能依赖上游一定返回等待时间，也不能把该行为作为发版标准。

```text
KIRO_RS_TEST_POSTGRES_URL=postgres://kiro_rs:kiro_rs_dev_password@127.0.0.1:25432/kiro_rs
KIRO_RS_TEST_REDIS_URL=redis://127.0.0.1:26379/0
feature/tests/run-cargo-scoped.sh external-pool-retry-after-real \
  -- cargo test --locked external_pool_retry_after -- --nocapture
```

结果：

```text
3 passed / 0 failed
```

该测试真实覆盖：

- `Retry-After: 4` 会覆盖外部池 rate limit 的默认冷却秒数；
- 未来 HTTP date 会被解析并限制在 7 天上限内；
- fake upstream 返回 429 后，池级运行态快照的冷却剩余时间会落在真实的 `Retry-After`
  窗口内，而不是仍然使用默认配置值。

### 跨池重试配置、终态错误与连续瞬态冷却

```text
KIRO_RS_TEST_POSTGRES_URL=postgres://kiro_rs:kiro_rs_dev_password@127.0.0.1:25432/kiro_rs
KIRO_RS_TEST_REDIS_URL=redis://127.0.0.1:26379/0
feature/tests/run-cargo-scoped.sh external-pool-retry-full-focused \
  -- cargo test --locked external_pool_ -- --nocapture
```

结果：

```text
146 passed / 0 failed
```

补充覆盖：

- “跨池重试状态码”可以关闭某个 HTTP 状态码的跨池继续尝试；
- 认证、配额、渠道禁用、端点配置错误和模型不可用会跳过同池重试，避免在明显
  不合格的同一外部池上重复发送；
- 可跨池的终态账号错误仍会放弃当前池并尝试后续池；
- 连续瞬态失败会把冷却从基础值拉长到更长窗口，并在成功后清除失败计数；
- 成功恢复路径会一起清理池级冷却、模型级冷却、并发租约和连续瞬态失败计数。

### 外部失败后回本地的边界收窄

这次补充验证确认：外部失败并不会无条件回本地凭证。只有本地优先
fallback 到外部的请求，且当前本地 fresh 路由状态仍为 `Ready`、还有可调度
容量时，才允许 `local rescue` 回本地一次；直连外部、本地无凭证、全禁用、
模型不兼容、Redis 调度降级和风险熔断都保持外部侧终态，不再隐式回本地。

验证命令：

```text
feature/tests/run-cargo-scoped.sh handlers-local-rescue-focused \
  -- cargo test --locked external_local_rescue -- --nocapture

feature/tests/run-cargo-scoped.sh handlers-local-rescue-budget1 \
  -- cargo test --locked local_rescue_requires_remaining_shared_attempt_budget_for_five_rounds -- --nocapture

feature/tests/run-cargo-scoped.sh handlers-local-rescue-budget2 \
  -- cargo test --locked preflight_external_error_can_rescue_once_then_attempt_budget_blocks_cycle_five_rounds -- --nocapture
```

结果：

```text
external_local_rescue_*: 3 passed / 0 failed
local_rescue_requires_remaining_shared_attempt_budget_for_five_rounds: 1 passed / 0 failed
preflight_external_error_can_rescue_once_then_attempt_budget_blocks_cycle_five_rounds: 1 passed / 0 failed
```

补充 focused 验证：

```text
feature/tests/run-cargo-scoped.sh external-pool-config-defaults \
  -- cargo test --locked default_runtime_controls_are_conservative -- --nocapture
```

结果：

```text
1 passed / 0 failed
```

## 代码与界面落点

- “外部池最多尝试”“跨池重试状态码”“网络错误跨池重试”“协议错误跨池重试”
  “同池重试次数”“同池重试状态码”“同池重试间隔”已加入运行配置、默认值、
  校验和两套 UI；
- 外部池失败循环在记录“外部尝试”后，先执行请求大小保护重试，再执行同池重试，
  同池重试耗尽后才写入冷却、排除当前池并选择其他外部池；
- “清除冷却”已加入两套外部池管理界面；
- 默认“外部池最多尝试”为 0，表示按当前符合路由、启用、模型、容量和冷却条件
  的候选池自动完成一轮，而不是固定只发一次；
- 默认“同池重试状态码”仍可由页面修改，实际发送前不会把普通不可重试的 400
  强行变成同池重试。

## 尚未关闭的边界

- 本轮是本地 fake upstream 与隔离存储验证，不代表三台现网升级后的生产复发
  已经关闭；
- 请求明细还没有展示完整的“候选池筛选原因”，仍需区分“冷却”“容量不足”
  “模型不支持”“优先级未到”等原因；
- “模型（请求）”“模型（本地解析）”“模型（上游）”的展示仍可进一步拆分为
  实际外部发送值和本地别名值；
- 多实例同时写冷却、清除冷却与正在发送的竞态仍需要在更大范围的调度/混沌门禁
  中复核。
- 泛化 `5xx` 自动硬禁用外部池未在本轮实现。当前选择是延长临时冷却并允许手动
  清除；是否把长期不可用池自动禁用，应等生产错误率窗口和恢复策略明确后再作为
  独立配置实现。
