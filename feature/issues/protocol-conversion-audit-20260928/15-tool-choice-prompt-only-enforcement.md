# P15 `tool_choice` 的 `any`/`tool` 无法强制：响应侧只记录违规，不重试

Status: partially-fixed / none 与 disable_parallel_tool_use 已在响应侧执行 / any 与 tool 违规只记录不重试
Severity: Low
Area: request
Discovered: 2026-09-28 协议互转审计
Verified-against: feat/prompt-injection-routes 工作区（基于 65661db，2026-10-23）

## 问题与影响

Kiro 的请求模型里没有 `toolChoice` 字段（`src/kiro/model/requests/kiro.rs`）。代理分三层模拟 Anthropic 的 `tool_choice`：

1. **结构化过滤**（`bodyConversion.toolChoiceSteering`，默认开启，见 `src/model/config.rs:194`、`src/model/config.rs:241`）：`selected_tool_indices` 的处理是 `none` 返回空列表，`{type:"tool",name}` 只保留命中的工具，`auto`/`any`/未知类型保留全部工具（`src/anthropic/converter/tools.rs:301-345`）。
2. **响应侧拦截**（同样受 `bodyConversion.toolChoiceSteering` 控制，`src/anthropic/tool_use_policy.rs`）：转换时生成 `ResponseToolPolicy`（`src/anthropic/converter.rs:549-579`），流式和非流式响应都经过 `ResponseToolGate::admit` 决定 tool_use 是否下发。
3. **提示词**（注入项 B3 `toolChoicePrefix`、B4 `toolChoiceTurnReminder`，按 `promptInjection` 的全局默认和路由设置开关）：B3 把 `<tool_choice>…</tool_choice_policy>` 前缀写进 system 对应的 history user 消息（`src/anthropic/converter/tools.rs:504-525`、`src/anthropic/converter/history.rs:71`），B4 在当前 user 消息末尾追加一句提醒（`src/anthropic/converter/tools.rs:529-547`、`src/anthropic/converter.rs:611-616`）。提示词依附在结构化处理上：`toolChoiceSteering` 关闭时 B3/B4 也不注入（`src/anthropic/converter.rs:183-187`），避免提示和实际发出的工具列表矛盾。strict profile 下注入项被阻断，只剩结构化过滤和响应侧拦截。

各模式的实际效果：

| `tool_choice` | 官方保证 | 代理行为 | 剩余缺口 |
| --- | --- | --- | --- |
| `none` | 模型不会调用工具 | 当前 tools 清空；历史工具以"不可调用"占位定义加回（满足 Kiro 校验，`src/anthropic/converter/tools.rs:64-68`），同时登记到 `ResponseToolPolicy` 的不可用名单；响应里对这些工具的调用被丢弃，`stop_reason` 由 `tool_use` 改为 `end_turn` | 无 |
| 未声明 tools | 不能调用工具 | 同 `none`（`tools_unavailable_this_turn`，`src/anthropic/converter/tools.rs:633-640`） | 无 |
| `any` | 必须至少调用一个工具 | 全部工具 + B3 "You MUST call at least one of the available tools" + B4 提醒；完成后若没有 tool_use，记录 `tool_choice_violation=any_without_tool_use` | 模型仍可能只回文本，不重试 |
| `{type:"tool",name}` | 必须调用这个工具 | 只发这一个工具 + B3/B4；完成后记录 `named_tool_without_tool_use` 或 `named_tool_mismatch`。名字不存在时退化成发送全部工具，只打一条 warn（`src/anthropic/converter/tools.rs:330-334`） | 同上 |
| auto/any/tool + `disable_parallel_tool_use: true` | `auto` 最多 1 个，`any`/`tool` 恰好 1 个 | B3 追加 "Call at most one tool in this turn"；响应侧 `max_tool_uses=1`，第二个起的 tool_use 丢弃（`src/anthropic/converter/tools.rs:600-608`） | 无 |

影响（剩余部分）：

- 依赖 `tool_choice={type:"tool"}` 做结构化抽取的 SDK 客户端，偶尔仍会拿到纯文本、`stop_reason="end_turn"`，解析失败。违规会写进 usage 记录的 `tool_choice_violation` 字段（`src/anthropic/usage.rs:323`）和 warn 日志，便于统计违规率。

## 官方协议对照

> 方向约定：kiro.rs 基于 Kiro 对外提供 Claude Code（Anthropic）协议接口。客户端（如 Claude Code CLI）发出 Claude Code 协议请求，经转换后变为 Kiro 协议，调度到最终上游 Kiro；Kiro 的响应再转换回 Claude Code 协议返回给客户端。下文引用的"Anthropic 文档/官方"一律指 **Claude Code 协议规范**，即接口侧必须满足的行为；"Kiro 侧"指上游 Kiro 协议的要求。Anthropic 官方 API 不是本项目的上游。thinking 签名均为 Kiro 原生签名，在转换中原样透传。

- **Anthropic（已验证事实，官方文档）：** `tool_choice` 有 `auto` / `any` / `tool` / `none` 四种。`any`/`tool` 由服务端强制执行（官方文档说明这时 API 会预填 assistant 内容，模型不会在 tool_use 之前输出思考文本）。`disable_parallel_tool_use` 是 `tool_choice` 对象里的可选布尔字段。扩展思考开启时只允许 `auto`/`none`（官方文档，本仓库没有实测）。
- **Kiro（经验推断）：** 本仓库和 sub2api 都没有记录 Kiro 支持 `toolChoice`。分层约定：结构化过滤和响应侧拦截归 `bodyConversion.toolChoiceSteering`，提示词归注入项 B3/B4，关闭提示词不影响结构化语义（见 [提示词注入改造方案](../../todo/prompt-injection-routes-20261023/README.md)）。
- **历史占位工具的约束（converter 注释，未独立实测）：** "Kiro API 要求：历史消息中引用的工具必须在 tools 列表中有定义"（`src/anthropic/converter.rs:539-543`）。所以 `none` 时不能去掉占位工具，只能标注不可调用并在响应侧拦截。

