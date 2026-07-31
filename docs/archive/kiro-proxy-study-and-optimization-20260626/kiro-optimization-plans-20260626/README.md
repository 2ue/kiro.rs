# Kiro 代理优化实施方案索引

本目录不是项目对比结论，而是后续可直接进入开发排期的实施方案集合。每个文档都必须能脱离当前会话独立阅读和执行。

## 全局约束

- 本目录只描述方案，不代表已经改动业务代码。
- 所有对下游返回的信息必须使用统一英文口径。
- 对下游不得暴露内部模块概念，包括但不限于 pool、fallback、external、credential、备用、调度内部细节。
- 产品文案统一使用 account / 账号概念；代码里的历史字段名可以保留，但新增对外字段和管理端文案必须避免引入新的混乱概念。
- `/cc`、`/ha`、`/na`、`/dfcache/*` 的现有行为必须保持兼容；任何新能力都必须有默认关闭或兼容迁移策略。
- 不得默认启用 full response cache；缓存优化必须先保证语义正确、内存有上限、可回滚。
- 任何会改变调度、重试、限流、streaming 或错误映射的方案，都必须先具备可复现测试和回滚开关。

## 推荐实施顺序

1. [真实压测与异常测试工具](./04-loadtest-and-chaos-test-harness.md)
2. [结构化调度失败原因](./02-selection-failure-reasons.md)
3. [观测链路与错误归一化](./10-observability-trace-and-error-normalization.md)
4. [Tool-use 异常格式回归矩阵](./05-tool-use-malformed-regression.md)
5. [Stream idle 与上游异常处理](./06-stream-idle-and-upstream-exception.md)
6. [账号调度模块拆分](./01-token-manager-module-split.md)
7. [调度策略、健康分与优先级解释](./03-scheduler-strategies-health-score.md)
8. [profileArn 与 region 自愈](./07-profile-arn-region-self-heal.md)
9. [cachePoint 与缓存归一化](./08-cachepoint-and-cache-normalization.md)
10. [Endpoint failover 策略](./09-endpoint-failover-policy.md)
11. [管理端账号体验与缓存边界展示](./11-admin-account-ux-and-cache-bounds.md)
12. [总实施顺序、依赖与发布门禁](./12-implementation-sequence-and-dependencies.md)

## 来源分析文档

详细项目对比已经拆分在 [`../kiro-proxy-study-20260626/`](../kiro-proxy-study-20260626/README.md)。本目录的方案吸收这些项目的优点，但不直接照搬它们的实现。
