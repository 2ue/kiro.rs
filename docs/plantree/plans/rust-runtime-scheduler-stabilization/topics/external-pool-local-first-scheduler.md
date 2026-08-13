# 外部池与本地凭证调度剩余项

Last reviewed: 2026-07-28 Asia/Shanghai

Related:

- [外部池调度影响本地凭据与 fallback 矩阵缺失](../../../../../feature/issues/external-pool-scheduler-interference-and-fallback-matrix-20260727.md)
- [整体调度架构分析](../../../../../feature/issues/scheduler-architecture-analysis-purpose-and-plan.md)

## 已完成但需要继续守住的边界

本轮已修复：

- 外部池开启后，本地优先路径不再同步等待外部池 PgSQL/Redis。
- raw preflight、parsed preflight、本地失败 fallback 的 route gate 改为 cached/no-wait。
- direct external policy 命中后不因为本地账号存在而退回本地。
- 外部池失败后 local rescue 保持 bounded、local-only、共享 attempt budget，防止明显回环。

这些不代表整个外部池调度已经最终完成。

## P0-1：本地容量不足时的“排队优先 vs 外部池接管优先”需要显式配置

问题：

- 当前行为已经避免了冷缓存时同步等待外部池，但“本地容量不足时应该排队还是优先外部池接管”仍不是完整产品策略。
- 用户期望外部池作为本地凭证不够用时的补充，但容量不足可能有多种含义：短暂并发满、RPM 满、Redis degraded、全部冷却、无模型兼容、无账号。

建议设计：

- 新增显式策略，例如：
  - `localCapacityOverflowPolicy=queue_first`
  - `localCapacityOverflowPolicy=external_first_when_cached_ready`
  - `localCapacityOverflowPolicy=external_first_with_bounded_wait`
- 不要再让外部池缓存是否可用隐式决定用户策略。

验收：

- 表驱动测试覆盖本地容量正常、并发满、RPM 满、Redis degraded、全部冷却、无账号。
- usage routeSubtype 能解释每次是本地排队、本地失败、外部池接管还是 local rescue。

## P0-2：无本地账号时的临时外部池直连与回切本地

用户目标：

> 外部池开启、非直连、本地没有账号时，相当于直连外部池；但后续如果添加本地账号，应尽快回到本地优先。

未完成点：

- 当前实现依赖本地 route state 和缓存失效，缺少明确的“无本地账号临时直连”状态机。
- 新增本地账号后如何快速回切，需要事件触发或短 TTL 保证。

建议设计：

- RoutePlanner 明确输出 `TemporaryExternalDirectBecauseNoLocalCredential`。
- 本地凭据新增/启用/模型限制更新时，发布本地容量 generation 事件。
- local-first 路由读取本地 generation，尽快从临时外部池回切本地。

验收：

- 无本地账号 + 外部池开启 + 非直连：请求走外部池。
- 添加本地账号后，下一轮或短 TTL 内走本地优先。
- 外部池不可用时返回明确 external capacity/error，不伪装成本地账号错误。

## P0-3：外部池 cooldown 策略需要产品化

问题：

- 当前外部池会因 429、5xx、网络、协议、模型不可用等进入 cooldown，但用户反馈“冷却不能手动恢复，很不好”，且需要控制哪些错误才冷却。
- 外部池错误集中爆发时，cooldown 写入和缓存失效可能放大调度压力。

建议设计：

- 为每类错误提供配置：
  - 是否触发 cooldown。
  - pool 级还是 model 级。
  - 冷却时长。
  - 是否计入 auto-disable。
  - 是否允许手动恢复。
- 管理页面提供：
  - 当前 cooldown 原因。
  - 剩余时间。
  - 手动解除 cooldown。
  - 手动禁用/启用池。

验收：

- 429 burst、500 burst、protocol error、network timeout、model unavailable 分别可测试。
- 关闭某类 cooldown 后，对应错误不写 cooldown。
- 手动恢复后，外部池可重新参与调度。
- cooldown 写入失败不影响本地主路径。

## P0-4：外部池内部 retry/failover 与本地 fallback/rescue 要彻底解耦

问题：

- 外部池内部多池 failover、本地失败 external fallback、外部失败 local rescue 是三套不同语义。
- 如果不统一状态机，很容易出现“本地 -> 外部池 -> 本地 -> 外部池”的隐式回环或 attempts 放大。

当前已有防护：

- local rescue 使用 provider-local entrypoint。
- local rescue `max_sends=Some(1)`。
- local rescue 共享 `InferenceAttemptBudget`。

仍需补强：

- Route trace 明确记录每次转换。
- 所有转换必须有上限和禁止回边。
- 外部池内部 retry 不应再次触发 handler 级 fallback。

验收：

- 表驱动测试证明每种错误路径最多触发预期 sends。
- usage 能区分：
  - local attempt
  - external fallback
  - external internal failover
  - local rescue
  - final error
- 没有任何测试能构造无限 fallback/rescue 循环。

## P1：外部池坏配置隔离与管理面修复

问题：

- 生产曾看到空 `api_key` 外部池记录。
- 坏记录不应该每轮请求重复解析、重复告警、重复刷新，尤其不能污染本地路径。

建议：

- dispatchable 外部池查询在存储层过滤明显坏记录。
- 管理面显示配置错误原因。
- 坏记录默认不参与 dispatch snapshot。
- 提供一键禁用/修复提示。

验收：

- 空 `api_key`、非法 base_url、非法 auth_type、模型规则非法等坏记录不会进入调度。
- 坏记录存在时，本地请求 p95 不受影响。
