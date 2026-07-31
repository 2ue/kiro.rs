# 项目文档状态审计与 plan-tree 整理方案

Last reviewed: 2026-07-28 Asia/Shanghai

Scope:

- 只包含当前项目 `/Users/yuanfeijie/Desktop/procode/kiro.rs` 内的文档。
- 不扫描整台机器，不处理其他项目文档。
- 本文已经升级为当前计划的文档处置索引。当前变更迁移了当前有效 TODO 到本 plan，并已按批次归档明确过时的历史分析/旧专题目录；不删除任何文件。

## 0. 结论

应该使用现有 `docs/plantree/` 来整理项目文档，但不要再新建一套平行规划体系，也不要一次性批量移动 300+ 个 Markdown。

当前项目已经存在：

- `docs/plantree/README.md`：明确写着它是 durable planning registry 和 authority entrypoint。
- `docs/plantree/plans/system-architecture-modernization/indexes/legacy-document-disposition.md`：已经做过一次旧文档处置审计。
- `docs/archive/README.md`：已经有归档规则和多个归档批次。
- `feature/`：当前问题、证据、发布、测试、TODO 的工作区。

所以正确做法是：

1. 保留 `docs/plantree/` 作为长期规划/权威入口。
2. 保留 `feature/issues` 和 `feature/evidence` 作为当前迭代的问题与证据工作区。
3. 对 `docs/analysis`、`docs/*.md`、旧专题目录做“文档 disposition 审计”，给每个文档贴状态，而不是直接删除。
4. 按技术域一批一批归档，更新所有入站链接。
5. 所有“当前要做”的事项注册到本 plan 或其他 owning plan；所有“已完成但有证据价值”的进入 history/evidence index；所有“过时但有参考价值”的进入 archive。

## 1. 初步盘点

只读盘点结果：

- 项目内 `docs/` + `feature/` 下 Markdown 约 `362` 个。
- 主要分布：
  - `docs/plantree/**`：已有长期 plan-tree。
  - `docs/archive/**`：已归档历史文档。
  - `docs/analysis/**`：近期/历史分析混合。
  - `docs/kiro-rs-root-cause-package-20260726T170519+0800/**`：生产事故证据包。
  - `docs/archive/kiro-proxy-study-and-optimization-20260626/**`、`docs/archive/scheduler-dispatch-redesign-history/**`：本轮已归档的旧专题研究/计划。
  - `docs/archive/request-and-protocol-history/**`、`docs/archive/cache-usage-and-production-history/**`、`docs/archive/scheduler-state-external-pool-runtime-history/**`、`docs/archive/external-project-learning-history/**`：本轮已归档的 root-level 历史分析。
  - `feature/issues/**`：当前和历史问题文档。
  - `feature/evidence/**`：可追溯验证证据。
  - `feature/todo/**`：轻量临时入口；当前有效事项已开始迁入本 plan。

## 2. 建议分类标准

每个文档应登记为以下之一：

| 分类 | 含义 | 处理方式 |
| --- | --- | --- |
| `active-authority` | 当前设计/规划/验收权威 | 保留在 `docs/plantree` 或注册到 plan-tree |
| `active-issue` | 当前仍需执行的问题文档 | 保留在 `feature/issues`，更新 Status 和验收状态 |
| `active-todo` | 当前明确未完成事项 | 注册到 owning plan 的 roadmap/topic；短期也可在 `feature/todo` 留入口 |
| `current-evidence` | 当前仍可证明某个结论的证据 | 保留在 `feature/evidence` 或 plan-tree history index |
| `historical-evidence` | 历史证据，不能当当前事实 | 归档或保留在 evidence，但标明 dated |
| `superseded-analysis` | 分析已被新文档/代码/测试取代 | 移入 archive 或在 disposition 表中标记 superseded_by |
| `partially-implemented` | 文档中部分实现、部分未实现 | 拆分：已实现进 evidence/history，未实现进 TODO |
| `direction-drift` | 与当前迭代方向不一致，但可能有参考价值 | 降级为 historical/reference，不能当执行计划 |
| `delete-candidate` | 无入站链接、无独立证据、已被完整取代 | 仅在单独审计明确批准后删除 |

