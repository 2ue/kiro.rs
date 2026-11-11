# 端点轮换与 Kiro 客户端版本手动管理分析

Status: `Implemented; FREE real-upstream matrix verified 2026-11-11; official desktop client wire capture remains out of scope`

Last reviewed: 2026-11-11 Asia/Shanghai

Review boundary: 真实上游验证只使用项目根目录 JSON 中的 KIRO FREE 账号（147 个，Social 137 / IdC 10），在指定实例 `127.0.0.1:19023` 上完成；推理只调用 `claude-haiku-4.5`。生产服务器保持只读。

Local verification: 见 §13 真实上游验证矩阵；PostgreSQL generation/CAS 集成测试已用临时库实跑通过。

## 结论先行

当前分析把端点选择和客户端版本管理分成三项独立能力：

1. **协议端点候选链**：同一个凭据、同一个 token、同一个 machine identity 内按候选顺序尝试协议实现；它不是账号轮询，也不是负载均衡。
2. **Region host 轮换**：同一个凭据内按账号自己的 Region 主次顺序切换上游 host；它不改变 `auth_region`、token 绑定或凭据持久化值。
3. **Kiro 客户端版本手动管理**：检查 candidate、显式 apply、pin 和显式 rollback；`currentRelease` 只代表官方发布元数据，不代表新协议参数已经验证。版本状态 CAS 已实现。本范围不做自动版本回退、canary 或 last-known-good。

服务自身二进制更新不属于本分析范围；它与 Kiro 请求头中的 `KiroIDE-*` 版本无关。

默认关闭新行为时，现有 `credential.endpoint`、`defaultEndpoint`、Region 行为、配置版本和请求预算保持兼容。任何新请求都必须在下游尚未提交时才能尝试备用端点或备用账号。

分析按“固定请求快照 -> 选择账号并准备身份 -> 按需 Profile discovery -> 构造 Region/endpoint 候选 -> 分类推进 route 或账号”组织。刷新 token、查询额度/Profile/模型与推理 endpoint fallback 是不同操作；它们共享当前账号身份上下文，但不共享推理候选链。

## 当前冻结的执行顺序

以下是当前 dirty tree 的状态和剩余验证顺序：

1. **协议基线**：对照当前 Rust、Kiro-Go、Kiro-account-manager、kiro-manager-lite 的 refresh、usage、Profile、models 和 inference 合同，记录认证类型、Region、Profile ARN、UA/SDK 版本和响应归一化差异。
2. **隔离协议验证**：已使用 FREE IdC 账号对 MCP Profile header 做官方 endpoint A/B；A/B 证明缺少 header 会返回 `400 profileArn is required for this request`，补 Builder ID placeholder 后返回 `200`，代码已补齐该 header。其余端点失败矩阵、请求身份和版本状态用 fake upstream/fixture 核对。
3. **协议状态模型**：按认证类型区分 refresh、Portal/REST usage、Profile、models、MCP 和 inference；refresh token 轮换先持久化再发布。导入成功不等于账号可用。
4. **调度合同**：核心 Region/endpoint fallback、身份刷新门禁、physical send cap 和请求级配置快照已在 dirty tree；请求入口固定一次 runtime config，账号循环只 clone 该快照，继续补充取消、错误边界和发送归因证据。
5. **端点候选链**：inference、models、MCP 已接入同账号候选；首期不跨认证类型把 OAuth `ide` 与 API Key `cli` 互相回退。inference、models、MCP、Profile discovery 和 REST usage 各自构造有界 Region 候选；Portal usage、overage 写操作和日常 auth refresh 保持固定 host/Region。
6. **客户端版本手动管理**：candidate 发现和显式 apply/pin/rollback 已实现；不加入自动版本回退、canary、last-known-good 或服务二进制 updater。

真实验证限定为协议 A/B，不用于故障注入或调度压力测试。FREE IdC A/B 已确认 MCP 必须携带 Builder ID placeholder Profile header；完整失败矩阵、重试边界和版本状态仍由 fake upstream、fixture 与单测验证。

## 1. 目标与非目标

### 目标

- 在保留当前端点选择行为的前提下，允许管理员选择首选协议端点。
- 对支持同一账号的协议端点建立有序 fallback，并在请求级 trace 中记录每次实际发送的端点、Region 和原因。
- 保证普通模式下同一账号可按配置和资格尝试备用 Region；该路径已在当前 dirty tree 实现。
- 增加立即检查/立即拉取 Kiro 版本元数据的管理能力，避免等待 12 小时后台周期。
- 让版本 candidate、active、previous 和显式回退操作可观察、可持久化、可审计。
- 用脱敏 fixture/wire 证据比较客户端身份版本与固定 SDK/API 参数；不自动推导或应用新协议参数。
- 明确手动 apply、pin、rollback 和关闭自动检查的语义。

### 非目标

- 不把 `ide`、`cli`、`codewhisperer`、`amazonq`、`runtime` 直接做成无条件轮询池。
- 不在不同账号之间复用 token、profile ARN、machine identity 或会话绑定。
- 不把 Region host、Runtime 域名开关和协议 endpoint 名称混称为同一种端点。
- 不根据 `currentRelease` 自动推导或自动改写新的请求参数、请求体字段或事件协议。
- 不做客户端版本自动回退、canary、last-known-good 或自动健康回滚。
- 不实现服务自身二进制检查、下载、替换、重启或回退。
- 不在已向下游提交首个 SSE 事件后重放同一请求。

## 2. 当前代码基线

以下事实以本地工作树 `HEAD=acfdd10ca16469286d69a92b9d99adb75242418f`（`v0.0.244`）及其未提交改动为准；端点轮换、Region 资格和版本生命周期改动均仍处于 dirty tree 验证阶段。

