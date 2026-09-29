# P17 错误状态码与信封偏离官方：未知模型 400、无可用账号 503、未匹配路由空 body 404、count_tokens 纯文本 400/415/422、`"429"` 子串误判限流

Status: partially-fixed-in-439e5ce (413 request_too_large; other items open)
Severity: Low
Area: aux
Discovered: 2026-09-28 协议互转审计
Verified-against: a4227c1 (2026-09-29)

## 问题与影响

官方 Anthropic API 的错误有固定的"HTTP 状态码 ↔ `error.type`"对应关系。本代理在以下几处偏离：

| # | 场景 | 当前 | 官方 |
| --- | --- | --- | --- |
| 1 | 模型不存在或不可用 | `400 invalid_request_error`，`The requested model is not available for this endpoint.` | `404 not_found_error` |
| 2 | 无可调度账号、所有账号冷却或用尽、attempt 预算耗尽、本地池风险熔断 | `503 api_error` | `529 overloaded_error`（服务过载），官方错误表里没有 503 |
| 3 | 未匹配路由（如 `GET /v1/models/{id}`、拼错的路径）、方法不匹配 | axum 默认的**空 body** 404 / 405，无 `request-id` | `404 not_found_error` JSON 信封 |
| 4 | `count_tokens` body 解析失败 | axum `Json` rejection 返回纯文本：缺少 Content-Type 415、语法错误 400、字段错误 422 | `400 invalid_request_error` JSON |
| 5 | 请求体超限 | `413 invalid_request_error` | `413 request_too_large` |
| 6 | WebSearch MCP 超时 | `504 api_error` | `504 timeout_error` |
| 7 | 错误字符串里包含 `"429"` 子串 | 被当作限流：返回 `429 rate_limit_error` 并带 `retry-after`，同时影响外部池 fallback 的原因判断 | 只有真实的 429 才返回 `rate_limit_error` |

影响：

- #1：Claude Code 对"模型不存在"有专门的提示（推断：遇到 404 `not_found_error` 时提示用户用 `/model` 换模型）。收到 400 时只显示通用的 API Error 原文。SDK 用户也无法用 `NotFoundError` 分支处理这种情况。
- #2：从经验看，Claude Code 对 503 和 529 都会重试，所以功能上没有中断。但 529 在 Claude Code 里有专门语义（"API overloaded"提示；连续 529 时可能触发 fallback model，推断）。用 503 表示过载，客户端就拿不到这层语义；运维看板把"容量不足"和"服务故障"混在同一个 5xx 类别里。
- #3、#4：返回非 JSON 的错误体，SDK 会解析失败，只能拿到 HTTP 状态码；没有 `request-id`，排障时无法和服务端日志对应。
- #7：误判后，客户端会按 `retry-after` 退避重试，真实的 5xx 或协议错误被伪装成限流。外部池 fallback 的原因也被记成 `local_transient_exhausted`。错误字符串里常见的数字都可能包含 `429`：凭据标签 `#1429`、邮箱 `user1429@…`、耗时 `14290ms`、字节数 `4290 bytes`、上游 request id 等。

定级 Low：大部分场景下客户端最终都会重试或展示错误，不会造成数据错误。但它会影响错误分类、观测数据和 SDK 兼容性。

## 官方协议对照

> 方向约定：kiro.rs 基于 Kiro 对外提供 Claude Code（Anthropic）协议接口。客户端（如 Claude Code CLI）发出 Claude Code 协议请求，经转换后变为 Kiro 协议，调度到最终上游 Kiro；Kiro 的响应再转换回 Claude Code 协议返回给客户端。下文引用的"Anthropic 文档/官方"一律指 **Claude Code 协议规范**，即接口侧必须满足的行为；"Kiro 侧"指上游 Kiro 协议的要求。Anthropic 官方 API 不是本项目的上游。thinking 签名均为 Kiro 原生签名，在转换中原样透传。

已验证事实（Anthropic Errors 文档）：

| HTTP | `error.type` | 含义 |
| --- | --- | --- |
| 400 | `invalid_request_error` | 请求格式或内容有问题 |
| 401 | `authentication_error` | API key 问题 |
| 402 | `billing_error` | 账单问题 |
| 403 | `permission_error` | 无权限 |
| 404 | `not_found_error` | 资源不存在（包括模型） |
| 413 | `request_too_large` | 请求超过大小上限 |
| 429 | `rate_limit_error` | 触发速率限制 |
| 500 | `api_error` | 内部错误 |
| 504 | `timeout_error` | 请求超时 |
| 529 | `overloaded_error` | API 暂时过载 |

