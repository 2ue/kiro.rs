# 审计与历史输入

Role: 保存事实盘点、生产只读取证摘要和迁移前分析；不直接作为最终通过结论

## 索引

- [历史状态总索引](analysis-status-index-20260715.md)
- [Claude Code 内部协议泄漏深度审计](claude-code-internal-leak-deep-audit-20260715.md)
- [调度生产 follow-up](scheduler-production-followups-20260715.md)
- [2026-07-14 本地待确认清单](local-todo-for-confirmation-2026-07-14.md)
- [2026-07-13 runtime usage/error follow-up](runtime-usage-error-followup-2026-07-13.md)
- [早期生产错误索引](legacy-production-error-index-2026-07-13.md)
- [迁移映射](migration-map-20260716.md)
- [当前代码事实矩阵](current-fact-matrix-20260716.md)：区分动态当前证据、静态当前证据、历史证据与未测试项。
- [单问题文档完整性审计](issue-document-completeness-20260716.md)：跟踪每类问题是否具备现象、根因、复现、方案、验收、当前证据和残余风险；不把结构命中当作测试通过。

审计文档中的结论必须在 `issues/` 或最终状态矩阵中经过当前构建复核后，才能升级为发布门禁证据。
