# Social/Gmail 凭据 api_protocol_error

Status: `fixed-for-json-labeled-eventstream / final-candidate-validated / production-body-fingerprint-still-recommended`

Severity: P1

## 状态

fixed for confirmed proxy-side protocol bug; production recurrence still needs body fingerprint if it reappears

## 影响

- 生产机器：152.53.194.142（本次只读排查）
- 影响账号：3 个 `gmail.com` 结尾的 `social` 凭据，已由管理员手动禁用以止血
- 影响路径：正式模型推理路径 `/v1/messages`、`/ha/v1/messages`、`/cc/v1/messages`
- 主要用户可见错误：`api_protocol_error`

## 现象

这三个 personal/social gmail 账号在管理台点击“测试”可以成功，但正式流量里经常失败。失败记录不是 401/403、不是 token refresh failure、不是额度禁用，而是上游推理请求返回：

```text
upstream_status=200
content_type=json
reason=api_protocol_error
```

也就是：HTTP 层成功，但响应不是服务端正式推理链路期望的 Kiro eventstream 响应。

## 生产证据

本次证据根目录：

```text
tmp/prod-evidence/20260725-004125-152-53-194-142-gmail-api-protocol
```

关键脱敏事实：

- 远端当前运行镜像版本是 `0.0.114`，revision `18b286efa47759b95b581f76a465a2bd9cb02983`。
- 3 个 gmail 凭据 id：`448`、`449`、`450`。
- 三个凭据当前都是 `disabled=true`，`disabled_reason=Manual`，禁用时间约 `2026-07-25 00:39:40~00:39:44 +08`。
- `failure_count=0`、`refresh_failure_count=0`；没有证据表明系统自动禁用或 refresh token 失败。
- 三个凭据都是 `auth_kind=social`，订阅显示 `KIRO PRO+`，`region/api_region=us-east-1`。
- 三个凭据没有持久化模型限制；当前生产 schema/记录里没有可用于调度过滤的 `supported_models` 限制。
- 最近 24 小时内，这组三个凭据相关 usage 记录：`16` 条，其中 `1` 条成功、`15` 条错误；记录到的上游尝试共 `44` 次。
- 失败样本显示单个正式请求会轮询 3 个 gmail 凭据，三次上游均返回 `200`，三次均被归类为 `protocol_error`，最终失败：
  - `req_01mkqLsh7UcRng5eeaFEDHDb`: `/v1/messages`, stream, `claude-opus-4.8`, attempts `[449,450,448]`, statuses `[200,200,200]`, actions `["retry","retry","fail"]`, duration `293768ms`。
  - `req_01R3cnYmxMjvYXC3UPm7ePRG`: `/v1/messages`, stream, `claude-opus-4.8`, attempts `[450,448,449]`, statuses `[200,200,200]`, actions `["retry","retry","fail"]`, duration `29562ms`。
  - `req_01wYmrDJQHp3J6uYwoedqzWJ`: `/ha/v1/messages`, non-stream, `claude-haiku-4.5`, attempts `[450,448,449]`, statuses `[200,200,200]`, actions `["retry","retry","fail"]`, duration `4751ms`。
- 一个成功样本是 `req_01ffPbHvJwULrF3NvJSQa1CM`: `/v1/messages`, non-stream, `claude-opus-4.6`, credential `449`, duration `1021ms`。
- 小请求也会失败：`req_01wY...` 的 payload guard 记录显示 finalBytes 约 `3448`、history entries `2`、无图片/无 tool 清理，input tokens `978`，仍返回 `200 JSON api_protocol_error`。
- 142 的全局外部池开关为 `externalPools.externalPoolsEnabled=false`；虽然外部池表里有一个启用池，但正式失败样本 `externalAttempts=0`，不会由外部池接管。
- `credentialProtocolErrorCooldownSecs=3`，协议错误只进入短暂临时冷却，不会持久禁用，也不会形成账号组级别隔离。

## 源码解释

正式推理链路在 `src/kiro/provider.rs` 中把响应分为：

- `2xx + eventstream`：交给下游解析，可能成功。
- `2xx + json/other/missing content-type`：不是成功推理响应，读 body 后进入 `classify_non_eventstream_body`。
- JSON body 如果没有标准 `error/code/reason/__type` 等可识别错误字段，会被归类为 `ApiUpstreamFailureKind::Protocol`，诊断 reason 为 `api_protocol_error`。

