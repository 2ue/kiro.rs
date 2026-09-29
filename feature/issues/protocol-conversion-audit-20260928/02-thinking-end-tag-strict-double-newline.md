# P02 XML thinking 结束标签只接受 `</thinking>\n\n`，正文被吞进 thinking 并误报 max_tokens

Status: open / documented / not-fixed
Severity: High
Area: stream
Discovered: 2026-09-28 协议互转审计
Verified-against: a4227c1 (2026-09-29)

> 2026-09-29 代码核对（HEAD a4227c1）：3a1306d 未改动 `src/anthropic/stream.rs` 与 converter，问题仍存在。`src/anthropic/stream.rs`、`converter/thinking.rs`、`converter/history.rs`、`handlers.rs:9778-9801` 的引用均与 HEAD 一致；仅 `extract_xml_thinking` 赋值行因 3a1306d 在 `local_body_pipeline.rs` 前部新增重试逻辑，从 `:302` 移到 `:331`。

## 问题与影响

XML thinking 提取模式（`thinking_enabled && extract_xml_thinking && !native_reasoning_seen`）下，流式解析器只把
**后面紧跟 `\n\n`** 的 `</thinking>` 当作结束标签。模型写成 `</thinking>\n正文`、`</thinking>正文` 或
`</thinking> 正文` 时：

1. 结束标签不被识别，整段正文连同 `</thinking>` 字面量都以 `thinking_delta` 发出；
2. 流结束时 flush 只在标签后全是空白时才补识别，这里后面有正文，识别失败；
3. `generate_final_events` 发现只有 thinking 块、没有 text/tool_use，强制 `stop_reason = "max_tokens"`，并补一个内容为 `" "` 的 text 块。

Claude Code 上的表现：

- 回答"消失"，只能在折叠的 thinking 区域里看到，主区域只有一个空格；
- `stop_reason = max_tokens`，Claude Code 会按输出截断处理（经验观察：新版本会自动发起"继续"请求或提示输出超限），造成额外请求和重复内容；
- 下一轮历史里这条 assistant 消息变成 `<thinking>整段内容</thinking>` + `" "`，模型上下文被污染。

若结束标签后紧接 tool_use（`</thinking>\n正文` 然后 tool_use 事件），tool_use 边界的补识别同样失败，
thinking 块未被正确关闭，正文仍在 thinking 里；stop_reason 为 tool_use（因为有 tool_use 块），不会误报 max_tokens。

影响面：非 strict compat profile（`extract_xml_thinking = allows_unsigned_thinking()`，
`src/anthropic/handlers/local_body_pipeline.rs:331`，`src/model/config.rs:2520-2522`）且上游未返回原生
`reasoningContentEvent` 的所有 thinking 请求。

## 官方协议对照

> 方向约定：kiro.rs 基于 Kiro 对外提供 Claude Code（Anthropic）协议接口。客户端（如 Claude Code CLI）发出 Claude Code 协议请求，经转换后变为 Kiro 协议，调度到最终上游 Kiro；Kiro 的响应再转换回 Claude Code 协议返回给客户端。下文引用的"Anthropic 文档/官方"一律指 **Claude Code 协议规范**，即接口侧必须满足的行为；"Kiro 侧"指上游 Kiro 协议的要求。Anthropic 官方 API 不是本项目的上游。thinking 签名均为 Kiro 原生签名，在转换中原样透传。

已验证事实：

- 官方 Anthropic 流式协议中 thinking 是独立 content block（`content_block_start` type=thinking → `thinking_delta` → `signature_delta` → `content_block_stop`），不存在"文本里的 XML 标签"；XML 标签是本代理为 Kiro 引入的私有约定。
- 代理注入给上游的提示只要求 "emit concise reasoning inside a `<thinking>...</thinking>` block before any visible text or tool call, and close the thinking block before continuing"
  （`src/anthropic/converter/thinking.rs:11`），**没有要求 `\n\n`**。
- 代理回放历史时，纯 tool_use 轮次的 unsigned thinking 被写成 `<thinking>…</thinking>`，后面**没有** `\n\n`
  （`src/anthropic/converter/history.rs:381-389`，只有同时有 text 时才加 `\n\n`）。模型会在上下文里看到不带 `\n\n` 的样例，这增加了它输出不带 `\n\n` 结束标签的概率。

经验推断：

- Claude 系列模型多数情况下在 `</thinking>` 后输出空行，但单换行或直接接正文在线上确实出现，频率未统计。

## 源码链与根因

