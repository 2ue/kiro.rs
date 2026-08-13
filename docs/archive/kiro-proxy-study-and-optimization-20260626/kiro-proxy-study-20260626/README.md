# Kiro 代理项目逐项对比研究

日期：2026-06-26  
范围：只分析，不实施业务代码改动。  
目标：拆开分析每个 Kiro 代理/网关/账号管理项目在调度、架构、缓存、Kiro 上游调用、协议兼容、测试与观测上的实现，提炼当前 `kiro.rs` 后续可以学习吸收的部分。

## 阅读方式

这次不再使用一个大文件泛泛总结，而是按项目拆分：

| 文档 | 内容 |
| --- | --- |
| [00-current-kiro-rs-baseline.md](./00-current-kiro-rs-baseline.md) | 当前项目基线，先明确现有优势、短板和不可退化能力 |
| [01-kirocc-prox.md](./01-kirocc-prox.md) | `kirocc-prox`，重点是调度边界、selector/conductor/runtime lease、OTel、idle reader |
| [02-kiroxy.md](./02-kiroxy.md) | `kiroxy`，重点是 rolling health、native headers、endpoint failover、loadtest、GateWriter |
| [03-local-kiro2api.md](./03-local-kiro2api.md) | 本地 `kiro2api`，重点是 cachePoint、thinking tag、SSE writer、工具结果转换 |
| [04-kiro-go.md](./04-kiro-go.md) | `Kiro-Go`，重点是 profileArn/region、自愈、账号池、Responses API |
| [05-9router.md](./05-9router.md) | `9router`，重点是多 provider registry、Kiro executor、thinking 统一化、tool history 400 防护 |
| [06-dntproxy.md](./06-dntproxy.md) | `dntproxy`，重点是选择失败原因、策略、model lock、fallback 边界 |
| [07-kiro-account-manager.md](./07-kiro-account-manager.md) | `Kiro-account-manager`，重点是桌面账号管理、Kiro 调用、prompt cache、代理与注册体验 |
| [08-rust-forks-and-cache.md](./08-rust-forks-and-cache.md) | Rust fork 与轻量项目：`pluto2sun/kiro2api`、`TsinHzl/kiro2cc-proxy`、`ndycode/kiro-rs`、`cp-coder9/kiro-gateway` 等 |
| [09-implementation-backlog.md](./09-implementation-backlog.md) | 从所有项目提炼出的当前项目后续学习落地清单 |

## 样本来源

远端项目已下载到：

`/Users/yuanfeijie/Desktop/procode/kiro-research`

同时分析本地已有目录：

- `/Users/yuanfeijie/Desktop/procode/9router`
- `/Users/yuanfeijie/Desktop/procode/Kiro-Go`
- `/Users/yuanfeijie/Desktop/procode/Kiro-account-manager`
- `/Users/yuanfeijie/Desktop/procode/kiro2api`
- `/Users/yuanfeijie/Desktop/procode/kirocc-prox`

## 总体判断

当前 `kiro.rs` 的生产化能力整体更完整，尤其是 PgSQL/Redis 状态、账号级 RPM、并发 lease、dispatch wait、usage 异步记录、外部账号池、错误归一化和请求 ID。多数外部项目不能整体替换当前项目。

真正应该学习的是外部项目在局部上的清晰实现：

- 调度模块边界：`kirocc-prox` 比当前项目更清晰。
- 健康权重与抗突发：`kiroxy` 比当前项目更容易解释。
- 真实 Kiro cachePoint：本地 `kiro2api` 和 `kiroxy` 有直接参考。
- Kiro native 调用形态与 endpoint fallback：`kiroxy`、`Kiro-Go`、`9router` 都有可研究价值。
- profileArn 与 region 自愈：`Kiro-Go` 做得最集中。
- 多 provider 失败原因模型：`dntproxy`、`9router` 比当前项目更结构化。
- 测试组织：`cp-coder9/kiro-gateway` 的隔离测试矩阵虽然实现轻，但测试覆盖组织值得借鉴。

## 优先级摘要

P0 适合优先学习：

- 把当前 `src/kiro/token_manager.rs` 拆成 selector、capacity/runtime lease、session affinity、failure reason、health score 几个模块。
- 增加结构化调度失败原因，不再只靠字符串和页面拼装解释。
- 建当前项目自己的真实压测/回归脚本，覆盖 TTFB、SSE、RPM、并发、账号失败恢复、内存增长。
- 补 Kiro upstream 200 JSON exception、idle stream、tool-use malformed、thinking 长会话的测试矩阵。

P1 适合在 P0 后吸收：

- weighted least-inflight 策略或作为现有 health 策略的可解释子策略。
- Kiro profileArn region 自检/自愈。
- feature flag 下的真实 Kiro cachePoint。
- endpoint failover 作为管理开关，不默认开启。
- OpenTelemetry 或兼容 trace exporter。

P2 暂不急：

- full response true cache。
- 桌面 MITM/注册器能力内置到服务。
- 多 provider 大 registry。
- synthetic agentic prompt 这种客户端行为改写。

