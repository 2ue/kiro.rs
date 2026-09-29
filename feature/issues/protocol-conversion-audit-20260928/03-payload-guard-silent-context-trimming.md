# P03 payload guard 超限时静默裁剪上下文（含 system prompt），仍返回 200

Status: partially-fixed-in-ce2fad6 (system pair protected, real-upstream verified; disclosure/reject mode open)
Severity: High
Area: request
Discovered: 2026-09-28 协议互转审计
Verified-against: a4227c1 (2026-09-29)

> 2026-09-29 工作树更新（尚未提交，见 [00 修复计划：2026-09-29 方案复核](00-current-protocol-fix-plan.md)）：本地路径已改为所有请求在转换前执行历史 thinking 丢弃。条件是 `payloadGuardEnabled`、`payloadShaping.enabled` 和 `discardHistoricalThinking` 同时为真，这也是默认配置。受保护的当前工具续写 assistant 保留 Kiro 原生签名。所以，本文中"本地首发保留全部历史签名 thinking"这一前提，在默认配置下不再成立，只有关闭上述任一配置时才成立。

> 2026-09-29 代码核对（HEAD a4227c1）：问题仍存在，3a1306d 未改动 `payload_guard.rs`，也没有加入 system 保护、in-band 披露或 reject 模式。修正：`apply_payload_shaping` 实际范围 `:2150-2226`，`trim_history_to_estimated_budget` 到 `:4474`，`local_body_pipeline.rs` 的引用按 HEAD 重排（prepare `:129-150`、重试请求构造 `:226-239`、首发 warnings `:323-330`）。补充了 LOCAL 与外部池在历史 thinking 丢弃上的差异，以及 3a1306d 新增的两条与体积无关、同样静默的历史 thinking 删减路径（本地转换回退、上游 `malformed_request` 剥离 reasoning 重试）。核对时工作区另有未提交的 `local_body_pipeline.rs` 改动（转换前按配置丢历史 thinking），不属于 HEAD，本文未计入。

## 问题与影响

Kiro local path 的 payload guard（`guard_kiro_request`，`src/anthropic/payload_guard.rs:445-716`）在 weighted 体积超过
`payloadGuardKiroMaxWeight`（默认 `1,300,000`，ASCII 计 1、非 ASCII 计 8，`src/anthropic/payload_guard.rs:33-34`、
`src/anthropic/payload_guard.rs:1578-1589`）时，会按顺序：

1. 截断历史 tool_result（默认每条 8,000 字符 head/tail）、裁剪历史 web_fetch、丢弃历史 thinking、压缩工具定义
   （`apply_payload_shaping`，`src/anthropic/payload_guard.rs:2150-2226`，仅在 `:582` 的超限分支 `size_limit_enabled && report.final_weight > max_weight && config.shaping.enabled` 内执行）；
2. 从最旧一轮开始整轮删除 history（`trim_history_to_estimated_budget`，`src/anthropic/payload_guard.rs:4411-4474`），
   删除后不插入任何“此前内容已省略”标记；
3. 若显式开启了 current shaping，再截断当前 tool_result / 当前 document / 当前用户文本 / 当前图片
   （`apply_current_payload_shaping_until_fit`，`src/anthropic/payload_guard.rs:3508` 起）。

裁剪后请求照常发往 Kiro，成功就返回 HTTP 200。模型在不知情的情况下丢失了上下文，客户端也不知道，
Claude Code 不会触发 compact。

本次核对后需要修正/补充审计的几点：

- 修正：审计说“截断 tool results / 当前 user content、丢弃图片”是超限默认行为，不准确。当前内容整形字段
  `fitCurrentPayloadToBudget`、`truncateCurrentToolResults`、`truncateCurrentUserContent`、`truncateCurrentDocuments`、
  `truncateCurrentImages` 默认全是 `false`（`src/model/config.rs:682-704`，`#[serde(default)]`），
  `current_payload_shaping_enabled` 默认返回 false（`src/anthropic/payload_guard.rs:3500-3506`）。默认只会动“历史”：
  历史 tool_result 截断、历史 thinking 丢弃、工具定义压缩、历史整轮删除。
