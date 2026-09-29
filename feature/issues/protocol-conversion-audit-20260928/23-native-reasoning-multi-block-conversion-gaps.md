# P23 原生 reasoning 多 block 互转的剩余缺口（P22 最小修复之外）

Status: partially-mitigated (old-history cases by pre-conversion shaping, working tree) / protected-turn + stream signature gaps open
Severity: Medium
Area: request + stream
Discovered: 2026-09-28，P22 分析过程中确认；为保持 P22 最小范围，从 P22 拆出

## 问题与影响

> 2026-09-29 更新（第 3 版，工作树内，见 [00 修复计划](00-current-protocol-fix-plan.md)）：本地路径在默认配置下，所有请求都会在转换前丢弃非受保护的历史 thinking。因此第 1 点和第 2 点里凡是发生在**旧 assistant** 上的情况，都已在转换前被消除，真实上游 T12、T13 均返回 200。仍然存在的只剩下面三处：一是冲突发生在**受保护的当前工具续写 assistant** 内部；二是配置关闭时，第 2 点的混用文案不会触发 fallback；三是第 3 点的流式签名问题。

项目定位：kiro.rs 基于 Kiro 对外提供 Anthropic 协议（Claude Code 协议）接口。客户端（如 Claude Code CLI）发来 Claude Code 协议请求，代理把它转换成 Kiro 协议，调度给最终上游 Kiro。Kiro 的响应再转换回 Claude Code 协议，返回给客户端。thinking 和签名是这套双向转换的一部分：签名始终是 Kiro 原生签名，代理负责原样转换和透传，保证 Claude Code CLI 回放历史时 Kiro 能校验通过。

[P22](22-consecutive-assistant-native-reasoning-merge-rejected.md) 的最小修复（工作树中的 fallback 实现）是：原始转换一旦命中 `consecutive ... merged losslessly` 或 `multiple or mixed native reasoning blocks` 这两条错误，就对副本强制丢弃**未受保护**的历史 thinking，然后重新转换。它能覆盖"相邻旧 assistant 各带 reasoning"和"旧 assistant 单条内有多个 reasoning"这两种情况。以下三个问题已在源码中确认存在，并且在 P22 fallback 之后仍然存在：

1. **单条 assistant 内有多个原生 reasoning block 时，本地转换直接 400**（请求方向）。
   - Claude Code 协议允许一条 assistant 消息里有多个 `thinking` / `redacted_thinking` block。
   - Kiro 的 `reasoningContent` 每条只能放一个值。转换器因此报错：`assistant history contains multiple or mixed native reasoning blocks; Kiro accepts one reasoningContent union value per assistant message`。
   - 如果多个 block 出现在旧 assistant 上，P22 fallback 已经能处理。
   - 残留场景：多个 block 出现在受保护的当前工具续写 assistant 上（该 assistant 的 thinking 不会被丢弃）。fallback 之后重新转换仍然失败，返回原 400。
2. **同一 assistant 内签名 thinking 与无签名 thinking 混用时，本地转换直接 400**（请求方向）。报错文案：`assistant history mixes native signed/redacted reasoning with unsigned thinking and cannot be represented losslessly`。
   - P22 fallback 的触发列表里**没有这条文案**。即使混用只发生在本来可以省略的旧 assistant 上，也不会触发 fallback，请求直接 400。
3. **同一次响应里出现第二段原生 reasoning 时，第二个 thinking block 缺少 `signature_delta`**（响应方向，流式）。
   - 客户端拿到的是一个没有签名的 thinking block。
   - 下一轮回放时，它会被当成无签名 thinking。如果同一条消息里还有第一段的签名 thinking，就会触发第 2 点的混用 400。
   - 这是请求方向第 1、2 点的一个直接来源。

第 4 点（配置边界）已关闭：P22 fallback 会强制开启 `enabled` 和 `discardHistoricalThinking`，不再依赖运行配置。回归测试 `local_reasoning_fallback_ignores_payload_shaping_disabled_for_five_rounds` 覆盖了这一点。第 4 点的历史记录保留在下文。

用户可见影响：Claude Code CLI 收到本地 400（`stage=handler_preflight`、`reason=local_body_prepare`、`category=unsupported_content`）。同一段历史每次都会原样回放，所以会话会一直卡住。

## 官方协议对照

- Claude Code 协议对接口的要求：
  - assistant 内容中可以有多个 `thinking` / `redacted_thinking` block，每个签名 thinking 都带自己的 `signature`。
  - 流式输出时，每个 thinking block 在 `content_block_stop` 之前，都要通过 `signature_delta` 发出自己的签名。
  - 在工具续写中，最后一条 assistant 里 tool_use 之前的 thinking 必须原样回传。
  - 旧 turn 的 thinking 可以省略。
