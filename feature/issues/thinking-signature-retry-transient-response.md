# Thinking signature retry 第二响应 transient 被误归类

Status: `fixed / provider-focused regression passed / production recurrence pending`

Severity: P0/P1。该问题会把本应按普通上游 transient 处理的 retry 第二响应，例如 429/5xx，误标成 `thinking_signature_retry_failed`，导致 usage/error 诊断混乱、local fallback 被阻断、凭据冷却语义不清晰。

Last verified: 2026-07-25

## 用户可见现象

生产样本来自 152.53.194.170，版本 117。

代表错误 ID：

```text
req_017YcyBchdfyzmnFEBixVw6L
```

对应 usage record：

```text
usage id: req_01iYokvTyBDsFNsnA1odmsmQ
endpoint: /ha/v1/messages
model request: claude-opus-4-8
upstream model: claude-opus-4.8
route: local_credential / local_error_no_fallback
public status/type: 429 / rate_limit_error
internal message:
  upstream_failure class=thinking_signature_retry_failed
  upstream_status=429
  public_status=502
  body_bytes=165
  reason=thinking_signature_retry_unexpected_response
```

这行内部诊断自相矛盾：

- usage 的公开状态是 `429 rate_limit_error`；
- provider 内部字符串却写死 `public_status=502`；
- `callFailureKind=ThinkingSignatureRetryFailed` 导致 local error classifier 不进入普通 transient fallback；
- 该请求真实发生了两次本地上游发送：第一次 400 `THINKING_SIGNATURE_INVALID`，第二次同凭据 retry 返回 429。

近 12 小时 170 聚类：

```text
thinking_signature_retry_failed upstream_status=429: 31
thinking_signature_retry_failed upstream_status=500 body_bytes=158: 19
thinking_signature_retry_failed upstream_status=500 body_bytes=202: 14
```

同一窗口内，相关凭据随后出现 `credential_risk_controlled / TemporarilySuspended` 事件。这说明上游确实出现了风险/限流/服务端异常波，不应把所有 retry 第二响应都当成签名协议兼容失败。

## 根因

`src/kiro/provider.rs::call_api_with_retry` 在处理 `THINKING_SIGNATURE_INVALID` 时有一个兼容 retry 分支：

1. 首次正式请求返回 `400 {"reason":"THINKING_SIGNATURE_INVALID"}`；
2. provider 构造移除历史 reasoningContent 的 retry body；
3. 同一凭据发送第二次请求；
4. 旧逻辑只允许第二响应满足：

```rust
retry_status.is_success()
  && retry_content_kind == eventstream/json-labeled-eventstream
```

否则统一返回：

```text
class=thinking_signature_retry_failed
reason=thinking_signature_retry_unexpected_response
public_status=502
```

这把三类语义混在了一起：

- 第二次仍返回 `400 THINKING_SIGNATURE_INVALID`：这是签名兼容 retry 仍失败，应该保持 typed fail closed。
- 第二次返回 `408/429/5xx`：这是普通上游 transient，应该按 rate-limit/server/timeout 冷却当前凭据，并允许现有 local transient fallback 规则接管。
- 第二次返回其他 4xx：这是普通 invalid request，不应伪装成 502。

旧逻辑对第二类没有分类，直接终止，造成 `local_error_no_fallback` 与内部 `public_status=502` 指纹。

## 修复方案

在签名 retry 第二响应读 body 后，按普通 provider 响应分类：

- `400 THINKING_SIGNATURE_INVALID`：保持 `KiroCallFailureKind::ThinkingSignatureInvalid`，不 fallback，不冷却。
- `408/429/5xx`：
  - 使用 `api_failure_diagnostic` 生成普通 `class=timeout/rate_limit/server_error`；
  - 调用 `token_manager.report_transient_failure_kind(...)` 写入对应 transient cooldown；
  - request attempts 中第二次发送的 `error_type` 变为 `rate_limit` 或 `server_error`；
  - 返回普通 provider error，不带 `ThinkingSignatureRetryFailed` failure kind；
  - 由既有 `fallbackOnLocalTransientExhausted` 规则决定是否进外部池。
