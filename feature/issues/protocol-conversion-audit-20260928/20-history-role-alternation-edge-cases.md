# P20 历史角色交替的边界情况：首条 assistant、合成 system 对后连续 assistant、伪造 "OK"、prefill 静默截断、多 text 块无分隔

Status: partially-fixed-in-be5ee15 (leading assistant kept; synthetic OK still open)
Severity: Low
Area: request
Discovered: 2026-09-28 协议互转审计
Verified-against: a4227c1 (2026-09-29)

> 2026-09-29 工作树更新（尚未提交，见 [00 修复计划：2026-09-29 方案复核](00-current-protocol-fix-plan.md)）：本地路径已改为所有请求在转换前执行历史 thinking 丢弃。条件是 `payloadGuardEnabled`、`payloadShaping.enabled` 和 `discardHistoricalThinking` 同时为真，这也是默认配置。受保护的当前工具续写 assistant 保留 Kiro 原生签名。所以，本文中"本地首发保留全部历史签名 thinking"这一前提，在默认配置下不再成立，只有关闭上述任一配置时才成立。

## 问题与影响

Kiro `history` 期望 user/assistant 严格交替、以 user 开头（本项目 payload guard 的 `align_history_to_user` 和大量测试都以此为前提）。
`build_history`（`src/anthropic/converter/history.rs:27-184`）在以下边界会破坏或“修补”交替：

1. messages 以 assistant 开头、无 system 且无合成前缀：history 以 assistant 开头
   （`src/anthropic/converter/history.rs:132-171`，assistant 先进 `assistant_buffer`，随后原样 push）。
2. messages 以 assistant 开头、有 system（或有 thinking/tool_choice 合成前缀）：history 为
   `user(system)`、`assistant("I will follow these instructions.")`、`assistant(首条 assistant)`，出现连续 assistant
   （`src/anthropic/converter/history.rs:97-102`、`src/anthropic/converter/history.rs:113-119`、`src/anthropic/converter/history.rs:137-146`）。
3. 补充（审计未列）：当前消息之前紧邻还有一条 user（客户端发来连续 user），`build_history` 把前一条 user 放入历史并自动配一个
   伪造的 `assistant("OK")`（`src/anthropic/converter/history.rs:173-181`）。官方语义是连续同角色消息合并为一个 turn，
   代理却凭空插入了一轮 assistant 回复。
4. 末尾 assistant（prefill）被静默截断到最后一条 user（`src/anthropic/converter.rs:433-446`），只计 `warnings.prefill_dropped`。
5. 同一条 assistant 消息内多个 `text` 块直接拼接，无分隔符（`src/anthropic/converter/history.rs:315-322`，
   `text_content.push_str(&text)`）；而 user 侧多个 text 块用 `"\n"` 连接（`src/anthropic/converter/content.rs:93`），
   连续 assistant 消息之间用 `"\n\n"` 连接（`src/anthropic/converter/history.rs:498-533`）。三处口径不一致。

上游是否已修补（核对结论）：

- 情况 1：会被修补，但方式是删除。payload guard 在 `config.enabled` 时总会执行
  `align_history_to_user`（`src/anthropic/payload_guard.rs:537-539`），它把 history 开头所有 assistant 直接 drain 掉
  （`src/anthropic/payload_guard.rs:4494-4503`），若这些 assistant 带 tool_use，后续配对修复还会删掉对应 tool_result
  （测试 `guard_aligns_leading_assistant_and_repairs_result`，`src/anthropic/payload_guard.rs:7696-7716`）。
  交替恢复了，但首条 assistant 的内容静默丢失，只有 `aligned_leading_entries` 计数和 opt-in 的
  `payload-aligned-history` 头。`payloadGuardEnabled=false` 时 guard 在 `src/anthropic/payload_guard.rs:466-475` 提前返回，不做 align，
  以 assistant 开头的 history 原样发出。
- 情况 2：没有修补。`align_history_to_user` 只看前导 assistant，这里 history[0] 是 user；`remove_unpaired_tool_uses` 等修复只处理配对。
  converter.rs 的 prefill 预处理（`src/anthropic/converter.rs:433-446`）只处理末尾，不涉及开头。全仓没有合并“合成 assistant +
  真实 assistant”的逻辑。
- 情况 3/4/5：没有修补，按设计如此。