- 错误体固定为 `{"type":"error","error":{"type":…,"message":…},"request_id":…}`，响应带 `request-id` 头。
- 流式响应已经开始（HTTP 200）之后，错误用 SSE `error` 事件表达（不在本 issue 范围，见 [P08](08-stream-content-length-exceeded-becomes-error-event.md)）。

经验推断（未验证）：

- Anthropic 官方 SDK 默认对 408、409、429 和所有 >=500 的状态码重试，并遵守 `retry-after` / `x-should-retry`。所以 503 和 529 的重试行为相同，区别只在于客户端对 `overloaded_error` 的专门处理和 UX。
- Claude Code 自己的重试循环会单独统计 529 和 `overloaded_error`（"Repeated 529 Overloaded errors"一类提示），在 Opus 下连续过载时可能切到 fallback 模型。

## 源码链与根因

#1 未知模型：

- `src/anthropic/handlers.rs:5706-5723` `resolve_request_model`：`ModelResolutionSource::Unsupported` 时返回 `400 invalid_request_error`。需要纠正审计的一处表述：默认的 `Compatible` 模式下，这个分支**很少被触发**。`src/anthropic/model_capabilities.rs:1140-1170` 只有在 model 为空，或者是 `ExactOnly` 模式时才返回 `unsupported`；其余未知模型会走 `pass_through` 发给上游。所以真实环境中更常见的路径是：上游返回 invalid model 错误，然后被 `src/anthropic/handlers.rs:5318-5331`（`map_provider_error`）以及 usage 映射 `src/anthropic/handlers.rs:5069-5076` 转成 `400 invalid_request_error`。
- `src/anthropic/handlers.rs:5691-5704` `conversion_error_response`：`ConversionError::UnsupportedModel` 同样返回 400。
- 公共文案：`src/anthropic/envelope.rs:22-23` `PUBLIC_MODEL_UNAVAILABLE_MESSAGE`。

#2 无可用账号或过载：

- `src/anthropic/handlers.rs:5250-5257`：`InferenceAttemptsExhausted`、`InferenceAttemptReservedForFallback`、`AuxiliaryAttemptsExhausted`、`AuxiliaryConcurrencySaturated`、`LocalPoolRiskCircuitOpen` 都返回 `SERVICE_UNAVAILABLE`，类型为 `api_error`（第 5270 行）。
- `src/anthropic/handlers.rs:5420-5440`："所有账号均已禁用 / 已用尽 / 没有支持当前模型的可用账号"等情况返回 `503 api_error`；usage 映射 `src/anthropic/handlers.rs:5128-5141` 同样处理。
- 其他 503：`src/anthropic/handlers.rs:6280-6285`（provider 未就绪）、`6356-6362`（本地路由策略阻断）、`11439-11446`（远程多模态容量不足，带 `retry-after: 1`）；`src/anthropic/websearch.rs:240-245`、`264-269`（MCP scheduler / attempt 限制）。

#3 未匹配路由：

- `src/anthropic/router.rs:410-418`：顶层 `Router::new().nest(...)…` 没有 `.fallback(...)`。`src/main.rs:651-698` 再 nest admin 路由、merge health 路由，也都没有 fallback。所以任何未注册的路径都会落到 axum 默认的 404（空 body）；方法不匹配时是默认的 405。
- 附带问题：auth middleware 挂在各个子路由的 `.layer` 上，未匹配的路径根本不会经过认证，未认证的请求也能拿到 404 或 405，从而探测出路由是否存在（信息泄露很轻微）。

#4 count_tokens：

- `src/anthropic/handlers.rs:28`（`Json as JsonExtractor`），`src/anthropic/handlers.rs:11404-11434`、`11505-11516`。详见 [P12](12-count-tokens-estimate-inconsistent.md)。对照 messages：`src/anthropic/handlers/request_entry.rs:152`、`515`（2026-09-29 核对时由 `153`、`516` 更正）用 `EntryRequestError::invalid` 生成规范的 `invalid_request_error`。

#5 413：`src/anthropic/request_body.rs:59-64` 返回 `413` 加 `"invalid_request_error"`。

#6 504：`src/anthropic/websearch.rs:258-263`（`WebSearchFailureKind::Timeout` 返回 `GATEWAY_TIMEOUT` 加 `"api_error"`）。

#7 `"429"` 子串：

