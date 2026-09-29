# P01 上下文超限 / 输入超阈值错误文案不符合官方格式，Claude Code 无法触发自动压缩

Status: fixed-in-439e5ce (not released)
Severity: High
Area: request
Discovered: 2026-09-28 协议互转审计
Verified-against: a4227c1 (2026-09-29)

> 2026-09-29 代码核对（HEAD a4227c1）：3a1306d 未改动 `map_provider_error`、`provider_public_error_for_message` 与 too-long 分类函数，问题仍存在。修正了移位的行号：`src/kiro/provider.rs` 的 Kiro 分类字面量移到 `:13590-13606`（`classify_bad_request_reason`），`local_body_pipeline.rs` 的 token 计数移到 `:312-320`，`provider_public_error_for_message` 的两个 too-long 分支在 `:5051-5067`。另外 3a1306d 让 `classify_bad_request_reason == "malformed_request"` 的 400 也能触发同凭据剥离 reasoning 重试（`reasoning_compatibility_retry_reason`），但 too-long 类仍归到 `content_length_exceeds_threshold`，不受影响。

## 问题与影响

上游（本地 Kiro 或外部池）拒绝"输入过长"时，代理统一改写为自定义英文文案：

- 输入内容长度超阈值（`CONTENT_LENGTH_EXCEEDS_THRESHOLD` / `Input is too long` / `prompt is too long` 等）→
  `400 invalid_request_error`，message 为
  `Request input content length exceeded the request threshold. This limit is separate from the model context window. ...`
- 上下文窗口已满（`context window is full` + `reduce conversation history`）→
  `400 invalid_request_error`，message 为 `Context window is full. Reduce conversation history, ...`
- 两者都经过 `public_error_response`，只要有 `error_id`，message 末尾还会追加
  ` If this continues, contact the administrator with error ID: <id>`（`src/anthropic/envelope.rs:28-30`）。

用户可见现象：长会话在某一轮后每次请求都返回上面这段文案；Claude Code 不会自动 compact，也不给出
`/compact` 提示，只把错误原文展示为 API Error。用户重试一直失败，只能手动 `/compact` 或 `/clear`。

影响面：

- 所有走 `map_provider_error` 的 stream / non-stream 请求（stream 在 HTTP 200 之前失败时同样返回 JSON 400）；
- 用量记录的 public error（`record_failure` → `provider_public_error_for_message`）也写入同样文案；
- 外部池已经返回官方格式 `prompt is too long: > 1000000 maximum` 时，代理反而把它改写成非官方文案
  （现有测试 `prompt_too_long_error_maps_to_input_length_message` 固化了这个行为）。

需要纠正审计里的一处表述："请求侧已识别 `prompt is too long`（~handlers.rs:5460）"不准确。
`src/anthropic/handlers.rs:5460` 是 `is_upstream_payload_too_long_error` 对上游错误字符串做的分类，
不是请求侧预检；它恰恰是把官方文案识别出来再改写掉的入口。

## 官方协议对照

> 方向约定：kiro.rs 基于 Kiro 对外提供 Claude Code（Anthropic）协议接口。客户端（如 Claude Code CLI）发出 Claude Code 协议请求，经转换后变为 Kiro 协议，调度到最终上游 Kiro；Kiro 的响应再转换回 Claude Code 协议返回给客户端。下文引用的"Anthropic 文档/官方"一律指 **Claude Code 协议规范**，即接口侧必须满足的行为；"Kiro 侧"指上游 Kiro 协议的要求。Anthropic 官方 API 不是本项目的上游。thinking 签名均为 Kiro 原生签名，在转换中原样透传。

已验证事实（Anthropic 公开 API 文档 / 错误码表）：

- 输入 token 超过模型上下文窗口：HTTP 400，`error.type = "invalid_request_error"`，
  `error.message` 形如 `prompt is too long: 208310 tokens > 200000 maximum`。
