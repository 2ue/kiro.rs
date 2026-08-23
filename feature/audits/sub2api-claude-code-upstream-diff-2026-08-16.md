# sub2api Claude Code 上游处理与当前项目外部池差异分析

日期：2026-08-16

范围：
- 当前仓库：`/Users/yuanfeijie/Desktop/project/2ue_kiro.rs`，本地 `main` 已同步到 `origin/main`，工作区在分析前为干净状态。
- 对比仓库：`/Users/yuanfeijie/Desktop/procode/sub2api`。
- 分析重点：在 sub2api 中接入 Claude/Claude Code 兼容上游的 `base_url` 和 key 后，请求如何转发到上游，包含 URL、认证、header、Claude Code 指纹、TLS 指纹、body 处理、重试/冷却、流式响应和诊断；再与当前 `2ue_kiro.rs` 的外部池实现对比。

## 总结

sub2api 的 Claude Code 上游转发更偏“Anthropic/Claude Code wire fidelity”：它围绕真实 Claude Code 流量维护固定 header 白名单、wire casing、`x-stainless-*` 指纹、`metadata.user_id` 重写、`x-client-request-id`、可选 TLS ClientHello 指纹，并且把 `/v1/messages` 和 `/v1/messages/count_tokens` 都按同一套上游策略处理。

当前 `2ue_kiro.rs` 的外部池更偏“调度与容量治理”：它支持外部池优先级、并发租约、跨池重试、同池短重试、流式提交前 failover、模型级冷却、本地 rescue、usage 投影和 wire/debug 诊断。它对 Claude Code wire 指纹没有 sub2api 那么强的专用建模，外部池 header 使用黑名单过滤而不是 Anthropic/Claude Code 允许名单，也没有 per-pool TLS 指纹或 Claude Code mimic 逻辑。

如果目标是“接入一个 Claude Code/Anthropic 兼容 baseurl + key 的外部池，并最大程度减少因 header/指纹不一致导致的上游误判”，最值得借鉴 sub2api 的是：

1. 外部池 outbound header 从黑名单过滤改成 Anthropic/Claude Code 允许名单，至少补充删除 `cookie`、`x-goog-api-key`，并增加测试。
2. 对 Anthropic API key 类型的外部池提供一个更明确的“Anthropic passthrough profile”：只替换认证、补 `content-type` / `anthropic-version`，保留必要 Claude Code header，但不随意 body mimic。
3. 可配置是否为 Anthropic 兼容上游追加 `?beta=true`，并明确 `anthropic-beta` 的最终来源。
4. 如要模拟真实 Claude Code OAuth 流量，再考虑 sub2api 的 `x-stainless-*` / `User-Agent` / `metadata.user_id` / TLS fingerprint；仅 API key 外部池不应默认引入 OAuth mimic。

## 证据索引

sub2api 关键代码：
- `backend/internal/handler/gateway_handler.go:122`：`POST /v1/messages` 入口。
- `backend/internal/service/gateway_forward.go:89`：主 Forward 流程、Claude Code mimic 判断、body 清理、重试和 failover。
- `backend/internal/service/gateway_upstream_request.go:21`：普通上游请求构造。
- `backend/internal/service/gateway_anthropic_passthrough.go:296`：Anthropic API key passthrough 请求构造。
- `backend/internal/service/gateway_count_tokens.go:430`：`count_tokens` 上游请求构造。
- `backend/internal/service/gateway_service.go:423`：允许透传 header 白名单。
- `backend/internal/service/header_util.go:8`：Claude CLI wire casing / header 顺序。
- `backend/internal/pkg/claude/constants.go:13`：Claude Code beta/header 常量。
- `backend/internal/service/identity_service.go:75`：账号级 fingerprint 缓存和 `metadata.user_id` 重写。
- `backend/internal/service/account_header_override.go:12`：账号级 header override。
- `backend/internal/service/account.go:892`、`:1837`、`:1923`、`:2006`：base URL、API key passthrough、TLS fingerprint、custom base URL 开关。
- `backend/internal/service/tls_fingerprint_profile_service.go:171`：TLS profile 解析。
- `backend/internal/pkg/tlsfingerprint/dialer.go:55`：Node.js/Claude Code TLS 指纹。
- `backend/internal/repository/http_upstream.go:231`、`:1327`：`DoWithTLS` 和 fingerprint transport。

