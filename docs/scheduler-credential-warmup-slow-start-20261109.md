# 新凭据预热慢启动修正（2026-11-09）

## 问题

59138 线上（北京时间 2026-11-09 16:54 起）新增账号后，预热中的账号（`credentialWarmupRequests=30`）立刻拿到大量请求，而当时其他账号并发充足：

| 时间 | 预热中新号 | 新号合计占比 | 每个新号/分钟 | 老号平均/中位数 |
|---|---|---|---|---|
| 16:54–16:55 | 1 个 | 4.0%–4.2% | 8 | 2.8 / 2 |
| 16:56–16:58 | 4 个 | 13.7%–19.1% | 5–11 | 2.6 / 2 |

新号拿到老号 3–4 倍的流量，约 4 分钟就耗完 30 次预热，然后回到普通水平。设计意图是「有余量时新号缓慢进入流量」，实际正好相反。

## 根因

旧逻辑（`strategy.rs` 的 `warmup_target_share_with_config` / `should_select_warming_from_totals`）：

```
target_share  = min(credentialWarmupSelectionPercent × 预热账号数, credentialWarmupMaxSelectionPercent)
current_share = 预热组近 60 秒选中数 / 全部候选近 60 秒选中数
current_share < target_share → 只在预热组里选，否则只在就绪组里选
```

1. **基准错误**：份额按号池总流量算，而不是按单个就绪账号的平均流量算。号池超过 20 个账号后，5% 就大于平均份额 `1/N`；88 个账号时新号拿到普通号的约 4.4 倍。号池越大，超配越多。
2. **没有爬坡**：从第一次请求起就按满额份额分流，与预热进度无关。
3. **会话粘性放大**：被预热抽中的新会话会绑定到新号，之后整段会话的请求都走新号，绕过份额控制。线上新号的请求里大部分是 `fallback_from_sticky`。
4. **份额判断在档位筛选之后**：更高 Fast/优先级档位里的新号直接成为最优档位，吃掉该档位的全部请求。

## 新设计

预热只在「有就绪账号可调度」时生效，按就绪账号的平均负载限量；没有就绪账号可用时，预热账号作为溢出容量照常接流量，不会因为预热让请求排队。

### 单账号配额（`retain_warmup_quota`，`selection.rs`）

- 负载 = 近 5 分钟选中数 + 当前在途请求数（`strategy::warmup_load`）。
  - 用 5 分钟窗口：配额常常不到每分钟 1 个请求，60 秒窗口分辨不出来。
  - 计入在途请求：选中记录发生在 token 就绪之后，中间这段时间到达的并发请求不能都看到「空闲的新号」然后一起涌进去。
- 权重 = `credentialWarmupSelectionPercent` 线性升到 100%（`strategy::warmup_weight`）：

  ```
  progress = (span - warmup_remaining) / span，span = max(credentialWarmupRequests, warmup_remaining)
  weight   = start + (1 - start) × progress
  ```

- 预热账号负载 < 权重 × 参照平均负载时保留，否则这次请求不考虑它。参照平均负载取同 Fast 同优先级的就绪账号，没有则取同 Fast，再没有则取全部就绪候选。
- 完全空闲（窗口内 0 次且无在途请求）的预热账号总可以接 1 个请求，保证低流量或空闲号池下预热仍能推进。
- 这一步发生在 Fast/优先级档位筛选**之前**：超出配额的预热账号不参与 `max(fast)` 和首选优先级计算，高档位的新号也是逐步放量。

### 总上限

预热账号合计负载达到 `credentialWarmupMaxSelectionPercent` 时，本次只在就绪账号里选。`0` 表示有就绪账号时预热账号只接溢出流量。

### 就绪账号过载保护

全局容量配置变更（`credentialRpm`、并发、负载均衡模式、预热参数等）会把所有活跃账号重新打回预热，这是既有行为，本次没有改动。此时就绪账号很少，严格限制预热账号会把流量全部压到最先完成预热的几个号上。所以权重和总上限都有下限，保证就绪账号的流量不超过公平份额的 `WARMUP_READY_OVERLOAD_LIMIT = 2` 倍：

```
权重下限 = ((ready + warming) / 2 - ready) / warming（ready 远多于 warming 时为 0，不起作用）
总上限下限 = 1 - 2 × ready / (ready + warming)
```

### 会话绑定（`credential_accepts_session_binding`，`manager.rs`）