| 能力 | 当前行为 | 关键证据 | 设计影响 |
| --- | --- | --- | --- |
| 协议 endpoint | 已注册 `ide`、`cli`、`codewhisperer`、`amazonq`、`runtime`；primary 仍按 `credential.endpoint > defaultEndpoint`，fallback 默认关闭 | `src/main.rs:471-504`、`src/kiro/endpoint/mod.rs:144-187` | 候选按认证类型过滤；API Key 保持 CLI 语义 |
| Region rotation | 候选固定为 `us-east-1`、`eu-central-1`；请求级 route 会按 Profile ARN 资格过滤 | `src/kiro/retry_pipeline.rs:42-165`、`src/kiro/provider.rs:13805-14280` | 同账号传输/502-504 失败可尝试合法备用 Region；账号级错误仍走账号调度 |
| 普通 Region fallback | inference、models、MCP、Profile discovery 和 REST usage 在各自开关与错误白名单下覆盖传输错误和 502/504；503、429、500 先交由响应体/账号错误分类 | `src/kiro/provider.rs:14000-14280,16140-16220`、`src/kiro/token_manager/refresh.rs:1306-1455` | 不能把所有 5xx 或 429 都解释成 Region 故障；Portal、overage 写操作和日常 auth refresh 不进入该候选链 |
| 账号绑定 | sticky affinity 绑定 credential id；失败时可在本请求临时排除账号 | `src/kiro/token_manager/manager.rs:7447-7449,7599-7643` | endpoint fallback 不能改变账号绑定语义 |
| 共享预算 | 每次真实 inference send 预留 `InferenceAttemptBudget`；下游提交后禁止重发 | `src/kiro/provider.rs:13802-13817`、`src/anthropic/inference_attempt_budget.rs:179-205,338-398` | 端点、Region、账号和外部池共用同一发送预算 |
| Kiro 版本发现 | 读取官方 metadata，状态持久化为 candidate/active/previous/generation；后台同步只发现 candidate；写入使用 generation 条件更新 | `src/kiro/kiro_version.rs:26-53,106-138,308-364`、`src/storage/postgres.rs:5011-5052` | 立即 check 与显式 apply/pin/rollback 已接入；不含自动回退 |
| Kiro 版本生效 | `applied`/`pinned` active 优先于 configured；关闭自动检查不会回退 active | `src/kiro/kiro_version.rs:166-210,270-303` | candidate 不自动等同已验证兼容版本 |
| 版本管理 UI/API | 已有 check/apply/pin/rollback 与主 UI/admin-ui 控件；apply/rollback 使用版本状态 CAS | `src/admin/router.rs:321-331`、`src/admin/service.rs:6260-6411` | 仅保留人工版本操作；不含自动回退 |
| Kiro 版本影响面 | `kiroVersion` 替换多个 Kiro operation 的 User-Agent 中的 KiroIDE 版本文本；SDK/API 版本仍是代码内固定值 | `src/kiro/endpoint/ide.rs:70-85`、`src/kiro/endpoint/cli.rs:56-81`、`src/kiro/token_manager/refresh.rs` | 修改 KiroIDE 版本号不等于抓取或升级整套客户端参数 |
| 查询操作覆盖 | models/MCP 按 operation 复用有界 Region × endpoint 候选；Profile discovery 与 REST usage 使用独立 Region 候选；Portal usage 固定 Portal host，overage 写操作单 Region，日常 refresh 固定 auth Region | `src/kiro/provider.rs:11531,12843,13367,14308`、`src/kiro/token_manager/refresh.rs:1262,1306-1455` | 各 operation 独立判断 fallback；不得借用 inference 游标，也不能把 Portal/overage 写操作扩成无条件重试 |
| 参数采集 | 显式 capture 可记录请求/响应及 endpoint、Region、clientVersion 路由字段；结构化字段路径快照和 normalized schema diff 尚未实现 | `src/kiro/provider.rs:280-384`、`src/diagnostics/capture.rs:219-235,649-670` | 现有 capture 可能保留完整合成请求体，不能称为结构化脱敏差异报告 |
| 服务二进制更新 | 不属于本 topic；当前由既有 release/部署流程管理 | `.github/workflows/` | 不移植 mata updater，也不与 KiroIDE 版本管理合并 |

参考项目 `kiro-rs-mata` 的端点链是同账号顺序 fallback：首选级别为凭据 endpoint、全局 `preferredEndpoint`、全局 `defaultEndpoint`；IDE 兼容链为 `ide -> codewhisperer -> amazonq -> runtime`，CLI 被排除。参考代码的 `auto` 分支会在检查 fallback 开关前直接返回完整候选链，与 UI 上“关闭 fallback 即只用首选端点”的文案不一致；移植时必须用测试固定开关语义。其服务二进制 updater 与本项目的 Kiro 客户端身份版本无关，本范围不移植。

当前版本同步的边界是：关闭 `kiroVersionAutoSyncEnabled` 只停止后续 candidate 检查，不会清除已经显式 applied/pinned 的 active；active 只由显式 apply、pin 或 rollback 改变。candidate 也不等于“新版本稳定”。

本地缺省 Kiro 版本已改为 `1.4.154`；官方 metadata 和安装包证据确认对应 streaming SDK `1.0.44`。这只影响缺省配置，不会覆盖已有数据库中显式保存的旧 `kiroVersion`。已有部署需通过立即检查与显式 apply 更新；apply 后关闭检查不会降级。旧版进程内 `LATEST` 没有持久化，重启后无法精确迁移旧 active 值。`KiroIDE-*` 仍只是身份字符串，不能代表完整官方请求体、SDK/API 或流协议已同步。

远端最新提交已修复 Builder ID/IdC 的 `getUsageLimits` 占位 `profileArn`；当前 HEAD 已包含该修复。本文其余结论以 dirty tree 的 fake upstream 验证为准。

## 3. 术语与数据模型

### 3.1 四个不能混用的维度

| 名称 | 示例 | 是否绑定账号 | 是否持久化 |
| --- | --- | --- | --- |
| `protocol_endpoint` | `ide`、`cli`、`codewhisperer`、`amazonq`、`runtime` | 是，候选请求复用当前凭据上下文 | 首选值可持久化，单次候选不写回 |
| `api_region` / host | `us-east-1`、`eu-central-1`、`runtime.{region}.kiro.dev` | 是，顺序必须按账号有效 Region | 只保存账号主 Region；override 为请求内临时值 |
| `kiro_client_version` | `1.4.121` | 否，默认进程/实例级 | candidate、active、previous 和显式回退状态持久化 |
| `service_binary_version` | 服务镜像或二进制版本 | 否，实例/部署级 | 由发布/部署系统持久化 |

### 3.2 端点路由快照

每个请求开始时捕获不可变 `EndpointRoutingSnapshot`：

```text
preferred_protocol_endpoint
credential_endpoint
candidate_endpoint_order
region_rotation_enabled
region_candidates_for_credential
endpoint_fallback_generation
kiro_client_version_snapshot
inference_attempt_budget
```

运行时配置热更新只影响后续请求，不改变已经开始的请求。启用诊断采集时，每个真实发送写入：

Inference 与 MCP 均在逻辑请求入口读取一次 runtime config；后续账号重试、endpoint/Region 候选和版本 resolver 使用该请求快照，不在账号循环中重新读取热配置。该行为已由 focused/full Rust 测试覆盖，后续只需保持回归门禁。

```text
request_id
attempt_index
credential_id
protocol_endpoint
api_region
host_kind                 # q / runtime / codewhisperer 等
selection_source          # credential / preferred / default / fallback
fallback_reason
status_or_network_error
downstream_committed
budget_consumed
```

### 3.3 版本状态

当前 dirty tree 已持久化 active、source、previous、candidate、checked_at、last_error 和 generation。版本值与生效来源分开表示：

```text
configured_version        # 现有 kiroVersion；当前回退目标需由管理员确认
candidate_version         # 当前仅验证 semver 格式，不代表 IDE/SDK 配对已验证
candidate_checked_at
active_version            # 当前实际写入 Kiro operation 请求头的版本
active_source              # configured / applied / pinned
previous_version           # 最近一次 active 切换前的版本
last_error
version_generation         # 当前已实现 CAS/多实例同步代次
```