当前项目关键代码：
- `src/external_pool.rs:196`：外部池认证类型 `Bearer` / `XApiKey`。
- `src/external_pool.rs:455`：外部池配置结构，包括 `base_url`、`api_key`、`auth_type`、body mode、stream retry、auto disable。
- `src/external_pool.rs:6611`：外部池单次请求构造。
- `src/external_pool.rs:10098`：外部池 `/models` URL 构造。
- `src/external_pool.rs:10108`：外部池 `/v1/messages` URL 构造。
- `src/external_pool.rs:10157`：外部池 header 过滤和认证注入。
- `src/external_pool.rs:10515`：当前 outbound header 黑名单。
- `src/external_pool.rs:11066`：外部池错误分类。
- `src/external_pool/retry_pipeline.rs:50`：同池/跨池重试判断。
- `src/model/config.rs:2797`：外部池全局调度、重试、冷却和诊断配置。
- `src/anthropic/handlers/request_entry.rs:62`：raw external route 保留补齐 `max_tokens` 后、兼容清理前的请求体。
- `src/external_pool/body_pipeline.rs:21`：外部池 body prepare。
- `src/anthropic/converter.rs:292`：从 `metadata.user_id` 提取本地 sticky session。
- `src/anthropic/handlers.rs:10894`：当前 `count_tokens` 本地计算。
- `src/anthropic/handlers/tests.rs:2895`：测试要求 `count_tokens` 不打到上游。
- `src/storage/postgres.rs:10602`：外部池 base URL 保存校验。

## 差异矩阵

### 1. 配置模型

sub2api：
- 账号模型按平台和账号类型区分：Anthropic `api_key`、`oauth`、`setup_token`、`service_account`、Bedrock 等。
- API key 账号的 `credentials.base_url` 决定上游 base URL；空值默认 `https://api.anthropic.com`。
- OAuth/setup token 不走 API key 的 `base_url`，而是通过 `extra.custom_base_url_enabled` + `extra.custom_base_url` 开启自定义中继。
- Anthropic API key 账号可通过 `extra.anthropic_passthrough` 开启“自动透传，仅替换认证”。
- OAuth/setup token 可通过 `extra.enable_tls_fingerprint`、`extra.tls_fingerprint_profile_id` 开启 TLS 指纹。

当前项目：
- 外部池以 `ExternalPool` 为核心，字段包括 `base_url`、`api_key`、`auth_type`、`max_concurrent_requests`、body mode、模型映射、stream retry、auto disable 等。
- `auth_type` 支持 `bearer` 和 `x_api_key`，默认是 `bearer`。
- 外部池没有账号类型概念，不区分“Anthropic API key passthrough”与“Claude OAuth mimic”。
- 外部池没有 per-pool TLS fingerprint、header override、Claude Code fingerprint profile 字段。

影响：
- 当前项目配置面更适合多外部池调度；sub2api 配置面更适合 Anthropic/Claude Code 账号语义。
- 如果当前项目要接更多 Claude Code 兼容服务，建议增加“外部池 profile”而不是把所有外部池默认改成 sub2api 行为。

### 2. URL 构造与 base_url 语义

sub2api：
- 默认 messages URL 是 `https://api.anthropic.com/v1/messages?beta=true`。
- API key 账号配置 `base_url` 后，上游 URL 是 `validated_base_url + "/v1/messages?beta=true"`。
- `count_tokens` 对应 `validated_base_url + "/v1/messages/count_tokens?beta=true"`。
- OAuth custom relay 使用 `buildCustomRelayURL(baseURL, path, account)`，路径后追加 `?beta=true`，如果账号配置了 proxy，会把 proxy URL 作为 query 参数传给中继。
- `validateUpstreamBaseURL` 支持安全 allowlist：开启 URL allowlist 时要求 HTTPS、允许 hosts、是否允许 private hosts；关闭 allowlist 时仍做 URL 格式校验，并由配置决定是否允许 HTTP。

当前项目：
- 外部池 messages URL 由 `external_pool_messages_url(base_url)` 构造：
  - base URL 以 `/v1` 结尾，则拼 `/messages`。
  - 否则拼 `/v1/messages`。
- 当前不会追加 `?beta=true`。
- `preserve_path` 字段存在，但测试明确期望 `/cc/v1/messages` 也固定转到 pool 的 `/v1/messages`。
- 外部池 models URL 类似：`/v1/models` 或 `/models`。
- 保存外部池时只校验 URL 非空、scheme 是 `http/https`、并发大于 0；没有 sub2api 那种 allowlist/private host 安全策略。

