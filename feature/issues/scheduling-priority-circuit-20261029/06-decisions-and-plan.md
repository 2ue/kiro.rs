# 决策、实施顺序、验收

## 决策

1. Fast 继续排在优先级前面。
2. `balanced` 行为不变：同样按优先级排序，但不限定只用首选档。
3. 首选档满时，在同一次选号内立刻降档，不排队。
4. 同会话在单账号的最大并发默认不变，开放为配置 `credentialSessionMaxConcurrentRequests`。
5. 会话迁回高档时接受上游多一次冷启动；本地模拟缓存不含账号，对外 usage 的缓存字段连续。
6. 不依赖按 key 限并发保护调度。

## 实施顺序

1. 现网只读数据确认账号分散的原因排序（见 02）。
2. 调度纯重构加场景矩阵基准测试。
3. 优先级规则、同档分散、会话并发上限，各自单独提交。
4. 熔断改造（03）。
5. 排队、租约回收、用量记录、健康检查（04）。
6. mock 上游端到端调度测试和全量测试。

提交列表见 [07-implementation.md](07-implementation.md)。

## 验收

| 项目 | 结果 | 依据 |
| --- | --- | --- |
| 优先级 0 有空闲时，新会话全部落在优先级 0；预热中的低档账号不越档 | 通过 | 场景矩阵；`priority_mode_splits_warmup_only_inside_the_preferred_tier`；mock 端到端 272/272 落在优先级 0 |
| 优先级 0 全满时，新请求在同一次选号内降到下一档，不排队 | 通过 | `priority_mode_overflows_to_next_tier_in_the_same_pick_when_preferred_is_full`；槽位竞争测试 |
| 绑定在低档的会话，高档有空闲后按限速迁回并改绑；临时满只借号不改绑 | 通过 | `sticky_sessions_bound_to_a_lower_tier_move_back_one_per_interval_and_rebind`；`sticky_session_in_preferred_tier_borrows_without_rebinding_when_bound_is_full` |
| 同档内新会话按绑定会话数拉平，历史成功数不影响 | 通过 | `new_sessions_spread_evenly_across_a_tier_by_active_bound_sessions`（30 个会话 3 个账号各 10 个） |
| 新导入账号被风控不触发全池熔断；比例阈值；打开时清空窗口；半开探测 | 通过 | `risk_circuit_*` 5 个测试 |
| 进程被强杀后，残留并发租约约 30 秒内回收；正常停止时退出前释放 | 通过 | Redis 集成测试；mock 端到端 kill -9 后 26 秒回收、SIGTERM 后 2 秒内归零 |
| 排队打满时拒绝在毫秒级返回，Retry-After 随排队长度变化 | 通过 | mock 端到端过载 19611 个 429，p50 1.8 毫秒，Retry-After 2–3 秒 |
| 被拒请求和中途取消的请求都有用量记录 | 通过 | `full_dispatch_queue_is_rejected_before_body_processing`；`request_cancelled_while_queued_records_client_dropped` |
| 健康检查能发现调度停止推进 | 通过 | `dispatch_progress_reports_a_stalled_queue`；mock 端到端 `/readyz` 报告推进状态 |