当前 candidate 解析只校验 `currentRelease` 版本号格式；元数据可解析、安装包可下载或 UA 可生成，都不能证明推理、工具、thinking、MCP 和流式协议稳定。版本 rollback 仅由管理员显式触发，使用现有 previous/configured 语义；不维护稳定版本标签，也不自动回退。

安装包核验已确认 `1.4.121 -> 1.0.44`、`1.4.154 -> 1.0.44`。只有登记的 IDE/streaming SDK 配对会更新 `codewhispererstreaming` UA 子版本；这不代表其他 SDK/API 标识或整套 wire 协议已更新。未知版本继续沿用 `1.0.34`。`active_source` 使用 `configured`、`applied`、`pinned`；旧配置值不会因缺省版本变化而自动升级。

### 3.4 凭据与 Kiro 官方操作协议

导入/导出 JSON 是管理器之间的凭据数据交换格式，不是 Kiro 官方 API。官方操作按认证类型和 operation 独立构造请求；调用前由账号调度层提供同一份 `CredentialProtocolContext`，其中包含 credential id、auth method、token、machine id、Profile ARN、auth region、API region 与代理。

| 操作 | 协议适配 | 关键身份规则 | 与推理 endpoint fallback 的关系 |
| --- | --- | --- | --- |
| Social 刷新 | Kiro `refreshToken` JSON | 使用账号 refresh token 与 auth Region；轮换后的 refresh token 先持久化再发布 | 不进入 IDE/CLI 推理候选链；日常刷新固定 auth Region |
| IdC / Builder ID 刷新 | AWS OIDC `/token` | 使用该账号 client id/secret、refresh token 与 auth Region | 不进入推理候选链；导入/验证可按 auth Region 策略尝试 |
| External IdP 刷新 | 配置的 OAuth token endpoint / form body | 保留该账号 issuer、client、scope 与 refresh token 语义 | 不进入推理候选链；使用凭据自己的 token endpoint |
| 额度查询 | 非 API Key 先走 Portal `GetUserUsageAndLimits` CBOR；IdC/External IdP 仅在认证或权限拒绝时回退 REST `getUsageLimits`；API Key 直接走 REST | REST 使用账号 effective API Region；Builder ID REST 使用 placeholder Profile ARN；REST 只在连接失败、合格 502/504 或明确 Profile ARN/Region mismatch 403 时换候选 Region | Portal 固定 Portal host；REST 候选不借用推理 route，普通权限 403、认证错误、429、500 等直接结束 |
| `GetUserInfo` | Portal CBOR | usage 缺 email 时补查；IdP/provider 从当前账号上下文传入 | 属于 Portal usage 流程，不进入推理 route |
| `ListAvailableProfiles` | External IdP 缺少真实 ARN 时调用 CodeWhisperer control plane；按 Region 候选逐个尝试 | 连接失败和合格 502/504 可继续；403/空列表按无结果处理且不继续；结果只用于当前账号 | 使用同账号 Region 候选，不借用 inference 游标；Profile ARN 跨 Region 是否允许由独立开关控制 |
| `ListAvailableModels` | IDE GET 与 CLI AWS JSON POST 分开；构造 Region × endpoint 候选，连接失败和合格 502/504 可换 route | 分页、Profile 与 API Key header 按协议；中途分页失败会丢弃当前 route 的部分结果，从下一个 route 重新拉完整 catalog | 共用 endpoint fallback 和 Region rotation 开关；普通协议/模型语义错误不换 route |
| WebSearch MCP | JSON-RPC `tools/call`；IdC/Builder ID 必须发送 placeholder Profile header；构造 Region × endpoint 候选 | API Key 不带 OAuth/Profile header；普通 400 是协议请求错误，直接 fail-fast | 连接失败和合格 502/504 可同账号换 endpoint/Region；MCP 400 不换 route、Region 或账号 |
| Overage `setUserPreference` | Q host JSON POST，成功后再刷新 usage | 使用账号 effective API Region 与可用真实 Profile ARN | 单次发送，无 Region/endpoint fallback |

代码层保留 operation 边界和独立请求/响应 DTO。Portal CBOR 与 REST fallback 只用于额度查询，不扩展解释为推理请求可以任意换 host。`getUsageLimits` 使用 Builder ID placeholder；MCP 也有独立的必需 Profile header 规则，FREE IdC A/B 已验证缺少该 header 会返回 `profileArn is required for this request.`。模型列表在 `KiroIDE-1.4.x` 下也要求 Builder ID/IdC 带 placeholder（不带返回 `400 Invalid profileArn.`，`0.11.107` 时不要求），现与 MCP/usage 使用同一规则。

### 3.5 协议交付矩阵

| Operation / 能力 | 当前状态 | 缺口与交付边界 |
| --- | --- | --- |
| Social、IdC/Builder ID、External IdP refresh | 已有按认证类型区分的实现 | 核对脱敏请求/响应合同与 refresh token 轮换；不进入推理 endpoint 候选链 |
| `getUsageLimits` | API Key 走 REST；其他认证优先 Portal CBOR，认证/权限错误按类型回退 REST；Builder ID REST 使用 placeholder | REST 以账号 effective API Region 为首选，连接失败、合格 502/504 和明确 Profile ARN/Region mismatch 403 可继续候选；Portal 仍固定 host |
| `GetUserInfo`、`GetUserUsageAndLimits` | 已接入非 API Key usage 查询；usage 缺 email 时补查 UserInfo | Portal operation base 固定 host；REST 回退按认证/权限错误分类，并使用独立 usage Region 候选 |
| `ListAvailableProfiles` | External IdP 缺真实 ARN 时按 Region 候选发现 | 连接失败、合格 502/504 可换 Region；403/空列表按无结果处理，不继续候选 |
| `ListAvailableModels` | 同账号 endpoint 候选与 Region 候选均已接入；连接失败与合格 502/504 可换 route | 分页中途失败丢弃部分结果，从下一个 route 重新拉完整 catalog；不混用 inference 游标 |
| MCP 辅助操作 | 同账号 endpoint 候选与 Region 候选均已接入；IdC/Builder ID 带必需 placeholder；HTTP 400 fail-fast | 连接失败与合格 502/504 可换 route；普通协议 400 不换 endpoint、Region 或账号 |
| Overage `setUserPreference` | 当前 Q host 单次发送；成功后再查询 usage | 按账号 effective API Region；没有 Region/endpoint fallback |
| Inference | 支持受限的同账号 route 候选 | Region 在外层、endpoint 在内层；只对允许的传输/502/504 情形推进，并受身份、commit 与发送预算约束 |
| IDE/SDK 版本配对 | KiroIDE UA 版本可配置；部分 SDK/API UA 值固定在代码 | 缺少新旧官方客户端 IDE/SDK 配对来源、请求参数与 SSE 合同证据；不能由 `currentRelease` 推导 |