- 请求体字节数超过网关限制：HTTP 413，`error.type = "request_too_large"`。
- 本仓库 50 MiB 请求体限制已返回 413（`src/anthropic/request_body.rs:15`、`:58-64`），
  但 `error.type` 用的是 `invalid_request_error` 而非官方的 `request_too_large`，是一个相邻的小偏差，本 issue 一并记录，不在推荐方案主线。

经验推断（未对 Claude Code 源码逐行验证，来自线上行为观察和社区逆向资料）：

- Claude Code 以 message 包含 `prompt is too long`（大小写不敏感）判断上下文超限，据此：
  展示 "Prompt is too long" 及 `/compact` 建议，并在开启 auto-compact 时触发 reactive compact 后重试；
- 新版本会尝试用类似 `/prompt is too long[^0-9]*(\d+)\s*tokens?\s*>\s*(\d+)/i` 的规则解析 token 差额，
  解析失败时仍按"超限"处理，只是无法精确计算要裁掉多少。
- 413 `request_too_large` 在 Claude Code 中会提示请求过大（多与图片/文件相关），不触发 compact。

Kiro 侧（已验证于代码中的分类字面量，`src/kiro/provider.rs:13590-13606`，`classify_bad_request_reason`）：

- `CONTENT_LENGTH_EXCEEDS_THRESHOLD` / `Input is too long.`：Kiro 的输入长度阈值，按内容长度而非 token 计算，与模型上下文窗口不是同一个限制；
- `context window is full ... reduce conversation history`：Kiro 的上下文窗口满。

## 源码链与根因

调用链（stream 与 non-stream 共用）：

```
handle_stream_request / handle_non_stream_request
  -> provider.call_*  返回 Err（含上游 400 body）
  -> (可选) payload guard 裁剪后重试一次       src/anthropic/handlers.rs:7772-7800 (stream), :10088 附近 (non-stream)
  -> usage_context.record_failure(...)         src/anthropic/handlers.rs:4116-4133
       -> provider_public_error_for_message    src/anthropic/handlers.rs:5038-5066
  -> map_provider_error_with_admission_feedback  src/anthropic/handlers.rs:5233-5242
       -> map_provider_error                   src/anthropic/handlers.rs:5244
            is_upstream_payload_too_long_error   -> 自定义文案   :5291-5302
            is_upstream_context_window_full_error-> 自定义文案   :5304-5315
```

关键代码（`src/anthropic/handlers.rs:5291-5315`）：

```rust
if is_upstream_payload_too_long_error(&err_str) {
    let message = "Request input content length exceeded the request threshold. This limit is separate from the model context window. ...";
    return public_error_response(StatusCode::BAD_REQUEST, "invalid_request_error", message, request_id, error_id, ...);
}
if is_upstream_context_window_full_error(&err_str) {
    let message = "Context window is full. Reduce conversation history, ...";
    return public_error_response(StatusCode::BAD_REQUEST, "invalid_request_error", message, request_id, error_id, ...);
}
```

分类函数（`src/anthropic/handlers.rs:5453-5472`）把 `prompt is too long` 与 `CONTENT_LENGTH_EXCEEDS_THRESHOLD`
归到同一类，所以外部池的官方文案也被改写。

`public_error_response`（`src/anthropic/handlers.rs:3725-3746`）在有 `error_id` 时调用
`envelope::public_message_with_error_id` 追加后缀，同时已经写了 `x-error-id` 响应头。

根因：

1. 早期设计刻意区分"内容长度阈值"和"上下文窗口"两种限制（见测试
   `content_length_threshold_error_is_not_reported_as_context_window_full`，位于 `src/anthropic/handlers/tests.rs`；
   该文件工作区有未提交改动，行号在变化，本文对测试一律按函数名引用），
   面向人类可读，没有考虑 Claude Code 对官方文案的机器识别。
2. `map_provider_error` 签名只有 `(err, request_id, error_id, provider)`，拿不到本次请求的估算 token 和模型窗口，
   无法拼出 `N tokens > M maximum`。但调用方都持有 `usage_context.request.input_tokens` 与
   `context_window_tokens`（`src/anthropic/handlers.rs:265-266`，赋值见 `:4870-4882`）。

