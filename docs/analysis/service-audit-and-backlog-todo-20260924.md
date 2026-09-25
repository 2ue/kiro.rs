# 服务审计与并发积压修复 TODO

- 文档日期：2026-09-25（执行收尾）
- 当前代码基线：工作树 `main`，版本 `0.0.170`
- 关联计划：[服务问题审计与并发积压因果](../plantree/plans/rust-runtime-scheduler-stabilization/topics/service-audit-and-backlog-causality.md)
- 输入文档：
  - [服务问题审计 v0.0.170](./service-problem-audit-v0.0.170-20260924.md)
  - [现网并发积压与长耗时请求因果分析](./prod-concurrency-backlog-causality-20260925.md)

> `prod-concurrency-backlog-causality-20260925.md` 是用户提供的分析输入，不等同于
> 生产证据。当前执行日期也是 2026-09-25，但仍未取得生产配置、活动 lease 快照和
> 请求时间线，因此生产根因不能仅凭该文档或本地 fake-upstream 结果定案。

## 固定不变量

- 不新增或降低请求、调度、上游、流空闲、队列或总时长限制。
- 不因为请求已经持续很久而截断仍在输出合法数据的流。
- 不移动 API Key RPM 预留顺序。
- 不改变账号并发、队列容量、FIFO、排队超时、dispatch 超时、重试边界、客户端错误语义。
- 不向生产或其他项目服务施加压力；动态验证只使用项目归属实例
  `127.0.0.1:19023` 和 fake upstream。

## TODO 与状态

| 编号 | 结论/动作 | 状态 | 完成证据 |
| --- | --- | --- | --- |
| A1 | 审计 #1：只修正已确认的请求热路径，把 Redis dispatch queue lease 获取改为 `async/await`；不扩大到 63 个桥的无证据重构。 | 已实现并验证 | `src/kiro/token_manager/manager.rs`；Redis 取消/lease 测试、全量 Rust 门禁通过 |
| A2 | 审计 #2：Admin Key 使用共享 store；Redis 配置事件和 60 秒周期重载都刷新；旧 key 失效、新 key 生效。 | 已实现并验证 | `src/main.rs`、`src/admin/middleware.rs`；Admin 定向测试、全量门禁通过 |
| A3 | 审计 #3：改用 `parking_lot::RwLock`，空配置 key 或空请求 key 一律拒绝。 | 已实现并验证 | `admin_auth`、cloned key-store 单元测试 |
| A4 | 审计 #4：不单独做大范围同步/异步实现删除；A1 已减少一个实时桥，剩余桥等有测量证据后再处理。 | 评估完成，延期 | 计划专题 Assessment |
| A5 | 审计 #5：不在本轮拆分巨型模块；保留现有 `docs/refactor-plan/`，另起架构计划时再执行。 | 评估完成，延期 | 计划专题 Assessment |
| A6 | 审计 #6：不批量改 Clippy 基线；最终门禁记录当前基线与告警数量。 | 评估完成，延期 | 计划专题 Assessment |
| A7 | 双前端重复构建：按用户明确要求排除，不评估、不改动。 | 明确排除 | 用户指令 |
| A8 | 只增加 `.DS_Store` 忽略规则，不删除 `.DS_Store`、`.kilo` 或其他未知归属内容。 | 已实现并验证 | `.gitignore`、差异检查 |
| B1 | 新增 `credentialDispatchElapsedMs`，累计一次请求内各次凭据获取/排队耗时。 | 已实现并验证 | `InferenceAttemptBudget`、usage trace、三轮低容量真实运行 |
| B2 | 新增 `upstreamHeaderWaitMs`，累计每次实际上游发送等待 response headers 的耗时；失败和允许的 retry 也计入。 | 已实现并验证 | Kiro provider / external forward path、三轮低容量真实运行 |
| B3 | 保留旧 `upstreamHeaderMs` 口径，不新增总墙钟上限，不掩盖历史数据兼容性。 | 已实现并验证 | Usage trace 兼容字段、低容量运行结果 |
| B4 | 验证长请求占槽、队列释放、取消、慢但持续输出流；证明原本可完成的请求不会因本轮优化被掐掉。 | 已实现并验证 | `feature/evidence/service-audit-backlog-remediation-20260924.md`；`run8/run9/run10` 每轮通过 |
| B5 | 复核 `credential_dispatch_max_wait_secs=0` 的现网配置；未取得配置前不声称生产排队根因已证实。 | 待生产只读证据 | 计划 Open Questions |

## 执行顺序

1. 定向 Admin、时延 trace、Redis queue lease 测试：已完成。
2. 默认/no-default 全量测试、release build、Clippy baseline、diff/artifact gate：已完成。
3. fake upstream 并发积压矩阵：占槽、排队、释放恢复、取消、慢但持续输出：已完成，独立新数据库三轮通过。
4. 按项目测试实例策略复核 `19023` 归属；只运行 bounded fake-upstream L1/L3，不施加真实上游压力：已完成。
5. 将命令、通过/失败/跳过、候选二进制 SHA、资源清理和剩余缺口写回证据文档与计划索引：已完成。

## 完成判定

- 所有已实现项有源代码、单元/集成测试或动态证据支持。
- 队列和 lease 在成功、队列满、Redis 错误、超时、取消后都无泄漏。
- 新时延字段可选、camelCase，旧记录和 external trace 不因缺少字段而反序列化失败。
- fake upstream 释放容量后，原配置下被接受的正常请求完成；无新增拒绝类型。
- 持续产生合法数据的长流没有被新增墙钟上限截断。
- 生产因果结论只在取得 `credential_dispatch_max_wait_secs`、队列/账号快照及请求时间线后更新。

## 当前结论

本轮可落地的修复和优化已完成。低容量真实 HTTP runner 证明当前方案在容量被长流占用时
会让正常请求排队、在容量释放后继续完成，并且不会通过截断合法长流来制造吞吐。仍需
生产只读脱敏证据才能判断现网几百秒积压是否由长流主导、是否存在
`credential_dispatch_max_wait_secs=0`，以及是否叠加上游 header 延迟、重试放大或入口
队列等待。A4/A5/A6 是有意延期的独立维护工作；A7 按用户要求明确排除。
