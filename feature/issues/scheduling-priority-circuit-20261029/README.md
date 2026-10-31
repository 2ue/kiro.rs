# 本地账号调度：优先级、分散、熔断与 20:02 故障（2026-10-29）

状态：`implemented / branch fix/scheduler-smooth-dispatch / not-merged / not-released`。现网 59137，事发时版本 0.0.224（`c8eb99a3`），20:12 起为 0.0.225（`12e2db68`）。两版调度、熔断、租约代码相同。

分析方式：现网只读查询（kiro 的 PG、Redis、容器日志，dockerd 日志，同机 sub2api 的 PG 和日志）加代码阅读。没有改现网配置，没有重启。时间为北京时间。

| 文件 | 内容 |
| --- | --- |
| [01-priority-scheduling.md](01-priority-scheduling.md) | 优先级模式下仍打到优先级 10：预热份额和会话绑定都绕过优先级 |
| [02-account-spread.md](02-account-spread.md) | 并发 30 多只占约 20 个账号；同会话并发；会话迁移与缓存 |
| [03-local-pool-circuit.md](03-local-pool-circuit.md) | 本地账号池风险保护（全池熔断）误伤；Redis 熔断的潜在问题 |
| [04-incident-2002-queue-lease.md](04-incident-2002-queue-lease.md) | 20:02 全部失败、重启后并发不回收 |
| [05-design-scheduler-modules.md](05-design-scheduler-modules.md) | 调度模块拆分与优先级规则设计 |
| [06-decisions-and-plan.md](06-decisions-and-plan.md) | 待决策项、实施顺序、验收 |
| [07-implementation.md](07-implementation.md) | 实现结果、提交、现网需要的配置 |

01–06 的代码位置以 0.0.225 为准。