## 3. 初步分类结果

### 3.1 应保留为长期权威入口

- `docs/plantree/README.md`
- `docs/plantree/baseline/**`
- 已注册 plan：
  - `docs/plantree/plans/runtime-correctness-and-release-gates/**`
  - `docs/plantree/plans/admin-observability-routing-config/**`
  - `docs/plantree/plans/request-body-capability-modularization/**`
  - `docs/plantree/plans/greenfield-ai-gateway/**`
  - `docs/plantree/plans/system-architecture-modernization/**`，但它已标注 superseded/historical，不能当当前 Rust 迭代执行计划。

需要更新：

- `docs/plantree/README.md` 当前显示 `Status: Current as of 2026-07-21`，但项目已经经过后续 0.0.120+ / 0.0.123+ 修复和新问题分析。
- `runtime-correctness-and-release-gates` 的状态仍围绕 `v0.0.114`，需要刷新或新增一个当前 Rust runtime/scheduler stabilization plan。

### 3.2 当前仍有效但未实现的分析

- `docs/analysis/thinking-signature-remediation-plan-20260728.md`
  - 状态：`active-analysis / not-implemented`
  - 动作：不能归档；已注册到本 plan topic。
  - 已在 [thinking-signature-protocol-safety.md](../topics/thinking-signature-protocol-safety.md) 记录未完成项。

- `feature/issues/scheduler-architecture-analysis-purpose-and-plan.md`
  - 状态：`active-analysis-planned / not-implemented`
  - 动作：已注册到当前 Rust scheduler/runtime plan；原 issue 保留分析来源和证据。
  - 已在 [route-planner-capacity-ledger.md](../topics/route-planner-capacity-ledger.md) 记录未完成项。

- `feature/issues/external-pool-scheduler-interference-and-fallback-matrix-20260727.md`
  - 状态：`partially-implemented`
  - 已完成：cached/no-wait gate 热路径修复。
  - 未完成：策略产品化、cooldown、load/chaos、无本地账号临时外部池直连回切。
  - 已在 [external-pool-local-first-scheduler.md](../topics/external-pool-local-first-scheduler.md) 记录。

### 3.3 当前证据包，不应当成未来执行计划

- `docs/kiro-rs-root-cause-package-20260726T170519+0800/**`
  - 状态：`current/historical-evidence`
  - 动作：保留为证据包；需要在 plan-tree runtime/scheduler plan 的 history/evidence index 中登记。
  - 不应直接作为“最新设计方案”；设计结论应抽到 issue/todo/plan。

- `feature/evidence/**`
  - 状态：`evidence`
  - 动作：保持不可变/少改；只补索引，不改原证据含义。
  - 注意：旧 evidence 不是“当前已通过”，只能证明当时构建/配置/版本下的结果。

### 3.4 已有归档或可归档的历史文档

已归档：

- `docs/archive/request-body-modularization-20260706/**`
- `docs/archive/ui-planning-2026-06-to-07/**`
- `docs/archive/slow-first-token-and-stream-fluidity-20260629-20260709/**`
- `docs/archive/kiro-proxy-study-and-optimization-20260626/**`
- `docs/archive/scheduler-dispatch-redesign-history/**`
- `docs/archive/request-and-protocol-history/**`
- `docs/archive/cache-usage-and-production-history/**`
- `docs/archive/scheduler-state-external-pool-runtime-history/**`
- `docs/archive/external-project-learning-history/**`
- `docs/archive/release-114-hardening-history/**`

本轮已移动的第一批历史分析：