影响：
- 如果外部池目标是标准 Anthropic API，当前不带 `?beta=true` 与 sub2api 不同。多数 Anthropic 兼容实现可能不要求该 query，但如果某些中间层把 `beta=true` 当兼容开关，当前请求会不一致。
- 当前允许 HTTP，便于内网 mock/自建服务；安全性弱于 sub2api 的 allowlist 模式。

建议：
- 不建议全局强制加 `?beta=true`，避免破坏已有外部池；可以增加 per-pool 开关或 Anthropic profile 默认行为。
- 如外部池允许公网配置，建议补 URL allowlist/private-host 策略，至少对生产模式启用。

### 3. 上游认证头

sub2api：
- `GetAccessToken` 对 `AccountTypeAPIKey` 读取 `credentials.api_key`，返回 token type `apikey`。
- Anthropic API key 默认注入 `x-api-key: <token>`。
- 可通过 `extra.anthropic_apikey_auth_scheme = authorization_bearer` 切换为 `Authorization: Bearer <token>`。
- 构造上游请求时会删除/覆盖入站认证残留，尤其 passthrough 分支显式删除：
  - `authorization`
  - `x-api-key`
  - `x-goog-api-key`
  - `cookie`
- OAuth/setup token 则使用 `authorization: Bearer <access_token>`。

当前项目：
- `ExternalPoolAuthType` 支持：
  - `Bearer`：注入 `Authorization: Bearer <api_key>`。
  - `XApiKey`：注入 `x-api-key: <api_key>`。
- `forward_headers` 先按 `should_forward_header` 过滤客户端 header，再插入认证。
- 当前黑名单排除了 `authorization` 和 `x-api-key`，所以这两个不会从客户端透传。
- 当前黑名单未显式排除 `x-goog-api-key` 和 `cookie`。

影响：
- 认证模式本身当前项目已经覆盖 sub2api 的 API key 两种方式。
- 风险点在残留敏感 header：当前可能把客户端 `cookie` 或 `x-goog-api-key` 透传给外部池。即使多数 Claude Code 请求不会带这些字段，代理层不应依赖客户端不发送。

建议：
- 最小修复：把 `cookie`、`x-goog-api-key` 加入 `should_forward_header` 黑名单，并加测试。
- 更彻底：改为 sub2api 式允许名单。

### 4. Header 透传策略

sub2api：
- 使用显式允许名单，允许项主要是 Claude/Anthropic SDK 所需 header：
  - `accept`
  - `x-stainless-retry-count`
  - `x-stainless-timeout`
  - `x-stainless-lang`
  - `x-stainless-package-version`
  - `x-stainless-os`
  - `x-stainless-arch`
  - `x-stainless-runtime`
  - `x-stainless-runtime-version`
  - `x-stainless-helper-method`
  - `anthropic-dangerous-direct-browser-access`
  - `anthropic-version`
  - `x-app`
  - `anthropic-beta`
  - `accept-language`
  - `sec-fetch-mode`
  - `user-agent`
  - `content-type`
  - `accept-encoding`
  - `x-claude-code-session-id`
  - `x-client-request-id`
- API key / 非 mimic 路径按允许名单透传。
- OAuth mimic 路径跳过客户端 header 透传，改用固定 Claude Code mimic headers，避免客户端 `x-stainless-*`、`anthropic-beta`、`user-agent`、session/request id 与代理注入值冲突。

当前项目：
- 使用黑名单：
  - `host`
  - `connection`
  - `content-length`
  - `transfer-encoding`
  - `keep-alive`
  - `proxy-authenticate`
  - `proxy-authorization`
  - `te`
  - `trailer`
  - `upgrade`
  - `authorization`
  - `x-api-key`
  - `accept-encoding`
- 其它 header 默认都透传。
- 会补 `anthropic-version: 2023-06-01`。
- 缺 `content-type` 时补 `application/json`。

影响：
- 当前项目对未知 header 更宽松，兼容性强，但安全性和 wire 一致性弱。
- 对 Claude Code 兼容上游而言，过宽的 header 透传可能引入“客户端/代理/上游身份不一致”的问题。
- sub2api 的允许名单更适合 Anthropic/Claude Code 上游；当前黑名单更适合一般 HTTP API 代理。

