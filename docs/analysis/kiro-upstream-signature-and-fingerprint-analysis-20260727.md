# Kiro 上游"签名"与设备指纹全链路分析

日期：2026-07-27
范围：只读源码分析，未修改任何代码，未运行 `cargo test`。
方法：先由主分析给出断言，再用 codex CLI（`codex exec --sandbox read-only`，约 24 分钟 / 1.14M tokens）独立复核 17 条断言，最后交叉抽查 codex 的关键引用。
目的：回答"本项目对上游签名怎么处理、每个 session 是否同一签名、官方客户端是否一致"，并厘清"签名"一词在本仓库指代的多个不同机制。

## 1. 总结

**本项目对 Kiro/CodeWhisperer 上游不存在 AWS SigV4 请求签名。** `Cargo.toml` 无 `aws-sigv4` / `aws-sig*` / `hmac` 直接依赖；`src` 下无 `x-amz-date` / `x-amz-content-sha256` 生成逻辑。上游鉴权靠 `Authorization: Bearer <token>`，AWS JSON 1.0 + event-stream 只是协议壳。

`Cargo.lock` 里存在间接 `hmac`，来自依赖闭包，源码未使用。

用户口中的"签名"在本仓库实际对应五类互不相同的机制：

| 类别 | 是否密码学签名 | 是否发往上游 | 主要位置 |
| --- | --- | --- | --- |
| 设备指纹 machineId | 否（SHA256 派生） | 是（UA 内） | `src/kiro/machine_id.rs` |
| thinking / reasoning signature | 是（上游生成） | 是（请求体历史） | `src/anthropic/converter/history.rs:278` |
| tool_use_signature | 否（本地去重串） | **否** | `src/anthropic/stream.rs:764` |
| event-stream CRC32 | 否（完整性校验） | 是（帧内） | `src/kiro/parser/crc.rs` |
| 各类 SHA256 fingerprint / hash | 否（本地索引） | 否 | 见 §6 |

## 2. 设备指纹 machineId

`src/kiro/machine_id.rs:54` `generate_from_credentials` 派生优先级：

1. 凭据级 `machineId`（`machine_id.rs:56-60`）
2. 全局 `config.machineId`（`machine_id.rs:62-67`）
3. 按凭据类型派生，两条路径互斥不回落（`machine_id.rs:69-82`）
   - API Key 凭据：`sha256("KiroAPIKey/" + kiroApiKey)`
   - OAuth 凭据：`sha256("KotlinNativeAPI/" + refreshToken)`
4. 随机兜底 `sha256("KiroFallback/" + uuid_v4)`，按 `credentials.id` 进程内缓存（`machine_id.rs:84-108`）

`normalize_machine_id` 接受 64 位 hex 或 UUID（去连字符后重复一次补齐到 64）。

### machineId 的出现位置（共 5 处，非仅 IDE UA）

| 位置 | 文件:行 |
| --- | --- |
| IDE endpoint UA / x-amz-user-agent | `src/kiro/endpoint/ide.rs:50-65` |
| social token refresh 的 User-Agent | `src/kiro/token_manager/refresh.rs:670-684` |
| usage_limits / set_overage UA | `src/kiro/token_manager/refresh.rs:927-939`、`995-1014`、`1041-1055` |
| profile discovery 自建 headers | `src/kiro/provider.rs:7507-7518` |
| `profile_arn_discovery_key` 缓存键 | `src/kiro/provider.rs:7375` |

CLI endpoint 的 API/MCP UA **不含** machineId（`src/kiro/endpoint/cli.rs:55-67`）。

### 粒度

machineId 粒度是"每凭据"，不是"每 session"——provider 每次调用从凭据派生后放入 `RequestContext`，没有 session 输入（`src/kiro/provider.rs:8376-8393`、`8658-8672`、`10145-10197`）。凭据装载/新增时会补全并持久化（`src/kiro/token_manager/manager.rs:2158-2161`、`2326-2332`）。

两个例外：
- 全局 `config.machineId` 会让多个凭据共享同一值
- 无 `id` 的兜底凭据共享同一兜底值（`machine_id.rs:14-17`）

## 3. 五套 User-Agent

这是本项目最容易被低估的复杂点。上游身份形状不是一套，而是五套：