- `src/anthropic/handlers.rs:5392-5418`（`map_provider_error`）：`err_str.contains("429")`（第 5403 行）与"临时冷却 / 本地限流 / retry-after"等条件写在同一个 `||` 链里。
- `src/anthropic/handlers.rs:5100-5126`（usage 映射，第 5111 行）：同样的子串判断。
- `src/anthropic/handlers.rs:2526-2541`（外部池 fallback 原因，第 2528 行 `lower.contains("429")`；同一组里还有 `"502"`、`"503"`、`"504"` 子串）。
- 错误字符串的格式里带有凭据标签（`src/kiro/provider.rs:7823-7841`：`credential_log_label` 调用 `format_credential_log_label`，生成 `#<id> <label>`；2026-09-29 核对时由 `7823-7835` 更正），以及上游原始 body 片段，所以数字子串随处可见。

根因：错误映射是从"内部错误字符串"匹配关键词得到的，不是由结构化的错误类型（`KiroCallFailureKind` 或 HTTP status 字段）决定。状态码选择也沿用了通用网关的习惯（400 / 503 / 502），没有按官方错误表逐项对齐。路由层也没有统一的 fallback。

> 2026-09-29 代码核对（HEAD a4227c1）：3a1306d 没有修复 #1 到 #7 中的任何一项，上文所列的状态码和 `error.type` 映射都没变。它影响本文的地方有两处：
>
> 1. **新增 reasoning malformed 重试（错误分类）**：Kiro 首次返回 `400`，且 `classify_bad_request_reason(body) == "malformed_request"`（例如 `Improperly formed request.`），同时请求带有 retry body builder（历史里有 `reasoningContent`）时，`src/kiro/provider.rs:12206-12209` 通过 `reasoning_compatibility_retry_reason`（`src/kiro/provider.rs:13664-13677`）走和 `THINKING_SIGNATURE_INVALID` 相同的路径：同一凭据、剥掉历史 `reasoningContent` 后重试一次，attempt action 记为 `reasoning_malformed_retry_same_credential`。在此之前，这类请求走普通的 400 分支。现在对外的结果分四种：重试成功返回 200；重试返回 4xx 时按字符串规则映射（和以前一样）；重试返回 408/429/5xx 时也按字符串规则映射，仍然受 #7 子串误判影响；本地侧失败（发送上限、attempt 预算、builder 不可用或构建失败、传输错误、读 body 失败、意外响应）则带 `KiroCallFailureKind::ThinkingSignatureRetryFailed`，由 `src/anthropic/handlers.rs:5250-5287` 按结构化类型映射成 `502 api_error`，并且不进入外部池 fallback（`src/anthropic/handlers.rs:2464-2468`）。推荐映射表需要为这一类补一行。结构化分支先于 `contains("429")`（第 5403 行）执行，所以这条路径不受 #7 影响。usage 侧对这一类的映射没有逐条核对。
> 2. **拒绝记录的模型归因（观测）**：`ProviderNotReady`、`LocalRouteBlocked`、`MultimodalInvalid`、`ModelUnsupported`、`WebSearchUnsupported`、`LocalBodyPrepare`（`src/anthropic/handlers.rs:6286-6293` 等调用点），以及 request_entry 的各类拒绝（`src/anthropic/handlers/request_entry.rs:627-670`），现在都通过 `RequestRejectionUsageContext`（`src/anthropic/usage.rs:519-564`）把请求的 `model`、`stream`、`requested_max_tokens`（以及已解析时的 `upstream_model` / `model_resolution_*`）写进 usage 拒绝记录，不再固定为 `model="unknown"`、`stream=false`。只影响 usage 记录，HTTP 状态码、`error.type` 和响应信封都没变，#1（`ModelUnsupported` 仍是 400）、#2（`ProviderNotReady` / `LocalRouteBlocked` 仍是 503）的结论不变。

## 复现

### 最小复现（单测）

放进 `src/anthropic/handlers/tests.rs`（沿用 `content_length_threshold_error_is_not_reported_as_context_window_full` 的风格）：