- 修正：默认模式 `payloadGuardMode=on_too_long`（`src/model/config.rs:2687-2697`、`src/model/config.rs:4492-4494`）下，
  首发请求的 guard 配置是 `max_bytes=0 / max_kiro_weight=0 / trim_history=false`
  （`src/anthropic/handlers.rs:1020-1031`），不做按体积裁剪。静默裁剪发生在：上游先返回 too-long 类错误
  （`CONTENT_LENGTH_EXCEEDS_THRESHOLD` / `Input is too long` / `Context window is full` 等，
  `src/anthropic/handlers.rs:5453-5472`），代理用完整配置重建 body 并重试一次成功的路径
  （`src/anthropic/handlers.rs:7660-7666`、`src/anthropic/handlers.rs:7785-7804`）；以及运营方手动配置 `preemptive` 的路径。
- 新发现（比审计更严重）：Kiro history 的第 0、1 项是 system prompt 转成的 `user(system) + assistant("I will follow these instructions.")`
  合成对（`src/anthropic/converter/history.rs:97-102`、`src/anthropic/converter/history.rs:113-119`）。
  `trim_history_to_estimated_budget` 从下标 0 开始按“到下一个不带 tool_results 的 user 为止”删整轮，没有任何 system 保护
  （`src/anthropic/payload_guard.rs:4425-4471`；`payload_guard.rs` 内无任何 system 相关保护逻辑）。
  所以第一批被删的恰好就是 system prompt（包括注入的 thinking 前缀、tool_choice 前缀、分块写入策略）。
  P001 生产样本 `trimmedHistoryEntries=2`（见 `docs/analysis/p001-kiro-payload-guard-weighted-analysis-20260927.md`）
  与“正好删掉 system 对”一致，但该样本没有保留 body，这一对应关系属于推断。
- 与体积无关、始终执行的一项：`oversizedImageHandling=drop-with-placeholder`（默认，`src/model/config.rs:72-78`）时，
  超过 5 MB 的历史/当前图片会被丢弃并留占位文本（`src/anthropic/payload_guard.rs:558-580`、
  `src/anthropic/payload_guard.rs:2063-2078`、`src/anthropic/payload_guard.rs:2109-2126`）。这里有 in-band 占位，
  模型知道图片被省略，本文不把它列为主问题。
- 澄清（2026-09-29 核对）：LOCAL Kiro 路径上，`discardHistoricalThinking` 只在 `guard_kiro_request` 的超限分支
  （`src/anthropic/payload_guard.rs:582`，`apply_payload_shaping`）里生效；始终执行的 `apply_payload_safety_shaping`
  （`src/anthropic/payload_guard.rs:2063-2078`）只丢超大历史图片。所以默认 `on_too_long` 模式下，首发请求（`max_bytes=0`
  → `size_limit_enabled=false`）保留历史中的 Kiro 原生签名 thinking，只有 too-long 重试或 `preemptive` 超限时才丢。
  外部池路径不同：`apply_anthropic_payload_safety_shaping`（`src/anthropic/payload_guard.rs:2080-2107`）在
  `discard_historical_thinking=true`（默认）时无条件丢历史 thinking（保护当前 tool 续轮的 assistant）。