- Kiro 协议对上游请求的要求（仓库既有结论，见 `docs/analysis/official-claude-code-kiro-protocol-audit-20260928.md`「多块 thinking」一节）：
  - `assistantResponseMessage.reasoningContent` 只能是单个 union：一个 `reasoningText{text, signature}`，或者一个 `redactedContent`。
  - 是否支持数组形式或位置化的 reasoning，目前没有抓包证据。
- 结论：两侧结构不对称。Claude Code 协议允许多个，Kiro 协议只能放一个。转换层需要明确的取舍规则，现在的做法是整请求拒绝。

## 源码链与根因

### 1. 单条 assistant 多个原生 reasoning（请求方向）

- `src/anthropic/converter/history.rs:268-313`：每遇到一个 `redacted_thinking`，或者一个带非空 signature 的 `thinking`，就调用一次 `set_native_reasoning_content`。
- `src/anthropic/converter/history.rs:409-421`：`set_native_reasoning_content` 在 `current.is_some()` 时直接返回 `UnsupportedContent`。
- 根因：Kiro 单 union 这个限制被实现成了"整请求拒绝"。转换器没有"保留哪一个"的规则。

### 2. 签名与无签名混用（请求方向）

- `src/anthropic/converter/history.rs:300-302`：无签名的 thinking 被累积到 `thinking_content`，稍后以 `<thinking>` 文本形式放进 content。
- `src/anthropic/converter/history.rs:371-376`：只要同时存在 `native_reasoning_content` 和非空的 `thinking_content`，就报错。
- 根因同上。无签名 thinking 本来就无法作为 Kiro 原生 reasoning 回放，但转换器没有把它当成"可省略内容"处理。

### 3. 第二段原生 reasoning 缺失签名（响应方向，流式）

- `src/anthropic/stream.rs:2661-2740` 的 `process_reasoning_content`：
  - 每个 reasoning 事件都会把 `native_reasoning_finalized` 置为 false（2670 行）。
  - 如果前一段已经关闭，这里就等于开启了新的一段。
  - 新签名写入 `native_reasoning_signature`（2713 行）。
  - `native_reasoning_signature_sent` **只在 redacted 分支里重置**（2676 行），普通签名分支不重置。
- `src/anthropic/stream.rs:2642-2643`：收到可见正文时，调用 `close_native_reasoning_block` 关闭当前 thinking block。
- `src/anthropic/stream.rs:3359-3370` 的 `take_native_signature_delta_event`：只要 `native_reasoning_signature_sent == true` 就返回 None。
- 实际时序：
  1. reasoning(s1)
  2. text：关闭第一个 thinking，发出 s1，此时 `sent=true`
  3. reasoning(s2)：`finalized=false`，但 `sent` 仍为 true
  4. text 或结束：关闭第二个 thinking。因为 `sent=true`，**不会发出 s2**
- 结果：客户端收到的第二个 thinking block 没有签名。
- 未确认的部分：Kiro 在真实流量中是否会在一次响应里交错发送多段 reasoning。这一点还没有抓包样本，但代码路径是确定存在的。
- 非流式路径（`src/anthropic/handlers.rs:9742-9765`）是按段构造 thinking block 并附带各自签名的，本次没有发现同样的问题。多段场景下它的行为没有逐行验证。

### 4. shaping 关闭时 P22 仍然出现（已由 P22 fallback 关闭，仅保留记录）

- P22 的修复位于 `src/anthropic/handlers/local_body_pipeline.rs` 的 `prepare_with_plan`，只在 `plan.payload_guard.config.shaping.enabled` 为真，且 `discardHistoricalThinking` 为真时生效。
- 关闭这两个配置中的任意一个，相邻旧 assistant 各带 reasoning 的请求就会重新走到 `history.rs:485` 的报错。

## 复现

### 最小复现（单测）

请求方向，放在 `src/anthropic/converter.rs` 的 tests 模块中。已有同类测试 `multiple_or_mixed_native_reasoning_blocks_are_rejected_for_five_rounds`（`converter.rs:5724`），它断言的是当前的报错行为：

