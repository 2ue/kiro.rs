# 单问题文档完整性审计

Date: 2026-07-16

Role: 跟踪 `feature/issues/` 是否满足最终交付所需的问题分析、复现、修复和验证结构

Status: `structure-contract-pass / dynamic-evidence-open`；结构门禁已通过，但这不是问题已修复或测试已通过的证明

## 最终文档合同

除索引外，每个独立问题或紧密耦合的问题类最终必须明确包含以下内容：

1. 状态、严重级别和影响面。
2. 用户可见现象、已知指纹，以及没有固定指纹的同类变体。
3. 当前源码/运行链、根因和引入或放大条件。
4. 最小复现、多轮复现、长会话或大数据复现，以及适用的异常/并发/恢复复现。
5. 候选方案、选定方案、兼容性与性能取舍、回滚边界。
6. 可执行验收矩阵、当前构建身份、修复后结果、证据链接和残余风险。

旧文档即使已有历史结论，也必须按当前工作树重新复核；只有关键词或标题存在不表示内容正确或证据充分。

## 2026-07-16 结构扫描

权威可执行门禁：`node feature/tests/check-feature-docs.mjs`。当前结果为 29 份 issue 文档、26 个相对链接、0 个结构/链接失败。`OK` 仅表示结构完整，不表示动态验证完成。

| 文档 | 当前明显缺栏 |
| --- | --- |
| `02-stream-upstream-idle-timeout.md` | `OK`（历史生产分类保留；统一 precommit fault gate pending） |
| `03-client-dropped-downstream.md` | `OK`（分类成立；client-drop cleanup/resource gate pending） |
| `04-external-pool-prompt-too-long.md` | `OK`（分类/预检有历史证据；当前候选真实路由 pending） |
| `06-stream-upstream-status-error.md` | `OK`（历史分类成立；stream state-machine fault gate pending） |
| `07-stream-internal-read-error.md` | `OK`（历史分类成立；transport fault/recovery gate pending） |
| `08-image-format-unsupported-400.md` | `OK`（结构校验/400 分类有证据；真实 CLI image pending） |
| `09-intent-preamble-end-turn-no-tool-use.md` | `OK`（usage 观测有证据；长会话统计 pending） |
| `10-stream-end-turn-vs-silent-truncation.md` | `OK`（观测盲区已修；silent truncation 本身未证实，fault gate pending） |
| `11-stream-observability-and-trivial-text-optimization.md` | `OK`（实现存在；统一候选 CLI/load pending） |
| `aws-kiro-api-key-region-lifecycle.md` | `OK`（核心链路已有 provisional build 证据；最终构建、双 UI 和多实例门禁仍未关闭） |
| `empty-tool-description-400-invalid-tool-use-format.md` | `OK`（历史 fix 有证据；统一候选 CLI/MCP pending） |
| `evidence-skill-validation-and-redaction.md` | `OK`（本地 deterministic redaction 有证据；发布总门禁重跑 pending） |
| `external-pool-profiles-and-sse-safety.md` | `OK`（聚焦 response state machine 有证据；handler/CLI/长历史/load 仍 pending） |
| `payload-guard-semantics-limits-and-performance.md` | `OK`（历史/图片/raw identity 有聚焦证据；413/burst/L5 pending） |
| `postgres-startup-migration-atomicity.md` | `OK`（原子性与升级矩阵有当前证据；最终 release binary 绑定 pending） |
| `prompt-policy-tool-choice-and-count-tokens.md` | `OK`（后端/UI 解耦有聚焦证据；browser/CLI/count_tokens 正交矩阵仍 pending） |
| `protocol-capability-regression-matrix.md` | `OK`（完整能力合同已写；真实 CLI/长历史/故障/load 仍 pending） |
| `protocol-transcript-and-tool-history-leak.md` | `OK`（聚焦实现/状态机已有证据；真实 CLI、长历史、HTTP fault 与最终性能门禁未关闭） |
| `remote-multimodal-resource-and-ssrf-bounds.md` | `OK`（连接绑定与请求预算有聚焦证据；handler/CLI/load/最终候选仍 pending） |
| `redis-scheduler-degraded-and-fallback.md` | `OK`（fallback 红绿有证据；external hotpath/双实例/load pending） |
| `request-api-key-admission.md` | `OK`（单实例 admission 有聚焦证据；归因/多实例/L3-L5 pending） |
| `retry-budget-admission-and-rpm-amplification.md` | `OK`（shared budget/400/admission 有分批证据；429/500/partial、归因、多实例、load 仍 pending） |
| `stream-terminal-errors-and-precommit-retry.md` | `OK`（结构合同完整；完整 handler/fake-upstream/CLI fault gate pending） |
| `strict-local-first-distribution-and-multi-instance.md` | `OK`（E05/CapacityFull/degraded 有分批证据；E01/E02/E03 与最终性能仍 pending） |
| `thinking-and-signed-content-safety.md` | `OK`（聚焦状态机已有证据；真实 CLI、长会话、HTTP fault 与最终性能门禁未关闭） |
| `tool-property-key-invalid-400-tool-schema-invalid.md` | `OK`（历史可逆映射有证据；统一候选 CLI/MCP/长历史 pending） |
| `two-ui-cost-precision-and-config-authority.md` | `OK`（formatter/build 有证据；browser/save-refresh pending） |
| `upgrade-v101-v102-v103-smoke.md` | `OK`（81-phase 矩阵正在因 applied_at 修复重新绑定候选） |
| `usage-cleanup-safety-and-redis-isolation.md` | `OK`（soft/hard rollup、soft-tombstone 与 in-flight commit 合同已统一；cleanup 36/36 x3 外层通过，full/writer-performance/chaos/browser 门禁未关闭） |
| `websearch-mcp-protocol-usage-and-privacy.md` | `OK`（源码缺陷已确认；动态红测、实现与全矩阵 pending） |

## 新发现必须单独入册

v0.0.101 升级 failure fixture 已确认并已建立 [PostgreSQL startup migration 原子性专题](../issues/postgres-startup-migration-atomicity.md)。该问题不只是升级 smoke 的一条失败记录，专题覆盖：

- checksum mismatch 前后的 schema fingerprint、marker、业务数据和 runtime config；
- 连续失败多轮不改变任何状态；
- 修复后的整链 transaction 边界与 advisory lock 语义；
- 显式大 backfill/compression 不进入 startup 长事务；
- v0.0.101/v0.0.102/v0.0.103 failure recovery 和 second startup 证据。

## 关闭规则

- 每个专题完成真实复核后更新本表；不能为了结构完整而编造未执行结果。
- `feature/tests/reverification-matrix.md` 中适用 case 必须有当前构建报告和轮次。
- `feature/evidence/README.md` 必须能定位命令、revision、binary hash、隔离资源和清理结果。
- 所有专题不再缺栏且发布门禁无 `pending`/`partial` 后，本审计才可标为 `closed`。