- 新增（3a1306d 引入的两条静默删减路径，与体积无关）：
  - 本地转换回退：converter 以 "multiple native reasoning blocks" 两类 `UnsupportedContent` 拒绝时
    （`should_retry_local_conversion_after_reasoning_shaping`，`src/anthropic/handlers/local_body_pipeline.rs:75-84`），
    代理强制 `shaping.enabled=true`、`discard_historical_thinking=true`（`local_reasoning_fallback_shaping_config`，`:86-92`，
    **无视运营方关闭 shaping 的配置**），对 Claude Code 协议 payload 调 `sanitize_anthropic_messages_for_external_forwarding`
    丢掉历史 thinking 与超大历史图片后重新转换（`:186-215`）。成功则 200，只有一条 `tracing::info!`，无响应头、无 in-band 标记。
  - 上游重试：Kiro 返回 400 且 `classify_bad_request_reason == "malformed_request"`（如 `Improperly formed request.`）时，
    `reasoning_compatibility_retry_reason`（`src/kiro/provider.rs:13664-13677`，调用点 `:12206-12208`）现在也会用
    `build_thinking_signature_retry_body`（`src/anthropic/handlers.rs:6926`）剥离 history 的 `reasoningContent` 后同凭据重发一次
    （attempt action `reasoning_malformed_retry_same_credential`）。以前只有 `THINKING_SIGNATURE_INVALID` 触发。
    仅当 Kiro 请求 history 含 `reasoningContent` 时才挂这个 builder（`src/anthropic/handlers.rs:6850`、`:6897`）。
  两条路径删的都是历史 thinking（签名原样透传或整块删除，不会改写/伪造签名），语义影响小于删整轮 history，
  但同样是客户端不可见的上下文删减，纳入本 issue 的"披露"范围。

影响：

- 模型丢失早期对话、丢失 system prompt 后行为漂移（不再遵守 CLAUDE.md、工具使用约定、语言约束等），
  表现为“长会话后半段突然变笨/不守规矩”，难以从客户端定位。
- 历史 tool_result 被截到 head/tail 后，模型可能基于不完整的文件内容做编辑。
- Claude Code 看到 200，不会 compact；下一轮又带着同样超限的完整历史来，代理每轮都重复“先 400 再裁剪重试”，
  每轮多一次上游调用和一次失败计费风险。
- 推断（未完全追踪）：流式 usage 在收到 `contextUsageEvent` 时按百分比反推 input_tokens
  （`src/anthropic/handlers.rs:10752-10763`），反映的是裁剪后的上下文，Claude Code 基于 usage 的自动 compact
  阈值判断会被低估，裁剪状态会“粘住”。

## 官方协议对照

> 方向约定：kiro.rs 基于 Kiro 对外提供 Claude Code（Anthropic）协议接口。客户端（如 Claude Code CLI）发出 Claude Code 协议请求，经转换后变为 Kiro 协议，调度到最终上游 Kiro；Kiro 的响应再转换回 Claude Code 协议返回给客户端。下文引用的"Anthropic 文档/官方"一律指 **Claude Code 协议规范**，即接口侧必须满足的行为；"Kiro 侧"指上游 Kiro 协议的要求。Anthropic 官方 API 不是本项目的上游。thinking 签名均为 Kiro 原生签名，在转换中原样透传。

已验证事实（Anthropic 公开文档 / 常见官方错误体）：

- 输入超过上下文窗口：HTTP 400，`invalid_request_error`，message 形如 `prompt is too long: N tokens > M maximum`。
- 请求体过大：HTTP 413 `request_too_large`。
- 官方 API 不会替调用方删历史或截断 tool_result；上下文管理由客户端负责（Claude Code 的 compact、SDK 的 context editing
  都是显式、客户端可见的动作）。

经验推断（未在本次审计中抓包验证）：

- Claude Code 通过错误文案识别 prompt-too-long 并提示/执行 compact；非官方文案会被当作普通 400 展示。
  错误文案本身的问题见 [P01](01-context-window-error-message-not-official.md)。
- Kiro 上游的 weighted 阈值是黑盒实测（`sub2api-kiro` 的 1.3M 探测表），不是协议合同，见
  `docs/analysis/p001-kiro-payload-guard-weighted-analysis-20260927.md`。

## 源码链与根因

请求链：