## 源码链与根因

1. `convert_tools` → `parse_tool_choice`（`src/anthropic/converter/tools.rs:269-299`）→ `selected_tool_indices`（`src/anthropic/converter/tools.rs:301-345`）。`disable_parallel_tool_use` 由 `parallel_tool_use_disabled`（`src/anthropic/converter/tools.rs:591-597`）读取。
2. `convert_request_with_model_id` 生成 `ResponseToolPolicy`：`max_response_tool_uses` → `limit_tool_uses`，`required_response_tool_use` → `require_tool_use`，本轮不可用时历史占位工具 → `block_tool`（`src/anthropic/converter.rs:549-579`），结果放进 `ConversionResult.response_tool_policy`。
3. 响应侧：流式在 `StreamContext` 里持有 `ResponseToolGate`（`src/anthropic/stream.rs:1936-1946`），tool_use 开块前调用 `admit`（`src/anthropic/stream.rs:3292`、`src/anthropic/stream.rs:3655-3665`），被拒的调用不开块、不计入 `has_tool_use`；非流式在 `retain_admitted_tool_use_blocks` 之后修正 `stop_reason`（`src/anthropic/handlers.rs:13437-13451`）。
4. 完成时 `mark_response_tool_gate`（`src/anthropic/handlers.rs:3365-3374`）调用 `log_completion`（`src/anthropic/tool_use_policy.rs:204`），把 `tool_choice_violation`（`src/anthropic/tool_use_policy.rs:184`）和被丢弃调用的摘要写进 usage 记录。

根因（剩余缺口）：Kiro 没有强制工具调用的能力。丢弃多余调用可以在响应侧完成，但"必须调用"只能事后发现；代理目前选择只记录，不消耗额外上游请求去重试。

## 复现

### 单测（已有，描述当前行为）

- `src/anthropic/converter.rs`：`tool_choice_none_marks_history_placeholders_unavailable_and_blocks_them`、`auto_tool_choice_keeps_regular_history_placeholder_without_blocking`、`disable_parallel_tool_use_injects_prompt_and_limits_response_tool_uses`、`required_tool_choice_adds_turn_reminder_and_violation_diagnostics`。
- `src/anthropic/stream.rs`：`unavailable_placeholder_tool_use_is_dropped_and_turn_ends_normally`（`:8982`）、`disabled_parallel_tool_use_streams_only_the_first_tool_call`（`:9053`）。
- `src/anthropic/tool_use_policy.rs`：`required_tool_choice_violations_are_classified` 等门禁单测。

### 端到端复现（剩余缺口）

1. `tool_choice={"type":"tool","name":"get_weather"}`，用户消息是 "你好"，非流式请求连续发 20 次，统计 `content` 里没有 tool_use 的次数，以及 usage 记录里 `tool_choice_violation` 的计数，两者应一致。
2. 同样请求在路由上关闭 B3/B4 后重复，对比违规率，量化提示词的作用。
3. 在 strict profile 下重复第 1 步，确认没有前缀注入，违规仍被记录。

## 修复方案

### 候选方案

| 方案 | 做法 | 评价 |
| --- | --- | --- |
| A. 维持只记录（当前） | 违规写 usage 和日志，不重试 | 不增加上游消耗；客户端仍可能拿到纯文本 |
| B. 非流式有界重试 | `any`/`tool` 违规时在 `inference_attempt_budget` 内重试 1 次，仍不满足时返回原结果 | 能降低违规率，但增加延迟和上游消耗，需要独立开关 |
| C. 本地拒绝 | 违规时返回错误 | 破坏兼容性，不可取 |

### 推荐方案

先按 A 运行，用 usage 里的 `tool_choice_violation` 统计各模型、各路由的违规率。只有违规率明确影响结构化抽取类客户端时再做 B，且只覆盖非流式；流式已经向下游发出内容，无法无损重试，继续只记录。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)，只使用 `127.0.0.1:19023` 指定测试实例和隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，Cargo 通过 `feature/tests/run-cargo-scoped.sh` 运行，不得影响本机正在运行的 Claude Code CLI 与服务。

- 回归：上面"复现"列出的单测，以及 `test_tool_choice_none_omits_current_tools`、`test_tool_choice_named_tool_filters_current_tools`、`named_tool_choice_*` 系列、`all_items_off_disables_prompts_but_preserves_structured_tool_choice_semantics`、`disabled_tool_choice_prompt_subtoggle_keeps_structured_named_filtering`、`test_anthropic_strict_filters_tool_choice_without_prompt_injection`（`src/anthropic/converter.rs`）。
- 若实施方案 B：非流式 handler 夹具中 `tool_choice=tool` 且上游只回文本时重试 1 次（`upstream.hits()==2`），重试计入 attempt budget；流式不重试。
- 端到端：上面第 1 到 3 步各跑 20 次，记录违规率作为是否实施 B 的依据。

## 兼容性与风险

- `none` 时丢弃 tool_use 会让模型那一轮"说到一半"。官方语义本来就不允许调用工具，这比让客户端执行一个空参数工具安全。
- 流式丢弃多余 tool_use 时，被丢弃的调用已经消耗了 output token，usage 仍按上游计量。
- 响应侧拦截和结构化过滤一起受 `bodyConversion.toolChoiceSteering` 控制；关闭它会同时关闭 B3/B4，`none` 时历史占位工具也不再被拦截。
- 若实施方案 B，重试必须放在独立开关后面，默认关闭，并计入 attempt budget。
