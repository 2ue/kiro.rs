# 调度目标契约 focused 验证（2026-08-04）

Status: `focused-pass / complete-sustained-matrix-not-run`

## 验证范围

本轮验证目标是确认当前实现是否与已记录的本地账号/外部池调度边界一致，不是宣布统一调度器已经完成。

未修改运行时代码，未连接现网 `9022`，未使用远端账号或外部池凭证。Cargo 均通过 `feature/tests/run-cargo-scoped.sh` 执行，scoped target 在结束时清理。

## Rust focused 结果

### 外部池

命令批次：

```text
RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh scheduler-target-external-focused -- bash -lc 'cargo +1.92.0 test ...'
```

通过：

- 多池优先级/容量选择：`1/1`
- 连续瞬态失败冷却上浮并在成功后恢复：`1/1`
- 同池重试独立于池级尝试预算：`1/1`
- 同池重试优先于跨池切换：`1/1`
- 终态账号错误跳过同池重试并换池：`1/1`
- `Retry-After` 形成真实池冷却：`1/1`
- Raw 池 502 后按模型重选标准处理池：`1/1`
- Raw 正文不触发标准正文保护：`1/1`
- 外部直连策略产生统一直连原因：`1/1`
- 调度降级本地租约只允许在指定 fallback 路线：`1/1`

合计：`10 passed / 0 failed`。

### Handler / fallback / rescue

通过：

- 直连跳过 Raw 预解析：`1/1`
- 直连先解析模型：`1/1`
- 外部路径使用本地处理后的模型别名：`1/1`
- 请求错误不允许本地到外部 fallback：`1/1`
- 容量和瞬态错误允许按配置 fallback：`1/1`
- 终态本地路线阻止外部失败后的 local rescue：`1/1`
- local rescue 要求新鲜本地容量：`1/1`
- 直连对所有错误类型禁用 local rescue：`1/1`
- external preflight 只允许一次 rescue，预算耗尽后阻止回环：`1/1`

合计：`9 passed / 0 failed`。

本轮未设置 `KIRO_RS_TEST_POSTGRES_URL`，因此依赖真实外部备用池 PostgreSQL 的集成测试按测试设计跳过；这不是集成通过证据。

## Node 合同结果

命令：

```text
node --test \
  feature/tests/strict-local-first-routing.contract.test.mjs \
  feature/tests/scheduler-fairness-sticky-race.contract.test.mjs \
  feature/tests/e03-real-two-process-scheduler.contract.test.mjs \
  feature/tests/run-scheduler-redis-chaos-validation.contract.test.mjs \
  feature/tests/external-takeover-scheduler-degraded-nondocker.contract.test.mjs
```

结果：`104 tests / 92 passed / 12 skipped / 0 failed`。

这些是 runner 输入校验、隔离边界、清理契约和源码合同测试，不等同于真实双实例或 Redis chaos 已经执行。

## 文档与构建卫生

- `node feature/tests/check-feature-docs.mjs`：`74 issue documents / 330 relative links / 0 failure`
- `git diff --check`：通过
- 两个 Cargo scoped 批次均报告 `removed=true reservation_released=true`

## 当前结论

本轮证明了：

1. 外部直连不调度本地，且不隐式 local rescue。
2. 本地 fallback、外部同池/跨池重试、Retry-After 冷却和 bounded rescue 的 focused 边界存在。
3. Raw 正文处理与候选池资格已经解耦，模型解析字段参与外部路径。

本轮没有证明：

1. 低数字但持续报错的外部池会被健康高数字池稳定接管；当前代码仍是优先级硬排序。
2. 请求准入、本地等待、外部等待、重试间隔和 rescue 共享同一总 deadline。
3. 双实例冷却传播、租约竞态、三池故障波和 15–30 分钟 soak 后资源回落。
4. usage 已经记录完整候选拒绝、统一尝试账本和运行时配置快照。

因此本证据只能支持 `focused-pass`，不能把 [调度目标符合度矩阵](../docs/plantree/plans/rust-runtime-scheduler-stabilization/topics/scheduler-target-compliance-matrix.md) 的 `不符合` 或 `待测试` 项标为完成，也不能授权发版。