```text
local_body_pipeline::prepare (src/anthropic/handlers/local_body_pipeline.rs:129-150, initial_payload_guard_config @ :138)
  -> prepare_with_plan (:152)
     (3a1306d) 转换失败且为多 native reasoning 块 -> 强制丢历史 thinking 后重转 (:186-215)
  -> PayloadTooLongRetryRequest::new (local_body_pipeline.rs:226-239 -> handlers.rs:1162-1180)  // 保存完整配置，仅 on_too_long 且 maxBytes>0 时启用
  -> prepare_kiro_request_body(initial_payload_guard_config)  // local_body_pipeline.rs:242；on_too_long: 不按体积裁剪，也不丢历史 thinking
  -> call_api_stream_maybe_fail_fast
     Err(too-long) -> should_retry_payload_guard_after_provider_error (handlers.rs:7660-7666)
       -> build_retry_body_after_provider_error -> guard_kiro_request(完整配置) (handlers.rs:1211)
       -> warnings_header = retry_warnings_header (handlers.rs:7804)
       -> 再发一次；成功 => HTTP 200
```

guard 内部（`src/anthropic/payload_guard.rs`）：

```rust
// 476-481：maxBytes=0 时 weighted 限制也关闭
let max_weight = if config.max_bytes > 0 { config.max_kiro_weight } else { 0 };
// 582-597：超限 -> 历史 tool_result/thinking/web_fetch/工具定义 shaping
// 599-637：超限 -> 从 history[0] 开始整轮删除，直到 final_weight <= max_weight
while report.final_weight > max_weight && !request.conversation_state.history.is_empty() {
    let removed = trim_history_to_estimated_budget(...);   // 4411-4472，无 system 保护
    ...
}
// 699：仍超限只打 still_oversized，不报错
report.still_oversized = size_limit_enabled && report.final_weight > max_weight;
```

披露面现状：

- 有结构化报告和日志：`PayloadGuardReport`（`src/anthropic/payload_guard.rs:114-188`）、`log_payload_guard_report`，
  并写入 usage 诊断（`src/anthropic/handlers.rs:1228`）。这是运营侧可见的，不是客户端/模型可见的。
- 响应头 `x-kiro-rs-warnings`：`warning_header_fragment`（`src/anthropic/payload_guard.rs:272-395`）会输出
  `payload-trimmed-history=N`、`payload-history-tool-results-truncated=N` 等。
  - 首发路径受 `exposeProxyWarnings`（默认 `false`，`src/model/config.rs:4720-4722`）且非 strict profile 控制
    （`src/anthropic/handlers.rs:5789-5791`、`src/anthropic/handlers/local_body_pipeline.rs:323-330`）。
  - too-long 重试路径不一致：`merge_warning_headers(self.conversion_warnings, Some(&report))`
    （`src/anthropic/handlers.rs:1229`）只对 conversion warnings 做了开关判断（构造时 `local_body_pipeline.rs:234-236`），
    payload 片段无条件输出，并在 `handlers.rs:7804` 覆盖原头。即重试路径在 `exposeProxyWarnings=false`、甚至 strict
    profile 下也会带头。这本身不是坏事，但开关语义被绕过。
  - `payload-oversized=` 片段写的是 `final_bytes`（`src/anthropic/payload_guard.rs:391-392`），而 Kiro 判据是 weight，数值口径不一致。
  - 即使头存在，Claude Code 不读取自定义响应头，模型更看不到；对最终用户等于没有披露。
- 可配置项：`payloadGuardEnabled`、`payloadGuardMode`（`preemptive|on_too_long`）、`payloadGuardMaxBytes`（0 关闭所有按体积整形）、
  `payloadGuardKiroMaxWeight`、`payloadGuardTrimHistory`、`payloadShaping.*`（`src/model/config.rs:3912-3959`、`src/model/config.rs:645-710`）。
  没有“超限直接按官方 prompt-too-long 拒绝”的模式；`payloadGuardTrimHistory=false` 时仍会做历史 shaping，
  仍超限则透传给上游，由上游 400 + P01 的非官方文案返回。

