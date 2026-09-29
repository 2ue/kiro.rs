# P12 `count_tokens` 是非单调的字符启发式，口径与 `/v1/messages` 不一致；可选远程计数同步阻塞并外发完整 prompt；解析失败返回非 Anthropic 错误体

Status: open / documented / not-fixed
Severity: Medium
Area: aux
Discovered: 2026-09-28 协议互转审计
Verified-against: a4227c1 (2026-09-29)

> 2026-09-29 工作树更新（尚未提交，见 [00 修复计划：2026-09-29 方案复核](00-current-protocol-fix-plan.md)）：本地路径已改为所有请求在转换前执行历史 thinking 丢弃。条件是 `payloadGuardEnabled`、`payloadShaping.enabled` 和 `discardHistoricalThinking` 同时为真，这也是默认配置。受保护的当前工具续写 assistant 保留 Kiro 原生签名。所以，本文中"本地首发保留全部历史签名 thinking"这一前提，在默认配置下不再成立，只有关闭上述任一配置时才成立。对本文的影响：本地 `input_tokens` 估算仍然基于客户端的原始 payload。usage 的重新计算和整形逻辑属于核心逻辑，这次按要求没有改动。因此只要请求带有历史 thinking，本地估算就会比实际发给 Kiro 的内容略高，这和 count_tokens 的口径一致。

## 问题与影响

`POST {prefix}/v1/messages/count_tokens` 有四类互相独立的问题：

1. **估算算法本身不可靠**：按字符数除以 4，再按段乘以 1.0 到 1.5 的系数。这个系数对每个文本片段（每个 block、每个工具名、每个 schema）单独套用，所以同一段内容切成的片段越多，计数越高；而且函数不单调，396 个 ASCII 字符算出 148 token，400 个字符反而只算 130 token。
2. **和 `/v1/messages` 的口径不一致**：messages 路径拿到上游 `contextUsageEvent` 后，改用"上下文百分比 × 窗口大小"作为 input_tokens，只有没有这个事件时才回退到同一个本地估算。所以同一个请求，`count_tokens` 的结果和随后 messages 响应里的 `usage.input_tokens` 可能差很多。报告 usage 另有 prompt-cache 模拟放大，那是独立问题，本文只把它当作不一致的背景，不展开。
3. **远程计数（可选配置 `countTokensApiUrl`）**：
   - 调用方式是 `tokio::task::block_in_place` + `Handle::block_on`，会把 Tokio worker 线程同步占住，最长两段各 300 秒超时（发送和读 body 各一段）。
   - 每次调用都新建一个 HTTP client，不复用连接。
   - 请求体是完整的 `messages` / `system` / `tools`，发给第三方地址。
   - 这个函数不只被 `count_tokens` 调用，`/v1/messages` 的本地路径、WebSearch 路径、外部池 fallback（一次请求最多两次）和外部池 usage 投影也都会调用。所以一旦配置了远程地址，每个推理请求都会把完整 prompt 额外外发 1 到 3 次，并在热路径上阻塞 worker 线程。
4. **错误信封不对**：这四个 handler 用的是 axum 自带的 `Json` extractor。解析失败时返回 axum 的纯文本 body：缺少 `Content-Type` 是 415，JSON 语法错误是 400，字段类型或缺字段是 422。不是 Anthropic 的 `{"type":"error","error":{...}}`，也没有 `request-id`。`/v1/messages` 已经改用自定义 `MessagesBody`，并返回规范的 `invalid_request_error`，两个端点的行为不统一。

用户可见影响：

- Claude Code 等客户端用 `count_tokens` 判断"还剩多少上下文、是否需要压缩、附件能不能放下"时（推断：CLI 的部分上下文统计会调用这个端点），得到的数字与实际 usage 对不上：小片段多的会话被高估，大块文本被低估。
- 配置了远程计数后，推理热路径的延迟和 worker 占用会跟着第三方服务抖动，也多了一条隐私外发通道。
- 客户端发了畸形 body 时，拿到的纯文本 422 很难被 SDK 解析成结构化错误。

## 官方协议对照

> 方向约定：kiro.rs 基于 Kiro 对外提供 Claude Code（Anthropic）协议接口。客户端（如 Claude Code CLI）发出 Claude Code 协议请求，经转换后变为 Kiro 协议，调度到最终上游 Kiro；Kiro 的响应再转换回 Claude Code 协议返回给客户端。下文引用的"Anthropic 文档/官方"一律指 **Claude Code 协议规范**，即接口侧必须满足的行为；"Kiro 侧"指上游 Kiro 协议的要求。Anthropic 官方 API 不是本项目的上游。thinking 签名均为 Kiro 原生签名，在转换中原样透传。