stream 在 HTTP 200 之后的情况不走这里：上游在事件流中发 `ContentLengthExceededException`，
见 [P08](08-stream-content-length-exceeded-becomes-error-event.md)。

与 [04-external-pool-prompt-too-long](../04-external-pool-prompt-too-long.md) 的关系：该文把
`prompt is too long` 纳入 too-long 分类，并要求"对外 public message 改为清晰的上下文过长语义，但不透出外部池原文"。
本 issue 不改分类，只改对外文案格式；新文案由代理本地拼装，仍不透出外部池原文。

## 复现

### 最小复现（单测）

放入 `src/anthropic/handlers/tests.rs`，风格参照 `prompt_too_long_error_maps_to_input_length_message`：

```rust
#[tokio::test]
async fn upstream_too_long_error_is_not_recognized_by_claude_code_today() {
    for raw in [
        r#"流式 API 请求失败（凭据 #1 test@example.com）: 400 Bad Request {"message":"Input is too long.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}"#,
        r#"流式 API 请求失败（凭据 #1 test@example.com）: 400 Bad Request {"message":"Context window is full; reduce conversation history"}"#,
        r#"流式 API 请求失败（账号 #9 hidden）: 400 Bad Request {"error":{"message":"prompt is too long: > 1000000 maximum","type":"invalid_request_error"},"type":"error"}"#,
    ] {
        let response = map_provider_error(
            anyhow::anyhow!("{}", raw),
            Some("req_p01"),
            Some("err_p01"),
            None,
        );
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json");
        let message = value.pointer("/error/message").and_then(|v| v.as_str()).unwrap();
        // 当前行为：不包含官方关键字，Claude Code 无法识别
        assert!(
            !message.to_ascii_lowercase().contains("prompt is too long"),
            "当前实现已修复？message={message}"
        );
    }
}
```

修复后把断言反转为 `contains("prompt is too long")`，见"测试与验收"。

### 端到端复现

1. 配置一个本地 Kiro 凭证，关闭 payload guard 重试（或让裁剪后仍超限）。
2. 构造超长请求：

```bash
python3 - <<'EOF' > /tmp/p01.json
import json
big = "x " * 600_000
print(json.dumps({
  "model": "claude-sonnet-4-5", "max_tokens": 1024, "stream": True,
  "messages": [{"role": "user", "content": big}]
}))
EOF
curl -sS http://127.0.0.1:19023/v1/messages \
  -H 'content-type: application/json' -H 'anthropic-version: 2023-06-01' \
  -H "x-api-key: $KEY" --data-binary @/tmp/p01.json -D - | head -40
```

期望（当前）：`HTTP/1.1 400`，body 中 message 为 `Request input content length exceeded ...`。

3. 真实 Claude Code：`ANTHROPIC_BASE_URL` 指向代理，在长会话中持续读入大文件直到上游拒绝。
   观察：当前只显示 API Error 原文，不出现 compact 提示，auto-compact 不触发；修复后应出现
   "Prompt is too long" 并自动 compact（需开启 auto-compact）。

## 修复方案

### 候选方案

A. 固定文案 `prompt is too long`（不带数字）。改动最小，满足关键字识别，但 Claude Code 无法计算 token 差额。

B. 按官方格式 `prompt is too long: N tokens > M maximum`，N 取本地估算输入 token，M 取模型窗口。
   需要把 token 信息传进错误映射函数。

C. 透传上游原文。外部池已经是官方格式时可直接用，但本地 Kiro 的原文不是官方格式，且 04 号文档要求不透出外部池原文，不采用。

### 推荐方案

采用 B，并在数字不可信时退回 A。

1. 新增结构体和文案函数（`src/anthropic/handlers.rs`，靠近 `is_upstream_payload_too_long_error`）：

