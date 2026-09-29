# P27 Claude Code 自动压缩的触发机制与服务端职责

Status: fixed (client compaction on upstream too-long for every route; not released)
Severity: High
Area: response
Discovered: 2026-09-29
Verified-against: Claude Code CLI 2.1.283（源码字符串分析 + 本地 fake 服务端实测 + 真实上游实测）

## 问题与影响

usage 按路由挂载的策略整形，整形后的字段就是计费字段，必须保持不变。需要确认的是：在这个前提下，Claude Code 客户端（未关闭自动压缩）能否正常自动压缩，以及服务端应当怎样配合。

旧版本的实测结果（sonnet-4.5，200k 窗口）：会话上下文超出后，CLI 每一轮都收到 `Request input content length exceeded...` 并失败，从未触发压缩。原因是报错文案不符合协议，已由 P01 修复。

## 官方协议对照

以下是 CLI 2.1.283 的实际机制，均已实测确认：

- **上下文窗口**：由客户端判定，服务端不下发。模型名带 `[1m]`，或 `anthropic-beta` 含 context-1m 且模型支持时为 1M；否则取内置模型表的值，默认 200k。
- **已用量**：`input_tokens + cache_creation_input_tokens + cache_read_input_tokens`，加上 output，再加上新增消息的估算值。
- **主动压缩**：只有客户端配置了压缩窗口（settings 里的 `autoCompactWindow` 或环境变量 `CLAUDE_CODE_AUTO_COMPACT_WINDOW`）才会启用。阈值为 min(配置窗口, 模型窗口) − min(max_output, 20k) − 13k，`CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` 可以调整。判断依据是 usage。
  - 实测：配置 100k 时，日志为 `effectiveWindow=80000`、threshold 67000，触发压缩。
- **被动压缩（默认）**：未配置窗口（source 为 `auto`）时不做主动检查。API 返回 400、且 message 含 `prompt is too long` 或 `input is too long for requested model` 时，客户端压缩后重试。客户端会用正则 `prompt is too long[^0-9]*(\d+) tokens? > (\d+)` 解析实际值和上限。
  - 实测：默认配置下，usage 报到 190k 乃至 980k 都不会主动压缩；收到这条 400 后执行压缩，`trigger=auto`。
- **关闭**：`autoCompactEnabled=false`、`DISABLE_AUTO_COMPACT` 或 `DISABLE_COMPACT` 时，收到 400 也不压缩，只显示错误。

## 源码链与根因

- 客户端的主动路径完全依赖 usage 字段。usage 字段不能改，所以配置了自定义窗口的客户端，其阈值是用整形后的数字来比较的。这是 usage 整形策略本身的结果，服务端也收不到客户端的窗口配置。
- 默认客户端依赖被动路径，服务端必须在真实超限时返回协议格式的 `prompt is too long`，这由 P01 实现。

## 复现

### 最小复现（单测）

P01 的用例：`upstream_too_long_errors_map_to_prompt_is_too_long_for_five_rounds`、`prompt_too_long_messages_use_claude_code_protocol_prefix`。

### 端到端复现

- 真实上游：`tmp/thinking-budget-local/fix-evidence-20260929/compact/grow.sh`，`TURNS=4 CHUNK=150000`。
- fake 服务端：同目录下的 `fake.mjs`，`FAKE_MODE=too_long`、`usage_high`、`tool_loop`。

## 修复方案

### 候选方案

- A. 服务端主动信号（`833315c`）：服务端按上一轮真实上下文达到窗口 80% 时，提前返回 `prompt is too long`。问题在于它替所有客户端定了压缩点，会覆盖客户端自己配置的窗口，不符合 CLI 的标准行为。**已按要求撤销（`04d8272`）。**
- B. 保留标准的被动路径（P01）：只在 Kiro 真实超限时，返回协议格式的 `prompt is too long: N tokens > M maximum`。**当前采用。**

### 推荐方案

