# 单问题文档索引

Role: 每个独立根因或紧密耦合问题类的事实、复现、设计与验收权威

Status: `execute-ready / issue-by-issue implementation and verification in progress`

## 文档要求

每份最终问题文档必须包含：状态、严重级别、影响面、用户可见现象、所有已知指纹及无指纹变体、源码链、最小复现、多轮/长会话复现、异常与并发复现、根因、候选方案、选定方案、兼容性和性能风险、验收矩阵、修复后结果、残余风险。

## 已迁移问题材料

以下文档保留历史事实与建议，但其中状态需要按当前构建重新验证：

- [工具 description 为空或 schema 为 null](empty-tool-description-400-invalid-tool-use-format.md)
- [工具 property key 非法与可逆映射](tool-property-key-invalid-400-tool-schema-invalid.md)
- [流式上游 idle timeout](02-stream-upstream-idle-timeout.md)
- [客户端提前断开](03-client-dropped-downstream.md)
- [外部池 prompt too long](04-external-pool-prompt-too-long.md)
- [流式上游 status error](06-stream-upstream-status-error.md)
- [流式内部 read error](07-stream-internal-read-error.md)
- [图片格式错误](08-image-format-unsupported-400.md)
- [intent preamble 后 end_turn](09-intent-preamble-end-turn-no-tool-use.md)
- [end_turn 与 silent truncation](10-stream-end-turn-vs-silent-truncation.md)
- [流式观测与 trivial text](11-stream-observability-and-trivial-text-optimization.md)

## 本轮专题覆盖范围

- [真实本地账号模型请求 400 与 thinking 边界](real-account-model-invalid-400-20260901.md) - 2026-09-01；记录 220 条真实凭据、修复前 20x2 与 25 次模型矩阵、修复后真实 thinking 边界 4/4 HTTP 200。已确认入口字段规范化和显式 Claude minor 不静默降级；历史统一 `invalid_request_error` 的全部具体坏字段仍未完全归因，且本轮无外部池。
- [raw 路由顶层 `max_tokens` 类型和范围未统一校验](raw-max-tokens-invalid-400-20260902.md) - 2026-09-02；P004；已在 raw external 与本地账号共用入口统一拒绝 `null`、浮点数、0、负数和超过 `i32::MAX` 的 `max_tokens`，避免把确定性格式错误发送到上游、进入 thinking-signature rescue、换号或 external fallback。聚焦 Rust 9/9、本地长期实例 HTTP 5/5 首次 400 已通过；当前状态为 `fixed / local-runtime-verified / not-committed / not-released`。

以下范围均已有独立专题入口，但 `partial`/`pending` 项仍需在统一候选上完成动态证据，不因文档存在而视为关闭：Claude Code transcript/tool history；thinking 与 signed/redacted 内容；external raw/normalized/SSE/strict；payload/body/image/document/web fetch；prompt/tool_choice/thinking/chunk/count_tokens；stream/HTTP 200 exception；重试预算/API-key admission/RPM；Redis degraded/external fallback/local-first/多实例；usage cleanup；两 UI；旧版本升级；AWS API key/region；生产证据脱敏。

Usage cleanup 的最终产品合同是 soft cleanup 同步删除范围内明细及其累计统计、费用、credential summary、Dashboard、cache-read 和 duration rollup 贡献，hard cleanup 只物理删除 tombstone、不重复扣减；soft tombstone 存在期间同 ID 不复活，cutoff 后的新 ID 可写。hard cleanup 后不承诺永久 ID 防重。当前源码测试、两 UI 文案和真实浏览器交互已按该合同更新，并增加 in-flight writer/watermark transaction guard；`每批数量` 默认 250、后端/UI 上限已从 500 提高到 5,000，并补充旧 PostgreSQL CHECK 约束迁移和迁移关闭时的 schema compatibility guard；cleanup 过滤组最新 42/42 通过，但完整套件、writer 性能、Redis chaos、生产规模吞吐和动态多实例门禁未关闭。

## 当前权威专题