查询候选已按 operation 独立落地：models/MCP 有有界 Region/endpoint fallback，Profile discovery 与 REST usage 有有界 Region fallback；Portal usage 固定 Portal host，overage 写操作单 Region 单次发送，refresh 的 auth Region 与 inference 的 API Region 分开。各 operation 不共用 inference Region 游标。版本配对证据单独归档，不与 endpoint fallback 共用开关。

本方案不定义导入/查询的触发时机、后台周期、批量队列或 UI 操作流程；这些属于独立需求。凭据导入 adapter 只负责把本项目与 Kiro-Go、Kiro-account-manager、kiro-manager-lite 的 JSON/JSONL/文本形态归一化为本项目凭据模型，不把外部 Account 结构写入 Token Manager。

## 4. 协议端点轮换设计

### 4.1 选择优先级

保持与现有账号配置兼容；当前实现没有独立的 `kiroPreferredEndpoint` 字段，primary 仍使用：

```text
credential.endpoint
    > config.defaultEndpoint
```

- `credential.endpoint > config.defaultEndpoint`
- `kiroEndpointFallbackEnabled: bool`：默认 `false`，只控制同账号协议候选链。
- `kiroEndpointFallbackOrder: Vec<EndpointName>`：由服务端按已注册能力和认证类型过滤。

凭据显式指定 `cli` 时，默认只使用 `cli`；不得因为开启 fallback 自动把 CLI 请求改成 IDE 请求。`ide` 家族是否包含 `codewhisperer`、`amazonq`、`runtime`，由 endpoint capability registry 决定，而不是由 UI 文字猜测。

### 4.2 账号绑定规则

同账号 endpoint fallback 必须复用同一个：

- `credential_id`；
- access token / API key；
- machine identity；
- `profile_arn` 与 auth region；
- 会话和模型路由上下文。

因此，多端点轮换**需要绑定当前账号运行时上下文，但不需要建立“账号 × endpoint”关系表**。关系来自凭据的 primary endpoint、端点能力矩阵和全局候选策略。备用端点成功后不自动写回凭据 primary，避免一次瞬态故障永久改变账号配置。

### 4.3 候选资格

每个候选先经过纯函数过滤：

1. endpoint 已注册并声明支持当前请求类型；
2. auth method、profile ARN、模型和请求模式兼容；
3. 账号 Region 能为该 endpoint 构造有效 host；
4. 候选未在本请求中尝试过；
5. 共享 inference budget 仍有额度。

当前五个 endpoint 实现已经注册，但候选资格仍由 capability registry 过滤：OAuth/IdC/ExternalIdp 可进入 IDE 兼容族，API Key 保持 CLI；注册不等于上游协议已验证。

### 4.4 失败分类与切换矩阵

当前实现使用保守失败矩阵，避免把客户端请求错误重复发送到多个 endpoint。诊断目标应同时记录 `send_stage`、`request_identity_generation` 和 `downstream_committed`；其中 `send_stage` 与 identity generation 尚未形成完整持久化 trace。MCP 目前只由内部 WebSearch `tools/call` 调用；WebSearch 是只读查询，因此 502/504 候选重放会复用原 JSON-RPC body 与 id。若开放其他有副作用的 MCP tool，必须先定义操作幂等和 request id 规则，不能直接复用这条重放路径。

```text
send_stage = pre_send | connect | tls | headers_wait | body_read | decoded_event
request_identity_generation = { generation, conversation_id, agent_continuation_id }
downstream_committed = 首个非空下游字节即将交付时原子置 true
```

`pre_send/connect/tls` 可以证明请求体尚未交给上游；`headers_wait/body_read/decoded_event` 可能已经执行，只有请求身份成功更新、请求体可重建、仍未 `downstream_committed` 时才能重放。当前实现身份刷新失败会 fail-closed，不继续重放；外层账号重试也不会绕过该失败。

| 失败 | 同账号换 endpoint | 换账号 | 说明 |
| --- | --- | --- | --- |
| DNS 解析、连接建立、TLS 握手失败 | 是 | 是 | 请求体未发送；仍记录准确失败阶段 |
| 502、504 | 有条件 | 按现有账号策略 | 仅限未提交、请求身份重写成功、请求体可重建；共享同一预算 |
| 503 | 先读响应体分类 | 按分类 | 当前实现先经过 structured classifier；模型/请求语义错误不进入 endpoint fallback |
| 524 | 默认否 | 默认否 | 当前实现已明确 fail-closed；另设开关和独立证据后才允许重试 |
| 429 | 否 | 是 | 账号级限流只换账号；狂暴轮次也不切换 Region |
| 400 malformed body / schema / tool pair | 否 | 否或按现有分类 | 请求本身错误，不能靠 endpoint 解决 |
| 5xx 中的 tool/schema/client validation | 否 | 否 | 先按客户端错误 fail-closed，不能当网关瞬态 |
| endpoint-specific 400/404 | 否（当前未实现） | 按现有分类 | 未来需先定义结构化 reason 白名单 |
| 401、402、403、额度耗尽 | 否 | 是 | 账号认证/额度问题，沿用现有账号故障转移 |
| profile ARN / Region 不匹配 | 否 | 否或重新按账号 Region 构造 | 不能盲切 host；优先修正候选资格 |
| 已向客户端写出首个非空字节 | 否 | 否 | `DownstreamCommit=true` 后不重放 |

mata 的“除 401/402/403 外全部 fallback”不能直接照搬；当前项目已有 400 fail-closed、thinking signature、tool pair 和共享预算语义，必须按结构化错误矩阵落地。

当前每个账号 attempt 先构造有限 route 候选：Region 在外层，endpoint 在内层，即对一个 Region 按配置顺序尝试候选 endpoint，再进入下一个合法 Region。传输失败、502 和 504 在请求身份重写成功、下游尚未提交且仍有预算时推进下一个 route；500/503 交给响应体分类，账号级错误（400、401/402/403、429、额度、风控、refresh 失效）沿用账号调度，其中 400 按协议错误直接结束。狂暴模式仅对不带 `Retry-After` 的账号级 429 增加账号轮次，不推进 Region。每次物理发送增加 `total_send_attempts`，受共享 inference budget（调用方提供时）和 `max_sends` 硬上限约束；endpoint fallback 默认关闭。由于外层账号重试可能重新进入同一 primary，不能声称一个 `(credential, endpoint, Region)` 在整个请求内只尝试一次。

流式和非流式统一使用 `DownstreamCommit`：首个非空 SSE/JSON 字节准备交付时置位；响应头已收到但尚未交付首字节时仍可 fallback。首语义输出只用于指标和超时分类，不作为重放安全边界。

### 4.5 Region host 轮换