建议：
- 对外部池增加 header forwarding profile：
  - `generic`：保留当前黑名单策略，兼容已有外部服务。
  - `anthropic_passthrough`：使用 sub2api 允许名单，删除认证/cookie 类残留，补 Anthropic defaults。
  - `claude_code_mimic`：只在明确需要 OAuth mimic 时启用，默认不启用。

### 5. Header wire casing、顺序和 Claude Code 默认 headers

sub2api：
- 维护 `headerWireCasing`，把 Go canonical header 恢复成真实 Claude CLI 抓包中的大小写，例如：
  - `X-Stainless-OS`
  - `x-stainless-helper-method`
  - `x-app`
  - `X-Claude-Code-Session-Id`
  - `x-client-request-id`
- 维护 `headerWireOrder` 用于 debug 和抓包对比。
- `claude.DefaultHeaders` 对齐 Claude Code CLI：
  - `User-Agent: claude-cli/<version> (external, cli)`
  - `X-Stainless-Lang: js`
  - `X-Stainless-Package-Version: 0.94.0`
  - `X-Stainless-OS: Linux`
  - `X-Stainless-Arch: arm64`
  - `X-Stainless-Runtime: node`
  - `X-Stainless-Runtime-Version: v24.3.0`
  - `X-Stainless-Retry-Count: 0`
  - `X-Stainless-Timeout: 600`
  - `X-App: cli`
  - `Anthropic-Dangerous-Direct-Browser-Access: true`
- `applyClaudeCodeMimicHeaders` 会强制覆盖这些 header，流式请求额外补 `x-stainless-helper-method: stream`，并确保每个请求有新的 `x-client-request-id`。

当前项目：
- 使用 `http::HeaderMap` + `reqwest`，没有维护 Claude CLI 抓包级 wire casing 或 header 顺序。
- 不会主动生成 `x-client-request-id`。
- 不会主动生成或覆盖 `x-stainless-*`。
- Claude Code UA 只用于本地流式 keepalive 策略，不用于外部池请求指纹。

影响：
- 如果外部池背后是对 Claude Code 请求形态敏感的 Anthropic/OAuth 中继，当前项目的 header 指纹不如 sub2api 稳定。
- 如果外部池只是普通 Anthropic API key 兼容服务，不强制 mimic 反而更稳，避免伪造错误身份。

建议：
- 不要把 Claude Code mimic header 全局加到所有外部池。
- 可以增加 per-pool `header_profile = anthropic_passthrough | claude_code_mimic`，默认保持当前行为或选择更保守的 Anthropic passthrough。

### 6. `anthropic-beta` 策略

sub2api：
- 区分 OAuth mimic、OAuth 真客户端、API key、Haiku、count_tokens 等场景，计算最终 `anthropic-beta`。
- 计算顺序明确：
  1. 先计算最终 beta。
  2. 按最终 beta 对 body 做能力维度 sanitize。
  3. 再构造请求和写入 header。
- 如果账号 header override 覆写了 `anthropic-beta`，body sanitize 以覆写值为准，避免 header/body 不对称导致上游 400。
- OAuth mimic 使用完整 Claude Code mimic beta 集合。
- API key 默认 beta 不包含 OAuth beta。

当前项目：
- 外部池透传入口 headers，`anthropic-beta` 不在黑名单中，所以客户端传了就会转发。
- 未传时当前外部池不会主动补 `anthropic-beta`。
- 当前没有按最终 `anthropic-beta` 对外部池 body 做 sub2api 那种 context-management 字段净化。
- 外部池没有账号级 header override，因此也没有“覆写 beta 影响 body sanitize”的逻辑。

影响：
- 当前更尊重客户端请求，但遇到“客户端 body 带 beta 专属字段，header 没带对应 beta”的请求，外部池可能返回 400。
- sub2api 对 Anthropic 兼容细节更强，但策略也更复杂，错误引入会影响更多路径。

建议：
- 如果外部池开启 `anthropic_passthrough` profile，可借鉴 sub2api 的“按最终 beta 做 body sanitize”。
- 默认 profile 不建议主动改写 beta，避免影响非 Anthropic 外部池。

### 7. Claude Code 客户端检测与 mimic

sub2api：
- 检测 Claude Code 不只靠 UA，还看 system prompt 前缀和 `metadata.user_id`。
- 支持多个 Claude Code prompt 前缀：标准 CLI、Agent SDK、file search specialist、compact summary。
- 真实 Claude Code 客户端不做 body mimic，避免破坏真实客户端自带 system prompt、cache_control 和缓存策略。
- OAuth 账号 + 非 Claude Code 客户端才做完整 mimic：
  - 重写 system prompt。
  - 注入/重写 metadata。
  - 规范 cache_control。
  - 可选消息 cache 策略、工具名混淆、tools 最后断点。
  - Header 改成 Claude Code mimic headers。

