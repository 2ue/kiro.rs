# P15 `tool_choice` 只靠过滤工具和提示词来约束：`none` 仍可调用历史占位工具，`any`/`tool` 不保证调用，`disable_parallel_tool_use` 被忽略

Status: open / documented / not-fixed
Severity: Medium
Area: request
Discovered: 2026-09-28 协议互转审计
Verified-against: a4227c1 (2026-09-29)

## 问题与影响

Kiro 的请求模型里没有 `toolChoice` 字段（`src/kiro/model/requests/kiro.rs:31-65`）。代理在两层模拟 Anthropic 的 `tool_choice`：

1. **结构化过滤**（受 `bodyConversion.toolChoiceSteering` 控制，默认开启，见 `src/model/config.rs:176-178`、`src/model/config.rs:205-215`）：`selected_tool_indices` 的处理是 `none` 返回空列表，`{type:"tool",name}` 只保留命中的工具，`auto`/`any`/未知类型保留全部工具（`src/anthropic/converter/tools.rs:254-298`）。
2. **提示词引导**（受 `promptSteering.enabled`、`promptSteering.toolChoice.enabled`、非 strict profile、`toolChoiceSteering` 同时控制，默认全部开启，见 `src/anthropic/converter.rs:177-183`、`src/model/config.rs:440-455`）：把 `<tool_choice>…</tool_choice_policy>` 前缀注入到 system 对应的那条 history user 消息里（`src/anthropic/converter/tools.rs:445-474`、`src/anthropic/converter/history.rs:62-120`）。

各模式的实际效果：

| `tool_choice` | 官方保证 | 代理实际发给 Kiro 的内容 | 缺口 |
| --- | --- | --- | --- |
| `none` | 模型不会调用工具 | 当前 tools 被清空，但历史里用过的工具会以**空 schema 占位工具**加回来（`src/anthropic/converter.rs:519-543`、`src/anthropic/converter/tools.rs:59-79`），另外只有一句 "Do not call tools" 提示 | 模型仍然可以调用占位工具（例如 `bashHashd1e9567d`，参数 `{}` schema），响应侧会把它当成真实调用发给客户端 |
| `any` | 必须至少调用一个工具 | 全部工具，加上 "Use at least one available tool … **when a tool can satisfy the request**" | 措辞比官方弱（带条件），也没有任何校验 |
| `{type:"tool",name}` | 必须调用这个工具 | 只发这一个工具，加上历史占位工具，再加提示 | 模型可以不调用而直接回文本，也可以调用占位工具。名字不存在时会退化成发送全部工具，只打一条 warn（`src/anthropic/converter/tools.rs:280-287`） |
| 任意 + `disable_parallel_tool_use: true` | `auto` 时最多 1 个，`any`/`tool` 时恰好 1 个 | 字段被完全忽略（在 `src/` 下 grep 不到 `disable_parallel_tool_use`） | 可能一次返回多个 tool_use |

提示词引导的默认生效范围：converter 用的是全局 `prompt_steering.enabled`（`src/anthropic/handlers.rs:908`、`src/anthropic/handlers/local_body_pipeline.rs:168-184`），**不受** request-level `scope/routeRules` 的影响。`routeRules` 默认 `["/cc"]`，只对 `prompt_steering.rs` 注入 language/task/custom system prompt 生效（`src/anthropic/prompt_steering.rs:67-107`）。所以在默认配置下，所有非 strict 路由都会注入 tool_choice 前缀。运营方关闭总开关，或者使用 strict / anthropic-strict profile 时，`any`/`tool` 就**只剩结构化过滤，没有任何引导**。

> 2026-09-29 代码核对（HEAD a4227c1）：3a1306d 把 `ConverterOptions` 提成局部变量（`local_body_pipeline.rs:168-179`），首次转换在 `:180-184`，行号由 `166-181` 更正为 `168-184`。新增的 reasoning 回退重转（`:200`）复用同一份 `converter_options`，tool_choice 过滤与提示词注入行为不变。

影响：