```
process_assistant_response
  -> emit_assistant_response_content            src/anthropic/stream.rs:2639-2658
     -> process_content_with_thinking           src/anthropic/stream.rs:2743
        in_thinking_block 分支:
          find_real_thinking_end_tag_for         src/anthropic/stream.rs:280-311   (严格 \n\n)
          命中后剥离 close + "\n\n"               src/anthropic/stream.rs:2855-2858
          未命中时保留 13 字节尾巴，其余发 thinking_delta  src/anthropic/stream.rs:2859-2881
process_tool_use (边界)
  -> find_real_thinking_end_tag_at_buffer_end_for  src/anthropic/stream.rs:3454-3493 (要求标签后全是空白)
generate_final_events_with_reported_usage_mapper
  -> flush: find_real_thinking_end_tag_at_buffer_end_for  src/anthropic/stream.rs:3724-3784
  -> thinking-only 兜底: set_stop_reason("max_tokens") + " "  src/anthropic/stream.rs:3813-3823
```

`find_real_thinking_end_tag_for`（`src/anthropic/stream.rs:280-311`）：

```rust
// 如果标签后面内容不足以判断是否有双换行符，等待更多内容
if after_content.len() < 2 { return None; }
// 真正的 thinking 结束标签后面会有双换行符 `\n\n`
if after_content.starts_with("\n\n") { return Some(absolute_pos); }
// 不是双换行符，跳过继续搜索
search_start = absolute_pos + 1;
```

跳过后，标签已离开 13 字节保留区（`:2866-2869`），被当作普通 thinking 内容发出，后续不可能再识别。

`find_real_thinking_end_tag_at_buffer_end_for`（`src/anthropic/stream.rs:333-356`）只在 `buffer[after_pos..].trim().is_empty()` 时命中；
而此时 buffer 最多只剩 13 字节尾巴，对"标签后有正文"的场景基本无效。

thinking-only 兜底（`src/anthropic/stream.rs:3813-3823`）：

```rust
if self.thinking_enabled && self.extract_xml_thinking
    && self.thinking_block_index.is_some()
    && !self.state_manager.has_non_thinking_blocks()
{
    self.state_manager.set_stop_reason("max_tokens");
    events.extend(self.create_sanitized_text_delta_events(" "));
}
```

根因：`\n\n` 被当作"真结束标签"的判别特征，用来排除 thinking 里口头提到 `</thinking>`（如 "关于 </thinking> 标签"，见函数注释 `src/anthropic/stream.rs:263-279`）。
这个特征对模型输出没有协议保证，漏判代价（整段回答被隐藏 + 误报 max_tokens）远大于误判代价（剩余 thinking 变成可见文本）。

非流式路径：`extract_thinking_from_complete_text`（`src/anthropic/stream.rs:1134-1173`）先用严格规则，再用"末尾全空白"规则；
`</thinking>\n正文` 两者都不命中，返回 `(None, 原文)`（`src/anthropic/stream.rs:1154-1157`），由 `append_non_stream_reasoning_and_text`
（`src/anthropic/handlers.rs:9778-9801`）把**包含 `<thinking>` 标签的原文整体当 text** 输出。
表现不同（标签泄漏到正文，stop_reason 正常），但根因相同。审计原文只提到流式，这里补充。

## 复现

### 最小复现（单测）

放入 `src/anthropic/stream.rs` 的 `mod tests`，风格参照 `test_thinking_only_sets_max_tokens_stop_reason`：

```rust
#[test]
fn test_thinking_end_tag_with_single_newline_keeps_answer_visible() {
    for chunks in [
        vec!["<thinking>\nabc</thinking>\n正文"],
        vec!["<thinking>\nabc。</thinking>正文"],
        vec!["<thinking>\nabc\n</thinking> 正文"],
        // 跨 chunk：标签和正文分开到达
        vec!["<thinking>\nabc</thin", "king>\n", "正文"],
    ] {
        let mut ctx = StreamContext::new_with_thinking("test-model", 1, true, HashMap::new());
        let _ = ctx.generate_initial_events();
        let mut all_events = Vec::new();
        for chunk in &chunks {
            all_events.extend(ctx.process_assistant_response(chunk));
        }
        all_events.extend(ctx.generate_final_events());

        assert_eq!(collect_thinking_content(&all_events), "abc", "chunks={chunks:?}");
        assert_eq!(collect_text_content(&all_events), "正文", "chunks={chunks:?}");
        let delta = all_events.iter().find(|e| e.event == "message_delta").unwrap();
        assert_eq!(delta.data["delta"]["stop_reason"], "end_turn", "chunks={chunks:?}");
    }
}

#[test]
fn test_non_stream_thinking_end_tag_with_single_newline_is_extracted() {
    let (thinking, text) = extract_thinking_from_complete_text("<thinking>\nabc</thinking>\n正文");
    assert_eq!(thinking.as_deref(), Some("abc"));
    assert_eq!(text, "正文");
}
```

