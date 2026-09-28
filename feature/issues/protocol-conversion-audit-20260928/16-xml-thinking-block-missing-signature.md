# P16 XML 提取的 thinking 块没有 signature，跨上游回放不可用

Status: open / documented / not-fixed
Severity: Low
Area: response
Discovered: 2026-09-28 协议互转审计

## 问题与影响

XML 提取出来的 thinking 块（流式和非流式）既没有 `signature` 字段，也不发 `signature_delta`：

- 流式 `content_block_start` 为 `{"type":"thinking","thinking":""}`，之后只有 `thinking_delta`，关闭前没有 `signature_delta`；
- 非流式 content 为 `{"type":"thinking","thinking":"…"}`，没有 `signature` 键。

原生 `reasoningContentEvent` 带签名时，代理会在 `content_block_stop` 前发 `signature_delta`；非流式也会带 `signature`。

对 Claude Code 的影响：

- 继续使用本代理时基本无影响：Claude Code 把不带签名的 thinking 块回传，代理把它转成 `<thinking>…</thinking>` 文本放回历史（见下文），请求能正常发出。
- 同一会话切换到官方 Anthropic API（改 `ANTHROPIC_BASE_URL`、`--resume` 到另一上游、或经过同时支持两种上游的网关）时，
  历史中无签名的 thinking 块会被官方拒绝（400，`thinking.signature` 缺失或无效）。这是跨上游场景，不是本代理的主路径。
- 类型层面：Anthropic SDK 的 `ThinkingBlock` 类型把 `signature` 定义为必填字符串。缺失时 SDK 通常不会报错（按原样累积 JSON），
  但依赖该字段的客户端代码可能拿到 `undefined`。Claude Code 是否对 `signature === undefined` 有特殊分支，未验证。

纠正审计中的定位：审计说"native path emits signature (~3360-3430)"准确；另外需要补充，**原生 reasoning 没有签名时**
（`reasoningContentEvent.signature` 为空）行为与 XML 路径相同，同样没有 `signature_delta`（`src/anthropic/stream.rs:3359-3371`）。

## 官方协议对照

> 方向约定：kiro.rs 基于 Kiro 对外提供 Claude Code（Anthropic）协议接口。客户端（如 Claude Code CLI）发出 Claude Code 协议请求，经转换后变为 Kiro 协议，调度到最终上游 Kiro；Kiro 的响应再转换回 Claude Code 协议返回给客户端。下文引用的"Anthropic 文档/官方"一律指 **Claude Code 协议规范**，即接口侧必须满足的行为；"Kiro 侧"指上游 Kiro 协议的要求。Anthropic 官方 API 不是本项目的上游。thinking 签名均为 Kiro 原生签名，在转换中原样透传。

已验证事实（Anthropic extended thinking 文档）：

- 非流式 thinking 块形如 `{"type":"thinking","thinking":"…","signature":"…"}`；
- 流式先 `content_block_start`（文档示例中 content_block 为 `{"type":"thinking","thinking":""}`），再若干 `thinking_delta`，
  最后在 `content_block_stop` 前发一个 `signature_delta`；
- 多轮 tool use 时，客户端必须把上一个 assistant 轮次的 thinking 块连同 signature 原样回传，签名不可修改；
- signature 是加密的不透明值，只能由官方生成，第三方无法伪造，伪造或空签名在官方 API 上都会校验失败。

本仓库既有约束（`../../../docs/analysis/thinking-signature-remediation-plan-20260728.md` 第 32 行）：
"对 signature / redacted data 的唯一合法操作是原样带回。禁止：伪造、占位、合并、重排、重算……"。

经验推断：官方真实流式响应的 `content_block_start` 可能带 `"signature": ""` 占位，文档示例未体现，未抓包验证。

## 源码链与根因

响应侧：

- 流式 XML 路径：`process_content_with_thinking` 在开始标签处创建块（`src/anthropic/stream.rs:2772-2786`），
  `content_block` 只有 `type` 和 `thinking`；结束时（`:2840-2853`、最终 flush `:3740-3749` / `:3767-3782`）只发空 `thinking_delta` 和 `content_block_stop`。
  没有签名来源：Kiro 以纯文本返回 `<thinking>`，不存在可用签名。