- 依赖 `tool_choice={type:"tool"}` 做结构化抽取（把工具当作 JSON 输出 schema）的 SDK 客户端，偶尔会拿到纯文本，而 `stop_reason="end_turn"`，解析直接失败。
- `none` 时模型调用了历史占位工具，客户端会执行一个参数为空的工具。Claude Code 会报工具参数错误，或者执行出意料之外的行为。
- 设置了 `disable_parallel_tool_use` 的客户端（有些编排器一次只能处理一个 tool_result）收到多个 tool_use，可能卡死或丢结果。

## 官方协议对照

> 方向约定：kiro.rs 基于 Kiro 对外提供 Claude Code（Anthropic）协议接口。客户端（如 Claude Code CLI）发出 Claude Code 协议请求，经转换后变为 Kiro 协议，调度到最终上游 Kiro；Kiro 的响应再转换回 Claude Code 协议返回给客户端。下文引用的"Anthropic 文档/官方"一律指 **Claude Code 协议规范**，即接口侧必须满足的行为；"Kiro 侧"指上游 Kiro 协议的要求。Anthropic 官方 API 不是本项目的上游。thinking 签名均为 Kiro 原生签名，在转换中原样透传。

- **Anthropic（已验证事实，官方文档）：** `tool_choice` 有 `auto` / `any` / `tool` / `none` 四种。`any`/`tool` 由服务端强制执行（官方文档说明这时 API 会预填 assistant 内容，模型不会在 tool_use 之前输出思考文本）。`disable_parallel_tool_use` 是 `tool_choice` 对象里的可选布尔字段。扩展思考开启时只允许 `auto`/`none`（官方文档，本仓库没有实测）。
- **Kiro（经验推断）：** 本仓库和 sub2api 都没有记录 Kiro 支持 `toolChoice`。之前的决策文档 [prompt-policy 专题](../prompt-policy-tool-choice-and-count-tokens.md) 明确规定：结构化过滤归 `bodyConversion.toolChoiceSteering` 管，提示词归 `promptSteering.enabled` 总开关管，总开关不能删除结构化语义。本文不改变这个分层，只在它上面补上**响应侧执行**。
- **历史占位工具的约束（converter 注释，未独立实测）：** "Kiro API 要求：历史消息中引用的工具必须在 currentMessage.tools 中有定义"（`src/anthropic/converter/tools.rs:59-60`、`src/anthropic/converter.rs:519-521`）。所以 `none` 时不能简单地把占位工具也去掉，否则可能触发上游 400。

## 源码链与根因

1. `convert_tools` → `parse_tool_choice`（`src/anthropic/converter/tools.rs:222-252`）→ `selected_tool_indices`（`src/anthropic/converter/tools.rs:254-298`）。`parse_tool_choice` 只读 `type` 和 `name`，`disable_parallel_tool_use` 这个键从来不读。
2. `convert_request_with_model_id` 在 `convert_tools` 之后给历史工具补占位（`src/anthropic/converter.rs:519-543`），不看 `tool_choice`。
3. `generate_tool_choice_prefix`（`src/anthropic/converter/tools.rs:445-474`）只在 `inject_tool_choice_prefix()` 为真时生成前缀。
4. 响应侧（`src/anthropic/stream.rs:3436-3625`、`src/anthropic/handlers.rs:10677-10751`）完全不知道请求的 `tool_choice`：`StreamContext` 和 `handle_non_stream_request` 的参数里都没有这个信息（`src/anthropic/handlers.rs:9832-9852`）。

根因：Kiro 没有原生的强制能力，而代理只在请求侧做了 "尽力而为" 的引导，响应侧没有做校验或修正。

审计原述核对：
- `tools.rs:~254-298, 445-474` 和 `converter.rs ~528-543` 准确。占位逻辑的完整块是 `src/anthropic/converter.rs:519-543`。
- "any/tool 只靠 prompt steering（需要 prompt_steering 开启）"需要修正：`{type:"tool"}` **还有结构化过滤**，只发一个工具，这不依赖 prompt steering。`any` 确实只有提示词。prompt steering 默认开启，不受 routeRules 限制。

