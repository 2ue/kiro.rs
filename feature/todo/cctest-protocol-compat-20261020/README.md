# cctest 协议兼容改造（未完成任务交接）

来源会话：`claude --resume 7f53e926-4c65-435a-9d3c-29784eb413f1`（2026-10-20 07:20 起的最后一段任务）
整理时间：2026-10-20

## 当前状态（2026-10-20 更新）

- 进度：P1、P2、P3 已实现并提交到 `feat/protocol-compat`，未合并、未发布，等用户验收。
- 提交：`1f5be22` 工具 id、`f737d1f` 结构化输出、`474324d` WebSearch 模型驱动模式、`c55ba33` clippy 修复。
- 验证：全量测试 2543 + 31 通过；前端 tsc 和 vite build 通过；隔离实例真实账号回归通过；本地 Claude Code 多轮工具调用通过。

## 文档目录

| 文件 | 内容 |
|---|---|
| [01-original-requirements.md](01-original-requirements.md) | 用户原话和已拍板的约束 |
| [02-evidence.md](02-evidence.md) | 本地复现、题库实测、官方协议和开源检测器的判定条件、代码定位 |
| [03-analysis-and-plan.md](03-analysis-and-plan.md) | 逐项根因分析，以及待实施方案、测试计划和风险 |
| [04-open-items.md](04-open-items.md) | 尚未决定的问题和下一步执行清单 |
| [artifacts/](artifacts/) | 复现脚本、题库、实测原始数据 |

## 一句话结论

cctest 的失败项大多不是功能缺失，而是返回结构和官方 Anthropic 协议对不上。用户确认要做的是：工具调用 id、结构化输出、WebSearch 开关。代码执行不改；usage 和签名也不改，所以“流结构校验”和“签名校验”预计仍是部分通过。cctest 的“工具调用”失败很可能来自代码执行题，所以只改 id 未必能让这一项通过。