已验证事实（Anthropic Token Counting API 文档）：

- `POST /v1/messages/count_tokens` 接受和 Messages 相同的输入字段：`model`、`messages`、`system`、`tools`、`tool_choice`、`thinking`（以及 MCP 相关字段），返回 `{"input_tokens": N}`。
- 官方说明计数是估计值，可能和实际创建消息时的 input tokens 有少量差异。这里的差异指同一个 tokenizer 下的系统开销差异，不是不同算法之间的差异。
- 错误使用统一的 Anthropic 错误信封，请求体不合法时返回 `400 invalid_request_error`，每个响应带 `request-id` 头。

经验推断（未验证）：

- Claude 的 tokenizer 对英文文本大约 3.5 字符/token，对 CJK 文本大约 1 字符/token 或稍多。本实现"非拉丁字符 1 token/字符"对中文算是接近，对英文 1/4 加上 1.5 倍系数，短片段会高估大约 30%–50%。
- Kiro 上游不提供独立的 count_tokens 接口。唯一可信的真实值来自推理时的 `contextUsageEvent` 百分比和 `metadataEvent`。

## 源码链与根因

算法（`src/token.rs`）：

- `src/token.rs:82-106` `count_tokens`：非西文字符算 4 个单位，西文算 1 个单位，总单位除以 4，然后分段乘系数：`<100` ×1.5、`<200` ×1.3、`<300` ×1.25、`<800` ×1.2、其余 ×1.0，最后截断成 `u64`。在分段边界处不单调（99.x→148、100→130）。文件头注释 `src/token.rs:6` 和 `src/token.rs:78` 写的是"4.5 个字符单位"，但代码第 87 行是 `4.0`，注释和实现不一致。
- `src/token.rs:193-224` `count_all_tokens_local`：对 system 每一段、每条消息的每个 block、每个工具的 name / description / schema 分别调用 `count_tokens`，所以每个片段各自被乘一次 1.5 倍左右的系数。
- `src/token.rs:111-141` `count_all_tokens`：如果配置了 `api_url`，就执行 `tokio::task::block_in_place(|| Handle::current().block_on(call_remote_count_tokens(..)))`。
- `src/token.rs:144-190` `call_remote_count_tokens`：每次调用都执行 `build_client(config.proxy.as_ref(), 300, ..)`（第 152 行），body 是完整请求（第 155-160 行），读 body 的超时也是 300 秒（第 186 行）。
- 配置入口：`src/main.rs:620-626`，`src/model/config.rs:3590-3600`（`count_tokens_api_url` / `count_tokens_api_key` / `count_tokens_auth_type`）。

`count_tokens` handler（`src/anthropic/handlers.rs`）：

- `src/anthropic/handlers.rs:28` 把 `axum::Json` 导入为 `JsonExtractor`；`src/anthropic/handlers.rs:11404-11434` 和 `11505-11516` 的五个 handler 都用 `JsonExtractor<CountTokensRequest>`，没有自定义 rejection。
- `src/anthropic/handlers.rs:11455-11503` `count_tokens_for_endpoint`：先注入 prompt steering（第 11466-11471 行，默认对 `/cc` 生效，见 [P14](14-prompt-steering-alters-tool-behavior.md)），再处理多模态 source，然后调用 `token::count_all_tokens`（第 11491-11496 行），最后 `max(1)`。它不解析模型，也不校验模型是否存在。
- `src/anthropic/types.rs:459-470` `CountTokensRequest` 只有 `model` / `messages` / `system` / `tools`，`thinking` / `tool_choice` 会被 serde 静默忽略。

messages 路径同样调用 `count_all_tokens`（远程外发和阻塞在这里同样生效）：

- `src/anthropic/handlers/local_body_pipeline.rs:312-321`：本地请求的 input 估算（受 `plan.token_counting` 控制）。

  > 2026-09-29 代码核对（HEAD a4227c1）：行号由 `283-291` 更正为 `312-321`（原行号处是 tracing 日志）。3a1306d 新增的 reasoning 回退重转（`local_body_pipeline.rs:186-211`）会把裁剪掉历史 thinking 的 `shaped_payload` 发给 Kiro，但这里的估算仍基于未裁剪的原始 `payload`。它和 count_tokens 口径一致（两者都用原始 payload），但在触发回退时会高于实际发给 Kiro 的内容。本问题结论不变。