- 当前推理、models、MCP、Profile discovery 和 REST usage 路径已接入有界 Region 候选。只有连接失败、合格 502/504 和明确 Profile ARN/Region mismatch 403 可推进候选；普通 429、500、503 和协议 400 不切换 Region。
- Region 顺序由 `effective_api_region` 归一化：`eu-*` 账号优先 `eu-central-1`，其余优先 `us-east-1`，备用项随后。
- 当前候选集合固定为 `us-east-1` 与 `eu-central-1`。因此 `ap-south-2`、`ap-northeast-*` 等账号在打开 Region rotation 后，首选候选会按归一化规则落到 `us-east-1`；这是当前实现限制，不能写成始终保留原始 effective Region。
- `region_override` 只存在于本次 attempt；不写回账号的 `api_region`、`auth_region` 或凭据文件。
- `kiroRegionProfileArnMismatchAllowed` 是独立开关，默认 `false`。关闭时，不尝试无法证明 Profile ARN 与备用 API Region 兼容的候选；打开时允许以当前账号 Profile ARN 尝试备用 Region，但必须记录该 ARN/Region 组合未验证，并限制在共享请求预算内。只有脱敏协议证据证明兼容后，才可从候选资格中移除该限制。
- `kiroRuntimeDomainEnabled` 仍是 IDE 实现内部 host 选择，不自动与 q host 互相 fallback；若未来纳入候选，必须作为独立 `host_kind` 和资格矩阵处理。
- 狂暴模式只为账号级 429 增加账号轮次，预算为账号 × 轮次；Region fallback 是 host/Region 故障的同账号路径，使用独立候选与共享物理发送预算。

### 4.6 关闭开关和状态清理

- 关闭 `kiroEndpointFallbackEnabled`：新请求只使用 primary endpoint；已经开始的请求继续使用开始时的快照，不中途改变。
- 当前没有备用 endpoint 健康计数、熔断或 `endpoint_fallback_generation`；fallback 由开关和候选顺序直接决定。
- 请求开始时固定完整 runtime config、Kiro 版本和发送预算；运行中热更新只影响之后开始的请求。
- 不清理账号认证、Region 主值或凭据持久化字段；明确回滚只回滚运行时策略，不回滚账号身份资料。

### 4.7 一次推理请求的层次与顺序

```text
请求进入
  -> 固定 runtime config、Kiro client version 与发送预算快照
  -> 选择 credential A，准备 A 自己的 token、machine id、Profile 和 auth region
       -> 若需 refresh，使用 A 自己的认证协议；refresh token 轮换先持久化
       -> 若需发现 Profile，按 Profile operation 自己的 Region 候选查询；403/空列表按无结果处理
  -> 构造 A 的 route 候选：Region 外层，protocol endpoint 内层
  -> 对每个 route 预留共享发送预算并发送，记录 endpoint、Region、send stage 和错误分类
       -> 仅在分类允许、身份重写成功、下游未提交且预算充足时推进下一个 route
       -> 下游首个非空字节提交后终止重放路径
  -> 同账号 route 耗尽或错误分类要求账号级 failover
       -> 账号调度器选择 credential B，并清空 A 的 route 游标
       -> 用 B 自己的 token、Profile、machine id 和 primary endpoint/Region 重新开始
  -> 所有账号或共享发送预算耗尽后终止
```

端点 fallback 需要在单次请求运行时绑定当前 credential；不需要新增账号与 endpoint 的关系表。账号配置保留 primary endpoint，运行时策略保存 fallback enable/order，候选快照绑定 credential id。换账号时必须从新账号 primary 开始，不能沿用上一个账号的 endpoint 成功值、Region override、Profile 或 token。完整配置快照在整个下游请求中保持不变。

Region rotation 开关关闭时，请求只使用该 credential 的 effective API Region。开关打开时，候选顺序固定为 Region 外层、endpoint 内层：对一个 Region 依 endpoint 配置顺序尝试，再进入下一个合法 Region。错误分类可提前终止或转交账号调度；不得将同一 route 的重试扩展成无界循环。`auth_region` 不参与 Region 游标。狂暴模式“先换账号、账号排除后换 Region”只适用于当前已定义的账号级 429 策略，不能写成所有错误的全局顺序。

MCP、models 与 inference 使用各自发送管线，但共享协议 endpoint fallback 配置和发送预算；三者都可在开关开启且错误符合白名单时使用各自 Region 候选。Profile discovery 与 REST usage 也有独立的有界 Region 候选；Portal usage 固定 Portal host，overage 写操作单 Region 单次发送，refresh 使用 auth Region，不能借用 inference Region 游标。此次 MCP 400 的根因是 Builder ID/IdC 请求缺少必需的 `x-amzn-kiro-profile-arn`；FREE IdC 官方 A/B 在不带 header 时返回 `profileArn is required for this request`（400），带 Builder ID placeholder 时返回 200。代码已补 header。普通 MCP 400 的 fail-fast 只处理仍然存在的协议请求错误，不是本次 400 的修复手段。

## 5. Kiro 客户端版本设计

### 5.1 发现与立即刷新

保留官方 metadata URL 作为来源：

`https://prod.download.desktop.kiro.dev/stable/metadata-linux-x64-stable.json`

当前版本检查只解析并校验顶层 `currentRelease`，把版本号写入 candidate；尚未持久化 `releases[]`、发行说明、平台资产 URL 或签名信息。metadata 不能提供新版本真实请求参数。当前显式 capture 能保留本项目请求/响应和版本、endpoint、Region 元数据，但结构化字段路径快照、归一化 schema diff 与跨 active version 报告仍未实现；官方桌面客户端 wire contract 也未采集。

当前 Admin API：

```text
GET  /config/runtime                 # response 中包含 kiroVersionLifecycle
POST /config/kiro-version/check        # 立即拉取并解析，不改变 active_version
POST /config/kiro-version/apply       # 应用 candidate 或指定版本
POST /config/kiro-version/pin         # 固定指定版本
POST /config/kiro-version/rollback    # 回到 previous/configured
```

版本状态写入 PgSQL 独立表，并通过 Redis 事件和周期 reload 同步其他实例。PgSQL 使用 generation 条件写入；check 结果在冲突后基于最新状态重试，apply/rollback 冲突则返回 Conflict。版本操作保持人工触发；自动回退不在范围内。

新版本状态通过独立 PostgreSQL 行持久化并可热加载；旧实现的 `LATEST` 只存在进程内，旧实例退出后无法恢复。旧配置中保存的 `kiroVersion` 也不会因新缺省值自动改写；已有部署通过立即 check 和显式 apply 更新。版本 resolver 被 inference、MCP、Profile discovery、refresh、usage/overage 和 models 共用，因此 active version 是全服务请求身份。