当前项目：
- 有全局 `CompatProfile`：
  - `claude-code`：默认，保留适配 Claude Code CLI 和 Kiro upstream 的实用改写。
  - `anthropic-strict`：减少代理合成协议和 prompt 改写。
  - `debug`：类似 `claude-code`，但暴露 warning。
- 外部池 raw route 保留兼容清理前的 body；normalized mode 才走当前项目自己的结构化 body prepare。
- 当前没有“外部池 OAuth mimic”概念。
- Claude Code UA 主要用于本地响应 keepalive，不用于决定外部池 header/body mimic。

影响：
- 当前项目适合把 Claude Code 请求转给普通外部兼容池，但不适合伪装一个非 Claude Code 客户端为真实 Claude Code OAuth 客户端。
- sub2api 的 mimic 是为 Anthropic OAuth/Claude Code scoped credentials 服务的，不应直接套到当前所有 external pool API key 场景。

### 8. Body 处理

sub2api API key passthrough：
- 尽量少改 body。
- 仍做保守预清理：
  - `StripEmptyTextBlocks`
  - `FilterWebSearchHistoryBlocks`
  - 按最终 `anthropic-beta` 清理 body 中不被允许的 beta 字段。
- 禁止 400 request-body downgrade retry，避免自动改 body 破坏上游语义。

sub2api 普通 Forward：
- 对 OAuth/mimic/非 passthrough 做更多 body 处理：
  - system prompt rewrite。
  - metadata 注入。
  - cache_control 限制。
  - thinking block 签名错误修复。
  - thinking budget 修复。
  - tool/function signature 相关降级重试。
  - 模型映射。

当前项目：
- raw external route 保留 `effective_raw_body`，定义为“补齐 missing max_tokens 后、兼容清理前”的请求体。
- 外部池 `request_body_mode`：
  - `raw_passthrough`：尽量保留原始 body，可选 raw model rewrite。
  - `normalized`：用当前项目的 `MessagesRequest` 结构重建 body，并 overlay 原始字段，保留 messages/tools/system/metadata 等关键字段。
- normalized 模式支持 payload guard 和 payload guard retry。
- 当前没有 sub2api 的 `metadata.user_id` 重写、Claude OAuth system mimic、按 `anthropic-beta` sanitize context-management 的逻辑。

影响：
- 当前 raw passthrough 与 sub2api API key passthrough 在“少改 body”方向一致。
- 当前 normalized 模式更适合当前项目的 usage/cache/模型映射治理，但比 sub2api API key passthrough 更容易改变 wire body。
- 如果目标外部池对 Claude Code 原始请求敏感，建议使用当前 `raw_passthrough`，并借鉴 sub2api 的 header/认证清理，而不是开启更重的 body mimic。

### 9. `metadata.user_id`、session 和 sticky

sub2api：
- `IdentityService` 为每个账号缓存 fingerprint：
  - `User-Agent`
  - `X-Stainless-*`
  - 随机生成的 `ClientID`
  - 24 小时刷新，7 天 TTL。
- OAuth 账号会用账号 `account_uuid`、fingerprint `ClientID` 和原始 session 生成新的 `metadata.user_id`。
- session hash 使用 `SHA256(accountID::sessionTail)`，避免直接复用客户端原始 session。
- 可开启 `session_id_masking_enabled`，15 分钟内固定伪装 session id。
- 如果出站 header 已有 `X-Claude-Code-Session-Id`，会用 body 中最终 `metadata.user_id` 的 session id 覆盖该 header，保持 body/header 一致。
- sticky session 选择账号时优先使用 `metadata.user_id` 中的 session 信息。

当前项目：
- `converter.rs` 能从 `metadata.user_id` 提取 session UUID，用于本地 Kiro provider 的 conversation/sticky。
- 本地 token manager 有 sticky binding、Redis-backed session binding、sticky fallback、软失败等完整能力。
- 外部池自身没有 sub2api 那种“为上游重写 `metadata.user_id` 和同步 `X-Claude-Code-Session-Id`”能力。
- 外部池调度主要按 pool 可用性、模型支持、路由规则、并发租约、冷却/优先级等执行，不把 Claude Code identity fingerprint 作为上游身份构造的一部分。