## 复现

### 最小复现（单测）

放进 `src/anthropic/converter.rs` 的 `mod tests`，复用 `base_tool_choice_request`（`src/anthropic/converter.rs:3967-3985`）：

```rust
#[test]
fn audit_p15_tool_choice_none_still_exposes_history_placeholder_tools() {
    use super::super::types::Message as AnthropicMessage;

    let mut req = base_tool_choice_request(serde_json::json!({"type": "none"}));
    req.messages = vec![
        AnthropicMessage { role: "user".into(), content: serde_json::json!("run it") },
        AnthropicMessage {
            role: "assistant".into(),
            content: serde_json::json!([
                {"type": "tool_use", "id": "toolu_h1", "name": "read_file", "input": {"path": "a"}}
            ]),
        },
        AnthropicMessage {
            role: "user".into(),
            content: serde_json::json!([
                {"type": "tool_result", "tool_use_id": "toolu_h1", "content": "ok"},
                {"type": "text", "text": "now just answer, no tools"}
            ]),
        },
    ];

    let result = convert_request_with_options(&req, ConverterOptions::default()).unwrap();
    let tools = &result
        .conversation_state
        .current_message
        .user_input_message
        .user_input_message_context
        .tools;
    // 现状：none 仍然暴露了一个可调用的占位工具
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool_specification.description, "Tool used in conversation history");
}

#[test]
fn audit_p15_disable_parallel_tool_use_is_ignored() {
    let with_flag = base_tool_choice_request(
        serde_json::json!({"type": "auto", "disable_parallel_tool_use": true}),
    );
    let without = base_tool_choice_request(serde_json::json!({"type": "auto"}));
    let a = convert_request_with_options(&with_flag, ConverterOptions::default()).unwrap();
    let b = convert_request_with_options(&without, ConverterOptions::default()).unwrap();
    // 现状：history（含 tool_choice 前缀）与当前工具列表完全一致，没有任何约束或提示
    assert_eq!(
        serde_json::to_value(&a.conversation_state.history).unwrap(),
        serde_json::to_value(&b.conversation_state.history).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&a.conversation_state.current_message).unwrap(),
        serde_json::to_value(&b.conversation_state.current_message).unwrap()
    );
}
```

注意：这里只比较 `history` 和 `current_message`，因为 `conversation_id` / `agent_continuation_id` 可能是随机的（`src/anthropic/converter.rs:451-453`）。

### 端到端复现

1. `tool_choice={"type":"tool","name":"get_weather"}`，用户消息是 "你好"，非流式请求连续发 20 次，统计 `content` 里没有 tool_use 的次数。
2. `tool_choice={"type":"none"}`，历史里有 `Bash` 调用，用户消息是 "再跑一次上次的命令"，观察模型是否调用了 `Bash`（占位工具经反向映射后名字是 `Bash`，参数为空或随意）。
3. `tool_choice={"type":"auto","disable_parallel_tool_use":true}`，prompt 为 "同时读取 a.txt 和 b.txt"，观察是否返回 2 个 tool_use。
4. 在 strict profile 下重复第 1 步，确认没有前缀注入。

## 修复方案

### 候选方案

| 方案 | 做法 | 评价 |
| --- | --- | --- |
| A. 响应侧校验 + 有界修正（推荐） | 把 `ToolChoiceDirective` 和 `disable_parallel` 传到响应侧，按下面的规则执行 | 能真正保证语义，但要处理流式的 "已经发出去的内容无法撤回" |
| B. 只加强提示词 | 把 `any` 的措辞改成无条件的 MUST；给 `disable_parallel` 加提示 | 成本低，但仍然没有保证 |
| C. 本地拒绝 | 不支持的组合直接 400 | 破坏兼容性，不可取 |

### 推荐方案

A 为主，同时做 B 里低成本的部分：

1. **请求侧：**
   - `parse_tool_choice` 解析 `disable_parallel_tool_use`，输出到 `ConversionResult.tool_choice_contract`（directive + disable_parallel + Kiro 侧允许的工具名集合）。
   - `none`：占位工具保留（满足 Kiro 的约束），但把它们记录为 "不可调用"。
   - `any` 的提示去掉 "when a tool can satisfy the request" 条件。`disable_parallel` 追加一句 "Call at most one tool in this turn"。
