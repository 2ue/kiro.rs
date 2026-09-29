# P09 XML thinking 探测丢失正文空白，且正文中的裸 `<thinking>` 会被误判为思考开始

Status: open / documented / not-fixed
Severity: Medium
Area: stream
Discovered: 2026-09-28 协议互转审计
Verified-against: a4227c1 (2026-09-29)

> 2026-09-29 代码核对（HEAD a4227c1）：3a1306d 未改动 `src/anthropic/stream.rs` 与 `converter/thinking.rs`，两个缺陷均仍存在。所引行号与 HEAD 一致，只给"边界 flush 另有两处"补了具体行号。

## 问题与影响

XML thinking 提取模式下，在尚未完成一次 thinking 提取之前（`!in_thinking_block && !thinking_extracted`），
流式解析器对每个文本 chunk 做 `<thinking>` / `<think>` 开始标签探测。两个缺陷：

1. 空白丢失：当 buffer 尾部可能是开始标签前缀（如 `<`、`<th`）时，前面的内容只有"不是纯空白"才会发出。
   chunk 为 `\n\n<`、`\n<`、`  <` 这类形态时，空白被直接丢弃。
   例：模型输出 `代码如下：` 与 `\n\n<div>…` 分两个 chunk 到达，下游看到的是 `代码如下：<div>…`，Markdown 段落/代码块结构被破坏。
2. 误判 thinking：`thinking_extracted` 只在**找到结束标签**时置为真（`src/anthropic/stream.rs:2838`，边界 flush 另有两处：tool_use 边界 `:3470`、最终 flush `:3758`；`:3720` 是原生 reasoning 收尾，与 XML 探测无关）。
   模型这一轮没有思考（adaptive 模式常见）时，整条回答都处于"可能开始 thinking"状态；
   正文后面任何一个前后不是 `` ` " ' \ `` 的 `<thinking>` 都会开启一个 thinking 块，之后的正文全部进入 thinking。
   典型触发：模型在围栏代码块里写 XML/提示词样例（```` ```xml\n<thinking>\n... ````），或者讨论本项目这类代理代码。
   后果与 [P02](02-thinking-end-tag-strict-double-newline.md) 相同：剩余回答隐藏在 thinking 里；若没有后续 tool_use，最终被补 `" "` 并报 `max_tokens`
   （只有在已存在 text 块时才不会报 max_tokens，因为 `has_non_thinking_blocks()` 为真；但剩余正文仍然丢在 thinking 里）。

需要纠正审计表述："`thinking_extracted` set only after first open tag (~2838)" 不准确。
`src/anthropic/stream.rs:2838` 是在**结束标签**命中后设置；开始标签命中时只设置 `in_thinking_block = true`（`:2766`）。
结论不变：从未完整思考过的响应，整个生命周期都在探测开始标签。

影响面：非 strict profile、thinking 开启、未收到原生 reasoning 事件的流式请求。缺陷 1 每次命中都会丢字符（通常是换行），缺陷 2 需要正文出现未被引用包裹的开始标签。

## 官方协议对照

> 方向约定：kiro.rs 基于 Kiro 对外提供 Claude Code（Anthropic）协议接口。客户端（如 Claude Code CLI）发出 Claude Code 协议请求，经转换后变为 Kiro 协议，调度到最终上游 Kiro；Kiro 的响应再转换回 Claude Code 协议返回给客户端。下文引用的"Anthropic 文档/官方"一律指 **Claude Code 协议规范**，即接口侧必须满足的行为；"Kiro 侧"指上游 Kiro 协议的要求。Anthropic 官方 API 不是本项目的上游。thinking 签名均为 Kiro 原生签名，在转换中原样透传。

已验证事实：

- 官方 Anthropic 响应中 thinking 是独立 content block，且只会出现在 assistant 内容的开头（interleaved thinking 场景下出现在 tool_result 之后的新一轮开头），不会出现在一段 text 的中间。
- 代理注入的输出策略要求 thinking "before any visible text or tool call"（`src/anthropic/converter/thinking.rs:11`）。
  因此"已输出可见文本之后再出现 `<thinking>`"不属于这个私有协议的合法形态。