| 链路 | UA 形状 | 含 machineId | 证据 |
| --- | --- | --- | --- |
| IDE API / MCP | `aws-sdk-js/1.0.34 ... api/codewhispererstreaming#1.0.34 m/E KiroIDE-{ver}-{mid}` | 是 | `endpoint/ide.rs:57-64` |
| CLI API / MCP | `aws-sdk-rust/1.3.15 ... api/codewhispererstreaming/0.1.16551 ... app/AmazonQ-For-CLI` | 否 | `endpoint/cli.rs:55-67` |
| CLI management | `... api/codewhispererruntime/0.1.16551 ... m/F,C app/AmazonQ-For-CLI` | 否 | `endpoint/cli.rs:69-80` |
| usage_limits | `aws-sdk-js/1.0.0 ... api/codewhispererruntime#1.0.0 m/N,E KiroIDE-{ver}-{mid}` | 是 | `refresh.rs:1041-1055` |
| IdC refresh | `aws-sdk-js/3.980.0 ... api/sso-oidc#3.980.0 m/E KiroIDE` | 否 | `refresh.rs:837-857` |

另有两条 refresh 简化 UA：social refresh 为 `KiroIDE-{ver}-{mid}`（`refresh.rs:679-684`），external IdP refresh 为 `KiroIDE-{ver}`（`refresh.rs:764`）。

### 已发现的一致性偏移

`src/kiro/provider.rs:7493-7529` 的 `ListAvailableProfiles` 自建 headers **绕过了 endpoint trait**，UA 写 `api/codewhispererruntime#1.0.34`，而同一 IDE 链路的 API UA 是 `api/codewhispererstreaming#1.0.34`。两者 SDK 版本号一致但 api 段不同，是手写重复导致的偏移。未修复。

## 4. thinking / reasoning signature

这是唯一真正的密码学签名，由上游生成，本项目不生成也不验签。

- 入站：`src/anthropic/converter/history.rs:278-284` 提取历史 `thinking.signature`，非空时封装为 `ReasoningContent::reasoning_text(thinking, signature)`
- 出站：Kiro `ReasoningContentEvent.signature` 下发为 Anthropic `signature_delta`（`src/anthropic/stream.rs:2522-2533`）或非流式 `signature`（`src/anthropic/handlers.rs:8496-8516`）
- 失效判定：`src/kiro/provider.rs:12049-12058`，严格要求 HTTP 400 **且** JSON pointer `/reason` 或 `/error/reason` 精确等于 `THINKING_SIGNATURE_INVALID`
- 补救：`src/anthropic/handlers.rs:6046-6054` `build_thinking_signature_retry_body` 调 `clear_history_reasoning_content()` 剥掉全部历史 reasoningContent，用**同一凭据/token/machineId** 重发（`provider.rs:11063-11079`）；`thinking_signature_retry_body_builder.take()` 保证只消费一次（`provider.rs:10988-11005`）

"仅透传"需要限定——项目还会做：污染检测 fail-closed（`stream.rs:3115-3128`）、payload guard 超限整形时丢弃历史 thinking/signature（`payload_guard.rs:1619-1625`、`2066-2069`）。

`external_pool.rs:9317-9335`、`9418-9424`、`9496-9510` 的 `signature_delta` 处理不是独立类别，是外部 Anthropic 兼容 SSE 的同类签名字段缓冲。

## 5. session 维度：变与不变

| 字段 | 粒度 | 证据 |
| --- | --- | --- |
| machineId（UA 内） | 每凭据（跨 session 相同） | `provider.rs:8376-8393` |
| `Authorization: Bearer` | 每凭据（刷新后变） | `endpoint/ide.rs:114` |
| UA / x-amz-user-agent | 每凭据 + 每链路（见 §3） | — |
| `amz-sdk-invocation-id` | **每请求新 UUID v4** | `ide.rs:112`、`cli.rs:152` |
| `amz-sdk-request` | 三个值：主端点 `max=3`、profile discovery/usage_limits `max=1`、IdC refresh `max=4` | `ide.rs:113`、`provider.rs:7534`、`refresh.rs:857` |
| conversationId | 每 session | `converter.rs:295-370` |
| profileArn | 每凭据类型 | `protocol.rs:124-161` |

**回答"每个 session 是否同一签名"：是，指纹层面完全相同。** 唯一每请求变化的是 `amz-sdk-invocation-id`，那是 AWS SDK 的重试幂等 ID，不承担身份识别。

### conversationId 来源

`src/anthropic/converter.rs:295-315` `extract_session_id`：先按 JSON `session_id` 解析，失败则找 `session_` 后 36 字符。抽不到时按 `prompt_cache_simulation_mode` 分流（`converter.rs:333-339`）：HighCache 用 system+tools+首条 user 消息的 canonical JSON 做 SHA256 确定性 UUID v5 形（`converter.rs:339-370`），Disabled 则随机 UUID（`converter.rs:437-438`）。

注意 `is_valid_uuid`（`converter.rs:373-376`）只校验长度 36 与 4 个连字符，不是严格 UUID 解析，能放过非 hex 字符。

### 环境版本是固定字面量