- [当前问题状态索引与文档维护规则](current-issue-status-index-20260731.md) - 2026-07-31 当前 open/fix-pending/NO-GO/验证缺口汇总；代码或状态改动必须同步更新 owning issue、该索引和必要的 plan-tree 状态。
- [当前问题逐项分析优先级队列](issue-analysis-priority-queue-20260731.md) - 2026-07-31 按紧急度和难度排序的问题分析执行顺序；先处理本地账号 Claude Code/WebSearch/tools/image，再推进协议、调度、存储和 UI/release gates。
- [Claude Code 本地账号 WebSearch/tools/image 真实调用分析](claude-code-local-accounts-websearch-tools-image-analysis-20260729.md) - 2026-07-29/31 本地账号 7/8 + `claude-sonnet-4.5` 的当前权威记录；direct native `web_search_YYYYMMDD`、mixed native WebSearch、当前 Claude CLI `WebSearch`、工具命名/schema key 映射和 tool-result-only follow-up focused path 已验证，图片来源矩阵、模型报告和复杂工具历史仍 open；旧 external-pool 调试结论不能替代此本地账号诊断。
- [下游标准 usage 单字段超过 1m](downstream-usage-standard-field-over-1m-20260731.md) - 2026-07-31 三台生产部署只读证据显示 `input_tokens`、`cache_creation_input_tokens`、`cache_read_input_tokens` 可单字段超过 1m；标准字段 focused fix 已实现并测试：reported-usage cache creation 使用 `finalCacheCreationMaxTokens=400000` 与 `20000..45000` deterministic jitter，无 full `reportedUsage` 的本地 prompt-cache/`kiro_rs_tool` 路径也会 cap 标准 cache read/write，local credential 与 external pool failure 记录保留诊断估算并将标准字段归零；仍需 frozen/isolated usage-shape smoke、dashboard/API rollup 区分和生产复发观察。
- [Pro Max 账号卡片套餐显示](subscription-pro-max-card-label-20260801.md) - 根因确认不是截图：UI 套餐 helper 与后端 `subscription_key`/`subscription_rank` 缺少 `Pro Max` 分支，已补齐 `Pro Max` 标签、`pro_max` 筛选键和等级排序，并补充 Power/Pro Max 筛选项；focused Rust/UI/admin-ui 验证通过，最终候选浏览器门禁仍随 release gate 进行。
- [2026-08-01 生产外部池两类错误根因补充](20260801-production-external-errors-root-cause.md) - 外部池“输入上限预检”发送前硬拒绝绕过“请求大小保护”，以及外部池 route 缺少本地 Kiro 发送链路的兼容模型处理导致部分“模型处理”模式无法按配置生效；当前修复是取消内容长度发送前拒绝、保留调度/安全预检，并让直接外部池和本地失败 fallback 路径携带“模型（本地解析）”。
- [内置路由策略必须完全由配置决定](route-policy-config-authority-20260802.md) - 2026-08-02 P0 配置权威问题已完成后端与两套 UI focused 修复：`/cc`、`/v1`、`/ha`、`/na` 仍是内置入口，但缓存、usage、提示词、外部池等运行策略均由运行配置解析；`/cc` 可配置成无缓存，`/na` 可配置成高缓存，提示词引导按路径规则命中任意内置或自定义入口；全量 Rust、UI/admin-ui build、文档合同和提示词配置独立性测试通过，真实服务热加载/浏览器交互/生产复发观察仍作为后续门禁。
- [语言约束提示词首语言锁定](language-constraint-first-language-lock-20260802.md) - 2026-08-02 新登记；短/长历史、真实 Claude Code CLI 基础会话、模拟压缩摘要和相反首语言并发矩阵均未复现首语言锁定，真实自动 compact 阈值和异常样本仍待证据。
- [usage 清理安全与 Redis 隔离](usage-cleanup-safety-and-redis-isolation.md) - 2026-08-02/03 用户体验问题；后端安全合同、新旧 UI 语义、真实浏览器交互、Admin cache 写入竞态修复、`每批数量` 上限 5,000、PostgreSQL 约束迁移和迁移关闭时 schema compatibility guard 已有 focused pass，但动态多实例一致性、生产规模性能和 Redis chaos 仍未关闭，不能把已有 focused 证据当成完整修复。
- [159/170 现网 usage 错误审计与体验改进](production-usage-error-audit-159-170-20260802.md) - 2026-08-02 新登记；按用户澄清排在语言约束和 usage 清理之后。2026-08-03 已完成只读 evidence pass，两台均为 `v0.0.123`，形成 P001-P004 问题簇；超长预检和 usage 标准字段属于旧版本已记录类，外部 5xx 已跨池重试，外部 400 不自动重试而保留为 Admin 诊断增强候选。
- [HTML `<br>` 输出标签污染](html-br-output-tag-contamination-20260731.md) - 2026-07-31 记录 assistant 在正常 prose 中疑似输出 raw `<br>` / HTML-like tag 的复现边界；direct/stream/tool-result/history/CLI 正常场景未复现，web-display 与显式 standalone `<br>` 仅作为合法透传对照；过滤策略仍需真实异常样本或产品规则。
- [协议 transcript 与工具历史泄漏](protocol-transcript-and-tool-history-leak.md)
- [thinking 与签名内容安全](thinking-and-signed-content-safety.md)
- [thinking effort、adaptive mode 与 Kiro 上游映射](thinking-effort-adaptive-upstream-mapping.md)
- [裸 invoke 正文被升级为可执行工具调用](bare-invoke-text-upgraded-to-executable-tool-use.md)
- [payload guard 语义、上限与性能](payload-guard-semantics-limits-and-performance.md)
- [远程图片/文档辅助请求、资源上限与 SSRF 连接绑定](remote-multimodal-resource-and-ssrf-bounds.md)
- [external profile 与 SSE 安全](external-pool-profiles-and-sse-safety.md)
- [外部池成功请求 0 计费与非流式 usage 捕获分裂](external-pool-success-zero-billing.md)
- [外部池费用口径与 Dashboard 聚合差异](external-pool-billing-cost-statistics-20260803.md) - 2026-08-04 focused 复核通过：外部池原始成本优先按上游真实 usage，缺失时才使用本地估算 fallback；本地整形 usage 保持独立，PgSQL rollup、Redis Dashboard materialization、Admin UI build 和文档合同通过；生产升级后观察仍开放。
- [外部池直连、模型映射与跨池重试异常](external-pool-direct-model-retry-20260804.md) - 2026-08-04 跨 159/170/142 证据矩阵已补齐；`usage` 明细只保存请求决策轨迹，不保存完整运行时配置快照；当前证据未证明外部直连隐式回本地。P0 已修复并 focused 验证：“请求正文模式”不再作为选池前置筛选、Raw 入口可重选标准处理池、外部池默认补 `anthropic-version`；“外部池最多尝试”与“同池重试次数”已分离，“跨池重试状态码”“网络错误跨池重试”“协议错误跨池重试”“同池重试状态码/间隔”和“清除冷却”仍作为基础能力保留；其中“普通连续瞬态失败上浮为池级长冷却”的旧结论已被 [外部池高可用调度与冷却回归](external-pool-ha-scheduler-cooldown-regression-20260805.md) 覆盖。
- [外部池高可用调度与冷却回归](external-pool-ha-scheduler-cooldown-regression-20260805.md) - 2026-08-05 P0 根因已确认并修复：本进程消费自己的 Redis 外部池变更事件并清空刚合并的权威快照，导致候选池只剩最后创建项；不是优先级排序函数本身。修复增加 `origin` 标识并兼容旧事件，最终候选已通过 3 轮真实 HTTP 多池故障接管/恢复、256 并发、1800 RPM/60 秒、高并发资源回落、外部直连边界、隔离 PgSQL/Redis 和全量 Rust 门禁，`Publish Docker Images #164` 已成功发布 `v0.0.133`。状态为 `root-cause-fixed / real-http-verified / released-v0.0.133 / production-rollout-pending`，详见 [专项证据](../evidence/external-pool-ha-scheduler-validation-20260805.md)；更大 RoutePlan、候选可观测性和生产观察仍单独开放。
- [Stream terminal errors and precommit retry](stream-terminal-errors-and-precommit-retry.md) - 2026-08-06 外部池流式首语义输出前 error/空回子问题已有 focused implementation；2026-08-07 复跑正常输出/调度矩阵通过：`HTTP 200 -> message_start -> error` 等首语义输出前失败会在下游 commit 前丢弃 protocol-only 缓冲并在外部池预算内换池；全局默认和单池覆盖、storage/admin/UI、fake-upstream HTTP、正常 stream/non-stream 输出、external direct stream/non-stream、本地优先 fallback/rescue 配置回归均已通过 focused 验证。最终冻结候选也已通过真实 Claude CLI `2.1.221` fake-upstream gate（bare `20/20`、long-session `110 turns`、thinking-wire rerun `60/60`）和 L3/L4/L5 load/chaos（`9/9`、`12/12`、`900s` soak `6820/6820` 且 RSS/FD 回落）。生产 rollout 观察和 `yuenan` / `yuenan-1` 复核仍 pending，不能视为生产观察已关闭。
- [external pool Redis 协调、restart fencing 与 release backlog](external-pool-redis-coordination-and-release.md)
- [external pool 权威选池 PostgreSQL 扇出与发送 revision fence](external-pool-authoritative-selection-and-dispatch-fence.md)
- [重试预算、准入与 RPM 放大](retry-budget-admission-and-rpm-amplification.md)
- [Token refresh 失败波、自动恢复与集群 RPM](token-refresh-failure-wave-and-cluster-rpm.md)
- [请求 API Key admission](request-api-key-admission.md)
- [prompt policy、tool choice 与 count tokens](prompt-policy-tool-choice-and-count-tokens.md)
- [Redis scheduler degraded 与 fallback](redis-scheduler-degraded-and-fallback.md)
- [业务 Redis 与观测 Redis 故障域隔离](business-observability-redis-fault-domain.md)
- [高并发低 RPM、排队与凭据运行态假禁用](high-concurrency-low-rpm-runtime-quarantine.md)
- [调度队列租约续租导致内部 Redis RPM 放大](dispatch-queue-lease-renewal-rpm-amplification.md)
- [本地容量预检竞态与 external fallback 延迟](local-capacity-preflight-race-and-external-fallback-latency.md)
- [stream 终止错误与首输出前重试](stream-terminal-errors-and-precommit-retry.md)
- [strict local-first、账号分布与多实例](strict-local-first-distribution-and-multi-instance.md)
- [usage 清理安全与 Redis 隔离](usage-cleanup-safety-and-redis-isolation.md)
- [Redis usage writer 原子性、基数与 scheduler 隔离](redis-usage-writer-atomicity-cardinality-and-scheduler-isolation.md)
- [两套 UI 费用精度与配置权威](two-ui-cost-precision-and-config-authority.md)
- [PostgreSQL 启动迁移全链原子性与确定性失败放大](postgres-startup-migration-atomicity.md)
- [v0.0.101/v0.0.102/v0.0.103 升级 smoke](upgrade-v101-v102-v103-smoke.md)
- [AWS Kiro API Key 与 region 生命周期](aws-kiro-api-key-region-lifecycle.md)
- [生产证据 skill 校验与脱敏](evidence-skill-validation-and-redaction.md)
- [协议能力回归矩阵](protocol-capability-regression-matrix.md)
- [WebSearch/MCP 协议、错误、usage、attempt 与隐私边界](websearch-mcp-protocol-usage-and-privacy.md)
- [Native WebSearch 与 normalized 外部池 fallback 断路](websearch-normalized-external-fallback-preflight.md)
- [159/170 Native WebSearch MCP 错误聚类](prod-websearch-mcp-error-clusters-159-170-20260725.md)
- [Thinking signature retry 第二响应 transient 被误归类](thinking-signature-retry-transient-response.md)
- [凭据卡片 mcp_completion runtime 错误来源追踪](mcp-completion-runtime-card-error-source.md)
- [企业/API-key 200 EventStream EOF 被误判 api_error](enterprise-eventstream-usage-only-tool-eof.md)
- [运行时栈溢出与 handler future 大小](runtime-stack-overflow-and-handler-future-size.md)
- [Runtime completion storage bridge starvation](runtime-completion-storage-bridge-starvation.md)
- [159/170/142 生产实例运行时卡死：请求完成路径与存储/调度耦合](prod-runtime-completion-storage-coupling-159-170-142-20260727.md)
- [外部池调度影响本地凭据与 fallback 矩阵缺失](external-pool-scheduler-interference-and-fallback-matrix-20260727.md)
- [整体调度架构分析：本地凭证、外部池、fallback/rescue 与容量账本](scheduler-architecture-analysis-purpose-and-plan.md)
- [Dashboard observability redesign](dashboard-observability-redesign.md)
- [上游错误诊断隐私与响应体边界](upstream-error-diagnostic-privacy-and-bounds.md)
- [运行时饥饿下的上游 HTTP deadline](upstream-http-deadline-runtime-starvation.md)
- [JSON 空白压缩的逐字节语义与性能](json-whitespace-compression-byte-semantics.md)
- [Endpoint body 重写的字节语义与重复序列化成本](endpoint-body-rewrite-byte-semantics-and-cost.md)
- [验证构建产物生命周期与磁盘安全](validation-build-artifact-lifecycle-and-disk-safety.md)
- [Usage Dashboard P95 与时间窗语义](usage-dashboard-p95-and-window-semantics.md)