根因：guard 的设计目标是“尽量让 Kiro 接受”，把“删上下文”当作可静默执行的修复动作；没有区分
“不改变语义的协议修复”（配对修复、空内容占位）和“改变语义的上下文删减”，后者缺少客户端可见的披露和拒绝选项，
也没有把 system 合成对当作不可删除的前缀。

## 复现

### 最小复现（单测）

放入 `src/anthropic/payload_guard.rs` 的 `mod tests`（复用 `request_with_history`、`guard_config`、`TEST_MODEL`）：

```rust
#[test]
fn kiro_history_trim_must_not_drop_system_prompt_pair() {
    let mut history = vec![
        Message::User(HistoryUserMessage::new("SYSTEM RULES: always answer in Chinese", TEST_MODEL)),
        Message::Assistant(HistoryAssistantMessage::new("I will follow these instructions.")),
    ];
    for idx in 0..10 {
        history.push(Message::User(HistoryUserMessage::new(
            format!("user {} {}", idx, "x".repeat(500)),
            TEST_MODEL,
        )));
        history.push(Message::Assistant(HistoryAssistantMessage::new(format!(
            "assistant {} {}", idx, "y".repeat(500)
        ))));
    }
    let mut request = request_with_history(history);
    let (_body, report) =
        guard_kiro_request(&mut request, guard_config(5_000)).expect("guard should trim");

    assert!(report.trimmed_history_entries > 0);
    // 当前实现失败：history[0] 已是 "user 0 ..."，system 对被第一批删除
    let Some(Message::User(first)) = request.conversation_state.history.first() else {
        panic!("history must start with user");
    };
    assert!(first.user_input_message.content.starts_with("SYSTEM RULES"));
}

#[test]
fn kiro_history_trim_is_not_disclosed_in_band() {
    // 同上构造，断言裁剪后存在一条 in-band 省略标记（当前实现失败：没有任何标记）
}
```

### 端到端复现

1. 本地实例，Admin 运行时配置：`payloadGuardMode=preemptive`、`payloadGuardKiroMaxWeight=20000`（人为调低，便于小请求触发），
   `exposeProxyWarnings=false`。
2. 发送：

```bash
curl -sS -D - http://127.0.0.1:PORT/v1/messages \
  -H 'x-api-key: <KEY>' -H 'anthropic-version: 2023-06-01' -H 'content-type: application/json' \
  -d @- <<'JSON'
{"model":"claude-sonnet-4-5","max_tokens":256,
 "system":[{"type":"text","text":"Secret codeword is PINEAPPLE. Always end replies with it."}],
 "messages":[
  {"role":"user","content":"<重复约 30KB 的 ASCII 文本>"},
  {"role":"assistant","content":"ok"},
  {"role":"user","content":"What is the secret codeword?"}]}
JSON
```

3. 预期现象：HTTP 200，回答不知道 codeword；响应头无 `x-kiro-rs-warnings`；日志/usage 中
   `trimmedHistoryEntries>=2`。
4. 默认 `on_too_long` 路径：需要真实超过 Kiro 阈值的会话（约 1.3M weighted，例如 Claude Code 长会话反复 Read 大文件），
   观察一次上游 `CONTENT_LENGTH_EXCEEDS_THRESHOLD` 后同请求 200，且该响应带 `x-kiro-rs-warnings: payload-trimmed-history=...`
   （即使 `exposeProxyWarnings=false`）。

## 修复方案

### 候选方案

A. 新增超限处置策略（推荐基础）：`payloadGuardOverflowAction = trim_and_disclose | reject_prompt_too_long`。
   - `reject_prompt_too_long`：需要删减“语义内容”（历史整轮、tool_result 截断、current shaping）时不删，直接返回官方形态
     `400 invalid_request_error`，message 以 `prompt is too long:` 开头（与 P01 共用构造函数）。协议修复（配对、空内容占位、
     超 5MB 图片按现有 `oversizedImageHandling`）不受影响。
   - `trim_and_disclose`：保留现有裁剪能力，但强制 in-band 披露和 system 保护。