每个逻辑协议操作在开始时固定一次 active version，并在运行时 `Config` 快照中传递；该字段不序列化、不写入配置存储。账号调度内的 token refresh、Region/endpoint 重试、Profile discovery、最终请求及 request/response capture 都沿用同一版本。全局模型目录同步将所有采样账号视为一次操作；set-overage 与随后额度查询也共用一次版本快照。并发 apply 只影响之后开始的操作，不会让进行中的操作混用版本。

### 5.2 生效来源与显式回退

统一使用三个 active 来源：

| `active_source` | 请求头使用 | 关闭自动检查后是否回退 |
| --- | --- | --- |
| `configured` | configured 版本，或显式 rollback 恢复的 previous configured 快照 | 保持已选中的版本 |
| `pinned` | 持久化的管理员指定 `active_version` | 不回退，除非显式 rollback |
| `applied` | 管理员显式 apply candidate 或指定版本 | 不回退，只停止后续检查 |

这解决“更新后是否稳定、关闭开关是否回退”的歧义：

- **关闭自动检查**不应自动回滚已经明确应用或 pin 的版本；它只停止后续发现，不提供隐式回滚。
- **显式 rollback**改变 active version，并恢复记录的 `previous_version` 快照；即使 configured 值之后变化，也不会改成新的 configured 版本。该动作由管理员触发；系统不判断稳定性，也不自动改变版本。
- 为兼容当前行为，没有 active override 时使用 `active_source=configured`；历史 `auto`/`auto_applied` 状态读取后统一保存为 `applied`。`kiroVersionAutoSyncEnabled` 只控制版本发现，不控制 resolver；关闭检查不会回退已应用版本。
- 旧进程内 `LATEST` 没有可迁移的持久化数据，不能声称启动时可恢复。旧配置保持原值，管理员通过 check/apply 完成升级；首次 apply 会把旧 configured 版本记录为 previous。

版本状态使用 generation CAS 和 Redis reload；关闭检查、重启或关闭自动同步均不改变 active。PostgreSQL 并发集成测试需在提供 `KIRO_RS_TEST_POSTGRES_URL` 时运行。

### 5.3 版本与协议证据

`candidate_version` 只表示发现的官方版本，不代表兼容性已验证。后续证据按以下顺序采集：

1. 记录 metadata 的 `currentRelease` 与检查时间；
2. 检查官方安装包并将 IDE/streaming SDK 配对作为版本证据；当前 `1.4.154 -> 1.0.44` 已检查；
3. 使用真实 FREE IdC 样本验证 MCP header 差异，并用 fake upstream fixture 验证其他请求形状和手动版本切换；
4. 当前证据没有覆盖官方客户端全部请求参数、body schema 与 SSE event；需官方客户端 wire capture 才能补齐；
5. 未经证实的参数不自动改写 converter 或 active version。

fake upstream 只能验证本项目客户端行为，不能证明官方上游接受该版本，也不产生自动健康状态或回滚动作。

## 6. 新版本参数抓取与差异报告

### 6.1 能抓什么

版本 metadata 能抓到发行信息和资产 URL，不能推导新请求参数。请求参数必须从实际运行时采集：

- 请求方法、URL host、path、`protocol_endpoint`、Region；
- 规范化后的 headers 名称和值类别/哈希；
- body JSON 的字段路径、类型、数组长度、字符串长度和哈希；
- `additionalModelRequestFields`、thinking/output config、tool schema 形状；
- SSE event key 顺序、终止事件、usage 字段形状；
- HTTP 状态、结构化 reason、首字节耗时和总时长。

字段结构化快照的目标是不采集 prompt、token、refresh token、API key、cookie、完整 authorization、工具参数值或图片内容。`scripts/kiro_capture_schema_diff.py` 对两份 capture 归档按 phase/endpoint 比较 KiroIDE 身份、固定 SDK/API 标识、host/path、header 名称、body 字段类型与响应状态，只输出结构不输出值。当前显式 LLM capture 实际会记录完整请求/响应体；默认脱敏指定凭据类 headers，开启 `includeSecrets` 会保留这些原值，因此完整 capture 只应在 fake upstream 与合成 fixture 中使用。不能把这条完整采集管线描述为天然只保存结构或摘要。

要区分两类采集目标：

1. 本项目 capture 能证明本项目发出的 wire request 和收到的响应形状，可用于 fake upstream 下复现本地协议、对比 `active_version` 变化前后的服务行为。
2. 本项目 capture 不能凭空揭示官方 Kiro 新版桌面客户端内部新增的字段。若目标是官方客户端真实 wire contract，必须在可控官方客户端场景对其自身流量采集并做同样的脱敏；版本 metadata 或仅修改 `KiroIDE-*` 请求头都不能替代这一步。

目标流程是记录版本、endpoint、auth method、API Region、Profile 类型、请求方法/host/path、headers 结构、body 字段结构、SSE event 结构与 fake upstream 结果；`scripts/kiro_capture_schema_diff.py` 生成该差异报告（见 §13）。完整 LLM capture 限于合成请求，不读取、导入或修改真实账号池；本项目脱敏快照不能证明真实官方客户端的新 wire contract。

### 6.2 采集流程

```text
版本 candidate
  -> 固定本项目 active version 与 SDK/API UA 基线
  -> fake upstream 请求/响应 fixture
  -> 脱敏 wire snapshot
  -> normalized schema diff
  -> 人工确认差异并记录未证实项
  -> 需要官方客户端合同证据时另行采集其真实版本/SDK 配对
```

差异报告必须区分：

- 仅身份变化：`KiroIDE-*` 版本文本；
- 固定 SDK/API 版本变化；
- 新增/删除/类型变化的 body 字段；
- SSE event 或 usage 形状变化；
- 仅 host/Region 变化。

不能因为发现字段变化就自动改写 converter；先生成 profile 和回归 fixture，再单独立项修改协议。

## 7. 服务二进制更新不在范围内

本项目服务镜像/二进制继续由现有 release 与部署流程更新。参考项目的 check/download/stage/restart updater 不移植；本 topic 的 `kiroVersion` 只影响 Kiro 客户端身份字符串，不检查或替换 `kiro.rs` 自身。

## 8. 配置与管理面设计

### 8.1 运行时配置

当前已存在字段与后续建议字段分开记录：

```text
defaultEndpoint: String                         # 全局 primary；不是独立 preferred 字段
credential.endpoint: Option<String>              # 账号级 primary，优先于 defaultEndpoint
kiroEndpointFallbackEnabled: bool = false
kiroEndpointFallbackOrder: Vec<String> = [codewhisperer, amazonq, runtime]
kiroUpstreamRegionRotationEnabled: bool = false
kiroRegionProfileArnMismatchAllowed: bool = false
kiroVersion: String                               # 当前配置版本
kiroVersionAutoSyncEnabled: bool = false          # 当前实现：只控制 candidate 发现

# 后续建议，不应直接加入每个凭据：
kiroPreferredEndpoint: Option<String>             # 若需要独立于 defaultEndpoint 的全局首选
```