- 流式原生路径：`close_native_reasoning_block`（`src/anthropic/stream.rs:3373-3431`）→ `take_native_signature_delta_event`
  （`:3359-3371`）→ `create_signature_delta_event`（`:3345-3357`）。原生块的 `content_block_start` 同样没有 `signature` 字段（`:3415-3423`），
  测试 `thinking_start.data["content_block"]["signature"].is_null()` 显式断言了这一点（`src/anthropic/stream.rs:4762`）。
- 非流式：`append_non_stream_reasoning_and_text`（`src/anthropic/handlers.rs:9707-9813`）。原生分支有签名时写入 `signature`（`:9757-9762`）；
  XML 分支（`:9778-9801`）只写 `type` + `thinking`。

请求侧（回放）：

- `src/anthropic/converter/history.rs:287-302`：thinking 块有非空 `signature` → native `reasoningContent`；否则追加到 `thinking_content`。
- `src/anthropic/converter/history.rs:378-392`：`thinking_content` 以 `<thinking>{}</thinking>`（有 text 时后接 `\n\n`）拼入 assistant 文本。
  所以本代理自己产生的无签名 thinking，回放时退化成文本，不需要签名。
- `src/anthropic/converter/history.rs:371-376`：同一条 assistant 消息里既有签名/redacted reasoning 又有无签名 thinking 时，返回
  `ConversionError::UnsupportedContent("assistant history mixes native signed/redacted reasoning with unsigned thinking ...")`，
  handler 映射为 400 `invalid_request_error`（`src/anthropic/handlers.rs:5701`）。

混合的理论来源（未在线上确认）：一次流式响应先按 XML 打开了 thinking 块，之后上游又发 `reasoningContentEvent`。
`emit_assistant_response_content`（`src/anthropic/stream.rs:2639-2658`）在 `native_reasoning_seen` 后停止 XML 解析，
最终 flush 时直接清空 `thinking_buffer`（`:3716-3721`），XML 块和原生签名块可能同时出现在同一条 assistant 消息中；下一轮回放即触发上面的 400。
Claude Code 合并同一 message id 的多个 assistant 片段时也可能产生这种组合。

根因：XML thinking 是 Kiro 的文本约定，本身就没有签名。这不是可以"补"的缺陷，问题在于：

1. 响应形状与官方类型不一致（缺字段），客户端和跨上游回放无法区分"无签名"与"签名丢失"；
2. 混合组合在请求侧直接 400，没有降级路径。

## 复现

### 最小复现（单测）

放入 `src/anthropic/stream.rs` 的 `mod tests`（风格参照 `:4730-4782` 的原生签名测试）：

```rust
#[test]
fn test_xml_thinking_block_has_no_signature_today() {
    let mut ctx = StreamContext::new_with_thinking("test-model", 1, true, HashMap::new());
    let _ = ctx.generate_initial_events();
    let mut all_events = Vec::new();
    all_events.extend(ctx.process_assistant_response("<thinking>\nabc</thinking>\n\n正文"));
    all_events.extend(ctx.generate_final_events());

    let start = all_events
        .iter()
        .find(|e| e.event == "content_block_start" && e.data["content_block"]["type"] == "thinking")
        .expect("thinking start");
    assert!(start.data["content_block"].get("signature").is_none());
    assert!(all_events.iter().all(|e| e.data["delta"]["type"] != "signature_delta"));
}
```

请求侧混合组合（放入 `src/anthropic/converter` 的测试模块，按现有 history 测试的构造方式）：

```rust
let assistant = json!({
    "role": "assistant",
    "content": [
        {"type": "thinking", "thinking": "signed", "signature": "sig-opaque"},
        {"type": "thinking", "thinking": "unsigned"},
        {"type": "text", "text": "ok"}
    ]
});
// 期望当前实现返回 ConversionError::UnsupportedContent(... mixes native signed/redacted reasoning with unsigned thinking ...)
```

### 端到端复现

1. 非 strict profile，开启 thinking，通过代理完成一轮带工具调用的对话（上游返回 XML thinking）。
2. 保存 Claude Code 会话，把 `ANTHROPIC_BASE_URL` 改为官方 API 后 `claude --resume` 继续。
3. 官方返回 400，指向历史中该 thinking 块的 signature。

混合组合：直接 curl 发送上面的 assistant 历史到 `/v1/messages`，当前返回 400 `invalid_request_error`。

## 修复方案

### 候选方案

A. 保持现状（不输出 signature 字段），只写文档说明。零风险，类型不一致继续存在。