- `text_delta` 必须原样保留模型输出的空白；Claude Code 渲染 Markdown 时依赖换行。

经验推断：adaptive 模式下，模型不思考时响应开头常是正文；思考时开头可能有 `\n\n` 再接 `<thinking>`（`:2756-2757` 的注释提到这一点）。

## 源码链与根因

```
process_assistant_response -> emit_assistant_response_content      src/anthropic/stream.rs:2639-2658
  -> process_content_with_thinking                                  src/anthropic/stream.rs:2743
     if !in_thinking_block && !thinking_extracted                   :2750
        find_real_thinking_start_tag_with_variant (任意位置)        :2752-2753, 实现 :362-390
        命中: before_thinking 纯空白时丢弃                           :2758-2761
        未命中: thinking_open_tag_partial_start                      :2792-2807, 实现 :234-254
```

缺陷 1（`src/anthropic/stream.rs:2792-2801`）：

```rust
if let Some(retain_start) = thinking_open_tag_partial_start(&self.thinking_buffer) {
    if retain_start > 0 {
        let safe_content = self.thinking_buffer[..retain_start].to_string();
        if !safe_content.trim().is_empty() {          // 纯空白 -> 不发出
            events.extend(self.create_text_delta_events(&safe_content));
        }
        self.thinking_buffer = self.thinking_buffer[retain_start..].to_string();  // 但仍被截掉
    }
}
```

`thinking_open_tag_partial_start("\n\n<")` 返回 2（`:234-254`：非全空白时，从尾部 10 字节内找第一个能成为开始标签前缀的位置），
于是 `"\n\n"` 被截掉且未发出。已有测试 `test_partial_thinking_tag_prefix_is_still_buffered`（`src/anthropic/stream.rs:6206`）
断言 `"\n\n<th"` 之后 `thinking_buffer == "<th"`，这正是把丢弃行为固化了。它的原始意图是"响应开头的 `\n\n<thinking>` 不产生空 text 块"，
这个意图应保留，但实现方式是丢弃而不是暂存。

对照：整段 buffer 全是空白时 `thinking_open_tag_partial_start` 返回 `Some(0)`，内容被保留（`:238-240`），所以单独的 `"\n\n"` chunk 不会丢；
只有"空白 + 标签前缀"同在一个 buffer 时才丢。

缺陷 2：开始标签搜索不限制位置（`find_real_thinking_start_tag_for`，`src/anthropic/stream.rs:362-378`，只做前后一个字符的引用防护）；
状态门槛 `!self.thinking_extracted` 在未思考的响应中恒为真。tool_use 边界的 flush（`src/anthropic/stream.rs:3497-3506`）也不设置
`thinking_extracted`，工具调用之后的文本仍在探测。

非流式同类问题：`extract_thinking_from_complete_text`（`src/anthropic/stream.rs:1134-1173`）同样在全文任意位置找第一个开始标签；
若正文代码块里恰有 `<thinking>…</thinking>\n\n`，会把代码中间一段抽成 thinking 并拼接前后文本。非流式不存在缺陷 1。

## 复现

### 最小复现（单测）

放入 `src/anthropic/stream.rs` 的 `mod tests`：