版本状态字段不应混在每个账号凭据里；endpoint primary 仍属于凭据，endpoint fallback 策略属于运行时配置。

### 8.2 管理 UI

分成两个区域，避免一个“端点/版本”开关承载多个语义：

1. **协议端点调度**：首选 endpoint、候选链、同账号 fallback、Region rotation、当前 generation。
2. **Kiro 客户端身份版本**：当前 active、candidate、previous、立即检查、应用、pin、显式 rollback。

页面应明确区分“发现”和“应用”，并显示关闭自动检查不会回退已应用版本。版本状态不显示未经验证的“稳定”标签。

## 9. 实施阶段

### Phase 0：协议基线和观测合同（部分完成）

- Builder ID usage/MCP Profile 规则已实现；FREE IdC 官方 MCP A/B 已复现并修复遗漏 header 的 400：缺 header 时上游返回 `profileArn is required for this request`，补 Builder ID placeholder 后返回 200。
- 端点候选链、inference Region 资格和版本 candidate/active 分离已实现；请求采集记录 endpoint/Region/clientVersion。
- FREE IdC A/B 确认漏发 Profile header 是本次 MCP 400 根因；代码已补 Builder ID placeholder。修复后的普通协议 400 才进入 `InvalidRequest` fail-fast，不能把该边界描述成对本次缺 header 400 的修复。
- usage 归因仍以账号级 attempt 为主；结构化字段路径快照、normalized schema diff、官方客户端参数与 SSE wire 对照未覆盖。

### Phase 1：Region 语义修正（核心实现，focused/full Rust 与 UI build 已通过）

- inference 在开关开启时可对传输/502/504 尝试合法备用 Region；Profile ARN Region 不匹配由独立开关控制。
- models、MCP、Profile discovery 和 REST usage 按各自 operation 构造 Region 候选；Portal usage 固定 Portal host，overage 写操作单 Region，refresh 固定 auth Region，不借用 inference Region 游标。
- focused 测试覆盖连接失败、合格 502/504、Profile ARN 开关和 MCP header/400 边界。账号级 429 只换账号并按配置进入下一轮，不推进 Region。
- 全量 Rust 已通过 `3023 passed / 0 failed / 18 ignored`；`ui` 与 `admin-ui` 构建均通过。

### Phase 2：协议 endpoint 候选链（实现已落地，真实备用 endpoint 协议验证未完成）

- endpoint capability registry、候选解析、共享发送 budget 和错误白名单已接入；fallback 默认关闭。
- CLI 保持 API Key 单端点语义，OAuth 兼容族才允许候选 fallback。
- inference、models、MCP 可在允许的连接/502/504 情形换同账号 endpoint；models/MCP 同时可按独立 Region 候选换 route，普通协议 400 不换 endpoint、Region 或账号。
- fake upstream 覆盖同账号 fallback、结构化错误分类、身份刷新门禁和 physical send cap；endpoint-specific 400/404 白名单不在当前 fallback 规则内。
- FREE Social/IdC 真实上游已验证：`codewhisperer`、`amazonq` 支持推理、模型列表和 MCP；`runtime` 域名支持 MCP，推理对 FREE 账号返回 429 限流，`ListAvailableModels` 返回 404 UnknownOperation，因此 runtime 不进入模型列表候选链，作主端点时模型列表改走 q host。

### Phase 3：版本手动生命周期（核心实现，focused tests/UI build 已通过）

- 立即 check、candidate/apply/pin/rollback API、PgSQL 状态和 Redis reload 已实现。
- 旧 `kiroVersionAutoSyncEnabled` 已兼容迁移为“发现 candidate，不自动改变 active”。
- 只支持管理员显式 apply、pin 和 rollback；不做自动版本回退。
- 关闭自动检查只停止发现，不隐式回滚已应用版本。
- rollback 恢复保存的 previous 版本快照；configured 值后续变化不会改变回滚结果。
- 缺省版本为 `1.4.154`，对应已检查 SDK `1.0.44`；已有数据库里明确保存的旧版本需管理员显式 apply。

## 10. 验收矩阵

### 端点与账号

1. 默认配置的请求路径、headers、body、账号选择和预算与当前版本一致。
2. 显式 credential endpoint 优先于全局 `defaultEndpoint`；独立 `preferredEndpoint` 仍是后续建议字段。
3. 同账号 fallback 复用相同 token、machine identity、profile ARN 和 request context。
4. CLI 不会因启用 IDE fallback 被改发到 IDE 协议。
5. endpoint fallback 成功后不写回 credential primary。
6. 401/402/403、额度耗尽、malformed 400 不进入无条件 endpoint fallback。
7. 网络/瞬态错误在未提交下游时按白名单切换；已提交后绝不重放。
8. 每次物理发送受共享 inference budget 和 `max_sends` 限制；预算耗尽时停止。
9. endpoint 与 Region 同时启用时 route 数量受候选链、Region 槽位和发送上限约束；外层账号重试也计入同一个物理发送上限。
10. 官方 operation 合同独立：refresh 不走推理 endpoint fallback，Builder ID `getUsageLimits` 带兼容占位 Profile，API Key 不带 OAuth/Profile 字段。

### Region

1. us 账号顺序为 us -> eu，eu-* 账号顺序为 eu -> us。
2. 单账号普通模式能在 fake upstream 中尝试备用 Region。
3. Region override 不写回 auth/api region 或凭据文件。
4. 关闭 Region rotation 后不残留旧游标或健康抑制阻断主 Region。

### 版本与参数

1. `check` 立即拉取 metadata，失败时保留旧 candidate/active。
2. candidate、active、configured、previous 可区分并可查询；不维护自动回退基线。
3. 关闭自动检查不改变 pinned/applied active；rollback 只通过显式操作发生。
4. 多实例 CAS 不丢失版本状态；当前已有 generation 条件写入、check 冲突重试和 apply/rollback 冲突返回，操作幂等与 operation id 仍属后续工作。
5. 自动检查关闭、服务重启、配置热加载后 active version 保持已应用值；只有显式 rollback 能修改它。
6. token refresh 请求体字段值固定脱敏，采集 URL 时凭据类 userinfo/query 值脱敏；headers 默认按采集配置脱敏。现有完整 LLM capture 会记录请求和响应体，不强制限制为 fake upstream，因此本轮验证只用 fake upstream/合成 fixture，脱敏 wire snapshot 不含 prompt 或参数原值。
7. 新旧版本差异报告能区分身份、固定 SDK、body schema、SSE event 和 host 变化。
8. 参数差异不会自动修改 converter，必须经过 fixture 和人工确认。
9. fake upstream 报告记录请求数量、认证类型 fixture、发送上限、实例/PID/config、请求结果和脱敏错误 ID，不记录任何凭据值。

### 查询协议缺口

