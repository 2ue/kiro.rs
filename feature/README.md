# Runtime Correctness, Protocol Compatibility, And Release Hardening

Role: 本轮问题分析、复现、修复计划、验证证据与发布记录的最终交付入口

Status: `execute-ready / implementation-and-focused-verification-in-progress / NO-GO`

Last updated: 2026-07-18

## 目标

本目录统一承载本轮对 Claude Code CLI 协议泄漏、thinking、工具与搜索、图片、payload 处理、prompt 配置、外部池、账号调度、Redis、内部 RPM 放大、usage、两套 UI、旧版本升级与发布门禁的分析和落地记录。

所有旧文档中的“已修复”“已验证”都只视为待复核陈述。只有当前源码、可重复测试、带构建身份的运行证据三者一致，才会在本目录的最终状态矩阵中标记为通过。

## 阅读顺序

1. [最终汇总报告](final-report.md)：问题、复现、修复、验证、残余风险与发布判定的唯一总入口；完成前保持 `NO-GO`。
2. [事实与状态总索引](audits/analysis-status-index-20260715.md)：历史问题清单，正在逐项重新定级。
3. [Claude Code 深度审计](audits/claude-code-internal-leak-deep-audit-20260715.md)：上一轮动态复现结果，不等于修复后验收。
4. [单问题文档](issues/README.md)：每类问题的现象、影响、复现、根因、方案和验收条件。
5. [计划](plans/README.md)：事实稳定后形成的实施顺序、依赖、回滚和发布计划。
6. [测试](tests/README.md)：单点、组合、真实 CLI、长对话、故障注入、负载和升级矩阵。
7. [证据](evidence/README.md)：可追溯原始产物、构建哈希、配置快照和敏感信息规则。
8. [发布](releases/README.md)：最终门禁、提交、版本、tag、推送和发布后观察记录。

当前复核与执行入口：

- [当前实施状态](implementation-status.md)
- [当前代码事实矩阵](audits/current-fact-matrix-20260716.md)
- [当前代码重新验证矩阵](tests/reverification-matrix.md)
- [修复、验证与发布总体计划](plans/remediation-and-release-plan.md)

## 状态词

| 状态 | 含义 |
| --- | --- |
| `verified-fixed` | 当前源码机制明确，定向测试与真实路径回归均通过，反例和异常路径也已覆盖 |
| `protection-incomplete` | 已有保护，但存在绕过、路径差异、误删、性能或可观测性缺口 |
| `reproduced-defect` | 当前构建可稳定动态复现 |
| `static-evidence` | 源码机制已确认，但尚缺动态复现或运行证据 |
| `not-tested` | 尚未完成独立验证 |
| `production-evidence-required` | 本地只能验证机制，生产频率或真实部署事实仍需只读取证 |
| `blocked` | 缺少不可替代的输入或环境，且替代验证不能回答问题 |

## 当前阶段

- 工作模式：`execute-ready / status-update`；事实矩阵和分阶段方案已经足以驱动实现，仍按专题验收合同逐项关闭。
- 当前动作：修复剩余 P0/P1 阻断、统一 cleanup/usage/UI 等跨层合同，并把聚焦证据重绑到最终冻结候选。
- 实施约束：聚焦测试通过不等于完整回归；任何既有测试失败、产品文案与后端语义冲突或最终候选证据缺失都继续阻断发布。
- 发布约束：所有适用协议、CLI、负载、混沌、升级、UI 与发布门禁通过前不发版。
- 环境约束：不触碰 `127.0.0.1:9022`，不读取 `kiro_idc_users*.txt`，负载只使用隔离端口和假上游。按用户 2026-07-17 明确要求，不启动或操作 Docker；Docker-backed 场景交付 fail-closed 开发验证程序并记录动态执行豁免。

## 迁移说明

2026-07-16 将原 `docs/feature/` 及本轮散落在 `docs/` 下的相关分析迁入本目录。完整来源到目标映射见 [迁移映射](audits/migration-map-20260716.md)。旧入口保留 moved stub；历史内容保持原貌，只有链接和检索头会按新结构修正。

## 最终完成条件

- 每个确认问题都有独立文档，至少包含现象、影响、指纹与非指纹表现、复现步骤、原始证据、根因、修复设计、风险、验收用例和修复后结果。
- 汇总矩阵中的每个状态都能回链到源码、测试命令、报告和构建身份。
- 正常 body 语义、raw passthrough、thinking、图片、搜索、tool、MCP、count_tokens、stream、usage 等能力有单点与组合回归。
- 异常突发、Redis 慢/断、上游 400/429/500/断流、账号不可用、外部池满/错、客户端断开和恢复过程没有无界重试、RPM 反馈回路或持续资源增长。
- 旧版本数据集升级 smoke、两套 UI、构建、格式化和完整测试通过；Docker 动态 smoke 为本次用户明确豁免项，但对应验证程序必须编译并 fail closed，不能把未执行写成通过。
- Usage soft cleanup 删除范围内明细及其累计统计/费用/credential/Dashboard rollup 贡献，hard cleanup 不重复扣减；既有同 ID 重写合同和两套 UI 文案必须与此一致。
- Redis usage writer 的 snapshot、aggregate、seen marker 必须同一提交单元；高基数、WRONGTYPE、timeout 或断连时不得向读路径暴露半成品，也不得用并发 batch fanout 阻塞 scheduler 热路径。
- 分支先推送，再创建并推送基于远端版本权威计算出的新 tag；发布记录包含回滚点与发布后观察项。