采用 B，并继续跟进两点：
1. `/cc` 路由上，on_too_long 静默裁剪后重试成功时，CLI 感知不到上下文已满，会导致历史被静默丢弃（P03）。可以考虑为 Claude Code 路由提供一个配置项：超限时直接返回协议报错，交给客户端压缩。
2. 提升报错中 N 的准确度，让客户端解析出的 N−M 更接近真实差值。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)。

真实 CLI，sonnet-4.5，当时的构建包含 P01：每轮 150 KB，第 2 到 4 轮都先收到 `prompt is too long`，自动压缩（如 158,749 降到 38,947）后返回 `turn-N-ok`。撤销 `833315c` 不影响这条路径。

## 兼容性与风险

- 撤销后，usage 字段和计费不受任何影响。
- 配置了自定义窗口的客户端，是否触发压缩取决于整形后的 usage 数值。
- 首轮就超长的单条消息请求，客户端没有可压缩的内容，这一点与官方行为一致。

## 2026-09-29 最终方案与验证

### 方案（所有路由一致，只有 usage 策略按路由挂载）

- 新增配置 `payloadTooLongHandling`，对所有路由生效：
  - 默认值 `client_compaction`：Kiro 判定输入过长时，不再由服务端静默裁剪后重试，而是直接返回协议格式的 400 `prompt is too long: N tokens > M maximum`，由 Claude Code 自动压缩后重试。行为与官方 API 一致。
  - `trim_retry`：保留旧的服务端裁剪行为，可以按需切回。
- 例外：Claude Code 自己的压缩请求（最后一条 user 含 `create a detailed summary of the conversation`）仍走裁剪重试，保证压缩本身能完成。重试时做两件事：
  - 截断当前轮的 tool_result。压缩请求会把正在进行那一轮的工具结果放在当前消息里，只裁剪历史解决不了。
  - 把加权上限封顶为模型窗口的 90%。默认的 1.3M weighted 远高于 Kiro 按 token 计的真实上限，曾导致压缩请求重试后仍然超长。
- usage 字段不做任何改动。服务端不主动发压缩信号，`833315c` 已撤销。

### 真实上游验证（CLI 2.1.283，sonnet-4.5，200k 窗口，测试实例 `127.0.0.1:19023`，隔离 CLI 环境）

| 场景 | 旧版本 | 新版本 |
| --- | --- | --- |
| 逐步增长（每轮 40KB，共 10 轮） | — | 10 轮全部成功，第 7 轮自动压缩 |
| 每轮大幅增长（150KB，共 4 轮） | 第 2 轮起每轮都报错，会话卡死 | 第 2 到 4 轮都先压缩再成功 |
| 单轮工具循环中途超长（10 个文件并行 Read，每个 60KB） | 第 3 次读文件后报错，工具调度中断 | 自动压缩后继续执行，10 次工具调用全部完成，答案正确（跑了 2 次）。中间版本漏了压缩请求的特殊处理，结果压缩失败（`exhausted`），加上后修复 |
| 客户端关闭自动压缩（`autoCompactEnabled=false`） | — | 显示 `Prompt is too long`，由用户手动 `/compact`，与官方行为一致 |
| 客户端配置窗口 `CLAUDE_CODE_AUTO_COMPACT_WINDOW=100000` | — | 按整形后的 usage 主动压缩，6 轮全部成功，不报错 |

- 直接回放一个过长的压缩请求：第一次返回 400，服务端按窗口上限裁剪后重试，返回 200 并生成摘要。
- 回归：Opus thinking 23/23；Sonnet 的结果与之前一致（仅签名相关的 5 项因环境原因不适用）；转换回归 11/11。
- 真实上游多次出现瞬时 500。其中一次发生在签名重试之后的第二次请求上，该请求最终返回 502，属于签名重试路径已有的边界，与本改动无关，已单独记录。

### 对配置了自定义窗口的客户端的说明

客户端的主动阈值是用整形后的 usage 计算的。当前整形会把数值放大，所以这类客户端会比设定值更早压缩，不会晚压缩。服务端收不到客户端的配置，无法替它按真实值判断。
