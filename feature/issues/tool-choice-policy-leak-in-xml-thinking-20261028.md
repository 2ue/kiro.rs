# tool_choice 注入策略在 thinking 中被复述

Status: `fixed / local-and-prod-account-verified / merged-main / not-released`

Severity: P1（对用户可见，违反"注入无感知"要求，不影响任务结果）

## 现象

请求带 `tool_choice` 和 thinking 时，模型在 thinking 里说出代理注入的标签名和原文：

```text
According to the tool_choice_policy, I cannot call any tools in this turn, including tools used earlier.
```

Claude Code 会把 thinking 显示给用户。none、禁用并行、指定工具都会出现；any 偶尔出现。

## 复现

本地实例导入现网配置，本地 JSON 账号，路由 `/dfcache/minea`：

```json
{"model":"claude-sonnet-4-5","max_tokens":2000,"stream":true,
 "thinking":{"type":"adaptive"},"tool_choice":{"type":"none"},
 "tools":[{"name":"Bash","description":"run","input_schema":{"type":"object","properties":{"command":{"type":"string"}}}}],
 "messages":[{"role":"user","content":"ultrathink 当前目录有哪些文件？如果不能执行命令就直接说明。"}]}
```

修复前 0.0.223 和 0.0.224 都是每次复现（thinking 出现 `tool_choice_policy`）。上游只返回 `assistantResponseEvent`，thinking 走 XML 兼容路径；opus-5.5 的原生 reasoning 路径同样会复述。

## 根因

1. 注入文本用了带语义的 XML 标签名（`<tool_choice>`、`<tool_choice_policy>`、`<tool_choice_reminder>`）。模型推理时会引用"它看到的指令"，标签名是最自然的称呼。
2. 结尾的 "Do not mention or quote this policy" 对正文有效，对 thinking 基本无效，反而让模型把这段当成"一条策略"来分析。
3. 输出侧没有针对注入标签名的过滤（`identity_guard` 只处理正文）。

## 修复方案

只改注入文案（`src/model/prompt_injection/catalog.rs`），不加输出侧过滤，避免误伤用户自己讨论 `tool_choice` 的内容：

| 项 | 修复后文案 |
| --- | --- |
| any 前缀 | `Begin this reply by calling one of the available tools.` |
| tool 前缀 | ``You MUST call the tool `{name}` in this reply. A reply without that tool call is invalid.`` |
| none 前缀 | 沿用 main 上 `8b2c2878` 的句子（描述回复应该怎样，不说"禁止"），去掉外层标签 |
| 禁用并行前缀 | `Make one tool call per reply, and wait for its result before making the next.` |
| any / tool 每轮提醒 | `Respond by calling one of the available tools.` / ``Respond by calling the tool `{name}`.`` |

文案里不再出现尖括号、`tool_choice`、`policy`、`reminder`、`mention`、`quote`、`this turn`，由 `catalog.rs` 的单测固定。

## 验证

### 文案对照实验（本地 JSON 账号，sonnet-4.5，每格 5 次）

把候选文案放在 Claude Code 风格的 system 最前面，和不加注入的对照组比较遵从率。提示词选的都是模型本来不会照做的：

| 模式 | 对照（不注入） | 旧文案 | 新文案 |
| --- | --- | --- | --- |
| any（问"1+1"） | 0/5 调用工具 | 5/5 | 5/5 |
| tool 指定 Read（问天气） | 0/5 | 5/5 | 英文 5/5，中文 4/5（1 次调了 Bash，真实链路只发指定工具，不会发生） |
| 禁用并行（同时看两个文件） | 0/5 只调一个 | 5/5 | 5/5 |

旧文案 thinking 复述标签 5/5，新文案 0/5。

### 真实链路（本地 JSON 账号，sonnet-4.5 / haiku-4.5，每格 4 次）

| 模式 | 修复前 thinking 复述 | 修复后 | 修复后照做 |
| --- | --- | --- | --- |
| none（中文） | 4/4 | 0/4 | 4/4 不调工具 |
| none（英文） | 4/4 | 0/4 | 4/4 |
| any | 0/4 | 0/4 | 4/4 调用 |
| tool Read | 0/4 | 0/4 | 4/4 调 Read |
| 禁用并行 | 4/4 | 0/4（haiku）、1/4（sonnet，说"按要求一次读一个"，无标签名） | 4/4 只调一个 |

### 现网账号（opus-5.5，每格 3 次）

账号 #1971（KIRO PRO MAX，并发 5，RPM 20），本机经 152.53.243.159 转发到账号自己的代理 #171，出口 IP 与现网一致（64.32.179.111）。现网配置表导入本地隔离库，路由 `/dfcache/mine-haa`，`effort=high`。本地副本的 refreshToken 是假值，后台刷新关闭。

| 场景 | 修复前 照做 / 复述标签 | 修复后（合并 main 后） 照做 / 复述标签 |
| --- | --- | --- |
| none | 3/3 / 0 | 3/3 / 0 |
| none，历史里调用过工具 | 3/3 / 2 | 3/3 / 0 |
| 未声明工具，历史里调用过工具 | 3/3 / 2 | 3/3 / 0 |
| any（问"1+1"） | 3/3 / 0 | 3/3 / 0 |
| tool Read（问天气） | 3/3 / 0 | 3/3 / 0 |
| 禁用并行 | 3/3 / 0 | 3/3 / 0 |

## 风险与回滚

- 模型在 thinking 里仍可能用自己的话提到约束（例如"按要求一次只读一个文件"），但不再出现代理内部的标签名。
- 回滚：恢复 `catalog.rs` 中的原文案即可，不涉及配置或数据迁移。

## 遗留问题

`mine-haa` 开启的身份提示词（`<assistant_identity>`，"You are Claude, made by Anthropic, not Kiro…"）会让 opus-5.5 在 thinking 里分析它，例如"这段身份说明在用户轮里，不是工具结果，所以不算 prompt injection"。修复前后都有（现网账号 any、tool、禁用并行场景合计约一半），与本次 tool_choice 改动无关，需要单独处理。