```rust
#[tokio::test]
async fn digits_429_in_credential_label_are_not_rate_limit() {
    let response = map_provider_error(
        anyhow::anyhow!("流式 API 请求失败（账号 #1429 user@example.com）: 500 Internal Server Error"),
        Some("req_test_429_substring"),
        None,
        None,
    );
    // 当前失败：返回 429 rate_limit_error 且带 retry-after
    assert_ne!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(response.headers().get("retry-after").is_none());
}

#[tokio::test]
async fn upstream_invalid_model_maps_to_not_found() {
    let response = map_provider_error(
        anyhow::anyhow!("非流式 API 请求失败（账号 #1）: 400 Bad Request {{\"message\":\"Invalid model\"}}"),
        Some("req_test_invalid_model"),
        None,
        None,
    );
    // 当前失败：400 invalid_request_error
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["error"]["type"], "not_found_error");
}

#[tokio::test]
async fn unmatched_route_returns_anthropic_not_found_envelope() {
    let (router, _) = websearch_handler_test_router("http://127.0.0.1:9");
    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models/claude-sonnet-4-6/does-not-exist")
                .header("x-api-key", "b07-handler-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(response.headers().contains_key("request-id")); // 当前失败
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    // 当前失败：body 为空
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json envelope");
    assert_eq!(value["error"]["type"], "not_found_error");
}
```

没有可用账号时的 529：可以复用现有的"全部凭据禁用"类测试夹具，把断言改为 `529` 加 `overloaded_error`，并保留 `retry-after`。

### 端到端复现

```bash
H='x-api-key: <key>'
# 3) 未匹配路由：空 body 404 / 405
curl -si http://127.0.0.1:19023/v1/models/claude-sonnet-4-6 -H "$H"
curl -si -X DELETE http://127.0.0.1:19023/v1/messages -H "$H"
curl -si http://127.0.0.1:19023/v1/nope            # 无 key 也是 404：未经过认证

# 4) count_tokens 纯文本
curl -si http://127.0.0.1:19023/v1/messages/count_tokens -H "$H" -d '{}'

# 1) 未知模型（默认 Compatible 模式会先 pass-through 到上游，再被映射为 400）
curl -si http://127.0.0.1:19023/v1/messages -H "$H" -H 'content-type: application/json' \
  -d '{"model":"claude-nonexistent-9","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}'

# 2) 无可用账号：在隔离实例中禁用全部凭据后发送任意 messages 请求，观察 503 api_error
```

真实 Claude Code：用隔离环境执行 `--model claude-nonexistent-9`，记录 CLI 显示的提示（400 与 404 时的不同文案）。然后在全部凭据禁用的实例上发请求，记录 CLI 的重试次数和提示；再用 fake server 返回 529，对比重试次数和提示文案（用于确认上文的推断）。

## 修复方案

### 候选方案

A. 只修明确的缺陷（#3 fallback、#4 信封、#7 子串误判），状态码映射保持不变。风险最低。

B. 按官方错误表建立统一映射（推荐），并把映射从"字符串关键词"改为"结构化错误类型"优先。

C. 按路由区分：`anthropic-strict` 按官方映射，`claude-code` 保持现状。维护两套映射，收益有限。

### 推荐方案

采用 B，映射表如下（"保留"表示不变）：

| 内部原因 | 新状态 / type | 说明 |
| --- | --- | --- |
| 请求字段非法、转换失败（非模型问题）、上游 400 请求错误 | 400 `invalid_request_error` | 保留 |
| 本地 `Unsupported` 模型、上游 invalid model / model not found | 404 `not_found_error`，message 带上请求的模型 ID | #1 |
| 没有支持当前模型的可用账号 | 404 `not_found_error`（模型在这个部署里不可用） | 语义属于"资源不存在"；如果运营上更希望客户端重试，可以改成 529 |
| 所有账号冷却、调度容量不足、队列满、attempt 预算耗尽、本地池风险熔断、远程多模态容量不足 | 529 `overloaded_error` + `retry-after` | #2 |
| provider 未就绪（启动中） | 529 `overloaded_error` + `retry-after: 1` | #2 |
| 本地路由策略阻断 | 403 `permission_error` | 配置导致的拒绝，不属于临时故障 |
| 真实上游 429、本地 RPM 限流 | 429 `rate_limit_error` + `retry-after` | 保留，但必须由结构化信号触发 |
| 超时（上游、MCP） | 504 `timeout_error` | #6 |
| 请求体超限 | 413 `request_too_large` | #5 |
| 未匹配路由 | 404 `not_found_error` JSON + `request-id` | #3 |
| 方法不匹配 | 405，`invalid_request_error` JSON | #3（官方没有 405 条目，用 400 类 type） |
| 其他上游、内部错误 | 502 `api_error` 保留（或者改为 500 `api_error`） | 客户端对 5xx 的重试行为相同；为了减少变动，保留 502 |

实现要点：