```rust
#[derive(Debug, Clone, Copy, Default)]
struct PromptTooLongContext {
    /// 本地估算的输入 token（count_all_tokens），0 表示未计数
    estimated_input_tokens: i32,
    /// 模型上下文窗口：max_input_tokens_for(model) 或 get_context_window_size(model)
    context_window_tokens: i32,
}

fn official_prompt_too_long_message(ctx: Option<PromptTooLongContext>) -> String {
    match ctx {
        Some(c) if c.context_window_tokens > 0 && c.estimated_input_tokens > c.context_window_tokens => {
            format!("prompt is too long: {} tokens > {} maximum", c.estimated_input_tokens, c.context_window_tokens)
        }
        // 估算值不超过窗口（典型是 Kiro 按字节阈值拒绝）时不编造数字，保留关键字即可
        _ => "prompt is too long".to_string(),
    }
}
```

   说明：Kiro 的 `CONTENT_LENGTH_EXCEEDS_THRESHOLD` 按内容长度判断，经常出现"估算 token < 窗口"但仍被拒。
   这种情况下不能为了凑格式写出 `N > M` 的假数字，只输出 `prompt is too long`。

2. `map_provider_error` 增加参数 `too_long: Option<PromptTooLongContext>`（或新建
   `map_provider_error_with_prompt_context`，旧函数传 `None` 以减少改动面）。
   `:5291-5315` 两个分支改为：

```rust
if is_upstream_payload_too_long_error(&err_str) || is_upstream_context_window_full_error(&err_str) {
    let kind = if is_upstream_context_window_full_error(&err_str) { "context_window_full" } else { "content_length_threshold" };
    log_provider_warning_with_hint(&err_str, "请求被拒绝：输入过长", error_id);
    return prompt_too_long_error_response(
        official_prompt_too_long_message(too_long),
        request_id,
        error_id,
        [("x-kiro-too-long-kind", kind.to_string())],
    );
}
```

3. `prompt_too_long_error_response`：状态码 400、`invalid_request_error`，**不追加 error-id 文案后缀**
   （message 保持官方纯净格式），`x-error-id` 头照常写入。诊断信息（阈值类型、原始 reason）只进日志和响应头。
4. 所有 `map_provider_error_with_admission_feedback` 调用点（`src/anthropic/handlers.rs:7615`、`:7763`、`:7947`、`:7978`、
   `:8074`、`:8097`、`:9924`、`:10069`、`:10241`、`:10272`、`:10368`、`:10391`、`:10591`）传入
   `Some(PromptTooLongContext { estimated_input_tokens: usage_context.request.input_tokens, context_window_tokens: usage_context.request.context_window_tokens })`。
   若 payload guard 重试后失败，N 应取裁剪后 body 的估算值（重试分支已重新计数时使用新值；未重新计数时退回方案 A）。
5. `provider_public_error_for_message`（`:5051-5067`）同步改为同一文案（无 token 上下文时用方案 A），保证用量记录与实际响应一致。
6. 相邻偏差（可选，同一提交或单独提交）：`src/anthropic/request_body.rs:61` 的 413 `error.type` 改为 `request_too_large`。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)，只使用 `127.0.0.1:19023` 指定测试实例和隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，Cargo 通过 `feature/tests/run-cargo-scoped.sh` 运行，不得影响本机正在运行的 Claude Code CLI 与服务。

单测（`src/anthropic/handlers/tests.rs`）：

- `too_long_errors_map_to_official_prompt_too_long_message`：三类上游原文（Kiro 阈值、Kiro 窗口满、外部池官方格式）均返回 400、
  `invalid_request_error`，message 以 `prompt is too long` 开头，不含 `error ID`、不含 `hidden`、不含凭证邮箱。
- `prompt_too_long_message_includes_token_numbers_when_estimate_exceeds_window`：`estimated=1_200_000, window=1_000_000` →
  message 等于 `prompt is too long: 1200000 tokens > 1000000 maximum`。