影响：
- 当前本地 sticky 和 sub2api 上游身份伪装不是同一层。
- 如果外部池上游依赖 `metadata.user_id` / `X-Claude-Code-Session-Id` 一致性，当前可能完全透传客户端状态；这对 API key passthrough 通常没问题，对 OAuth mimic 场景可能不够。

建议：
- API key 外部池：不建议默认改写 `metadata.user_id`，保持客户端语义。
- OAuth/Claude Code scoped 外部池：需要单独 profile，再引入 sub2api 的 identity rewrite 和 session header sync。

### 10. TLS 指纹

sub2api：
- 只对 Anthropic OAuth/setup token 开放 TLS fingerprint。
- `ResolveTLSProfile`：
  - 未启用返回 nil，走普通 HTTP。
  - profile id > 0 从缓存取。
  - profile id == -1 随机选模板。
  - 启用但无模板时使用内置 Node.js 24.x 默认 profile。
- 底层使用 `utls` 构造 ClientHello，内置注释标明捕获自 Claude Code / Node.js 24.x，并记录 JA3/JA4。
- 默认 ALPN 是 `http/1.1`，`ForceAttemptHTTP2=false`。
- 直连、HTTP proxy CONNECT、SOCKS5 proxy 都支持在隧道内执行 uTLS 握手；HTTPS proxy 回退普通 transport。

当前项目：
- 外部池使用普通 `reqwest::Client::builder().build()`。
- 没有 per-pool TLS fingerprint。
- Cargo 同时具备 rustls/native-tls 相关特性，但这是 TLS backend 能力，不是模拟 Claude Code ClientHello。

影响：
- 当前外部池不具备 sub2api 的网络层指纹伪装能力。
- 对普通 API key 外部池，这通常不是必要能力。
- 对需要模拟 Claude Code OAuth 流量的上游，TLS 指纹可能是重要差异。

建议：
- 不建议把 TLS fingerprint 作为外部池默认能力；实现成本和风险都高。
- 如果确实需要，应设计为 per-pool opt-in，并限制到 Anthropic/Claude Code profile。

### 11. Header override

sub2api：
- Anthropic/OpenAI 仅 API key 账号支持 header override，Grok API key/OAuth 也支持。
- `header_override_enabled` + `header_overrides` 控制。
- 有严格禁止列表：
  - 连接/逐跳头：`host`、`content-length`、`transfer-encoding`、`connection` 等。
  - 认证/session：`authorization`、`x-api-key`、`x-goog-api-key`、`cookie`、`session_id`、`x-claude-code-session-id`、`x-client-request-id` 等。
  - `content-type`、`accept-encoding`、WebSocket 握手头。
- 覆写应用在出站构造最后，确保配置值对同名允许 header 生效。

当前项目：
- 外部池没有 header override 配置。
- 只能靠客户端 header 透传和系统默认补充 `anthropic-version` / `content-type`。

影响：
- 当前不能给某个外部池补充“中间层准入 header”或覆写特定 `anthropic-beta`。
- 但也减少了误配置破坏认证/会话的风险。

建议：
- 如需引入 header override，应复制 sub2api 的阻止列表思路，不能开放任意 header 覆写。
- 建议先实现 header profile/allowlist，再考虑 override。

### 12. `/v1/messages/count_tokens`

sub2api：
- `count_tokens` 构造上游请求，使用与 messages 类似的 base URL、认证、header、fingerprint、mimic 和 `anthropic-beta` 策略。
- API key passthrough 也删除认证残留、补默认 header、应用 header override。

当前项目：
- `count_tokens` 在本地计算。
- 测试明确断言所有 built-in routes 的 `count_tokens` 不应命中上游。

影响：
- 当前行为更稳定、少消耗外部池；但计数结果是本地估算/实现语义，不一定和某个外部池上游完全一致。
- sub2api 更接近“完整代理 Anthropic API”，当前项目更接近“messages 推理代理 + 本地 token count”。

建议：
- 不建议为了对齐 sub2api 直接改动当前 `count_tokens`，因为会破坏现有测试和产品语义。
- 如果需要某些外部池精确计数，可另加 opt-in，不改变默认。

### 13. 重试、failover 和冷却