B. 仅补披露，不提供拒绝模式。改动小，但运营方无法选择“官方语义优先”。
C. 默认改为拒绝。与 `docs/analysis/p001-kiro-payload-guard-weighted-analysis-20260927.md` “不建议把超限一律 reject 作为默认”
   和 [payload guard 语义专题](../payload-guard-semantics-limits-and-performance.md) “不能把 soft target 突然改成 hard reject”
   的既有结论冲突，且没有新证据推翻，不采用。

### 推荐方案

A + 默认 `trim_and_disclose`，维持既有决策（默认不 reject、保留 `on_too_long`），同时修掉本次发现的两个硬伤：

1. system 前缀保护（必须，独立于模式）。
   - 在 `trim_history_to_estimated_budget` 增加 `protected_prefix_len` 参数；`guard_kiro_request` 计算：
     若 `history[0]` 是 user 且 `history[1]` 是内容等于 `"I will follow these instructions."` 的 assistant，则保护前 2 项。
     更稳妥的做法是在 converter 侧给 `KiroRequest` 带一个不序列化的 `system_prefix_len` 元数据（类似
     `tool_cache_point_insert_after`），避免靠字符串识别。
   - `align_history_to_user` 在保护前缀之后执行。
2. in-band 披露（`trim_and_disclose`）。
   - 删完历史后，在保护前缀之后插入一对合成消息：
     `user: "[Earlier conversation history (N turns) was omitted by the gateway because the request exceeded the upstream size limit.]"`
     + `assistant: "Understood."`；或追加到第一个保留 user 的 content 前缀（不新增 turn，避免改变 alternation）。推荐后者。
   - 截断的 tool_result 已有 head/tail 省略标记（沿用现状），核对标记文本包含“truncated by gateway”。
3. 响应头统一。
   - `build_retry_body_with_base` 的 payload 片段也走 `should_expose_proxy_warnings`；另外新增一个不受开关控制、
     只含聚合事实的头（如 `x-kiro-rs-context-trimmed: history=N`），或者直接复用 `x-kiro-rs-warnings` 但对“语义删减类”片段强制输出。
     需在 Admin 文档中明确。
   - `payload-oversized` 片段改输出 `final_weight`，并附 `basis=kiroWeighted`。
4. `reject_prompt_too_long` 分支伪代码：

```rust
// guard_kiro_request 内，进入 582 行的语义 shaping 前
if config.overflow_action == OverflowAction::RejectPromptTooLong
    && size_limit_enabled
    && report.final_weight > max_weight
{
    return Err(PayloadGuardError::PromptTooLong {
        weight: report.final_weight,
        max_weight,
    });
}
// payload_guard_error_response:
PayloadGuardError::PromptTooLong { weight, max_weight } => envelope::error_response(
    StatusCode::BAD_REQUEST,
    "invalid_request_error",
    format!("prompt is too long: {} weighted units > {} maximum", weight, max_weight),
),
```

   `on_too_long` + reject：上游 too-long 后不再做裁剪重试，直接返回上面的官方形态错误（与 P01 统一）。
5. 配置：`PayloadGuardConfig` 增加 `overflow_action`；`src/model/config.rs` 增字段、默认 `trim_and_disclose`；Admin API / 两套 UI 同步。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)，只使用 `127.0.0.1:19023` 指定测试实例和隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，Cargo 通过 `feature/tests/run-cargo-scoped.sh` 运行，不得影响本机正在运行的 Claude Code CLI 与服务。

- 单测：上面两条红测转绿；`reject_prompt_too_long` 下超限返回 `PromptTooLong` 且 `request` 未被修改；
  未超限时两种模式 body 与现状 byte-identical（沿用现有 clean body identity 测试）。