管理台测试路径在 `src/admin/service.rs::test_credential`：

- 使用指定 credential id，绕过正式调度负载均衡。
- 使用很小的默认 prompt `hi`。
- 单次模型测试和正式业务请求的 `/cc`、`/ha`、流式、thinking/signature、长上下文、多轮会话、工具调用组合不同。
- Admin 测试不进入 `usage_records`，所以“测试成功”不能直接作为正式流量成功率证据。

## 当前判断

最可能原因不是账号登录失效，而是这组三个 social/gmail personal 凭据在某些正式推理请求形态下，上游返回了 HTTP 200 JSON 非 eventstream 响应。服务端没有保存原始上游 JSON body，因此目前不能精确判断该 JSON 是：

- 账号/订阅/能力不满足的提示；
- profileArn / region / endpoint 组合异常提示；
- social/personal 账号专用的非推理响应；
- thinking/signature 或路径兼容导致的非 eventstream 响应；
- 或上游临时协议变更。

但可以确定：

- 这不是 refresh token 失败。
- 这不是系统自动把账号持久禁用。
- 这不是单纯大请求、图片、tool 清理导致，因为小请求也复现。
- 这不是外部池已接管但失败；142 全局外部池实际上未启用，失败样本没有 external attempts。
- 管理台“测试成功”只说明某个单点 liveness/request shape 成功，不代表正式流量所有协议组合可用。

## 根因

当前能确认的根因有两层：

1. 生产记录只保存了脱敏后的 `upstream_status=200 content_type=json reason=api_protocol_error`，没有保存 JSON body fingerprint/top-level keys，因此历史样本无法区分“真实 JSON 错误 envelope”和“content-type 标错但 body 是 binary EventStream”。
2. 旧 provider header 阶段过早信任 `content-type: application/json`，会把部分 Kiro 合法的 JSON-labeled binary EventStream 当作非 eventstream 成功体并归类为协议错误。当前工作树已改为交给 handler body sniff。

## 为什么手动禁用是合理止血

在 0.0.114 + 当前配置下：

- protocol error 只冷却 3 秒。
- 没有账号组/本地池级别 protocol 熔断。
- 三个 gmail 凭据没有 supported model 限制，会继续进入候选池。
- 一个正式请求可能连续打三个 gmail 凭据，形成上游尝试放大。
- 全局外部池关闭，失败不会被外部池接管。

所以手动禁用这三条凭据是合理止血，避免反复用同一组 personal 账号打非预期协议响应。

## 建议修复/优化

### 2026-07-25 已补充的协议兼容修复

生产只读证据只能看到 `upstream_status=200 content_type=json reason=api_protocol_error`，没有保存原始 body。结合后续本地协议复核，发现一种真实 Kiro 行为不能按 header 直接判失败：

```text
HTTP 200
content-type: application/json
body: 实际是 AWS binary EventStream frames
```

旧逻辑把 `2xx + application/json` 视为非 eventstream 成功体，进而按 `api_protocol_error` 处理。当前修复改为：

- `src/kiro/provider.rs`
  - 对 `2xx + application/json` 不再在 provider header 阶段直接判定终局失败；
  - 只记录 `response_headers_received`，把 response body 交给 stream/non-stream handler；
  - handler 通过 body sniff 区分“JSON 错误 envelope”和“JSON header 标错的 binary EventStream”。
- `src/anthropic/handlers/tests.rs`
  - 新增 `BinaryEventStreamWithJsonContentType` fixture；
  - stream/non-stream 各 5 轮，确认 JSON-labeled binary EventStream 会成功解析，不输出 error event。
- `src/kiro/provider.rs` tests
  - 新增 `json_content_type_response_headers_remain_for_handler_sniffing_for_five_rounds`；
  - 确认 provider 对 JSON content-type 的 2xx response 只释放给 handler sniff，不提前写 success，也不增加 credential success count。

已跑验证：

```text
RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh protocol-billing-focused -- \
  bash -lc 'cargo test --locked --all-targets provider_status_and_non_eventstream_matrix_is_private_typed_and_bounded && cargo test --locked --all-targets handler_binary_eventstream_with_json_content_type_is_body_sniffed_for_five_rounds && cargo test --locked --all-targets external_pool_billing_matches_dashed_opus_request_to_dotted_pricing_model'

结果：
- provider_status_and_non_eventstream_matrix_is_private_typed_and_bounded: passed
- handler_binary_eventstream_with_json_content_type_is_body_sniffed_for_five_rounds: passed
- external_pool_billing_matches_dashed_opus_request_to_dotted_pricing_model: passed
- scoped target cleaned: size_kib=1730452 removed=true reservation_released=true
```