> 2026-09-29 代码核对（HEAD a4227c1）：本文引用的 `history.rs`、`content.rs`、`converter.rs`、`payload_guard.rs` 行号在 HEAD 上全部核对一致，5 种边界情况均仍存在。3a1306d 没有改动 converter 的角色交替逻辑，只在本地路径新增了一次转换重试：当转换报 "consecutive assistant messages contain multiple native reasoning blocks..." 或 "assistant history contains multiple or mixed native reasoning blocks..." 时，先用 `sanitize_anthropic_messages_for_external_forwarding`（`src/anthropic/payload_guard.rs:2143-2148`）按 `enabled=true, discardHistoricalThinking=true` 整形 Anthropic 消息，再转换一次（`src/anthropic/handlers/local_body_pipeline.rs:75-92`、`:186-214`）。这一重试不处理首条 assistant、伪造 "OK"、prefill 截断或多 text 块拼接。另外需要注意 payload 整形的实际口径：本地 Kiro 路径上，`discardHistoricalThinking` 只在 `guard_kiro_request` 的超限分支里生效（`src/anthropic/payload_guard.rs:582-584`，`size_limit_enabled && report.final_weight > max_weight && config.shaping.enabled`），常驻的 `apply_payload_safety_shaping`（`:2063-2078`）只丢超大历史图片。所以默认 `on_too_long` 模式下首次发送会保留历史中的 Kiro 原生签名 thinking；`align_history_to_user`（`:537-539`）删除前导 assistant 时，其 reasoning 也随之丢失。外部池路径则在 `shaping.enabled` 下通过 `apply_anthropic_payload_safety_shaping`（`:2080-2106`）直接丢弃历史 thinking（`discardHistoricalThinking` 默认 true）。

影响评估（为什么是 Low）：Claude Code 正常会话总是 user 开头、严格交替、无 prefill；上述情况主要出现在 SDK/第三方客户端、
手工构造请求、或客户端 compact 后首条变成 assistant 摘要的场景（经验推断，未抓包）。Kiro 对连续 assistant 是否 400 没有本地证据。

## 官方协议对照

> 方向约定：kiro.rs 基于 Kiro 对外提供 Claude Code（Anthropic）协议接口。客户端（如 Claude Code CLI）发出 Claude Code 协议请求，经转换后变为 Kiro 协议，调度到最终上游 Kiro；Kiro 的响应再转换回 Claude Code 协议返回给客户端。下文引用的"Anthropic 文档/官方"一律指 **Claude Code 协议规范**，即接口侧必须满足的行为；"Kiro 侧"指上游 Kiro 协议的要求。Anthropic 官方 API 不是本项目的上游。thinking 签名均为 Kiro 原生签名，在转换中原样透传。

- 已验证事实（Anthropic 文档）：连续的同角色消息会被合并为一个 turn；最后一条是 assistant 时视为 prefill，模型从该文本继续生成。
- 需核对（本次未重新请求官方 API）：首条消息为 assistant 是否被官方接受。早期官方要求首条必须是 user，后续文档有放宽的说法。
  本文不以此为修复依据，只要求“代理不静默删内容、不伪造回合”。
- 已知（代码注释与官方公告，未在本次重新验证）：部分新模型不再支持 assistant prefill，官方会返回 400；旧模型支持并续写。
  代理静默丢弃意味着：对支持 prefill 的模型，官方会从 prefill 续写，而这里不会，且响应不以 prefill 文本开头的约定也被打破。
- 多 text 块：官方把同一消息的多个 text 块作为独立块渲染，块与块之间的边界对模型可见；代理无分隔直接拼接可能把
  `"...end."` 和 `"Next..."` 粘成 `"...end.Next..."`（这是推断，官方内部渲染细节未公开）。
- Kiro 侧：history 以 user 开头、交替出现是本项目长期假设，未见官方文档；是否拒绝连续 assistant 未验证。

## 源码链与根因

```text
convert_request_with_model_id (converter.rs:417)
  433-446  末尾非 user -> 截断到最后一条 user（prefill 丢弃）
  build_history (history.rs:27)
    64-121  system/合成前缀 -> [user(system), assistant("I will follow these instructions.")]
    132-159 按角色累积：遇到 user 刷出 assistant_buffer，遇到 assistant 刷出 user_buffer
            —— 没有检查“上一个已 push 的是不是同角色”
    161-171 末尾 assistant_buffer -> push
    173-181 末尾 user_buffer -> push + 伪造 assistant("OK")
guard_kiro_request
  537-539 align_history_to_user -> 删除前导 assistant
```