（第 2、3 组的 thinking 期望值分别是 `abc。` 和 `abc\n`，写正式测试时按用例单独断言。）

当前实现下：第一个测试 `collect_thinking_content` 得到 `abc</thinking>\n正文`，text 为 `" "`，stop_reason 为 `max_tokens`；
第二个测试 `thinking == None`。

### 端到端复现

fake upstream 事件序列（`assistantResponseEvent`，可用 `src/anthropic/handlers/tests.rs` 的 `eventstream_test_frame` 构造）：

```
assistantResponseEvent {"content":"<thinking>\n用户想要一个函数"}
assistantResponseEvent {"content":"。</thinking>\n下面是实现：\n```rust\nfn a() {}\n```"}
（流结束，无 messageStatus / contextUsage）
```

下游请求：

```bash
curl -N http://127.0.0.1:19023/v1/messages -H 'content-type: application/json' \
  -H 'anthropic-version: 2023-06-01' -H "x-api-key: $KEY" -d '{
  "model":"claude-sonnet-4-5","max_tokens":4096,"stream":true,
  "thinking":{"type":"enabled","budget_tokens":2048},
  "messages":[{"role":"user","content":"写一个空函数"}]}'
```

当前结果：只有 thinking 块包含全部内容，末尾一个 `" "` text 块，`message_delta.stop_reason = "max_tokens"`。

真实 Claude Code：非 strict profile，开启 thinking，多轮让模型只调用工具（历史中出现不带 `\n\n` 的 `<thinking>…</thinking>`），
之后普通问答轮次出现"回答为空 / 输出超限"的概率上升。可在日志中按 `stop_reason_source = local_inferred_max_tokens` 且 thinking-only 过滤。

## 修复方案

### 候选方案

A. 完全放宽：thinking 块内第一个未被引用字符包裹的 `</thinking>` 即结束，不看后缀（审计原建议）。
   简单，漏判为零；但 thinking 中口头提到裸 `</thinking>` 时会提前结束，剩余思考变成可见正文。

B. 分级规则：`\n\n` 优先；另外接受以下形态：
   - B1 `</thinking>` 后紧跟 `\n`（单换行）；
   - B2 `</thinking>` 位于行首（前一个字符是 `\n` 或它是 thinking 内容的开头）；
   - B3 `</thinking>` 后紧跟非空白、非引用字符（如 `</thinking>正文`），且前一个字符是句末标点或换行；
   仍拒绝"行中间 + 后面是空格 + 文本"的形态（`关于 </thinking> 标签`），这是现有注释要防的口头提及。

C. 事后修复：流结束时发现 thinking-only，再把最后一个 `</thinking>` 之后的内容补发为 text。
   thinking_delta 已经发出去，无法撤回，只能重复发送，不可取。

### 推荐方案

采用 B（覆盖绝大多数真实形态，同时保留对口头提及的防护），并补充观测：

1. `find_real_thinking_end_tag_for` 改为返回 `Option<(usize, usize)>`：(标签起点, 需要消费的后缀长度)。

```rust
fn find_real_thinking_end_tag_for(buffer: &str, tag: ThinkingXmlTag) -> EndTagMatch {
    // EndTagMatch::{Found { pos, consume }, NeedMore, NotFound}
    for pos in unquoted_positions(buffer, tag.close) {      // 复用 valid_unquoted_tag
        let after = &buffer[pos + tag.close.len()..];
        if after.len() < 2 { return EndTagMatch::NeedMore; } // 仍等待，保留 13 字节尾巴
        let prev = buffer[..pos].chars().next_back();
        let at_line_start = prev.map_or(true, |c| c == '\n');
        if after.starts_with("\n\n") { return Found { pos, consume: 2 }; }
        if after.starts_with('\n')  { return Found { pos, consume: 1 }; }          // B1
        if at_line_start            { return Found { pos, consume: leading_ws(after) }; } // B2
        let next = after.chars().next().unwrap();
        if !next.is_whitespace() && prev.is_some_and(is_sentence_end_or_newline) {
            return Found { pos, consume: 0 };                                        // B3
        }
        // 其余视为口头提及，继续搜索
    }
    NotFound
}
```

   `is_sentence_end_or_newline` 包含 `。．.！!？?：:）)` 等；具体集合以测试样本为准。

   注意：流式场景下 `thinking_buffer` 只剩尾巴，`buffer[..pos]` 为空不代表"行首"——前一个字符可能已作为 thinking_delta 发出。
   需要在 `StreamContext` 增加 `last_emitted_thinking_char: Option<char>`，每次发 thinking_delta 时更新；
   `pos == 0` 时用它代替 `prev`。仅当 thinking 块刚开始、尚未发出任何内容时才视为"thinking 开头"。

