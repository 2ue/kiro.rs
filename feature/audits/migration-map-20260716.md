# 文档迁移映射

Date: 2026-07-16

Role: 记录本轮最终交付目录的来源、目标、回滚与链接修复范围

## 决定

用户指定仓库根目录 `feature/` 为本轮最终文档目录。原 `docs/plantree/` 继续作为项目级计划注册表，不搬迁；它链接到 `feature/`，但不替代面向本轮问题的最终材料。

## 来源到目标

| 来源 | 目标 | 处理 |
| --- | --- | --- |
| `docs/feature/README.md` | `feature/audits/legacy-production-error-index-2026-07-13.md` | 保留历史索引原意，降级为审计输入 |
| `docs/feature/<problem>.md` | `feature/issues/<problem>.md` | 保留单问题材料，逐项重新复核 |
| `docs/feature/local-todo-for-confirmation-2026-07-14.md` | `feature/audits/` | 历史待办输入 |
| `docs/feature/runtime-usage-error-followup-2026-07-13.md` | `feature/audits/` | 历史运行态跟踪输入 |
| `docs/analysis-status-index-20260715.md` | `feature/audits/analysis-status-index-20260715.md` | 旧总索引，状态重新核验 |
| `docs/claude-code-internal-leak-deep-audit-20260715.md` | `feature/audits/claude-code-internal-leak-deep-audit-20260715.md` | 上一轮动态审计输入 |
| `docs/prompt-injection-inventory-and-centralization-plan-20260715.md` | `feature/plans/` | 候选方案输入，尚非最终实施计划 |
| `docs/scheduler-dispatch/production-followups-20260715.md` | `feature/audits/scheduler-production-followups-20260715.md` | 生产现象与源码分析输入 |

## 安全与回滚

- 迁移使用 Git rename 或同文件系统移动，没有删除文档正文。
- `docs/feature/README.md` 保留 moved stub，仓库内所有已知入站链接同步更新。
- 回滚只需按本表反向移动；在最终发布前不会清理历史内容。
- 忽略目录中的原始报告继续保留在原位置，由 `feature/evidence/` 建立索引，不复制敏感或大体积产物。