1. Portal GetUser* CBOR 已接入非 API Key usage 查询并固定 Portal host；REST `getUsageLimits` 使用独立 Region 候选，只有连接失败、合格 502/504 或明确 Profile ARN/Region mismatch 403 才继续。
2. Profile discovery 仅在 External IdP 缺真实 ARN 时调用；连接失败和合格 502/504 可换 Region，403/空列表按无结果处理，不继续候选。
3. Models/MCP 已有同账号 endpoint 与 Region fallback；models 分页中途失败会丢弃当前 route 的部分结果后重拉，MCP 400 按协议请求错误直接结束。
4. `1.4.121`、`1.4.154` 到 SDK `1.0.44` 有安装包证据；其他固定 SDK/API 参数、body schema 和 SSE event 仍需可追溯的官方 wire evidence。

## 11. 风险、取舍与待决问题

### 已接受的设计取舍

- 端点候选链优先可靠性和可解释性，不追求把所有 host 都轮一遍。
- 版本自动发现与人工版本操作分离；默认不把官方最新等同兼容。
- 参数抓取默认只记录结构和摘要，完整请求体只允许隔离诊断 profile。
- 账号绑定保持现有 scheduler 语义，避免同账号 endpoint fallback 变成跨账号隐式共享。

### 待决问题

1. 首期 endpoint capability 是否只启用当前 `ide`/`cli` 的受限语义，还是导入某个已验证的 mata endpoint 实现？当前设计默认不跨 `ide` 与 API Key `cli` 回退。
2. endpoint-specific 400/404 reason 白名单不在当前 fallback 规则内；MCP Profile header 缺失已由 FREE IdC 上游 A/B 确认并修复。
3. 旧 `LATEST` 进程值不可迁移；已有配置通过显式 check/apply 更新，不会因默认值变化自动覆盖。
4. 多实例版本状态 generation/CAS 已实现；操作幂等和 Redis 事件冲突仍需独立评估。

## 12. 可执行入口

当前分析已定位实现与协议证据缺口；后续执行前的门禁为：

- 真实备用 endpoint 仍需使用项目根目录 FREE 账号逐项验证；当前已有的 MCP A/B 只证明 Profile header 修复，不等价于所有 endpoint wire 兼容；
- usage/overage/Profile 的 operation-specific fallback 仍需用真实协议证据确认边界；当前代码只对明确的连接、网关错误和 Profile ARN/Region mismatch 403 放行候选，overage 写操作仍不重试；
- 官方客户端完整 headers/body/SSE 的版本差异抓取；
- 提供 `KIRO_RS_TEST_POSTGRES_URL` 后运行 PostgreSQL CAS 集成测试。

相关分析：[当前项目与 kiro-rs-mata 多端点调度对比](../../../../analysis/kiro-rs-mata-endpoint-routing-comparison-20261110.md)、[Kiro 官方协议操作与参考项目对照](../../../../analysis/kiro-official-protocol-operations-reference-comparison-20261110.md)、[kiro-manager-lite 协议与版本调研](../../../../analysis/kiro-manager-lite-protocol-and-version-analysis-20261110.md)。

## 13. 2026-11-11 真实上游验证矩阵

账号：项目根目录 FREE JSON（147 个）。指定实例 `127.0.0.1:19023`，版本 `0.11.107` 与 `1.4.154` 各跑一遍；推理只用 `claude-haiku-4.5`。

| Operation | ide | codewhisperer | amazonq | runtime |
| --- | --- | --- | --- | --- |
| 推理 `generateAssistantResponse` | 200 | 200 | 200 | 429（FREE 账号限流，非协议错误） |
| `ListAvailableModels` | 200 | 200 | 200 | 原生 404 UnknownOperation；修复后走 q host 返回 200 |
| WebSearch MCP | 200 | 200 | 200 | 200 |

| 账号操作 | 结果 |
| --- | --- |
| 147 个 FREE 账号信息刷新（Portal CBOR usage） | 修复前 0/147；修复后 128 成功，19 个为 refresh token 被上游 403 拒绝 |
| WebSearch 端到端（`/v1/messages` 原生工具） | 修复前账号有模型列表后全部 `websearch_mcp_scheduler_unavailable`；修复后 10 条结果 |
| 版本 check → apply → 重启 → pin → rollback | 修复前 check 后 apply 必然 409；修复后全链路成功且重启后 active 保持 |

本轮通过真实上游发现并修复的协议缺陷：

1. Portal `GetUserUsageAndLimits` 对 Social token 按 `Idp` cookie 校验来源；导入 JSON 缺 `provider` 时原实现发送 `BuilderId`，上游 401。现缺省按 `Google` 发送（`Github` 同样被接受）。
2. Portal CBOR 响应把整数写进浮点字段（如 `currentOverages: 0`）、日期包在 tag 1 里，原实现直接反序列化失败。现先转 JSON 值再解析。
3. 版本状态 CAS 写入用 `INSERT ... SELECT ... WHERE $2 = 0`，已有行时永远不会进入 `ON CONFLICT` 分支，第二次及以后的写入全部失败；改为 expected=0 走插入、否则走条件 `UPDATE`。原集成测试从未实跑，用临时库跑后同样暴露该缺陷。
4. `KiroIDE-1.4.x` 下 `ListAvailableModels` 要求 Builder ID/IdC 带 placeholder `profileArn`，且返回 `"supportedInputTypes": null`；原实现一个被拒、一个解析失败。
5. runtime 域名不提供 `ListAvailableModels`。
6. 不带模型的请求（WebSearch MCP）在账号有自身或订阅等级模型列表后被判 `model_not_supported`，导致 WebSearch 不可用。现无模型请求不受模型列表限制。
7. 官方 1.4.154 的 runtime 流解码器把 `:message-type: event` 帧里的 `error`、`throttlingError`、`validationError`、`serviceUnavailableError` 当作异常抛出；本项目原把它们归为 Unknown 丢弃，流在零输出时结束，表现为空回。现映射为 InternalServer / Throttling / Validation / ServiceUnavailable 异常，前三类之外按上游瞬态失败处理。
8. 组织 IdC 与 Builder ID 导入形态相同，原实现只对 External IdP 调 `ListAvailableProfiles`，组织 IdC 永远拿不到真实 profileArn。现对 profileArn 为空且未声明 BuilderId 的 IdC 也查询：查到真实 ARN 写回；收到 403 视为 Builder ID，把占位 ARN 存为标记，之后不再查询。导入后与额度查询前都会先补全；多个 profile 时优先账号 API Region。FREE IdC 实测：重新导入后 403 → 存占位 → 模型列表、账号信息、单号测试均成功，且未再次查询。

`kiro_capture_schema_diff.py 0.11.107.zip 1.4.154.zip` 在 api / mcp / models 三类请求上只报告 KiroIDE 身份与 `api/codewhispererstreaming#1.0.34 → 1.0.44` 的差异；host、path、header 名称、body 字段类型与响应状态均无变化。该结论只覆盖本项目发出的请求，不能代表官方桌面客户端的完整 wire contract。

