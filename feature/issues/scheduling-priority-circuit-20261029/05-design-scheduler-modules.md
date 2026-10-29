# 调度模块拆分与优先级规则设计

## 现状

- 4 种模式：`priority`（默认）、`balanced`、`health_balanced`、`weighted_least_inflight`。运行时用 `Mutex<String>` 保存（`manager.rs:963`），未知值静默按 priority 处理（5867）。
- 按模式区分的只有两处：排序的字符串 `match`（5839）、槽位竞争时 priority 只在同档里找备选（5620）。其余步骤四种模式共用，代码里没有策略抽象。
- 重复：`balanced_selection_key` 与 `priority_selection_key` 完全一样（`strategy.rs:275-301`），balanced 实际也按档调度；`health_balanced` 与 `weighted_least_inflight` 的打分公式完全一样（33-59、246-273）；`effective_max_concurrent_requests` 有两份（strategy.rs:303、capacity.rs:127）。

## 模块拆分

外层流程最容易出 bug（门锁、槽位预占、Redis 并发租约、RPM 预约、Token 刷新），保持不动。只把每种模式自己的决策抽出来：

| 模块 | 内容 |
| --- | --- |
| `candidates`（共用） | 可调度过滤：启用、额度、模型、代理、冷却、RPM、并发、路由排除、空回排除；同时产出拒绝原因，selectionFailure 直接复用 |
| `warmup` / `fast`（共用） | 预热分组、Fast 档筛选，写成纯函数，由策略决定在哪一步调用 |
| `sticky`（共用） | 绑定的读写、迁移、限速；是否沿用绑定由策略决定 |
| `strategy` trait | `select`（从候选中选号）、`sticky_decision`（沿用、迁移、借号）、`slot_race_alternatives`（槽位竞争后的备选范围） |
| `priority` / `balanced` / `health_weighted` | 每个模式一个文件；health 与 weighted 共用一份实现，两个模式名都保留 |

模式改为枚举，配置校验时拒绝未知值。

## 优先级模式规则

1. 先过共用过滤。路由规则、模型、本请求的空回排除都在这一步处理，所以某一档对这个请求全不可用时，会自然落到下一档。
2. Fast 仍排在优先级之前（沿用用户之前定的规则）。
3. 首选档 = 当前还有可调度容量的最小 priority，只在这一档里选。
4. 预热只在首选档内部分流。
5. 首选档整档满（并发、RPM、冷却）时立刻降到下一档；槽位竞争时用同一规则。
6. 会话绑定：
   - 绑定账号在首选档：沿用（现状）。
   - 绑定账号低于首选档，且首选档有空闲：改绑到首选档，复用 Fast 迁移的限速（每进程每秒 1 个），同一会话在一段时间内只迁一次。
   - 绑定账号暂时满：临时借号，不改绑（现状）。
7. 同档内排序见 [02-account-spread.md](02-account-spread.md)。

## 避免引入 bug

分两步，各自单独提交：

1. **纯重构，行为不变。** 先把现在的选号结果做成场景矩阵基准测试（4 种模式 × 预热 × Fast × 绑定 × 满载、限流、冷却），重构前后逐条一致。
2. **只改 priority 的行为**，其他模式保持原样。新行为单独写测试：预热不越档、满档降档、槽位竞争降档、绑定迁回限速。

最后用 mock 上游加真实 `claude -p` 做调度测试（见本地测试环境记录）。