- `prompt_too_long_message_omits_numbers_when_estimate_not_above_window`：`estimated=300_000, window=1_000_000` → message 等于 `prompt is too long`。
- `estimated_input_tokens=0`（token counting 关闭，`src/anthropic/handlers/local_body_pipeline.rs:312-320`）→ 无数字。
- 响应头包含 `x-error-id` 与 `x-kiro-too-long-kind`。

需要更新的已有测试：

- `content_length_threshold_error_is_not_reported_as_context_window_full`：
  断言 `input content length exceeded` / `separate from the model context window` 需改为官方格式断言；"区分两种限制"改由响应头 `x-kiro-too-long-kind` 验证。
- `prompt_too_long_error_maps_to_input_length_message`：改名并反转断言；`!message.contains("1000000")`
  需保留语义为"不透出外部池原文里的数字"，因为数字应来自本地估算。
- 其他引用 `CONTENT_LENGTH_EXCEEDS_THRESHOLD` 的测试（`grep -n CONTENT_LENGTH_EXCEEDS_THRESHOLD src/anthropic/handlers/tests.rs` 当前有 5 处）逐个检查是否断言了旧文案。

端到端验收：

- curl 超长请求返回 `400` + `{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long..."}}`；
- 真实 Claude Code 长会话触发超限后出现 compact 提示；开启 auto-compact 时自动压缩并重试成功；
- usage 记录中的 public error message 与响应一致。

## 兼容性与风险

- 行为变化：依赖旧英文文案做告警/统计的外部脚本会失效；需在 release note 说明，诊断改看 `x-kiro-too-long-kind` 头和日志。
- 语义风险：Kiro 字节阈值拒绝也会被 Claude Code 当成"上下文满"而 compact。这是期望行为（压缩历史同样能降低字节数），
  但若拒绝主要由单个超大 tool_result 或图片引起，compact 后仍可能失败；这种情况需要 payload guard 处理，不在本 issue 内。
- 去掉 error-id 文案后缀：用户在 Claude Code 界面看不到 error ID，只能从响应头/日志关联。可接受，因为这是用户可自行处理的错误。
- 回滚：新文案集中在一个函数，回滚为旧常量即可；调用点新增参数可保留。
- 性能：无额外计算，token 估算已在请求开始时完成。
- 未验证项：Claude Code 的识别规则和 token 差额解析规则来自经验观察，未对其发布包源码逐行核对；需要用当前 Claude Code 版本做一次真实验收。

## 修复结果与验证（2026-09-29）

- 修复（`439e5ce`）：上游 `CONTENT_LENGTH_EXCEEDS_THRESHOLD` 或 context window full 时，返回 400 `invalid_request_error`，文案改为 Claude Code 协议格式。本地估算的 token 超过模型窗口时，返回 `prompt is too long: N tokens > M maximum`；没超过时（Kiro 按内容阈值拒绝）不编造数字，返回以 `prompt is too long:` 开头的说明。error id 只放在 `x-error-id` 响应头里，message 保持纯净格式；另外新增 `x-kiro-too-long-kind` 响应头。外部池的两条文案也加上了 `prompt is too long:` 前缀。相邻问题一并修复：请求体超过 50 MiB 时，`error.type` 改为 `request_too_large`。
- 单测：`prompt_too_long_messages_use_claude_code_protocol_prefix`、`upstream_too_long_errors_map_to_prompt_is_too_long_for_five_rounds`，并更新了已有的 too-long 和 413 用例；全量 `cargo test --bin kiro-rs` 通过。
- 真实上游：900 KB 和 1.1 MB 的请求被 Kiro 拒绝后，返回 `prompt is too long: 225933 tokens > 200000 maximum`，message 后面不再附加 error-id 后缀。
- 验证环境：真实上游验证使用 `127.0.0.1:19023` 指定测试实例和隔离的 CLI `HOME`/`CLAUDE_CONFIG_DIR`，未改动本机正在运行的 Claude Code CLI 环境。证据见 `tmp/thinking-budget-local/fix-evidence-20260929/`。
