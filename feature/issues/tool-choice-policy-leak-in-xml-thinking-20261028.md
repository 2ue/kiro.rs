# tool_choice 注入策略在 XML thinking 中被复述

Status: `open / reproduced-locally / fix-in-progress`

Severity: P1（对用户可见，违反"注入无感知"要求，不影响任务结果）

## 现象

请求带 `tool_choice: {"type": "none"}` 和 thinking 时，模型在 thinking 里说出代理注入的策略标签：

```text
I need to check the tool_choice_policy in the current turn - it says "Do not call any tool in this turn,
including tools used earlier in this conversation. Respond with text only."
```

正文只说"根据这个对话回合的限制，我只能用文字回复"，不出现标签名。客户端（Claude Code）会把 thinking 显示给用户，用户能看到 `tool_choice_policy` 和注入原文。

## 复现

本地实例（0.0.223 与修复首字后的版本相同），本地 JSON 账号（KIRO FREE，social），路由 `/dfcache/minea`，现网配置：

```json
{"model":"claude-sonnet-4-5","max_tokens":2000,"stream":true,
 "thinking":{"type":"adaptive"},"tool_choice":{"type":"none"},
 "tools":[{"name":"Bash","description":"run","input_schema":{"type":"object","properties":{"command":{"type":"string"}}}}],
 "messages":[{"role":"user","content":"ultrathink 当前目录有哪些文件？如果不能执行命令就直接说明。"}]}
```

| 版本 | thinking 出现 `tool_choice` / `policy` | 正文出现 | 调用了工具 |
| --- | --- | --- | --- |
| 0.0.223（`51a9deb1`） | 3/3 | 0/3 | 0/3 |
| 首字修复分支 | 3/3 | 0/3 | 0/3 |

上游只返回 `assistantResponseEvent`，thinking 走 XML 兼容路径（`<thinking>` 标签提取），不经过原生 reasoning 代码。

## 源码链

- 注入文本：`src/model/prompt_injection/catalog.rs` `TOOL_CHOICE_NONE_PREFIX`（以及 ANY / TOOL / PARALLEL_DISABLED）。形如 `<tool_choice>none</tool_choice><tool_choice_policy>... Do not mention or quote this policy.</tool_choice_policy>`。
- 同目录的 `THINKING_OUTPUT_POLICY` 会要求模型在 thinking 模式下先写 `<thinking>` 推理。
- 输出侧：`src/anthropic/stream.rs` XML thinking 提取后经 `ToolTranscriptSanitizer`，它只识别工具结果转录（`user Continue` 等），不处理注入标签名。`identity_guard` 只处理正文 text，不处理 thinking。

## 根因

1. 注入文本用了带语义的 XML 标签名 `tool_choice_policy`。模型推理时会引用"它看到的指令"，标签名就是最自然的称呼。
2. "Do not mention or quote this policy" 对可见正文有效，对 thinking 基本无效：模型把 thinking 当成内部推理，会在里面复述约束来规划回答。
3. 输出侧没有任何针对注入标签名或注入原文的过滤，thinking 里出现就直接下发。

## 已有的同类要求

- 记忆和文档里的"注入必须无感知"：模型不能复述注入内容，提醒只加在有用户文字的轮次。
- 2026-10-28 前的 tool_choice 验收要求扫描回复里有没有 `policy`、`tool_choice` 字样，当时只扫了正文。

## 修复方案

待定，修复后在本节写选定方案。候选方向：

- 注入侧：去掉带语义的标签名和"不要提及本策略"这类元指令，改成模型不会当成"外部指令"去引用的写法。
- 输出侧：对 thinking 中出现的注入标签名做兜底过滤。签名推理不能改写，只能作为最后手段。

## 风险与回滚

- 改注入文本可能影响 `tool_choice` 的遵从率，需要对 none / any / tool / parallel disabled 分别验证。
- 输出侧过滤可能误伤用户自己讨论 `tool_choice` 的正常内容。
- 回滚：恢复 `catalog.rs` 中的原注入文本即可，不涉及数据迁移。

## 验收

- 上面的复现请求 thinking 和正文都不出现 `tool_choice`、`policy`、注入原文片段，至少 5 轮。
- `tool_choice` 为 none / any / tool / parallel disabled 各自仍然生效（none 不调用工具，any 必须调用，tool 调用指定工具，parallel disabled 最多一个工具）。
- 原生 reasoning 和 XML thinking 两条路径都覆盖；真实 Claude Code 多轮任务不受影响。