```rust
#[test]
fn test_whitespace_before_partial_tag_prefix_is_not_dropped() {
    let mut ctx = StreamContext::new_with_thinking("test-model", 1, true, HashMap::new());
    let _ = ctx.generate_initial_events();
    let mut all_events = Vec::new();
    all_events.extend(ctx.process_assistant_response("代码如下："));
    all_events.extend(ctx.process_assistant_response("\n\n<"));
    all_events.extend(ctx.process_assistant_response("div>hi</div>"));
    all_events.extend(ctx.generate_final_events());
    assert_eq!(collect_text_content(&all_events), "代码如下：\n\n<div>hi</div>");
    assert_eq!(collect_thinking_content(&all_events), "");
}

#[test]
fn test_unquoted_thinking_tag_inside_answer_is_not_treated_as_thinking() {
    let mut ctx = StreamContext::new_with_thinking("test-model", 1, true, HashMap::new());
    let _ = ctx.generate_initial_events();
    let mut all_events = Vec::new();
    all_events.extend(ctx.process_assistant_response("示例提示词：\n```xml\n"));
    all_events.extend(ctx.process_assistant_response("<thinking>\n先分析\n</thinking>\n```\n结束"));
    all_events.extend(ctx.generate_final_events());
    assert_eq!(
        collect_text_content(&all_events),
        "示例提示词：\n```xml\n<thinking>\n先分析\n</thinking>\n```\n结束"
    );
    assert_eq!(collect_thinking_content(&all_events), "");
    let delta = all_events.iter().find(|e| e.event == "message_delta").unwrap();
    assert_eq!(delta.data["delta"]["stop_reason"], "end_turn");
}