根因：交替不变量没有在 converter 内集中保证，而是依赖“输入本来就是交替的”+ guard 的删除式修补；
合成 system 对与真实消息之间也没有衔接检查。

## 复现

### 最小复现（单测）

放入 `src/anthropic/converter.rs` 的 `mod tests`：

```rust
fn req_with(system: Option<Vec<SystemMessage>>, msgs: Vec<(&str, serde_json::Value)>) -> MessagesRequest {
    MessagesRequest {
        model: "claude-sonnet-4".to_string(),
        max_tokens: 1024,
        messages: msgs.into_iter().map(|(role, content)| super::super::types::Message {
            role: role.to_string(), content,
        }).collect(),
        stream: false, system, tools: None, tool_choice: None,
        thinking: None, output_config: None, metadata: None,
    }
}

#[test]
fn leading_assistant_with_system_does_not_create_consecutive_assistants() {
    let req = req_with(
        Some(vec![SystemMessage { text: "rules".to_string(), cache_control: None }]),
        vec![("assistant", serde_json::json!("summary of earlier work")), ("user", serde_json::json!("continue"))],
    );
    let history = convert_request(&req).unwrap().conversation_state.history;
    for pair in history.windows(2) {
        assert!(
            !matches!(pair, [Message::Assistant(_), Message::Assistant(_)]),
            "consecutive assistant in history" // 当前实现失败
        );
    }
    // 首条 assistant 内容不应丢失
    assert!(serde_json::to_string(&history).unwrap().contains("summary of earlier work"));
}

#[test]
fn consecutive_user_before_current_is_merged_not_answered_with_fake_ok() {
    let req = req_with(None, vec![("user", serde_json::json!("part 1")), ("user", serde_json::json!("part 2"))]);
    let result = convert_request(&req).unwrap();
    let body = serde_json::to_string(&result.conversation_state).unwrap();
    assert!(!body.contains("\"content\":\"OK\""), "fabricated assistant OK"); // 当前实现失败
    assert!(result.conversation_state.current_message.user_input_message.content.contains("part 1"));
}

#[test]
fn assistant_text_blocks_keep_a_boundary() {
    let req = req_with(None, vec![
        ("user", serde_json::json!("hi")),
        ("assistant", serde_json::json!([{"type":"text","text":"First."},{"type":"text","text":"Second."}])),
        ("user", serde_json::json!("next")),
    ]);
    let body = serde_json::to_string(&convert_request(&req).unwrap().conversation_state).unwrap();
    assert!(!body.contains("First.Second.")); // 当前实现失败
}
```

无 system 的首条 assistant 删除行为已有测试固化（`src/anthropic/payload_guard.rs:7696`），修复时需要同步调整预期。

### 端到端复现

```bash
curl -sS http://127.0.0.1:PORT/v1/messages -H 'x-api-key: <KEY>' -H 'anthropic-version: 2023-06-01' \
  -H 'content-type: application/json' -d '{
  "model":"claude-sonnet-4-5","max_tokens":128,
  "system":[{"type":"text","text":"You are terse."}],
  "messages":[
    {"role":"assistant","content":"Earlier I found the bug in parser.rs line 42."},
    {"role":"user","content":"Which file and line did you mention?"}]}'
```

观察请求体采样中 history 为 `user, assistant, assistant`，记录 Kiro 返回（补证“连续 assistant 是否被拒绝”）。
去掉 `system` 再发一次：history 首条 assistant 被 guard 删除，模型答不出 `parser.rs line 42`。

prefill：`messages` 末尾加 `{"role":"assistant","content":"{\"answer\":"}`，官方（支持 prefill 的模型）会续写 JSON；
代理返回的文本不以该前缀衔接，响应头（开启 `exposeProxyWarnings` 时）含 `prefill-dropped=1`。

## 修复方案

### 候选方案

A. 在 `build_history` 末尾增加一个集中式 `normalize_alternation(history)`：合并相邻同角色项、首项为 assistant 时前插合成 user。
B. 在累积阶段处理：push 前检查上一项角色，同角色则合并。
C. 维持现状，只补文档和 warning。

### 推荐方案

B（在源头处理，信息不丢），辅以对 prefill 的显式策略：

