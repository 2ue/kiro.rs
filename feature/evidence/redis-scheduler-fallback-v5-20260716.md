# Redis Scheduler Fallback v5 Evidence

Status: `focused-tests-passed-integration-chaos-pending`

时间：2026-07-16 02:58:32 +0800

源码基线：`401473ca1649`，工作树包含未提交专项修改。测试二进制 SHA-256：`34c119ac1cca88eff85f465933415134647ac60102890f8b384d751465cb41e0`。该哈希只标识本次 dirty-tree test binary，不是发布二进制。

## 已执行

```text
cargo test model::config::tests -- --nocapture
47 passed, 0 failed
runtime migration subset repeated twice against the same test binary: 6/6, 6/6

cargo test scheduler_fallback_toggles -- --nocapture
three rounds: 2/2, 2/2, 2/2

cargo test external_pool_ -- --nocapture
three rounds: 66/66, 66/66, 66/66

(cd ui && pnpm run build)
TypeScript + Vite production build passed

(cd admin-ui && pnpm run build)
TypeScript + Vite production build passed

git diff --check -- <touched files>
passed
```

覆盖：缺字段、显式 false/true、migration marker 0/1/2/3/4 到 5、marker 5 false 保留、external enabled/disabled、三类旧 fallback 的全开/部分关闭矩阵、普通 capacity 与 scheduler degraded 分类隔离、wait=0 的 30 秒有效上限、external disabled 最终错误无 attempt、两套 UI 默认与最小等待约束。

## 证据限制

`KIRO_RS_TEST_POSTGRES_URL` 和 `KIRO_RS_TEST_REDIS_URL` 未设置，所以 `external_pool_` 组内需要真实隔离 PgSQL/Redis 的用例在测试体内明确跳过。66/66 证明编译、纯函数和无外部依赖路径通过，不能代替以下未完成验收：

- 50/74/75/90/150/500 ms Redis 延迟注入，各 3 轮；
- connection reset、Redis restart、Lua commit-unknown；
- external eligible/full/cooling/disabled 真实路由矩阵；
- 双实例 lease、恢复、无超卖和无残留；
- usage summary/cleanup 压力下 scheduler p95/p99 与 degraded 数量；
- 生产只读升级后 recurrence 复核。

上述项目完成前，本专题状态只能是 partial fix，不能作为最终 release gate 通过证据。