| 原路径 | 新路径 | 状态 |
| --- | --- | --- |
| `docs/analysis/slow-first-token-20260629.md` | `docs/archive/slow-first-token-and-stream-fluidity-20260629-20260709/slow-first-token-20260629.md` | archived historical reference |
| `docs/analysis/prod-slow-first-token-12h-factual-analysis-20260706.md` | `docs/archive/slow-first-token-and-stream-fluidity-20260629-20260709/prod-slow-first-token-12h-factual-analysis-20260706.md` | archived historical production analysis |
| `docs/analysis/kiro-vs-sub2api-first-token-correlation-20260707.md` | `docs/archive/slow-first-token-and-stream-fluidity-20260629-20260709/kiro-vs-sub2api-first-token-correlation-20260707.md` | archived historical comparison |
| `docs/analysis/claude-code-stream-fluidity-and-abrupt-stop-analysis-20260709.md` | `docs/archive/slow-first-token-and-stream-fluidity-20260629-20260709/claude-code-stream-fluidity-and-abrupt-stop-analysis-20260709.md` | archived historical stream-fluidity analysis |
| `docs/analysis/external-pool-cache-ttl-split-20260708.md` | `docs/archive/cache-usage-and-production-history/external-pool-cache-ttl-split-20260708.md` | archived historical external-pool cache/usage billing analysis |
| `docs/kiro-proxy-study-20260626/**` | `docs/archive/kiro-proxy-study-and-optimization-20260626/kiro-proxy-study-20260626/**` | archived historical external-project comparison |
| `docs/kiro-optimization-plans-20260626/**` | `docs/archive/kiro-proxy-study-and-optimization-20260626/kiro-optimization-plans-20260626/**` | archived historical optimization plan and implementation record |
| `docs/scheduler-dispatch/**` | `docs/archive/scheduler-dispatch-redesign-history/scheduler-dispatch/**` | archived historical implemented scheduler strategy |
| root-level request/protocol history | `docs/archive/request-and-protocol-history/**` | archived historical request/protocol analysis |
| root-level cache/usage/production history | `docs/archive/cache-usage-and-production-history/**` | archived historical cache/usage/production analysis |
| root-level scheduler/state/external-pool/runtime history | `docs/archive/scheduler-state-external-pool-runtime-history/**` | archived historical scheduler/state/runtime analysis |
| root-level external-project learning history | `docs/archive/external-project-learning-history/**` | archived historical comparison notes |
| `feature/release-114-hardening/**` | `docs/archive/release-114-hardening-history/release-114-hardening/**` | archived historical v0.0.114 hardening package |
| `feature/final-report.md`、`feature/implementation-status.md`、`feature/plans/**` | `docs/archive/runtime-correctness-feature-workspace-history/**` | archived historical feature workspace status/plans |

已有旧审计建议 Archive Later 的大类：

- `docs/analysis/*.md` 中多数旧生产/慢首字分析。
- 剩余 `docs/*.md` 当前只保留明确 keep 项；如出现新 root-level 历史分析，应按技术域归档。

动作：

- 不直接移动。
- 按技术域归档：
  1. thinking/signature/protocol 历史。
  2. scheduler/external pool/runtime 历史。
  3. dashboard/usage/cache 历史。
  4. UI/admin 历史。
- 每批归档前必须检查入站链接。

### 3.5 明显偏离当前短期方向的文档

- `docs/plantree/plans/greenfield-ai-gateway/**`
  - 状态：`valid target architecture reference / direction-drift for current Rust hotfix work`
  - 原因：它是新仓库 Go/React 目标方案；当前用户要求是在现有 Rust 项目继续修复、测试、发版。
  - 动作：保留为长期参考，但当前调度热修/签名修复不能以它为执行计划。

- `docs/plantree/plans/system-architecture-modernization/**`
  - 状态：`superseded / historical reference`
  - 动作：保留其中问题分析和不变量；不能当当前实施路线。