```rust
#[test]
fn single_assistant_with_two_native_reasoning_blocks_is_rejected_today() {
    let message = super::super::types::Message {
        role: "assistant".to_string(),
        content: serde_json::json!([
            {"type": "thinking", "thinking": "t1", "signature": "sig-1"},
            {"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"file_path": "/a"}},
            {"type": "thinking", "thinking": "t2", "signature": "sig-2"},
            {"type": "tool_use", "id": "toolu_2", "name": "Read", "input": {"file_path": "/b"}}
        ]),
    };
    let error = convert_assistant_message(&message, &mut HashMap::new(), ConverterOptions::default())
        .expect_err("current behavior: rejected");
    assert!(error.to_string().contains("multiple or mixed native reasoning"));
    // 修复后应改为：Ok，且 reasoning_content 按选定规则只保留一个，两个 tool_use 都保留
}

#[test]
fn signed_and_unsigned_thinking_in_one_assistant_is_rejected_today() {
    let message = super::super::types::Message {
        role: "assistant".to_string(),
        content: serde_json::json!([
            {"type": "thinking", "thinking": "signed", "signature": "sig-1"},
            {"type": "thinking", "thinking": "unsigned"},
            {"type": "text", "text": "answer"}
        ]),
    };
    let error = convert_assistant_message(&message, &mut HashMap::new(), ConverterOptions::default())
        .expect_err("current behavior: rejected");
    assert!(error.to_string().contains("losslessly"));
}
```

响应方向，放在 `src/anthropic/stream.rs` 的 tests 模块中，风格参照 `test_native_reasoning_content_uses_cumulative_deltas`（`stream.rs:4715`）：

```rust
#[test]
fn second_native_reasoning_segment_emits_its_own_signature() {
    use crate::kiro::model::events::ReasoningContentEvent;

    let mut ctx = StreamContext::new_with_thinking("test-model", 1, true, HashMap::new());
    let _ = ctx.generate_initial_events();
    let mut events = Vec::new();
    events.extend(ctx.process_kiro_event(&Event::ReasoningContent(ReasoningContentEvent {
        text: "first".to_string(), signature: Some("sig-1".to_string()), redacted_content: None,
    })));
    events.extend(ctx.process_assistant_response("visible one"));
    events.extend(ctx.process_kiro_event(&Event::ReasoningContent(ReasoningContentEvent {
        text: "second".to_string(), signature: Some("sig-2".to_string()), redacted_content: None,
    })));
    events.extend(ctx.process_assistant_response("visible two"));
    events.extend(ctx.generate_final_events());

    let signatures: Vec<_> = events.iter()
        .filter(|e| e.event == "content_block_delta" && e.data["delta"]["type"] == "signature_delta")
        .map(|e| e.data["delta"]["signature"].as_str().unwrap_or_default().to_string())
        .collect();
    // 当前行为：["sig-1"]（缺 sig-2）；修复后：["sig-1", "sig-2"]
    assert_eq!(signatures, vec!["sig-1".to_string(), "sig-2".to_string()]);
}
```

配置边界（第 4 点）：已由 `local_reasoning_fallback_ignores_payload_shaping_disabled_for_five_rounds` 覆盖，无需新增测试。

### 端到端复现

1. 单条 assistant 多 block。下面的请求中，最后一条 user 是 tool_result，所以该 assistant 是受保护的工具续写，P22 的 shaping 不会丢弃它的 thinking：

```bash
curl -sS http://127.0.0.1:19023/cc/v1/messages \
  -H 'content-type: application/json' -H 'x-api-key: <key>' -H 'anthropic-version: 2023-06-01' \
  -d '{"model":"claude-sonnet-4-5","max_tokens":512,"thinking":{"type":"enabled","budget_tokens":1024},
       "tools":[{"name":"Read","description":"read","input_schema":{"type":"object","properties":{"file_path":{"type":"string"}}}}],
       "messages":[
         {"role":"user","content":"read a and b"},
         {"role":"assistant","content":[
           {"type":"thinking","thinking":"t1","signature":"sig-1"},
           {"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"/a"}},
           {"type":"thinking","thinking":"t2","signature":"sig-2"},
           {"type":"tool_use","id":"toolu_2","name":"Read","input":{"file_path":"/b"}}]},
         {"role":"user","content":[
           {"type":"tool_result","tool_use_id":"toolu_1","content":"A"},
           {"type":"tool_result","tool_use_id":"toolu_2","content":"B"}]}]}'
```

   预期（当前）：HTTP 400，报 `multiple or mixed native reasoning blocks`；usage 页面出现 `local_body_prepare` 采样记录。

2. 签名与无签名混用：把上面 assistant 中第二个 thinking 的 `signature` 去掉，预期 HTTP 400，报 `... cannot be represented losslessly`。

3. 流式第二段签名缺失：
   - 用 fake upstream 按顺序发送 `reasoningContentEvent(text, sig-1)`、`assistantResponseEvent`、`reasoningContentEvent(text, sig-2)`、`assistantResponseEvent`。
   - 抓取代理的 SSE 输出，确认只有一个 `signature_delta`。
   - 把这份输出按 Claude Code 的方式拼成下一轮 assistant 历史（一条消息，两个 thinking），再发给代理，就会得到第 2 点的 400。
   - 真实 Kiro 是否会产生这个序列，需要在开启 thinking 的长工具任务中抓包统计单次响应的 reasoning 段数。

