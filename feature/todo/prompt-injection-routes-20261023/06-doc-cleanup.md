# 06 旧文档处理清单

原则：文档里只保留最终结果。整篇讲的都是旧机制的，直接删除，同时修正指向它的链接和索引；只有部分段落涉及旧机制的，就地改写成最终行为，不加"更正"之类的说明。事故记录里的现场快照（当时的配置值、request id）属于事实，保留原样。

这一步和代码改动一起提交，并且在代码合并之后执行。在那之前，这些文档描述的仍然是线上的真实行为。

## 删除

| 文件 | 原因 |
|---|---|
| `feature/issues/prompt-policy-tool-choice-and-count-tokens.md` | 主题是"总开关管住所有新增提示"，这个总开关本次删除 |
| `feature/evidence/prompt-control-independence-20260716.md` | 是上一条的证据 |
| `feature/tests/prompt-control-independence.mjs` | 检查旧 setter 和 `bodyConversion` 的独立性，相关字段已删除 |
| `feature/tests/prompt-default-parity.mjs` | 检查三处默认文本是否一致；改造后默认文本只在后端一处 |
| `feature/issues/protocol-conversion-audit-20260928/14-prompt-steering-alters-tool-behavior.md` | 结论已经并入本目录：分块注入的范围由 02/03 定义，阈值问题见 05 第 6 条 |
| `docs/archive/runtime-correctness-feature-workspace-history/plans/prompt-injection-inventory-and-centralization-plan-20260715.md` | 被本目录 01 取代；文中的注入点清单已经过时（例如 `Continue` 占位已不存在） |

## 改写相关段落

| 文件 | 改写内容 |
|---|---|
| `feature/issues/route-policy-config-authority-20260802.md` | 把"提示词引导按 routeMode/routeRules 命中"改为"注入项不带路由规则，由路由显式设置"；验收矩阵里提示词相关的几行同步改写 |
| `feature/issues/protocol-conversion-audit-20260928/15-tool-choice-prompt-only-enforcement.md` | 现状已经变了：`disable_parallel_tool_use` 和 none 时占位工具的拦截都已实现（`tool_use_policy.rs`），any/tool 违规会记录为 `tool_choice_violation`。按现状重写；剩下的缺口只有"any/tool 违规时不重试" |
| `feature/issues/protocol-conversion-audit-20260928/README.md` | 删掉 P14 这一行；P15 的摘要按新现状写 |
| `feature/issues/protocol-conversion-audit-20260928/21-tool-schema-and-result-field-fidelity.md` | 开关路径改为 `promptInjection` 的 `chunkedWriteToolSuffix` |
| `feature/issues/protocol-conversion-audit-20260928/25-thinking-parameter-validation-gaps.md` | `force_visible_thinking` 的来源改为路由功能 `thinkingTrigger` |
| `feature/issues/protocol-conversion-audit-20260928/05-pascalcase-tool-names-rewritten-with-hash.md` | 删掉指向 prompt-policy 专题的那句 |
| `feature/issues/protocol-conversion-audit-20260928/12-count-tokens-estimate-inconsistent.md` | 修正指向已删除文档的链接 |
| `feature/issues/language-constraint-first-language-lock-20260802.md` | 源码链改为 `promptInjection` 和 A2/A3，"路径由 scope/routeRules 决定"改为"由路由设置决定" |
| `feature/issues/10-stream-end-turn-vs-silent-truncation.md` | "/cc prompt steering 的任务质量提示"改为"任务质量注入项（A1）" |
| `feature/issues/thinking-effort-adaptive-upstream-mapping.md` | 第 156、245 行关于总开关的描述改为最终的开关结构 |
| `feature/issues/two-ui-cost-precision-and-config-authority.md` | 删掉 promptSteering 和 bodyConversion 相互覆盖的描述（字段已删除） |
| `feature/issues/README.md`、`current-issue-status-index-20260731.md`、`issue-analysis-priority-queue-20260731.md` | 删掉已删除文档的条目；保留的条目按新描述改写 |
| `feature/evidence/README.md`、`final-regression-rerun-20260722.md`、`final-candidate-c0d-static-cli-load-ui-20260721.md`、`lightweight-contract-regression-20260721.md` | 删掉对已删除证据和脚本的引用；"总开关仍是提示词注入总门"这一节删除 |
| `feature/audits/analysis-status-index-20260715.md`、`migration-map-20260716.md`、`issue-document-completeness-20260716.md`、`claude-code-internal-leak-deep-audit-20260715.md` | 修正链接；"两个 UI 保存时互相覆盖"一段删除 |
| `docs/archive/runtime-correctness-feature-workspace-history/README.md`、`plans/README.md`、`final-report.md` | 删掉被删方案的条目和 PRO-007 指向的链接 |
| `docs/analysis/official-claude-code-kiro-protocol-audit-20260928.md` | 现场快照保留。第 266–300 行"修复后 inject_thinking_prefix 的条件"和第 676 行，改写为最终条件：B1 开关 + 模型没有原生字段 |
| `docs/plantree/plans/rust-runtime-scheduler-stabilization/topics/official-claude-code-kiro-protocol-audit-20260928.md`、`implementation-status.md`、`roadmap.md` | "prompt steering 用 routeMode/routeRules"改为最终结构 |
| `docs/plantree/plans/request-body-capability-modularization/topics/body-processing-inventory.md` | 删掉 `thinkingPromptControls`，`toolChoiceSteering` 的说明改为"只控制过滤和拦截" |
| `docs/testing/claude-code-cli-real-account-test-requirements-20260929/01、02、03、04` | 用例前置条件里的"关闭 promptSteering 总开关"改为"在路由上关闭对应注入项"；CC-U-57 改为按注入项逐项关闭；CC-Q-02 的"只在 /cc 注入"改为"按路由设置" |
| `docs/testing/claude-code-cli-real-account-run-20260929.md` | 第 61 行的待决定项改为指向 05 第 6 条 |
| `config.example.json` | 删掉 `chunkedToolPolicy` 和 `thinkingPromptControls`，补上 `promptInjection` 和 `routeFeatures` 示例 |
| `README.md` | 补一节"提示词注入"：说明配置位置、新装默认值，并链接到管理页面 |

## 不处理

- `feature/todo/cctest-protocol-compat-20261020/*`：用的是"身份隐藏 / 输出过滤"这类产品说法，描述的是测试现象，名称变化不影响结论。
- `docs/archive/kiro-proxy-study-*`、`docs/archive/external-project-learning-history/*`：外部项目的学习笔记，不描述本项目的开关。
- `feature/issues/bare-invoke-*`、`feature/evidence/bare-invoke-*`：这里的 "prompt-injection" 指的是安全意义上的注入文本，和本次改造无关。

完成后执行 `node feature/tests/check-feature-docs.mjs`，确认没有断链；再全库 grep `promptSteering|prompt_steering|chunkedToolPolicy|thinkingPromptControls`，除迁移代码和本目录外应该没有结果。