限制：

- 该修复覆盖“header 标错但 body 是合法 EventStream”的协议兼容问题。
- 如果上游真的返回 JSON 错误 envelope，handler 仍应 fail-closed，不应当成成功。
- 生产那 3 个 Gmail/social 账号的历史错误没有原始 body，因此不能反证所有 `api_protocol_error` 都由这个 header 标错引起；仍需要后续受控 body fingerprint/top-level keys 诊断来区分其他 social/personal 能力问题。

1. 增强“测试账号”语义：
   - UI 明确区分“账号认证/订阅可用”和“正式推理协议可用”。
   - 支持按正式路径测试：`/v1`、`/cc`、`/ha`，stream/non-stream，thinking 开关，小/中 payload。
   - 测试成功后可选择写入 `supported_models` 或 capability 标记，供调度过滤。

2. 增加 protocol error 的安全隔离：
   - 对同一 credential/model/endpoint 的连续 `2xx JSON non_eventstream` 设置更长冷却。
   - 对同一 capability cohort 的多个凭据在短窗口内连续同类失败时，打开组级 circuit，避免一个请求连续打完整个账号组。
   - 组级 circuit 不应持久禁用账号，只应临时阻止正式调度并允许 admin 手动测试。

3. 补充诊断但保持隐私：
   - 对 `2xx JSON non_eventstream` 存储脱敏 body fingerprint、顶层 keys、标准错误字段、body size、content-type、endpoint、model、stream、thinking/source flags。
   - 默认不存原始 body；需要管理员显式打开短时采样才能保存脱敏截断样本。

4. 外部池配置/路由：
   - 如果希望本地 transient/protocol 失败后进入外部池，必须开启 `externalPools.externalPoolsEnabled=true`。
   - 外部池是否接管 protocol error 应按配置明确显示在 UI 和 usage 里，避免“表里有启用池但全局关闭”的误解。

5. supported model/capability：
   - personal/social 凭据导入后默认不要假设所有模型/路径都可用。
   - 支持基于实际 liveness matrix 写入模型/路径能力，调度时严格过滤。

## 后续验证建议

在不影响生产账号的前提下，用本地/测试账号复现：

- social 账号，小 prompt，`/v1` non-stream。
- social 账号，小 prompt，`/v1` stream。
- social 账号，小 prompt，`/ha` non-stream/stream。
- social 账号，`/cc` stream。
- 启用/禁用 thinking signature 路径。
- 长上下文但无 tool/图片。
- tool/图片 payload。
- 连续 3 个同类 social 凭据返回 `2xx JSON non_eventstream` 时，验证不会逐个账号打满，不会持久禁用，只临时隔离并可 fallback。

## 复现

最小本地复现不需要真实 Gmail 账号：

1. fake Kiro upstream 返回 `HTTP 200`。
2. response header 写 `content-type: application/json`。
3. body 写入合法 AWS binary EventStream frames，包含正常 assistant response/context usage/metering frames。
4. 修复前 provider 在 header 阶段把它归类为 `api_protocol_error`。
5. 修复后 provider 仅把 response 交给 handler，handler sniff body 后正常解析，stream/non-stream 均返回成功。

生产级复现仍需要受控账号或短时脱敏采样：

1. social/Gmail credential。
2. `/v1`、`/cc`、`/ha` 三类正式路径。
3. stream/non-stream、thinking on/off、小 prompt/长上下文/tool/图片组合。
4. 记录脱敏 body fingerprint/top-level keys，确认失败样本是 JSON 错误 envelope 还是 JSON-labeled binary EventStream。

## 当前限制

生产 usage 里没有保存原始上游 JSON body，容器 stdout 在目标窗口没有日志。因此本轮不能确认上游 JSON 的精确业务语义。要进一步精确定位，需要：

- 在测试环境抓取同类响应 body；或
- 临时启用受控、脱敏、限量的 `2xx JSON non_eventstream` 诊断采样；或
- 用户明确授权对一个已隔离账号做单次指定模型/路径真实调用。

## 2026-07-25 最终候选验证补充

最终候选二进制：

