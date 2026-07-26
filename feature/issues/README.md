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

以下范围均已有独立专题入口，但 `partial`/`pending` 项仍需在统一候选上完成动态证据，不因文档存在而视为关闭：Claude Code transcript/tool history；thinking 与 signed/redacted 内容；external raw/normalized/SSE/strict；payload/body/image/document/web fetch；prompt/tool_choice/thinking/chunk/count_tokens；stream/HTTP 200 exception；重试预算/API-key admission/RPM；Redis degraded/external fallback/local-first/多实例；usage cleanup；两 UI；旧版本升级；AWS API key/region；生产证据脱敏。

Usage cleanup 的最终产品合同是 soft cleanup 同步删除范围内明细及其累计统计、费用、credential summary、Dashboard、cache-read 和 duration rollup 贡献，hard cleanup 只物理删除 tombstone、不重复扣减；soft tombstone 存在期间同 ID 不复活，cutoff 后的新 ID 可写。hard cleanup 后不承诺永久 ID 防重。当前源码测试与两 UI 文案已按该合同更新，并增加 in-flight writer/watermark transaction guard；cleanup 过滤组 36/36 x3 外层通过，但完整套件、writer 性能、Redis chaos 和 UI browser 未关闭。

## 当前权威专题

- [协议 transcript 与工具历史泄漏](protocol-transcript-and-tool-history-leak.md)
- [thinking 与签名内容安全](thinking-and-signed-content-safety.md)
- [thinking effort、adaptive mode 与 Kiro 上游映射](thinking-effort-adaptive-upstream-mapping.md)
- [裸 invoke 正文被升级为可执行工具调用](bare-invoke-text-upgraded-to-executable-tool-use.md)
- [payload guard 语义、上限与性能](payload-guard-semantics-limits-and-performance.md)
- [远程图片/文档辅助请求、资源上限与 SSRF 连接绑定](remote-multimodal-resource-and-ssrf-bounds.md)
- [external profile 与 SSE 安全](external-pool-profiles-and-sse-safety.md)
- [外部池成功请求 0 计费与非流式 usage 捕获分裂](external-pool-success-zero-billing.md)
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