## 2026-07-26 当前工作树增量状态

- WebSearch normalized external fallback：preflight 与后置 fallback 均已实现；本地无凭证/全禁用/容量/Redis degraded 按 `selectionFailure` 精确分类，不再一律映射成 Redis degraded。
- MCP/WebSearch 辅助失败：普通 MCP completion、429/5xx/body-read/protocol 不再写主模型凭据 cooldown；只在本次请求内换号，避免凭据卡片 `mcp_completion upstream_error` 与主调度假不可用。
- PgSQL usage/dashboard 隔离：新增 `postgres.usageMaxConnections` 与独立 usage pool；旧表缺 114+ dashboard/usage/计费列的迁移已补齐并加入 schema guard。
- 已通过聚焦回归：WebSearch 全量 29 tests、thinking/output_config、thinking signature、pricing、PG schema/usage pool、request admission、local_pool fast-fail、scheduler degraded/external fallback、MCP 辅助健康隔离。
- 已完成冻结候选二进制验证：`kiro-rs` SHA-256 `7268b3e722f03a40179d205e7b5917b86d696cd8bf1d5f6533d3b1347ea30bec`。C0 静态/全量 Rust/release build/clippy baseline 已通过且 scoped target 清理完成。
- 已完成真实 Claude Code CLI fake-upstream 协议验证：bare invoke `20/20`、long-session `5 sessions / 110 turns / 100 tool pairs / leakMatches=0`、thinking-wire `60/60`；证据见 [candidate-c0-claude-cli-real-protocol-20260726](../evidence/candidate-c0-claude-cli-real-protocol-20260726.md)。
- 已完成 fake-upstream 负载/异常恢复验证：L3 `9/9`、L4 `12/12`、L5 `60s soak + recovery` 全部通过；证据见 [candidate-c0-load-chaos-20260726](../evidence/candidate-c0-load-chaos-20260726.md)。
- 真实上游成功 smoke 当前受环境阻塞：本地 `9022` 的持久化凭据全部处于 disabled/runtime bad state（TemporarilySuspended/Manual/QuotaExceeded），继续真实调用会增加账号风险；不把该环境阻塞伪装为产品 pass。

## 2026-07-27 runtime/storage 发布候选状态

- 当前冻结候选：`kiro-rs` SHA-256 `40ec70c7036826807f3d59701fe02de8eada7c8d88f265ad4a68fde55ff3c9d3`，`kiro_loadtest` SHA-256 `a9b03d0dbe3f4456939641b434fcc3781ea6f6909a31dff393100d2bcbcc81c8`。
- 已通过 full Rust all-target：main `1816 passed / 0 failed / 6 ignored`，loadtest `31 passed / 0 failed`。
- 已通过真实 Claude Code CLI fake-upstream：bare `20` cases，long-session `5 sessions / 110 turns / 100 tool pairs / leakMatches=0`，thinking-wire `60/60`，Claude Code CLI `2.1.220`。
- 已通过负载/异常：L3 `9/9`、L4 `12/12`、L5 第二轮 `461/461` 长流 + recovery `12/12`，RSS/FD 在 60 秒 idle 后回落。
- 已通过本地 9022 health/readiness/dashboard split endpoint smoke；真实账号成功路径未验证，原因是本地 PgSQL 权威凭据全 disabled，未强行启用。
- 当前发布前剩余：最终 diff/artifact gate、清理 raw 临时产物、按发布技能提交/打 tag/推送。