- 其他 4xx：按 `invalid_request` 返回，不写成 `thinking_signature_retry_failed`。
- 读 body 失败、构造 retry body 失败、发送失败、预算拒绝等仍保持 `ThinkingSignatureRetryFailed`，因为它们确实是兼容 retry 机制本身失败。

本轮没有把 retry 后继续同请求内换本地账号做进来。原因是当前循环计数把 signature retry 当额外 send，但主循环 attempt 计数没有感知额外 send；贸然继续会有突破 explicit `max_sends` 的风险。当前修复先保证分类、冷却和 fallback 语义正确；同请求内继续换本地账号应作为单独“发送预算重构”处理。

## 代码变更

- [src/kiro/provider.rs](/Users/yuanfeijie/Desktop/procode/kiro.rs/src/kiro/provider.rs)
  - 签名 retry 第二响应新增 `retry_after` 提取。
  - 第二响应 `408/429/5xx` 改为普通 transient 分类与冷却。
  - 第二响应其他 client error 改为普通 invalid request。
  - fake provider 增加 `thinking_signature_rate_limited_second` 场景。

## 红绿复现

最小复现不需要真实上游：

1. fake Kiro upstream 第一次返回：

```json
{"reason":"THINKING_SIGNATURE_INVALID"}
```

并使用 HTTP 400。

2. provider 构造去 reasoningContent 的第二请求。
3. fake upstream 第二次返回 HTTP 429 或 500。

修复前：

```text
class=thinking_signature_retry_failed
reason=thinking_signature_retry_unexpected_response
callFailureKind=ThinkingSignatureRetryFailed
last attempt error_type=thinking_signature_retry_failed
credential transient cooldown: none
```

修复后：

```text
HTTP 429:
  class=rate_limit
  callFailureKind=None
  last attempt error_type=rate_limit
  cooldown reason contains api_rate_limit

HTTP 500:
  class=server_error
  callFailureKind=None
  last attempt error_type=server_error
  cooldown reason contains api_server_error
```

重复 `400 THINKING_SIGNATURE_INVALID` 和 retry body read failure 仍保持 typed fail-closed，不被误改成 transient。

## 已执行验证

命令通过 scoped target 运行，结束后 target 自动清理：

```bash
feature/tests/run-cargo-scoped.sh thinking-signature-fmt -- cargo fmt
feature/tests/run-cargo-scoped.sh thinking-signature-provider-tests -- \
  cargo test -q thinking_signature -- --nocapture
```

结果：

```text
14 passed; 0 failed
```

新增覆盖：

- `thinking_signature_retry_retryable_second_response_is_transient_for_five_rounds`
  - 429 x stream/non-stream x 5 轮；
  - 500 x stream/non-stream x 5 轮；
  - 确认不含 `thinking_signature_retry_failed`；
  - 确认 `callFailureKind=None`；
  - 确认 cooldown reason 分别含 `api_rate_limit` / `api_server_error`。

保留覆盖：

- `thinking_signature_second_response_always_terminates_typed_and_bounded_five_rounds`
  - 重复 `THINKING_SIGNATURE_INVALID` 仍 typed；
  - retry response body read failure 仍 typed；
  - 不冷却、不泄漏私有 body marker。

## 生产验证建议

发布后在 152.53.194.170/159 查：

```sql
select left(coalesce(error_message,error_detail,''), 220), count(*)
from usage_records
where created_at >= now() - interval '2 hours'
  and coalesce(error_message,error_detail,'') ilike '%thinking_signature%'
group by 1
order by count(*) desc;
```

期望：

- `thinking_signature_retry_failed upstream_status=429` 不再新增；
- 429 第二响应应变成普通 `class=rate_limit upstream_status=429 public_status=429`；
- usage route 若外部池配置允许，应出现 `external_fallback_after_local_attempts` 或其他已有 local transient fallback subtype；
- 凭据 runtime last error/cooldown 应显示 rate-limit/server，而不是 `thinking_signature_retry_failed`。

## 残余风险

- 本修复不保证 retry 第二响应后同请求内继续换本地账号；这是发送预算/attempt ledger 重构问题。
- 如果上游第二响应为真实 400 业务错误，仍会按 invalid request 返回，不应 fallback。
- 如果 retry response body 读取失败，仍是 `thinking_signature_retry_response_read_failed`；若后续生产显示 429 body read failure 很多，需要再按 status 优先分类。