## 修复方案

### 候选方案

- A. 转换层引入确定性的取舍规则（推荐）。当一个 Kiro assistant history 项需要容纳多个原生 reasoning 时，只保留一个，其余直接省略。省略时不转成文本，签名也不改写。无签名的 thinking 在同一条消息已有原生 reasoning 的情况下省略，不再整请求拒绝。
  - 保留哪一个：受保护的工具续写 assistant，保留最后一个 tool_use 之前最近的原生 reasoning；其他 assistant 保留最后一个。
  - 需要用真实 Kiro 抓包验证这个规则能通过 Kiro 的签名校验。
- B. 扩展内部模型，把多个 reasoning 按位置发给 Kiro。前提是 Kiro 支持数组或位置化 reasoning。目前没有证据，暂不可行。
- C. 维持拒绝，只优化报错文案。会话仍会卡死，不采用。

### 推荐方案

1. 响应方向（第 3 点，改动最小，先做）：在 `stream.rs` 的 `process_reasoning_content` 中，识别"上一段已关闭、新一段开始"（进入时 `native_reasoning_finalized == true`，且不是 redacted 分支）。此时重置 `native_reasoning_signature_sent = false`，并清空上一段的 `native_reasoning_signature` 和 `native_reasoning_content`，保证每个 thinking block 发出自己的 `signature_delta`。
2. 请求方向（第 1、2 点）：按方案 A 修改 `history.rs` 中 `set_native_reasoning_content` 的冲突处理，以及 371-376 行的混用判断。
   - 函数需要额外知道"是否是受保护的工具续写 assistant"，判定逻辑与 `src/anthropic/payload_guard.rs:1709` 的 `active_anthropic_tool_turn_assistant_index` 保持一致。
   - 省略的数量写入 `ProxyWarnings`，便于观测。
   - 这一步放在开关之后，默认开启前先用真实 Kiro 验证。
3. 第 4 点：已由 P22 fallback 关闭，无需额外改动。
4. 第 2 点的最小改动选项：把 `cannot be represented losslessly` 这条文案加入 P22 的 fallback 触发列表（`should_retry_local_conversion_after_reasoning_shaping`）。这样旧 assistant 上的混用会被 shaping 去掉，与 P22 同等处理；受保护 assistant 上的混用仍然走方案 A。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)，只使用 `127.0.0.1:19023` 指定测试实例和隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，Cargo 通过 `feature/tests/run-cargo-scoped.sh` 运行，不得影响本机正在运行的 Claude Code CLI 与服务。

- 单测：
  - 上面三个草稿，修复后按注释翻转断言。
  - 保留 `one_native_reasoning_survives_consecutive_assistant_merge`（`converter.rs:5791`）和 `test_native_reasoning_content_uses_cumulative_deltas`，二者必须继续通过。
  - 新增：redacted 与签名 thinking 同处一条消息时，按规则保留一个，且 `redacted` 数据不被修改。
- 回归：`cargo test --bin kiro-rs reasoning`、`cargo test --bin kiro-rs thinking`、`cargo test --bin kiro-rs payload_guard`。
- 端到端：
  - 上面 curl 1 和 2 返回 200，且上游 body 中 `reasoningContent` 只出现一次，签名与保留的那个 block 完全一致。
  - 真实 Claude Code CLI 开启 thinking，执行 20 轮以上的多工具任务，没有 `local_body_prepare` / `unsupported_content` 400，也没有 Kiro 签名校验 400。
- 生产观察：usage 页面中 `multiple or mixed native reasoning` 和 `cannot be represented losslessly` 这两类 diagnostic 的采样记录归零。

## 兼容性与风险

- 第 3 点只影响"一次响应多段 reasoning"的场景，单段场景行为不变。风险低。
- 第 1、2 点会把"拒绝"改为"有规则地省略"。如果保留规则与 Kiro 的签名校验逻辑不一致，拒绝会从本地 400 变成上游 400。所以必须先有真实 Kiro 抓包结论，再默认开启；开关关闭即可回滚到当前行为。
- 省略的是 Kiro 单 union 无法承载的额外 block。保留的 block 与签名原样透传，互转链路中的签名来源不变。
- 性能影响可以忽略，只在冲突分支上多一次选择。
- 未验证项：
  - Kiro 真实流量中单次响应的 reasoning 段数。
  - Claude Code CLI 回放时是否把同一响应的多个 block 放在同一条 assistant 消息中。
  - 取舍规则能否通过 Kiro 的校验。