1. 首条 assistant：
   - 有合成 system 对时，把首条真实 assistant 的内容合并进合成 assistant（`"I will follow these instructions.\n\n" + 内容`，tool_uses 一并迁移），
     避免连续 assistant，也不丢内容。
   - 无合成对时，前插 `user(".")`（或 `"[conversation continues]"`），而不是让 guard 删除。
   - 之后 guard 的 `align_history_to_user` 仅作为不变量兜底，理论上不再命中；命中时打 warn 日志。
2. 连续 user 在当前消息之前：不再伪造 `"OK"`。把紧邻当前消息的 user_buffer 与当前消息合并
   （文本用 `"\n"` 连接，images/tool_results 合并），与官方“合并同角色 turn”一致。需要注意 tool_results 合并后配对校验在
   `validate_tool_pairing` 之前完成。
3. prefill：新增配置 `prefillHandling = drop | reject`（默认 `drop` 保持现状）。`reject` 时返回 400
   `invalid_request_error`，文案说明上游不支持 assistant prefill。不建议伪造续写（例如把 prefill 塞进 user 指令），
   因为那会改变语义且不可靠。
4. assistant 多 text 块：非空块之间插入 `"\n\n"`，与连续 assistant 合并口径一致。注意 `ToolTranscriptSanitizer` 以流式方式处理，
   分隔符需要经过 `sanitizer.push("\n\n")`，与 `src/anthropic/converter/history.rs:528-530` 同样处理。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)，只使用 `127.0.0.1:19023` 指定测试实例和隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，Cargo 通过 `feature/tests/run-cargo-scoped.sh` 运行，不得影响本机正在运行的 Claude Code CLI 与服务。

- 上面 3 条单测转绿；`guard_aligns_leading_assistant_and_repairs_result` 保持（guard 兜底行为不变），新增 converter 级测试证明
  guard 不再需要删除。
- 回归：正常交替会话、Claude Code 真实会话的 history 序列化结果 byte-identical（新增逻辑只在边界触发）。
- 连续 assistant 合并与 native reasoning 的交互需与 [P22](22-consecutive-assistant-native-reasoning-merge-rejected.md) 一致：
  合成 assistant 不带 reasoning，合并真实 assistant 的 reasoning 不应触发“多个 reasoning”错误。
  HEAD 上即使触发了该错误，本地路径也会走 3a1306d 的丢弃历史 thinking 重试；验收时应断言首次转换就成功、没有走这次重试，
  否则等于静默丢掉了首条 assistant 的 Kiro 原生签名 thinking。
- prefill `reject` 模式返回 400，`drop` 模式行为与现状一致。
- 补证：一次“连续 assistant”真实上游请求的返回码写入本文。

## 兼容性与风险

- 多 text 块插入分隔符会改变历史 assistant 文本，影响依赖精确前缀的 prompt cache（仅对含多 text 块的消息）；属于一次性缓存失效。
- 合并当前消息前的连续 user 会改变 current content，影响 current tool_result 占位判断，需要测试覆盖“前一条 user 只含 tool_result”。
- 首条 assistant 合并进合成 assistant 后，如果首条 assistant 带 tool_use，其 tool_result 在下一条 user，配对关系保持；
  需要测试确认 `validate_tool_pairing` 仍通过。
- `prefillHandling=reject` 若误设为默认，会影响少数依赖 prefill 的 SDK 用户，因此默认保持 `drop`。

## 修复结果与验证（2026-09-29）

- 修复（`be5ee15`）：对话以 assistant 开头时，在它前面插入占位 user `.`，保留 assistant 的内容，并保证 history 以 user 开头、交替排列（有无 system 两种情况都覆盖）。伪造的 `"OK"`、prefill 截断、多个 text 块直接拼接，这几项仍未处理。
- 单测：`leading_assistant_turn_is_kept_behind_placeholder_user_for_five_rounds`。
- 真实上游：以 assistant 开头的对话，在有 system 和无 system 两种情况下都返回 200，模型能复述其中的口令 `OSPREY`。
- 验证环境：真实上游验证使用 `127.0.0.1:19023` 指定测试实例和隔离的 CLI `HOME`/`CLAUDE_CONFIG_DIR`，未改动本机正在运行的 Claude Code CLI 环境。证据见 `tmp/thinking-budget-local/fix-evidence-20260929/`。