`src/model/config.rs:3692-3705`：`kiro_version = "0.11.107"`、`node_version = "22.22.0"`、`system_version` 按 `std::env::consts::OS` match（macos → `darwin#24.6.0`）。均不探测真实系统版本。

## 6. 其他 SHA256 fingerprint（本地用途，不发上游）

| 用途 | 位置 |
| --- | --- |
| 下游请求 API key 身份 ID | `src/common/auth.rs:14-31`、`85-88` |
| 凭据 secret hash | `src/storage/postgres.rs:466-496` |
| profile ARN discovery 缓存键（混入 machineId） | `src/kiro/provider.rs:7310-7375` |
| prompt-cache fingerprint | `src/anthropic/prompt_cache.rs:53-57`、`277-303` |
| tool-format debug fingerprint / body_sha256 | `src/anthropic/tool_format_debug.rs:530-552`、`1086-1161` |
| Redis session / model hash | `src/storage/redis_cache.rs:6849-6874` |
| token refresh limits fingerprint | `src/kiro/token_manager/auxiliary.rs:283-288`、`337-356` |

## 7. 已排除的伪线索

- `src/bin/kiro_loadtest.rs:1932-1943` 的 `x-amz-security-token`：fake server 抓包的敏感头脱敏名单，不是生产请求生成
- `src/kiro/parser/crc.rs:1-18` + `frame.rs:109-132`：AWS event-stream 完整性校验（ISO-HDLC CRC32），不是签名
- TLS：只有 `TlsBackend::{Rustls, NativeTls}` 后端选择（`config.rs:15-25`、`http_client.rs:449-472`），**无 JA3 / ClientHello 指纹伪造**。自然 TLS 指纹随 reqwest/rustls 版本变化，项目未显式控制

## 8. 与官方客户端对比（仓库外事实，源码无法证明）

以下三点标注为不可从本仓库独立验证：

- Claude Code CLI 不直连 Kiro，它打 Anthropic `/v1/messages` 用 `x-api-key` 或 OAuth Bearer，同样无 SigV4。仓库内可确认的是本项目提供 `/cc/v1` 兼容路由（`src/anthropic/router.rs:321-345`），入站认证支持 `x-api-key` 或 Bearer（`router.rs:154-158`、`common/auth.rs:44-66`），再转换到 Kiro endpoint 并构造 Kiro IDE / Amazon Q CLI 形状 UA
- 官方 Kiro IDE 的 machineId 是真实设备 ID（每台机器不同，通常来自 VS Code machineId），而本项目是从凭据材料哈希出的稳定值
- 语义上"一个凭据 ≈ 一台虚拟设备"，但受全局 `config.machineId` 覆盖影响

## 9. 风险与可行的缓解

多 session 共用一个凭据高并发时，上游看到"同一 machineId + 同一 UA"的大量并发。这是当前设计的既有特征，不是 bug；但若上游做设备维度频控，该维度可聚合。

缓解手段的边界需要说清：
- 凭据级 `machineId` 覆盖**只能换值，不能把多个 session 分散成多个 machineId**
- 真正分散需要多个凭据 / 不同 secret；且相同 secret 的重复凭据会被去重逻辑拦截（`src/admin/service.rs:1506-1539`）
- 全局 `config.machineId` 会反向把多个凭据压成同一 machineId，需避免误用

## 10. 待处理的小瑕疵（未修复）

1. `Cargo.toml:35` 注释写"CRC32C 计算"，实际用 `CRC_32_ISO_HDLC`（`crc.rs:5-8`）。CRC32C 是 Castagnoli 多项式，与 ISO-HDLC 不同；注释错误但代码正确（AWS event-stream 正是 ISO-HDLC）
2. `provider.rs:7493-7529` `ListAvailableProfiles` UA 的 `codewhispererruntime` vs `codewhispererstreaming` 偏移（见 §3）
3. `converter.rs:373` `is_valid_uuid` 非严格 UUID 校验

## 11. 审计过程中被推翻的初版断言

记录在案以便复查：

| 初版断言 | 复核结论 |
| --- | --- |
| machineId 只出现在 IDE 端点 UA | **WRONG**，共 5 处（§2） |
| CLI 路径所有凭据 UA 字节完全一致 | 过度概括，CLI 有 streaming/runtime 两套且随配置变化 |
| `amz-sdk-request` 是硬编码常量 | 过窄，有三个值 |
| thinking signature 仅透传 | 过强，还有污染检测/整形丢弃/retry 剥离 |
| 每凭据一个 machineId | 需补全局覆盖与无 id 兜底两个例外 |
| 打散只能靠凭据级 machineId 覆盖 | 误导，见 §9 |
| 上游鉴权仅靠 Bearer | 过宽，refresh 链路有 body/form 认证，外部池可用 x-api-key |