- `src/anthropic/handlers.rs:1726-1754` `ExternalFallbackContext::refresh_payload`：一次调用里执行两次 `count_all_tokens`（原始 payload 和 sanitize 后的 payload），调用点在 `src/anthropic/handlers.rs:6340` 和 `6408`。
- `src/anthropic/handlers.rs:6532-6537`：WebSearch 路径。
- `src/external_pool.rs:15831-15838`、`src/external_pool/usage_projection.rs:193-201`：外部池 usage 投影。

messages 路径的 input_tokens 最终来源（和 count_tokens 分叉的地方）：

- `src/anthropic/stream.rs:2436-2458`：收到 `contextUsageEvent` 时，`percentage × context_window_tokens / 100` 写入 `context_input_tokens`。
- `src/anthropic/stream.rs:3831-3839`：最终估算优先用 `context_input_tokens`，没有时才用本地估算 `input_tokens`。

根因：

1. 没有统一的 token 估算抽象。`count_tokens` 和 messages 各自取值：前者只用启发式，后者优先用上游百分比。
2. 启发式把"短文本放大系数"用在了片段级别，而不是整个请求级别。
3. 远程计数最初按"同步工具函数"设计，后来被复用到异步热路径，没有改成 async，也没有加调用范围限制。
4. count_tokens 端点没有复用 messages 的 body 解析和错误信封逻辑。

## 复现

### 最小复现（单测）

放进 `src/token.rs` 的 `mod tests`：

```rust
#[test]
fn count_tokens_is_monotonic_in_text_length() {
    let mut previous = 0;
    for len in 1..4_000 {
        let tokens = count_tokens(&"a".repeat(len));
        // 当前失败：len=400 时 130 < len=396 时 148
        assert!(tokens >= previous, "len={len} tokens={tokens} previous={previous}");
        previous = tokens;
    }
}

#[test]
fn fragmented_text_is_not_inflated_relative_to_whole_text() {
    let whole = count_tokens(&"abcd".repeat(1_000));
    let fragments: u64 = (0..100).map(|_| count_tokens(&"abcd".repeat(10))).sum();
    // 当前失败：whole=1000，fragments=100*15=1500
    assert!(
        fragments <= whole * 11 / 10,
        "fragments={fragments} whole={whole}"
    );
}
```

放进 `src/anthropic/handlers/tests.rs`（复用 `websearch_handler_test_router`）：

```rust
#[tokio::test]
async fn count_tokens_invalid_json_uses_anthropic_error_envelope() {
    let (router, _) = websearch_handler_test_router("http://127.0.0.1:9");
    for (body, label) in [("{", "syntax"), (r#"{"model":"claude-sonnet-4-6"}"#, "missing messages")] {
        let response = router
            .clone()
            .oneshot(multimodal_handler_request(
                "/v1/messages/count_tokens",
                body.to_string(),
            ))
            .await
            .unwrap();
        // 当前失败：400/422 text/plain
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{label}");
        assert!(response.headers().contains_key("request-id"), "{label}");
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json envelope");
        assert_eq!(value["type"], "error");
        assert_eq!(value["error"]["type"], "invalid_request_error");
    }
}
```

远程阻塞问题：`COUNT_TOKENS_CONFIG` 是进程级 `OnceLock`，在测试里设置会污染其它测试，所以不给直接的单测草稿。修复后应该把远程调用改成可注入的 async trait，再用 `#[tokio::test(flavor = "current_thread")]` 断言不会 panic。当前实现在 current_thread runtime 上调用 `block_in_place` 会直接 panic，这是 Tokio 文档说明的行为。

### 端到端复现

```bash
BODY='{"model":"claude-sonnet-4-6","max_tokens":64,"messages":[{"role":"user","content":"hello"}],
      "tools":[{"name":"Read","description":"Read a file","input_schema":{"type":"object","properties":{"p":{"type":"string"}}}}]}'

# 1) count_tokens
curl -s http://127.0.0.1:19023/v1/messages/count_tokens -H 'x-api-key: <key>' \
  -H 'content-type: application/json' -d "$BODY"

# 2) 同一 body 的 messages 非流式 usage（关闭 reportedUsage 模拟后比较，或比较 usage 记录中的 raw input）
curl -s http://127.0.0.1:19023/v1/messages -H 'x-api-key: <key>' \
  -H 'content-type: application/json' -d "$BODY" | jq .usage

# 3) 错误信封
curl -si http://127.0.0.1:19023/v1/messages/count_tokens -H 'x-api-key: <key>' \
  -H 'content-type: application/json' -d '{'
curl -si http://127.0.0.1:19023/v1/messages/count_tokens -H 'x-api-key: <key>' -d '{}'   # 415
```