2. 调用点按 `consume` 剥离，不再硬编码 `"\n\n".len()`：
   - 流式 `src/anthropic/stream.rs:2855-2858`；
   - 非流式 `src/anthropic/stream.rs:1147-1150`（非流式也改用同一规则；文本完整时 `NeedMore` 视为"后缀为空"，交给末尾规则）。
3. 13 字节保留区（`src/anthropic/stream.rs:2866-2869`）保持不变：B1-B3 最多需要标签后 2 字节判断，现有保留长度足够。
4. `find_real_thinking_end_tag_at_buffer_end_for`（tool_use 边界、最终 flush）改为同一分级规则，而不是只接受"后面全是空白"。
5. 观测：在跳过未被引用包裹的 `</thinking>` 时计数（`skipped_unquoted_close_tags`），thinking-only 兜底命中且计数 > 0 时打
   `tracing::warn!(reason = "thinking_close_tag_not_accepted")`，便于用线上样本校准规则。
6. thinking-only 兜底本身（`:3813-3823`）不在本 issue 修改；规则放宽后它只在"模型真的只输出了 thinking"时触发。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)，只使用 `127.0.0.1:19023` 指定测试实例和隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，Cargo 通过 `feature/tests/run-cargo-scoped.sh` 运行，不得影响本机正在运行的 Claude Code CLI 与服务。

新增单测：

- `test_thinking_end_tag_with_single_newline_keeps_answer_visible`（上面草稿，含跨 chunk 用例）；
- `test_thinking_end_tag_at_line_start_followed_by_text`：`<thinking>\nabc\n</thinking>正文`；
- `test_thinking_end_tag_after_sentence_end_followed_by_text`：`<thinking>\nabc。</thinking>正文`；
- `test_inline_mention_of_close_tag_is_still_ignored`：`<thinking>\n关于 </thinking> 标签的解释\n</thinking>\n\n正文`
  → thinking 为 `关于 </thinking> 标签的解释\n`，text 为 `正文`；
- tool_use 边界：`<thinking>\nabc</thinking>\n我来读取文件` 后接 tool_use 事件 → thinking 关闭，text 为 `我来读取文件`，然后是 tool_use 块；
- 非流式 `extract_thinking_from_complete_text` 同组用例；
- `<think>` 短标签（`THINK_XML_TAG`）同组用例各一条。

需要更新的已有测试（`src/anthropic/stream.rs`）：

- `test_find_real_thinking_end_tag_basic`（`:6321`）：`"</thinking>\n"` 仍为 `None`（只有 1 字节，NeedMore）；
  `"</thinking> more"` 需要按新规则确认（行首 → 命中）。该用例的期望值会变化，需要按新语义重写，并补充 `"x</thinking> more"` → `None`。
- `test_find_real_thinking_end_tag_with_quotes` / `_with_backticks` / `_mixed`：保持不变，作为引用防护回归；
  若函数返回类型改变，断言改为比较 `pos`。
- `test_text_after_thinking_strips_leading_newlines`：保持通过。
- `test_final_flush_filters_standalone_thinking_end_tag`、`test_tool_use_immediately_after_thinking_filters_end_tag_and_closes_thinking_block`：保持通过。

端到端验收：

- 上面的 fake upstream 序列返回 thinking=`用户想要一个函数。`，text=`下面是实现：…`，`stop_reason=end_turn`；
- 线上观察 `thinking_close_tag_not_accepted` 告警与 thinking-only `max_tokens` 的比例在发布后明显下降。

## 兼容性与风险

- 仍未覆盖的形态：`abc</thinking> 正文`（行中、前面不是句末标点、后面是空格）。它和口头提及 `关于 </thinking> 标签` 在字符层面无法区分，
  推荐方案有意不接受；若线上样本显示这类形态占比高，再评估方案 A。
- 误判风险：B2/B3 可能把 thinking 中另起一行的 `</thinking>` 或句末紧跟的 `</thinking>` 当结束。影响是后续思考内容以可见 text 输出，不丢内容、不误报 max_tokens。
- strict profile 不启用 XML 提取，不受影响；原生 `reasoningContentEvent` 路径不受影响。
- 与 [P09](09-xml-thinking-whitespace-loss-and-false-detection.md) 同改 `process_content_with_thinking`，建议同一 PR，测试一起跑。
- 性能：只多几次字符比较，可忽略。
- 回滚：规则集中在 `find_real_thinking_end_tag_for`，可回退为只接受 `\n\n`。
- 未验证项：线上 `</thinking>` 后缀形态的分布没有统计，B3 的标点集合需要用真实样本校准。