2. **响应侧，非流式（可以完全执行）：**
   - `none`：丢弃所有 tool_use（如果 `stop_reason` 是 `tool_use`，改成 `end_turn`）。只剩空内容时算作一次可重试的失败。
   - `any`/`tool`：没有 tool_use，或者调用的不是指定工具时，在 `inference_attempt_budget` 允许的范围内重试一次。仍然不满足时，返回原结果，同时写 warning 和 usage 标记，不伪造 tool_use。
   - `disable_parallel`：只保留第一个 tool_use。
3. **响应侧，流式（只能约束 "还没发出去" 的部分）：**
   - `disable_parallel`：第一个 tool_use 的 `content_block_stop` 之后，后续 `toolUseEvent` 不再发给下游（它们的 id 从来没到达客户端，所以不会有配对问题）。`stop_reason` 仍为 `tool_use`。
   - `none`：遇到 tool_use 时不发 `content_block_start`，丢弃对应的 input 增量，最后 `stop_reason` 改成 `end_turn`。
   - `any`/`tool`：流式没法无损重试。只做观测（warning 和 usage 字段 `tool_choice_violation=true`）。在下游还没 commit 之前（没有任何 content block 发出）可以考虑复用 precommit retry 机制，但只作为后续选项。
4. 可观测性：usage 记录增加 `tool_choice_mode` 和 `tool_choice_violation`，先上线观测，量化违规率之后再决定是否开启重试。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)，只使用 `127.0.0.1:19023` 指定测试实例和隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，Cargo 通过 `feature/tests/run-cargo-scoped.sh` 运行，不得影响本机正在运行的 Claude Code CLI 与服务。

- 单测：上面两个 `audit_p15_*` 测试改成修复后的期望：`none` 时占位工具被标记为不可调用；`disable_parallel` 进入 contract。
- 流式单测（`src/anthropic/stream.rs` 的 `mod tests`，参照 `stream_suppresses_continue_transcript_and_keeps_structured_tool_boundary`，`src/anthropic/stream.rs:4221`）：
  - `disable_parallel` 时两个 `ToolUseEvent`，只输出 1 个 tool_use 块，`stop_reason=tool_use`；
  - `none` 时一个 `ToolUseEvent`，没有 tool_use 块，`stop_reason=end_turn`，文本正常输出。
- 非流式 handler 夹具（参照 `CompleteToolWithoutStatus`）：`tool_choice=tool` 且上游只回文本时，重试 1 次，`upstream.hits()==2`。
- 回归：`test_tool_choice_none_omits_current_tools`、`test_tool_choice_named_tool_filters_current_tools`、`operator_prompt_master_off_preserves_structured_tool_filtering`、`named_tool_choice_*` 系列（`src/anthropic/converter.rs:1599`、`src/anthropic/converter.rs:3988-4220`，含 `test_anthropic_strict_filters_tool_choice_without_prompt_steering` 于 `src/anthropic/converter.rs:4200`）。
- 端到端：上面第 1 到 3 步各跑 20 次，统计修复前后的违规率。

## 兼容性与风险

- `none` 时丢弃 tool_use 会让模型那一轮 "说到一半"。但官方语义本来就不允许调用工具，这比让客户端执行一个空参数工具要安全。
- 非流式重试会增加延迟和上游消耗，必须计入 attempt budget（参见 retry 放大相关专题），并且默认只重试 1 次。
- 流式截断多余的 tool_use 时，被截掉的调用已经消耗了 output token，usage 仍按上游计量。
- 和 [prompt-policy 专题](../prompt-policy-tool-choice-and-count-tokens.md) 的分层一致：响应侧执行属于 "结构化语义"，受 `bodyConversion.toolChoiceSteering` 控制，不受 prompt 总开关影响。
- 回滚：contract 传递和响应侧执行放在一个独立开关后面。