#[test]
fn test_leading_whitespace_before_thinking_is_still_suppressed() {
    // 保留原有意图：响应开头的 "\n\n<thinking>" 不产生空 text 块
    let mut ctx = StreamContext::new_with_thinking("test-model", 1, true, HashMap::new());
    let _ = ctx.generate_initial_events();
    let mut all_events = Vec::new();
    for chunk in ["\n\n<th", "inking>\nabc</thinking>\n\n正文"] {
        all_events.extend(ctx.process_assistant_response(chunk));
    }
    all_events.extend(ctx.generate_final_events());
    assert_eq!(collect_thinking_content(&all_events), "abc");
    assert_eq!(collect_text_content(&all_events), "正文");
}
```

当前实现下：第一个测试得到 `代码如下：<div>hi</div>`；第二个测试把 `\n先分析\n</thinking>\n```\n结束` 的大部分放进 thinking
（结束标签后是单换行，还叠加了 P02），text 只剩 `示例提示词：\n```xml\n`；第三个测试当前通过，作为回归保护。

### 端到端复现

fake upstream `assistantResponseEvent` 序列：

```
{"content":"下面是 system prompt 示例："}
{"content":"\n\n<"}
{"content":"thinking_mode>enabled</thinking_mode>\n"}
{"content":"<thinking>\n这里是示例\n</thinking>\n\n完毕"}
```

期望下游 text 原样包含全部四段；当前第二段的 `\n\n` 丢失，且第四段 `<thinking>` 开启 thinking 块，`这里是示例\n` 进入 thinking，text 中缺失该段。

真实 Claude Code：非 strict profile、开启 thinking（adaptive 更容易复现），请模型"给出一个包含 `<thinking>` 标签的提示词模板，放在 xml 代码块里"。

## 修复方案

### 候选方案

A. 只修空白：部分前缀暂存时，把前面的空白一起留在 buffer，最终确认不是标签时一并发出。

B. 限定探测窗口：只在"响应开头（仅允许前导空白）"探测 XML thinking 开始标签；一旦发出过任何非空白 text 或开始了 tool_use，就关闭探测。

C. 维护代码围栏状态，围栏内不探测。能减少误判，但开头窗口之外的误判仍在，复杂度更高。

### 推荐方案

A + B 一起做。B 与注入策略"thinking 位于任何可见文本和工具调用之前"一致，也覆盖代码围栏场景，C 不再需要。

1. `StreamContext` 新增 `xml_thinking_probe_closed: bool`，默认 `false`。满足任一条件时置 `true`：
   - 发出过非空白 text（在 `process_content_with_thinking` 的 text 发送分支判断）；
   - 开始 tool_use（`process_tool_use`，`src/anthropic/stream.rs:3495-3506` 附近）；
   - 已完成一次 thinking 提取（现有 `thinking_extracted` 逻辑保留）。
2. 状态门槛 `src/anthropic/stream.rs:2750` 改为 `!self.in_thinking_block && !self.thinking_extracted && !self.xml_thinking_probe_closed`；
   探测关闭后走现有 `else` 分支（`:2884-2891`）直接发 text。
3. 探测窗口内的开始标签必须满足：`thinking_buffer[..start_pos].trim().is_empty()`，即前面只有空白。
   否则（前面已有正文）视为普通文本，并关闭探测。
4. 空白暂存（修缺陷 1）：`thinking_open_tag_partial_start` 返回位置后，把 `retain_start` 向前扩展到连续空白的起点：

```rust
let mut retain_start = retain_start;
while let Some((idx, ch)) = self.thinking_buffer[..retain_start].char_indices().next_back() {
    if !ch.is_whitespace() { break; }
    retain_start = idx;
}
// [..retain_start] 含非空白 -> 发出并关闭探测；空白与前缀一起留在 buffer
```

   后续若确认是开始标签，沿用 `:2758-2761` 丢弃前导空白（保持原意图）；若确认不是，空白随文本一起发出。
   结合第 3 条，探测窗口内 buffer 的非前缀部分只可能是空白，所以实际只会发生在响应开头。
5. 非流式 `extract_thinking_from_complete_text`：同样要求开始标签前只有空白（`before.trim().is_empty()`），否则不提取。
   `:1164-1167` 已经在 `before` 为纯空白时丢弃它，改动后 `before` 只可能是空白。
6. tool_use 边界 flush（`src/anthropic/stream.rs:3497-3506`）逻辑不变，只额外设置 `xml_thinking_probe_closed = true`。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)，只使用 `127.0.0.1:19023` 指定测试实例和隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，Cargo 通过 `feature/tests/run-cargo-scoped.sh` 运行，不得影响本机正在运行的 Claude Code CLI 与服务。

新增单测：上面三个草稿，另加：

- `\n` 单换行、`  ` 空格、`\r\n` 在前缀前的变体；
- 多 chunk：`"文本"`、`"\n"`、`"\n<"`、`"t"`、`"able>"` → text 原样；
- tool_use 之后的文本包含裸 `<thinking>` → 仍是 text；
- `<think>` 短标签同组用例；
- 非流式：`"正文 <thinking>\nx\n</thinking>\n\n尾部"` → 不提取，原文作为 text。

需要更新的已有测试（`src/anthropic/stream.rs`）：

- `test_partial_thinking_tag_prefix_is_still_buffered`（`:6206`）：断言改为 `thinking_buffer == "\n\n<th"` 且未发出 text_delta。
- `test_short_initial_text_flushes_without_waiting_for_thinking_tag`（`:6185`）：保持通过；补充断言 `xml_thinking_probe_closed == true`。
- `test_find_real_thinking_start_tag_*`（`:6283` 起）：函数本身不改，保持通过。
- `test_tool_use_flushes_pending_thinking_buffer_text_before_tool_block`（`:6223`）：保持通过。

端到端验收：上面的 fake upstream 序列在 stream / non-stream 下 text 与上游原文逐字节一致；正常 `<thinking>` 开头的响应行为不变。

## 兼容性与风险

- 行为变化：模型先输出一句正文再 `<thinking>` 的响应，thinking 不再被提取，标签会以文本出现。这违反注入策略，应当可见而不是被静默改写；需在观测中统计该形态（`tracing::debug!(reason = "xml_thinking_after_visible_text")`）。
- 首包延迟：探测窗口只在开头，关闭后不再缓冲尾部，后续 text 延迟略降。开头的空白暂存最多几个字节。
- 与 P02 同改 `process_content_with_thinking`，建议合并到一个 PR。
- 回滚：`xml_thinking_probe_closed` 可通过常量开关短路为旧行为。
- 未验证项：Kiro 上游是否会在开头输出非空白前导字符（如 BOM 或零宽字符）再接 `<thinking>`；如果有，需要把这类字符纳入"前导空白"。