sub2api：
- API key passthrough 对非 400 可重试错误做同账号重试。
- 400 禁止通用 body downgrade retry；普通 Forward 只对已知可修复 400 做内部修复重试。
- 可 failover 的错误会返回 `UpstreamFailoverError`。
- handler 层记录 `writerSizeBeforeForward`，如果流式内容已经写给下游，禁止 failover，避免 SSE 拼接污染。
- 400 failover 默认关闭，只有配置开启且 `shouldFailoverOn400` 命中时才切换。

当前项目：
- 全局外部池默认：
  - 跨池重试状态码：408、425、429、5xx、502、503、504、529。
  - 同池短重试默认 1 次，延迟 500ms。
  - 网络错误可跨池重试。
  - 协议错误可跨池重试。
  - 流式响应在有效语义输出前可 pre-output retry/failover。
- 400 默认分类为不可重试，payload too large / context full 可通过 payload guard retry 特例内部消化。
- 普通 rate limit、server error、network error 会记录 soft failure，默认只做优先级罚分，不升级池级冷却；`external_pool_transient_failure_cooldown_threshold` 默认 0，避免外部服务抖动时把池快速推入冷却。
- `model_unavailable` 默认模型级冷却，不是池级冷却。
- 只有 `misconfigured_endpoint` 属于硬冷却。
- auto disable 默认关闭，且按 reason/阈值控制。

影响：
- 当前项目在调度容量保护上比 sub2api 更细：同池重试、跨池重试、soft failure 降权、模型级冷却、本地 rescue 都是外部池专用能力。
- sub2api 在“不要流式写出后 failover”和“不要随便对 400 改 body”方面的原则，与当前项目已基本一致。
- 当前项目已经符合“错误尽可能内部消化，不要随便冷却导致调度不足”的方向，尤其是软失败默认不升级硬冷却。

建议：
- 继续保持当前冷却策略，不应为了模拟 sub2api 改成更激进的池级冷却。
- 可以借鉴 sub2api 的更细分 ops error 记录，但不要削弱当前的调度保护。

### 14. 流式响应和响应头

sub2api：
- 对流式响应，只有上游成功且开始写出后才向客户端传 SSE。
- handler 层用 `streamStarted` / writer size 判断能否 failover。
- API key passthrough 会转发 Anthropic passthrough response headers，并解析 stream usage。
- SSE 内 `event:error` 可转成 failover error，但如果已经写出内容，则 handler 禁止切换。

当前项目：
- 流式外部池有 pre-read before commit 机制：在有效内容提交给下游前先读一段，用来发现上游提前 error、HTML、协议污染、空流、idle 等问题。
- 提交前失败可以换池；提交后通过 downstream stream 继续，不再安全 failover。
- 会注入 ping keepalive，防止长 thinking/tool_use 阶段被中间代理或客户端误判断流。
- 会过滤/转发响应头，并关闭代理 buffering。
- 能做 stream usage capture/projection。

影响：
- 当前项目的流式容错比 sub2api 更偏生产调度场景，更复杂也更强。
- sub2api 的原则和当前一致：流式已经写出就不能换账号/换池。

### 15. 诊断记录

sub2api：
- 有 Claude mimic debug、ops upstream error event、可选 upstream error body logging。
- 有 TLS fingerprint debug、header wire order debug。
- API key passthrough会标记 `anthropic_passthrough`，错误事件中记录 passthrough。

当前项目：
- 外部池有 usage debug 和 wire debug：
  - `external_pool_usage_debug_enabled`
  - `external_pool_wire_debug_enabled`
  - 对非流式/流式 usage、wire body、响应形状有专门记录。
- 有外部池 attempts、error id、request id、raw upstream error、diagnostics、capacity/routing trace。

影响：
- 当前项目诊断面更适合调度和 usage 问题。
- sub2api 诊断面更适合 Claude Code wire 指纹问题。

建议：
- 如果要排查 Claude Code 外部池误判，当前 wire debug 可以补“outbound header profile dump”，但注意脱敏。

### 16. 安全边界

sub2api：
- URL allowlist/private host 策略更完整。
- Header override 阻止列表严格。
- OAuth mimic 路径跳过客户端 header 透传，避免伪装头和客户端头冲突。
- API key passthrough 删除认证/cookie 残留。

当前项目：
- URL 校验较宽松，允许 HTTP。
- Header 黑名单较短，兼容性强但可能透传不必要 header。
- 没有 header override，因此少一类配置风险。
- 对外部池错误信息有 sanitization/raw upstream error 限制，客户端 public error 不暴露敏感细节。