B. 输出空签名：流式 `content_block_start` 带 `"signature": ""`，非流式带 `"signature": ""`，**不发** `signature_delta`。
   形状与官方类型一致；空字符串不是伪造值，不承诺任何校验；回放时 `history.rs:294` 的非空过滤会把它当无签名处理，路径不变。
   跨上游回放仍然失败（官方拒绝空签名），这一点无法由代理解决。

C. 把 XML thinking 输出为 text 块（`<thinking>` 不提取）。跨上游回放不会失败，但 Claude Code 不再以 thinking 形式展示，也无法区分思考与正文。仅适合作为 strict profile 的现有行为（strict 已不提取 XML）。

D. 伪造或复用签名：违反仓库约束，官方也会校验失败，不采用。

E. 请求侧对混合组合降级：丢弃无签名 thinking 或将其转为文本并与签名块并存。丢弃会造成信息损失；
   转文本需要确认 Kiro 是否接受"reasoningContent + 文本中含 `<thinking>`"。未验证前不做。

### 推荐方案

B，并以配置开关保护；E 另开 issue 跟踪。

1. 流式：`src/anthropic/stream.rs:2779-2785` 的 `content_block` 改为 `{"type":"thinking","thinking":"","signature":""}`。
   原生路径 `:3415-3423` 是否同步改动需要单独评估：原生块随后会发真实 `signature_delta`，官方 SDK 用 delta 覆盖该字段，同步改动后形状更一致；
   现有断言 `src/anthropic/stream.rs:4762` 需同步调整。
2. 非流式：`src/anthropic/handlers.rs:9788-9791` 的块增加 `"signature": ""`；原生无签名分支（`:9753-9763`）同样补空字符串。
3. 不发送空 `signature_delta`（`take_native_signature_delta_event` 的非空判断保持不变）。
4. 配置：`compat_profile` 已控制 XML 提取（`src/anthropic/handlers/local_body_pipeline.rs:302`）。建议加一个布尔项
   `emit_empty_thinking_signature`（默认 true），出现客户端兼容问题时可关闭，恢复旧形状。
5. 文档：在用户文档中说明"经代理产生的 thinking 在官方 API 上不可回放；需要跨上游切换的用户使用 strict profile 或在切换前 `/clear`"。

## 测试与验收

单测：

- `test_xml_thinking_block_start_has_empty_signature`：`content_block_start.content_block.signature == ""`，且无 `signature_delta`；
- 非流式 XML thinking：content[0] 含 `"signature": ""`；
- 原生有签名：`signature_delta` 仍在 `content_block_stop` 之前，值为原始签名（回归，已有 `:4764-4782` 断言）；
- 原生无签名：与 XML 路径一致；
- 请求侧：`{"type":"thinking","thinking":"x","signature":""}` 回放为 `<thinking>x</thinking>` 文本（验证 `history.rs:294` 的空值过滤）；
- 请求侧：签名块 + `signature: ""` 块的混合组合仍返回原错误（行为不变，记录为已知限制）；
- 开关关闭时恢复旧形状。

需要更新的已有测试：`src/anthropic/stream.rs:4762` 的 `signature.is_null()` 断言（仅在原生路径同步改动时）；
`src/anthropic/handlers/tests.rs` 中对非流式 thinking 块做整体 JSON 相等比较的用例（按 `"type": "thinking"` 搜索后逐个确认）。

端到端验收：真实 Claude Code 在本代理上多轮 thinking + 工具调用正常；UI 仍显示 thinking；`--resume` 同一会话继续使用本代理无回归。

## 兼容性与风险

- 客户端若把 `signature: ""` 当作"有签名"并在其他逻辑里使用（例如按是否存在字段决定是否剥离 thinking），行为可能变化；这一点未对 Claude Code 源码验证，因此需要开关。
- 不解决跨上游回放：任何方案都无法让官方接受非官方生成的 thinking，文档必须写清楚。
- 性能：无影响。
- 回滚：关闭开关或回退两处 JSON 构造。
- 未验证项：官方流式 `content_block_start` 是否带 `signature: ""`；Claude Code 对缺失/空签名的处理分支；上游是否会在同一响应中同时出现 XML thinking 与原生 reasoning。
- 关联：[P02](02-thinking-end-tag-strict-double-newline.md)、[P09](09-xml-thinking-whitespace-loss-and-false-detection.md)（同一 XML thinking 管线）。