远程计数：把 `countTokensApiUrl` 指向本地一个延迟 10 秒响应的 HTTP 服务，并发发送 `worker_threads` 个 `/v1/messages` 请求。可以观察到其余请求（包括 `/health`）的延迟明显上升，同时 mock 服务收到完整的 prompt。

## 修复方案

### 候选方案

A. 只修错误信封和 async 远程调用，启发式不动。改动最小，但口径不一致和非单调问题都还在。

B. 统一估算器（推荐）：一个 `TokenEstimator` 同时服务 count_tokens 和 messages 的回退路径，算法做到请求级、单调；可以额外用上游 `contextUsage` 做按模型的校准。

C. 实现真实 tokenizer：Claude 的 tokenizer 没有公开，没法做到精确，不可行。

### 推荐方案

采用 B：

1. **统一估算器**：新建 `token::estimate_request(&EstimateInput)`，count_tokens 和 messages（本地、WebSearch、外部池投影、fallback refresh）都调用它，输入是同一份经过 prompt steering 之后的 payload。
   - 片段只累加原始的 char units，最后对请求整体做一次 `/4` 并乘一次系数。系数要么去掉分段（统一 1.0 到 1.1），要么改成单调的连续函数。
   - 继续保留图片和文档的专门估算（`src/token.rs:329-388`）。
   - 修正注释中 4.5 / 4.0 的不一致。
2. **按模型校准（可选第二阶段）**：messages 路径拿到 `contextUsageEvent` 后，记录 `actual / estimated` 的比例，按模型维护有上限的 EMA，count_tokens 把这个比例乘到估算结果上。这样两者的差异会收敛到"同一个会话的系统开销"，不再是两套不同算法之间的差距。比例要夹在 0.5–2.0 之间，避免异常样本把结果拉偏。
3. **远程计数**：
   - 改成真正的 async 调用（`.await`），client 放进 `OnceLock<reqwest::Client>` 复用；
   - 超时从 300 秒降到秒级（例如 3 秒连接、5 秒总时长），失败时回退本地估算，并加熔断；
   - 默认只在 `count_tokens` 端点使用，messages 热路径不再调用远程；如果确实需要，再单独加一个显式开关；
   - 配置文档和启动日志里写明"会把完整 prompt 发送到该地址"。
4. **错误信封**：count_tokens 改用和 `MessagesBody` 同类的 bytes extractor，然后 `serde_json::from_slice`；失败时返回 `400 invalid_request_error`，并带 `request-id`；body 超限继续返回 413 的规范信封。
5. **字段**：`CountTokensRequest` 增加 `thinking` / `tool_choice`（可选），估算时把它们带来的系统开销计入；未知模型按 [P17](17-error-status-and-envelope-deviations.md) 的映射返回 404。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)，只使用 `127.0.0.1:19023` 指定测试实例和隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，Cargo 通过 `feature/tests/run-cargo-scoped.sh` 运行，不得影响本机正在运行的 Claude Code CLI 与服务。

- 单调性：长度 1..10k 的 ASCII 文本和 CJK 文本，估算值都不下降。
- 片段无关性：同一文本切成 1、10、100 段，估算差值不超过 10%。
- 一致性：同一 payload 下，`count_tokens` 与 messages 本地回退估算（`input_tokens` 字段来源为 RequestEstimate 时）完全相等。有校准时，用 fake upstream 回放 `contextUsageEvent`，5 轮后 count_tokens 与 messages 的 raw input 差异不超过 15%。
- 远程：用 mock 延迟 10 秒的远程服务，count_tokens 在 5 秒内回退本地估算；并发 64 个 `/v1/messages` 时 mock 命中数为 0（热路径不调用）；在 `current_thread` runtime 下不 panic。
- 信封：语法错误、缺少字段、字段类型错误、缺少 `Content-Type` 四类请求，都返回 `400 invalid_request_error` JSON 并带 `request-id`。
- 五个入口前缀都覆盖；prompt steering 开和关两种情况下都符合"count_tokens 与 messages 用同一份 payload"。

## 兼容性与风险

- 去掉 1.5 倍系数后，短对话的 count_tokens 数值会下降。依赖"高估留余量"的客户端，压缩触发时机会推后。靠近上下文上限时，可以用第 2 步的校准加上固定安全余量来补偿。
- messages 本地回退估算变了，会影响没有上游 usage 时的计费或配额估算。需要和 usage 相关的 issue 协调上线，并在发布说明里标注。
- 把远程计数移出热路径，会改变已经配置 `countTokensApiUrl` 的部署的行为（messages 不再调用远程）。需要在配置迁移说明里写清楚；如果有人依赖远程结果计费，要提供显式开关。
- 415 改成 400 属于状态码变化，只影响错误路径，风险低。