1. **#7**：删掉三处 `contains("429")`（`src/anthropic/handlers.rs:5111`、`5403`、`2528`），改用 `KiroCallError` 中 attempts 最后一项的 HTTP status，或 `KiroCallFailureKind` / `TransientFailureKind::RateLimit` 这类结构化字段来判断。如果只能匹配字符串，至少要限定成 `: 429 ` / `429 Too Many Requests` 这样的边界形式。`"502"`、`"503"`、`"504"` 的子串判断（`2536-2538`）也一并改掉。
2. **#3**：在 `src/anthropic/router.rs:410-418` 的顶层 Router 上加 `.fallback(anthropic_not_found)` 和 `.method_not_allowed_fallback(...)`。它们生成 JSON 信封并带 `request-id`。admin 和 UI 路由有自己的前缀，需要确认 fallback 只对 API 路径返回 Anthropic 信封，或者对所有路径都返回同一种 JSON 也可以接受。
3. **#4**：见 [P12](12-count-tokens-estimate-inconsistent.md)，改用 bytes extractor 加 serde，返回规范信封。
4. **#1、#2、#5、#6**：统一在 `public_error_response` 和 `usage_public_error` 的调用点替换状态码与 type。公共文案常量不变，保持 `error_id` 追加规则。
5. 映射表写进代码注释或合同文档，作为以后新增错误分支时的依据。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)，只使用 `127.0.0.1:19023` 指定测试实例和隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，Cargo 通过 `feature/tests/run-cargo-scoped.sh` 运行，不得影响本机正在运行的 Claude Code CLI 与服务。

- 表驱动测试：上表每一行构造一个内部错误（包括结构化的 `KiroCallFailureKind`），断言状态码、`error.type`、是否带 `retry-after`，以及 usage 记录中的 `status_code` / `error_type` 与响应一致。每行 5 轮。
- 子串负例：`#1429`、`14290ms`、`4290 bytes`、`req-…429…` 等 10 种以上含 429 的非限流错误都不能被判为 429；真实的 `429 Too Many Requests` 仍然判为 429。外部池 fallback 原因的判定同样覆盖。
- 路由：未匹配的 GET / POST / DELETE、`/v1/models/{id}`（如果 [P11](11-models-endpoint-openai-shape.md) 已实现，则改为命中）、`/dfcache/unknown/v1/messages` 都返回 JSON 信封加 `request-id`。
- 回归：现有的 `map_provider_error` 测试组（`src/anthropic/handlers/tests.rs:8921` 起，即 `content_length_threshold_error_is_not_reported_as_context_window_full`，另有 `6490`、`6548` 两处循环用例；2026-09-29 核对时由 `8877` 更正）中，状态码的变化都要逐条确认并有意识地更新，不能批量替换。
- 真实 Claude Code：未知模型时显示模型相关的提示；无可用账号时显示 overloaded 类提示并重试；两种情况下 CLI 都不崩溃。

## 兼容性与风险

- 503 改为 529：会影响下游中转（sub2api 等）、监控告警规则和本项目的外部池错误分类。外部池侧已经把 529 列为可重试的暂态错误（见 [外部池 HA 调度冷却回归](../external-pool-ha-scheduler-cooldown-regression-20260805.md)），但仍需检查所有 `== 503` 的判断。
- 529 可能让 Claude Code 在 Opus 上触发 fallback model（推断），导致用户在不知情的情况下换了模型。如果不希望这样，可以只对非 Opus 模型或非 `/cc` 路由返回 529，或者保留 503；这需要运营方决定。
- 400 改为 404：依赖"400 即不可重试"的客户端不受影响（404 同样不会重试）。依赖 message 文本匹配的脚本需要注意 message 的变化。
- 加路由 fallback 后，原来返回空 404 的路径改为返回 JSON。前端的静态资源路径（`/admin`、`/ui`）要确认不会被 API fallback 截走。

## 修复结果与验证（2026-09-29）

- 修复（`439e5ce`）：请求体超过 50 MiB 时，返回的 `error.type` 改为 `request_too_large`。其余几项（未知模型 404、503/529、未知路由的 JSON 404、`"429"` 子串判断）仍未处理。
- 验证环境：真实上游验证使用 `127.0.0.1:19023` 指定测试实例和隔离的 CLI `HOME`/`CLAUDE_CONFIG_DIR`，未改动本机正在运行的 Claude Code CLI 环境。证据见 `tmp/thinking-budget-local/fix-evidence-20260929/`。
