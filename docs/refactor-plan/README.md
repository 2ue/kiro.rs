# 多渠道账号调度系统重构方案

本目录是针对 `~/Desktop/procode/2ue_kiro.rs` 的独立分析与重构计划。它只以当前源码、配置样例、前端工程和构建脚本为依据，不属于任何项目规划树，也不要求先实施代码改动。

这套文档的目标是让没有参加当前会话的实现者，也能明确知道：当前系统实际上做了什么、哪些行为必须保持、哪些边界必须重新划分、目标模块如何协作、如何逐阶段迁移，以及如何证明重构没有改变业务逻辑。

## 阅读顺序

1. [01-现状与不可回归不变量](<01-现状与不可回归不变量.md>)：建立源码事实和行为基线。
2. [02-目标架构与模块边界](<02-目标架构与模块边界.md>)：确定 Go 后端、React 前端和插件化模块的总结构。
3. [03-渠道账号领域模型与 Provider 契约](<03-渠道账号领域模型与 Provider 契约.md>)：定义统一的渠道/账号概念，以及 Kiro 和 Claude Code Base URL+Key 的隔离方式。
4. [04-通用调度、路由与 fallback](<04-通用调度、路由与 fallback.md>)：把可复用调度内核和 Provider 特殊指标拆开，完整保留现有路由能力。
5. [05-缓存模块与缓存重建规则](<05-缓存模块与缓存重建规则.md>)：这是本次重构的行为保护重点，记录计算、整形、scope 和成功提交语义。
6. [06-全局与覆盖配置及管理 API](<06-全局与覆盖配置及管理 API.md>)：说明如何拆解当前巨大的 `RuntimeConfig`，并保持旧 API/配置可迁移。
7. [07-前端重构方案](<07-前端重构方案.md>)：规划 React + Tailwind CSS 管理台、独立缓存页面和组件约束。
8. [08-实施阶段、数据迁移与发布](<08-实施阶段、数据迁移与发布.md>)：给出从行为冻结到切换回滚的执行顺序。
9. [09-测试与验收矩阵](<09-测试与验收矩阵.md>)：定义实现完成的证据，而不是只依赖编译通过。
10. [10-风险、决策与待确认项](<10-风险、决策与待确认项.md>)：集中记录迁移风险、版本矩阵和需要在开工前确认的事项。
11. [11-实施蓝图与现状映射](<11-实施蓝图与现状映射.md>)：把现有 Rust 模块、旧字段、请求时序和阶段交付物映射到 Go 目标实现。

## 方案结论先看

- **统一产品概念**：产品层只使用“渠道（channel）”“账号（account）”“Provider 类型（providerType）”“协议（protocol）”。“本地 Kiro 账号”和“外部池”是两类独立渠道实现，不再作为互相嵌套的业务对象。
- **严格隔离协议**：Kiro Provider 私有拥有 Kiro token、endpoint、machine id、EventStream、Kiro request/response 转换和本地额度语义；第三方 Claude Code Base URL+Key Provider 只做 Anthropic/Claude Code 兼容请求透传、必要的模型映射、SSE 解析和 usage projection，不调用 Kiro converter。
- **复用调度语义，不复用错误边界**：通用调度内核负责候选筛选、优先级、sticky、容量 lease、队列、冷却、重试预算和 fallback 状态机；Provider 以能力和额度指标插件提供特殊可用性判断与评分。
- **缓存成为独立模块**：路由缓存策略单独配置、单独页面、单独 API。原有缓存计算和整形逻辑按“raw → shaped → reported”三层迁移，真实上游 metadata 优先，只有成功请求才更新 tracker。
- **配置采用 global → route → channel → account 的覆盖链**：每个页面只写入自己的 schema，使用版本号/ETag 防止覆盖未知字段；Kiro 专属设置不能出现在外部账号表单中。
- **技术栈确定**：后端采用 Go，前端采用 React + TypeScript + Tailwind CSS。所有后续设计均按这一分层执行。

这不是把现有 Rust 目录机械翻译成 Go。当前代码的价值是提供行为证据和回归样本；目标代码按新的领域边界重建。任何“复用”都必须先回答三个问题：它属于通用业务语义还是某个 Provider 的实现？它的输入/输出契约是什么？它如何被独立测试和替换？

## 现状证据范围

核心证据来自：

- `src/anthropic/`：Anthropic/Claude Code 入口、body pipeline、缓存、usage、SSE 和路由。
- `src/kiro/`：Kiro endpoint、协议转换、token manager、额度/容量/sticky/cooldown。
- `src/external_pool.rs` 及其四个 pipeline 子模块：外部 Base URL+Key 请求、池选择、lease、质量评分、重试和 usage projection。
- `src/model/config.rs`：当前运行配置、路径路由、缓存策略、外部池策略和归一化/校验。
- `src/storage/`、`src/admin/`：PostgreSQL/Redis 状态、管理 API 和运行配置持久化。
- `ui/` 与 `admin-ui/`：两套 React 管理台、API client、缓存/运行配置表单。
- `README.md`、`config.example.json`、Docker 和前端构建配置：部署、版本和默认行为基线。

## 用户要求到文档的映射

| 要求 | 主要文档 |
| --- | --- |
| 本地 Kiro 与外部账号彻底分离 | [03](<03-渠道账号领域模型与 Provider 契约.md>)、[02](<02-目标架构与模块边界.md>) |
| 保持 fallback、路由 allow/deny/all 和既有特性 | [01](<01-现状与不可回归不变量.md>)、[04](<04-通用调度、路由与 fallback.md>) |
| 只有 Kiro 处理 Kiro/Claude Code 协议互转 | [03](<03-渠道账号领域模型与 Provider 契约.md>) |
| 通用调度 + Provider 特殊指标 + 后续扩展 | [02](<02-目标架构与模块边界.md>)、[04](<04-通用调度、路由与 fallback.md>) |
| 独立缓存配置模块和页面 | [05](<05-缓存模块与缓存重建规则.md>)、[07](<07-前端重构方案.md>) |
| 全局配置与覆盖配置 | [06](<06-全局与覆盖配置及管理 API.md>) |
| Go/React/Tailwind 技术方案与版本策略 | [02](<02-目标架构与模块边界.md>)、[07](<07-前端重构方案.md>)、[10](<10-风险、决策与待确认项.md>) |
| 模块化/插件化和可靠性 | [02](<02-目标架构与模块边界.md>)、[03](<03-渠道账号领域模型与 Provider 契约.md>) |
| 缓存计算逻辑原样迁移 | [05](<05-缓存模块与缓存重建规则.md>)、[09](<09-测试与验收矩阵.md>) |
| 统一渠道/账号概念，预留 OpenAI | [03](<03-渠道账号领域模型与 Provider 契约.md>) |
| 需要从当前代码落到实施任务和交付物 | [11](<11-实施蓝图与现状映射.md>) |