## 4. 应该怎么用 plan-tree 整理

已新增当前计划根，而不是把所有 TODO 永久留在 `feature/todo`：

```text
docs/plantree/plans/rust-runtime-scheduler-stabilization/
  README.md
  roadmap.md
  implementation-status.md
  topics/
    external-pool-local-first-scheduler.md
    route-planner-capacity-ledger.md
    thinking-signature-protocol-safety.md
    validation-and-release-gates.md
  decisions/
  history/
    evidence-index.md
  indexes/
    document-disposition.md
```

其中：

- 本 plan 的 `topics/*` 作为当前工作队列。
- `docs/plantree/plans/rust-runtime-scheduler-stabilization/` 作为长期权威和状态入口。
- `feature/issues/*` 保留问题详情、复现、根因、验收。
- `feature/evidence/*` 保留实际证据。
- `docs/archive/*` 保存已失去当前执行权威但仍有历史价值的文档。

## 5. 不建议的做法

不要：

- 不要一次性把 362 个 Markdown 全部移动。
- 不要删除旧分析，除非单独完成入站链接、证据价值、superseded_by 审计。
- 不要让 `feature/todo` 取代 `docs/plantree`，否则会形成第二套规划入口；当前有效 TODO 已迁入本 plan。
- 不要把旧 evidence 标成“当前通过”。
- 不要把 greenfield plan 当成当前 Rust 项目的短期修复计划。
- 不要把已经 superseded 的旧分析继续作为代码实现依据。

## 6. 执行 TODO

### P1-1：建立当前 Rust runtime/scheduler stabilization plan

Status: `done / initial plan root created`

动作：

- 在 `docs/plantree/plans/` 下新增当前 Rust runtime/scheduler 计划，或更新 `runtime-correctness-and-release-gates` 为当前版本状态。
- 把原 `feature/todo` 中 P0 项映射到 roadmap。

验收：

- `docs/plantree/README.md` 注册该计划。
- 有明确 Current Phase、Last Landed、Next Target。

### P1-2：建立 document disposition index

动作：

- 生成 `indexes/document-disposition.md`。
- 初始覆盖：
  - `docs/analysis/**`
  - `docs/kiro-rs-root-cause-package-20260726T170519+0800/**`
  - `feature/issues/**`
  - `feature/evidence/**`
  - `docs/*.md` 中旧分析文档。

字段：

| 字段 | 含义 |
| --- | --- |
| Path | 文档路径 |
| Current class | active-authority / active-issue / evidence / superseded-analysis / archive-candidate |
| Status header | 文档自身 status |
| Superseded by | 新权威或源码/测试 |
| Action | keep / update / split / archive-later / delete-candidate |
| Notes | 风险和链接 |

### P1-3：按技术域归档旧文档

Status: `in-progress / first archive batches completed`

建议批次：

1. `done`: slow-first-token / stream-fluidity 历史。
2. `done`: 2026-06-26 Kiro proxy study / optimization plans。
3. `done`: old scheduler-dispatch redesign。
4. `done`: root-level request/body/protocol 历史。
5. `done`: scheduler/external-pool/runtime root-level 历史。
6. `done`: usage/dashboard/cache root-level 历史。
7. `next`: UI/admin 历史和 feature/workspace 历史二次整理。

每批验收：

- 入站链接已更新。
- archive README 记录原路径、来源、恢复方式。
- plan-tree source map 指向新位置。
- 不丢证据。

### P1-4：刷新 stale plan-tree 状态

需要更新：

- `docs/plantree/README.md` 的 Current as of 日期。
- `runtime-correctness-and-release-gates` 的 v0.0.114 状态。
- 当前 0.0.120+ / 0.0.123+ 之后的 runtime/scheduler 修复证据。

验收：

- 进入项目时不会误以为 v0.0.114 是最新完成状态。
- 当前 Rust 修复路线和 greenfield 长期路线边界清楚。