- 单测：重试路径在 `exposeProxyWarnings=false` 时的头行为符合新约定；`payload-oversized` 输出 weight。
- handler 测试（`src/anthropic/handlers/tests.rs`）：fake upstream 首次返回 `CONTENT_LENGTH_EXCEEDS_THRESHOLD`，
  - `trim_and_disclose`：exactly one retry，第二个 body 的 history[0] 仍是 system，且含省略标记；
  - `reject_prompt_too_long`：不重试，下游 400 message 以 `prompt is too long` 开头。
- 真实 Claude Code：长会话触发一次超限，`reject` 模式下 CLI 出现 compact 提示/行为；`trim` 模式下后续轮次仍遵守 system 中的约束。
- 性能：沿用 payload guard release probe，clean body 序列化次数不变。

## 兼容性与风险

- system 保护会让“只剩 system + current 仍超限”的请求更早 still_oversized，最终由上游 400，这是正确结果。
- in-band 省略标记会改变 prompt，影响 prompt cache 命中（前缀变化），但裁剪本身已经破坏前缀，增量影响有限。
- `reject` 模式对非 Claude Code 客户端（无自动 compact）体验更差，所以不设为默认，由运营方按路由/客户端选择。
- 头部语义调整可能影响依赖现有 `x-kiro-rs-warnings` 的排障脚本，需要在发布说明里写明。
- 阈值本身仍是黑盒模型，本方案不改变 1.3M 默认值，也不改变 `on_too_long` 默认。

## 修复结果与验证（2026-09-29）

- 修复（`ce2fad6`）：history 裁剪先删除 system prompt 合成对之后的会话轮次，只有其余可删的轮次都删完仍然超限时，才删除 system 对。
- 单测：`history_trim_keeps_system_prompt_pair_for_five_rounds`、`history_trim_drops_system_pair_only_as_last_resort`。
- 真实上游新发现（2026-09-29，sonnet-4.5 测试账号）：Kiro 的 `CONTENT_LENGTH_EXCEEDS_THRESHOLD` 实际表现更接近按 token 计的上下文上限（约 200k token）。纯 ASCII 的 900 KB 请求就会被拒绝；1.85 MB 的请求经 on_too_long 重试裁剪到 weighted 1,237,949（低于 1.3M 安全阈值）后，仍然被拒绝。因此，对 ASCII 为主的大请求，现有 weighted 1.3M 阈值基本无法让重试成功。本次没有修改阈值，因为改默认值会影响所有调度。结合 P01 的修复，这类请求现在会收到 `prompt is too long: N tokens > M maximum`，Claude Code 可以据此自动 compact。按模型窗口 token 设置重试目标，需要单独立项评估。
- 仍然开放的部分：静默裁剪的披露方式，以及可配置的 reject 模式。
- 验证环境：真实上游验证使用 `127.0.0.1:19023` 指定测试实例和隔离的 CLI `HOME`/`CLAUDE_CONFIG_DIR`，未改动本机正在运行的 Claude Code CLI 环境。证据见 `tmp/thinking-budget-local/fix-evidence-20260929/`。

## 修复结果与验证补充（2026-09-29，Opus 1M）

- 使用 `claude-opus-4-8`（1M 上下文）发送约 4.5 MB 的 45 轮历史，并在 system 中写入口令。第一次请求被 Kiro 以 too-long 拒绝；on_too_long 重试删除了 66 条历史，weighted 1,246,437 时返回 200，模型准确说出了 system 中的口令 `ZEBRAFINCH`。这说明裁剪之后 system prompt 仍然保留着。修复前，system 所在的 history[0..2] 会最先被删掉。
- 对照：同一个口令问题在不裁剪的情况下也能答对，说明判断依据是有效的。
- 1M 上下文：约 1.15 MB 的内容（约 30 万 token）发给 `claude-opus-4-8` 后直接返回 200，没有触发裁剪。