预热账号接收新会话绑定也按同一权重限量：它的活跃绑定会话数要低于「就绪账号平均活跃绑定会话数 × 权重（含过载下限）」。超出时这次请求照常转发，只是不绑定，会话的下一次请求重新选号。没有就绪账号时照常绑定，避免整个号池预热期间失去会话粘性。

已绑定会话的档位迁移（`should_migrate_sticky_to_better_tier`）不再把预热账号当成「更优档位」：会话既然不会绑过去，迁移只会让它白白丢一次绑定。等账号预热完成后，会话再逐步迁移。

## 场景覆盖

| 场景 | 行为 | 测试 |
|---|---|---|
| 大号池新增 1 个号 | 起步 5% × 平均负载，全程低于平均 | `new_credential_in_a_large_pool_ramps_up_slowly`、`large_pool_does_not_overfeed_a_new_credential` |
| 预热进度推进 | 配额线性放大 | `warmup_weight_ramps_from_start_to_full`、`quota_grows_with_warmup_progress` |
| 并发突发 | 在途请求计入负载 | `in_flight_requests_count_against_the_quota` |
| 就绪账号全满/冷却/限流 | 预热账号接溢出流量，不排队 | `warming_credential_takes_overflow_when_ready_credentials_are_full`、`warming_credentials_take_overflow_when_no_ready_one_is_dispatchable` |
| 批量导入大量新号 | 受总上限约束 | `group_cap_limits_a_large_warming_batch` |
| 就绪账号很少（含全局配置重置预热） | 放宽配额，就绪账号 ≤ 2 倍公平份额 | `few_ready_credentials_relax_warmup_instead_of_becoming_hot_spots`、`few_ready_credentials_share_load_with_warming_ones` |
| 全部账号都在预热 | 不限流，照常绑定会话 | `warming_pool_without_ready_credentials_still_binds_sessions`、`test_simulation_balanced_mode_spreads_new_warming_batch` |
| 新号在更高 Fast 档 | 超配额时不抢整个档位 | `warming_credential_in_a_better_fast_tier_ramps_up` |
| 新号在更高优先级档（priority 模式） | 同上，逐步放量 | `priority_mode_splits_warmup_only_inside_the_preferred_tier` |
| 新号在更低档位 | 不跨档位抢流量（档位规则不变） | 同上、调度矩阵 `warmup_in_lower_tier` |
| 配额参照取同档位 | 高/低 Fast 档负载差异大时不误判 | `quota_reference_is_the_same_tier`、`quota_reference_falls_back_when_the_tier_has_no_ready_credential` |
| 起始比例 / 总上限为 0 | 预热账号只接溢出流量 | `zero_start_percent_keeps_warming_to_overflow_only`、`group_cap_limits_a_large_warming_batch` |
| 会话粘性 | 预热号限量接收新绑定，已绑定会话不迁移到预热号 | `warming_credential_does_not_take_new_session_bindings_while_ready_ones_exist`、`sessions_do_not_migrate_to_a_warming_credential_in_a_better_tier` |
| 四种负载均衡模式 | 统一生效（在模式排序之前过滤） | 调度矩阵 `selection_scenario_matrix_matches_expected_dispatch_order` |
| 无 Redis 单实例 | 选号前裁剪本地窗口，过期计数不会卡住配额 | `select_next_credential_excluding` |
| 多实例 + Redis | 选中计数来自 Redis 共享窗口，在途数来自共享 lease | 沿用既有同步路径 |

## 配置建议

- 字段含义变化：`credentialWarmupSelectionPercent` 从「每个预热号占总流量的百分比」改为「起步时占就绪账号平均流量的百分比」。现有配置不用改，含义变了但方向一致（数值越小越慢）。
- 59138 当前配置 `credentialWarmupRequests=30`、`credentialWarmupSelectionPercent=5`、`credentialWarmupMaxSelectionPercent=50`，新逻辑下合适：新号从约 0.05 倍平均流量起步，随 30 次成功线性升到 1 倍。
- 想更慢就调小起始比例或调大预热次数；预热次数越大，爬坡越平滑。

## 已知限制

- 配额基于近 5 分钟负载。号池整体流量很低时，每个空闲预热号每 5 分钟至少能拿到 1 个请求；这是为了让预热能推进，在这种流量下不构成压力。
- 预热只看「请求条数」，不区分请求的 token 量；加权容量（`weightedCapacity`）开启时，选中次数按权重单位累加，配额自然按权重计算。