影响：
- 当前最大可见差异是 outbound header 过滤。
- 对生产环境，应该优先收紧外部池 Anthropic profile 的 header 转发。

## 当前项目比 sub2api 更强或更适合保留的部分

1. 外部池调度体系更完整：并发租约、全局队列、优先级、同池/跨池重试、soft failure 降权、模型级冷却、本地 rescue。
2. 400 默认不可重试，payload guard retry 只针对明确 payload/context 超限场景，符合“不要随意改 body”的原则。
3. `external_pool_transient_failure_cooldown_threshold` 默认 0，避免短抖动导致池级冷却，符合当前生产诉求。
4. `model_unavailable` 默认模型级冷却，不是池级冷却，避免单模型问题影响整个外部池。
5. `count_tokens` 本地计算，避免外部池额外流量和不稳定性；这是当前产品语义，不应轻易改。
6. 流式 pre-output retry 能在没写给下游前安全换池，比单纯 writer size 检测更主动。

## sub2api 更强、当前值得补齐的部分

1. Anthropic/Claude Code outbound header allowlist。
2. 显式删除 `cookie`、`x-goog-api-key` 等认证残留。
3. Claude Code header wire casing / x-stainless 指纹建模。
4. API key passthrough 的 `anthropic-beta` / body capability 对称处理。
5. `X-Claude-Code-Session-Id` 与 `metadata.user_id.session_id` 的一致性维护。
6. per-account/per-pool header override，且必须带严格阻止列表。
7. URL allowlist/private host 策略。
8. 可选 TLS fingerprint，仅在需要 Claude Code OAuth mimic 时启用。

## 建议落地顺序

P0：低风险、应优先做
- 给当前 `should_forward_header` 增加 `cookie`、`x-goog-api-key` 黑名单。
- 为 `forward_headers` 增加测试：
  - 客户端传入 `authorization`、`x-api-key`、`x-goog-api-key`、`cookie` 时均不透传。
  - pool `Bearer` 时只出现 `Authorization`，不出现 `x-api-key`。
  - pool `XApiKey` 时只出现 `x-api-key`，不出现 `Authorization`。
  - 保留当前 `anthropic-version` 行为。

P1：为 Claude/Anthropic 外部池增加 profile
- 增加 per-pool `headerProfile` 或等价字段：
  - `generic`：当前行为。
  - `anthropic_passthrough`：sub2api 允许名单 + 认证残留删除 + 默认 `content-type`/`anthropic-version`。
- 该 profile 不做 OAuth mimic，不改写 `metadata.user_id`，适合“baseurl + key”的 API key 外部池。
- 增加测试覆盖 Claude Code 常见 headers：
  - `x-stainless-*`
  - `anthropic-beta`
  - `x-client-request-id`
  - `x-claude-code-session-id`
  - `user-agent`
  - 未在允许名单中的自定义 header 不透传。

P2：按需补 Anthropic beta/body 对称处理
- 对 `anthropic_passthrough` profile，可考虑：
  - 可配置 `appendBetaQuery`，默认不改变已有外部池。
  - 计算最终 `anthropic-beta` 后再做 body capability sanitize。
- 这部分要小心，不要影响当前 raw passthrough 的“少改 body”语义。

P3：更重的 Claude Code mimic 能力
- 仅当确实要支持 Claude Code scoped OAuth 上游时再做：
  - Claude Code 检测。
  - `x-stainless-*` 固定指纹。
  - `metadata.user_id` rewrite。
  - `X-Claude-Code-Session-Id` 同步。
  - TLS fingerprint opt-in。
- 不建议用于普通 API key 外部池。

## 结论

sub2api 的方案可以概括为：围绕 Anthropic/Claude Code 官方流量形态构建请求，尽量让 header、body、metadata、TLS 都像真实 Claude Code；当前项目的方案可以概括为：围绕多外部池生产调度构建请求，优先保证容量、重试、failover、usage 和本地 rescue。

两者不是谁完全替代谁。当前项目已经有更强的调度容错，不应该回退成 sub2api 那种账号网关模型；但在“Claude Code 兼容外部池 baseurl + key”这个具体场景上，当前应该借鉴 sub2api 的 Anthropic API key passthrough 思路，尤其是 outbound header allowlist、认证残留清理、可选 beta/query 兼容、以及更明确的 per-pool profile。这样能提升 Claude Code 外部池的上游兼容性，又不会破坏当前外部池调度体系。