```text
kiro-rs sha256=25ea01fb741bdffb103fa95397f0fb29b60c8bffee9267741f563f388ae237a4
local service=existing 127.0.0.1:9022
Claude Code CLI=2.1.197
```

已完成验证：

- 完整 Rust bin 测试：`1784 passed / 0 failed / 6 ignored`。
- feature docs：`50 issue documents / 123 relative links` 通过。
- Node contract：`261 passed / 22 skipped / 0 failed`。
- build artifact inventory：`targets=0 reservations=0 target_processes=0 blockers=0`。
- direct stream：真实本地 social/IDE 凭据成功，`status=success`，`routeSubtype=local_success`，`pricingModel=claude-haiku-4-5`，`kiroMeteringUsage=0.006603686633499171`。
- direct non-stream：真实本地 social/IDE 凭据成功，`status=success`，`routeSubtype=local_success`，`pricingModel=claude-haiku-4-5`，`kiroMeteringUsage=0.003777497379767828`。
- direct `thinking.adaptive + output_config.effort=max`：真实 thinking block/delta 和 `thinking_tokens=4`，usage success，`pricingModel=claude-sonnet-4-6`，metering 非零。
- Claude Code CLI simple/tool/thinking/multi-turn/MCP：均通过，usage 非零，没有 `Tool results provided`、`<function_results>`、`*Hashxxxxxxxx`、`user Continue` 等内部泄漏指纹。
- 图片：标准 RGB PNG 成功；伪 PNG 本地拒绝且 `kiroMeteringUsage=0`；1x1 gray+alpha PNG 被上游 400 拒绝但 payload guard 显示未被本地改写。
- WebSearch：server_tool_use 和 web_search_tool_result 正常，message_stop 和 usage 正常。
- fake-upstream load/chaos：L1 fake smoke、L3 9/9、L4 12/12、L5 60s+60s idle 全部通过；错误爆发、429/500、invalid-tool、client-drop、mixed-chaos 后均能恢复。

本轮结论：

1. 已确认并修复一个足以解释大量 `upstream_status=200 content_type=json reason=api_protocol_error` 的代理侧 bug：旧 provider 在 header 阶段把 `2xx + application/json` 直接当作非 EventStream 协议错误；新逻辑把 body 交给 handler sniff，能够正确处理 JSON-labeled binary EventStream。
2. 已确认 EOF 无 `messageStatus` 但有 `contextUsageEvent`/`meteringEvent` 的 Kiro 成功响应不再被误判为 protocol error。
3. 本地使用真实 social/IDE 凭据的 stream、non-stream、thinking、CLI、tool、MCP、图片、WebSearch 均没有复现“上游成功但 usage 全错误”的问题。
4. 如果生产再次出现 `2xx JSON api_protocol_error`，优先看 body fingerprint/top-level keys。若 body 是合法 EventStream，应由本修复解决；若 body 是真实 JSON 错误 envelope，则属于账号 capability/profile/region/订阅/路径组合问题，需要按 body fingerprint 继续分型，不应再盲目换号重试整组账号。

## 2026-07-26 当前候选补证

当前冻结候选：

```text
kiro-rs sha256=7268b3e722f03a40179d205e7b5917b86d696cd8bf1d5f6533d3b1347ea30bec
```

补充验证：

- `2xx + application/json + binary EventStream body` 的 stream/non-stream handler sniff 回归仍在 C0/全量测试覆盖中。
- 真实 Claude Code CLI fake-upstream bare/long-session/thinking-wire 均通过，未出现 `api_protocol_error`、工具历史泄漏或 thinking/output_config 组合错误；见 [candidate-c0-claude-cli-real-protocol-20260726](../evidence/candidate-c0-claude-cli-real-protocol-20260726.md)。
- fake-upstream L3/L4/L5 通过，说明协议错误/invalid-tool/429/500/client-drop 不会让后续正常请求卡死；见 [candidate-c0-load-chaos-20260726](../evidence/candidate-c0-load-chaos-20260726.md)。

当前真实上游 success smoke 未执行：本地 `9022` 的持久化凭据已全部处于 disabled/runtime bad state（TemporarilySuspended/Manual/QuotaExceeded），继续真实调用会增加账号风险，不应把失败账号当成产品协议回归证据。发布后若生产仍出现 `upstream_status=200 content_type=json reason=api_protocol_error`，必须优先采集脱敏 body fingerprint/top-level keys，区分真实 JSON error envelope 与 JSON-labeled EventStream。
