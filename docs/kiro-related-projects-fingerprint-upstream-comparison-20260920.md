# 同类型 Kiro 项目：账号指纹、上游调用与调度实现对比

**分析日期：** 2026-09-20  
**近期窗口：** 2026-09-06 至 2026-09-20（含首尾）  
**项目背景：** 当前项目是基于 `kiro.rs` 的深度二开；本文对多个直接 fork、深度二开、多账号网关和近期活跃的 Kiro provider 进行源码级对比。  
**排除项：** 用户指定不再学习的项目未纳入证据、排名或结论。  
**方法：** 通过公开 Git 仓库的 commit history、源码和测试进行核验；README/发布说明只用于定位，不作为唯一事实来源。重点区分“近期核心代码提交”和“只有文档/发布提交”。

## 一、先给结论

没有一个项目同时解决了以下所有问题：

```text
每账号独立持久化身份
    + 协议合法的 profileArn
    + 账号级 machineId
    + 账号级 UA/版本 profile
    + 账号级 region/auth-region 分离
    + 账号级 proxy/TLS/HTTP client
    + 账号级 quota/429/402/403 故障状态
    + 会话 affinity 与上下文回放
    + 跨 region、跨 endpoint、跨账号的可证明调度
```

不同项目解决的是不同层面：

- **当前 v0.0.167**：region rotation、berserk、429 三分法、外部池质量调度最完整；
- **sunerpy/kiro-provider**：账号持久化、session affinity、context replay、per-account capacity 最深；
- **minpeter/kiro-lb**：endpoint affinity、账号健康、quota headroom、failover 诊断很成熟；
- **Yaocool/kiro-proxy**：身份 profile、IDE/CLI UA 基线、账号级 machineId/region/proxy 的组织方式最清楚；
- **TsinHzl/kiro2cc-proxy**：profileArn 生命周期、补全、持久化、缺失禁用原因处理最直接；
- **ZyphrZero/kiro.rs**：credential region 和无 profile ARN fallback 的边界处理有近期实证；
- **OmniProxy**：跨 provider 的失败分类、transport error 不误伤账号、endpoint/proxy pool 经验丰富；
- **suprim-gateway**：402 quota 直接换号，但 machineId 是主机级而不是账号级；
- **AIClient2API**：Builder ID/profileless 路由重构有价值，但身份指纹较弱；
- **mikeyobrien/pi-provider-kiro**：近期代码质量和流式/usage 管理活跃，但不是多账号网关级身份隔离方案。

因此，针对“每个账号要有独立 machineId 等信息，同时要优化关键上游调度”的目标，最合理的参考组合是：

```text
Yaocool 的 identity profile
+ TsinHzl / ZyphrZero 的 profileArn 生命周期
+ kiro-provider 的 per-account capacity/session replay
+ kiro-lb / OmniProxy 的健康分类与 endpoint failover
+ 当前 v0.0.167 的 region rotation + berserk + quality scheduling
```

## 二、近期活跃项目清单

| 项目 | 最近核心/主要提交 | 近期是否有核心代码 | 主要参考方向 |
|---|---|---:|---|
| [minpeter/kiro-lb](https://github.com/minpeter/kiro-lb) | `77d079a`，2026-09-18 | 是 | endpoint/account health、quota、failover |
| [sunerpy/kiro-provider](https://github.com/sunerpy/kiro-provider) | `e211adc`，2026-09-18 | 是 | account capacity、session/replay、auth lifecycle |
| [mxuanvan02/OmniProxy](https://github.com/mxuanvan02/OmniProxy) | `1269ac5`，2026-09-18 | 是 | 多 provider pool、transport/error classification |
| [Yaocool/kiro-proxy](https://github.com/Yaocool/kiro-proxy) | `63e70aa`，2026-09-14 | 是 | 统一 identity profile、UA、region、API key |
| [TsinHzl/kiro2cc-proxy](https://github.com/TsinHzl/kiro2cc-proxy) | `c7fa179`，2026-09-20；核心身份提交为 2026-09-15 | 是 | profileArn 补全、持久化、缺失禁用 |
| [ZyphrZero/kiro.rs](https://github.com/ZyphrZero/kiro.rs) | `3219d1c`，2026-09-17 | 是 | credential region、profileless fallback |
| [justlovemaki/AIClient2API](https://github.com/justlovemaki/AIClient2API) | `b12e25e`，2026-09-16 | 是 | Kiro auth、Builder ID 路由 |
| [suprim-corp/suprim-gateway](https://github.com/suprim-corp/suprim-gateway) | `f941a81`，2026-09-13 | 是 | 402/429/503 rotation |
| [mikeyobrien/pi-provider-kiro](https://github.com/mikeyobrien/pi-provider-kiro) | `ef72fc8`，2026-09-14 | 是 | 流式 timer、usage/cache 估算 |
| 当前项目 `kiro.rs` v0.0.167 | `35ce08e`，2026-09-20 | 是 | region matrix、berserk、quality scheduling |

## 三、逐项目事实分析

## 1. minpeter/kiro-lb

**仓库：** <https://github.com/minpeter/kiro-lb>  
**HEAD：** `77d079a5b0a6a8485cafd0d423018b3c17b08908`，2026-09-18

### 近期核心提交

- `33d95ca`，2026-09-12：复用 streaming pooled connections；
- `a200631`，2026-09-12：对齐 Kiro IDE 1.0.437 generation wire contract；
- `8c07eab`，2026-09-14：streaming reuse 合并；
- `47da998`，2026-09-18：Responses API；
- `3de54a6`、`cc54ee0`、`77d079a`，2026-09-18：错误体和中途流错误隔离。

### 身份/指纹事实

`kiro/utils.py`：

- IDE build 固定为 `KiroIDE-1.0.437-...-KAS/0.54.0`；
- `User-Agent` 的 OS、Node、SDK、API label 是固定模板；
- `get_machine_fingerprint()` 是 `sha256(hostname + username + "-kiro-lb")`；
- 该 fingerprint 由 gateway 主机和运行用户决定，不由 refresh token/profileArn 决定。

这意味着同一台 gateway 上的多个账号会共享这个机器 fingerprint。它适合标识“这个网关实例”，不满足“每个账号一个设备身份”。

### 账号/调度事实

`kiro/account_manager.py` 与 `kiro/endpoints.py` 记录：

- 账号 key 可以是 credential path 或 refresh-token hash；
- 账号独立记录 `failures`、`rate_limited_until`、`quota_exhausted_until`、`suspended_until`、`auth_dead_until`；
- 记录 quota headroom、quota reset、quota overage；
- 429 cooldown 与 circuit breaker 分开；
- endpoint affinity 是 `(account_key, model) -> endpoint`；
- endpoint cooldown 只在进程内，冷却 endpoint 会后置但不会永久删除；
- `runtime.{region}.kiro.dev` 优先，amazonaws endpoint 作为兼容路径；
- API region 有 per-account override，SSO/OIDC region 与 API region 分离；
- 内部 credential overlay 可单独保存 token 旋转结果。

### 优点、缺口与适用性

**优点：**

- 健康状态和 quota 状态明显比简单轮询成熟；
- endpoint affinity 能降低同一模型在不同 upstream 之间反复冷启动；
- 近期核心代码活跃，错误体和流式失败处理持续修正。

**缺口：**

- machine fingerprint 是 gateway-level，不是 account-level；
- UA 的 OS/Node/SDK/Kiro 版本是全局固定；
- endpoint cooldown 和 affinity 进程重启丢失；
- 没有当前项目 167 的 Redis 外部池 EWMA/probation/recovery-ramp。

**对当前项目的参考：** 适合借鉴“账号健康状态、endpoint affinity、quota headroom、失败分类”，不适合直接借鉴其 machine fingerprint 方案。

源码链接：

- [kiro/utils.py](https://github.com/minpeter/kiro-lb/blob/77d079a5b0a6a8485cafd0d423018b3c17b08908/kiro/utils.py)
- [kiro/account_manager.py](https://github.com/minpeter/kiro-lb/blob/77d079a5b0a6a8485cafd0d423018b3c17b08908/kiro/account_manager.py)
- [kiro/endpoints.py](https://github.com/minpeter/kiro-lb/blob/77d079a5b0a6a8485cafd0d423018b3c17b08908/kiro/endpoints.py)

## 2. sunerpy/kiro-provider

**仓库：** <https://github.com/sunerpy/kiro-provider>  
**HEAD：** `e7ac0bd8508f5fac33da218e233534da159628bc`，2026-09-18，package `3.5.5`

### 近期核心提交

- `e211adc`，2026-09-18：修复并发调度与客户端上下文回放；
- `65a4ad2`，2026-09-17：禁止 relogin 更换既有 profile；
- `3946a0b`，2026-09-17：修复 IDC profile 与 Claude reasoning 回放；
- `50900f6`，2026-09-16：历史会话额度耗尽后的账号切换；
- `4b62e51`，2026-09-15：共享原生状态并隔离 provider 路由；
- `f681223`，2026-09-16：卡死会话连续停滞后重新选路；
- `ee71f26`，2026-09-11：V3 原生调用保真与续接。

这些提交是实际 TypeScript 核心代码，不是只改 README。

### 身份/持久化事实

`src/storage/account-record.ts` 的 `ManagedAccount`/SQLite row 包含：

- `id`、`email`、`authMethod`；
- `region`、`oidcRegion`；
- `clientId`、`clientSecret`；
- `profileArn`、`startUrl`；
- `refreshToken`、`accessToken`、`expiresAt`；
- `rateLimitResetTime`、`isHealthy`、`unhealthyReason`、`recoveryTime`；
- `failCount`、`lastUsed`、`usedCount`、`limitCount`、`lastSync`、`overageCount`。

region 和 `oidc_region` 从 SQLite 读出后会经过 allowlist 验证，避免污染数据库后把凭据重定向到恶意 host。

### 调度事实

`src/core/account-selection.ts` 支持：

- sticky；
- round-robin；
- lowest-usage；
- permanent error/quota/rate-limit/recoveryTime 过滤。

`src/core/account-capacity.ts` 与 `pipeline-runtime.ts` 增加：

- per-account active/waiting lease；
- per-account queue depth；
- `leastQueuedAccountIds()`；
- 并发范围 1..10；
- queued request admission；
- capacity 变化后唤醒等待者。

近期修复的核心问题是“账号选择、容量租约、会话上下文和 replay 必须一致”，而不是简单在 429 后换一个数组元素。

### 缺口

- 没有完整的账号级 machineId/runtime fingerprint 派生系统；
- SDK 版本和 `USER_AGENT = "KiroIDE"` 主要是全局常量；
- region 是强类型和持久化字段，但没有当前项目 167 的 `region_override`/berserk 矩阵；
- 代理/TLS 不是按账号建立完整的独立设备画像。

**对当前项目的参考：** 这是账号级并发、session affinity、context replay、health/CAS 的首选参考；不能直接当作每账号指纹方案。

源码链接：

- [account-record.ts](https://github.com/sunerpy/kiro-provider/blob/e7ac0bd8508f5fac33da218e233534da159628bc/src/storage/account-record.ts)
- [account-selection.ts](https://github.com/sunerpy/kiro-provider/blob/e7ac0bd8508f5fac33da218e233534da159628bc/src/core/account-selection.ts)
- [account-capacity.ts](https://github.com/sunerpy/kiro-provider/blob/e7ac0bd8508f5fac33da218e233534da159628bc/src/core/account-capacity.ts)
- [kiro/auth.ts](https://github.com/sunerpy/kiro-provider/blob/e7ac0bd8508f5fac33da218e233534da159628bc/src/kiro/auth.ts)

## 3. mxuanvan02/OmniProxy

**仓库：** <https://github.com/mxuanvan02/OmniProxy>  
**HEAD：** `1269ac5a130f755a8709d14b3a362d718f33203c`，2026-09-18

### 近期核心提交

- `0036685`，2026-09-15：transport-level failure 不再错误 parking 账号；
- `8fe48d8`，2026-09-16：external SSE truncation 只换号、不 cooldown；
- `4982fae`，2026-09-16：对齐 Antigravity client fingerprint，未知 403 不再直接 ban；
- `1269ac5`，2026-09-18：没有可服务模型时回退到可服务模型。

需要准确区分：`4982fae` 的“client fingerprint”直接修的是 Antigravity，不是 Kiro；但它展示了一个重要的上游原则：客户端版本、body metadata、Client-Metadata header 必须按同一真实版本基线构造，未知 403 不能未经验证就永久禁用账号。

### 身份/网络事实

`config.Account` 记录：

- account UUID、email、auth method；
- access/refresh token；
- `Region`、`MachineId`、`ProfileArn`；
- per-account `ProxyURL`；
- external IdP 元数据；
- endpoint/base URL。

`proxy/kiro_headers.go`：

- machineId 从 refresh token 或 access token SHA-256 派生；
- Kiro IDE UA 带该 machineId；
- `tokentype: API_KEY` 与 `TokenType: EXTERNAL_IDP` 按认证类型添加；
- ksk API key 改用 CLI 形态 UA；
- account-level proxy URL 会缓存独立 `http.Client`。

### 调度/失败分类事实

`pool/cooldown_class.go` 和 `proxy/account_failover.go` 将错误分成：

- transient；
- rate limited；
- auth failed；
- no balance；
- Kiro truncated；
- model unavailable；
- unknown。

`0036685` 修复了 `http2: timeout awaiting response headers` 和 `can't assign requested address` 被误判为账号失败的问题。`8fe48d8` 将外部 SSE 截断视为“上游协议/传输问题”，换账号但不写账号 cooldown。

### 优点、缺口与适用性

**优点：**

- 跨 provider pool 的错误分类很细；
- 账号代理可以独立；
- Kiro endpoint/profileArn/region/API key 有较清楚的分支；
- 对“网络故障不是凭据故障”的边界处理优于简单 3 strikes。

**缺口：**

- machineId 派生虽然是账号级，但只取 token，不自动隔离 OS/Node/SDK/TLS；
- 同一 proxy URL 仍共享出口和 agent 缓存；
- `4982fae` 的版本 fingerprint 经验主要来自 Antigravity，不应未经验证直接迁移到 Kiro；
- pool 质量策略与当前 v167 的 Redis EWMA 不是同一套实现。

**对当前项目的参考：** 重点借鉴 transport/stream truncation/error class，不要把 Antigravity 的 fingerprint commit 当作 Kiro 已经验证的参数表。

源码链接：

- [kiro_headers.go](https://github.com/mxuanvan02/OmniProxy/blob/1269ac5a130f755a8709d14b3a362d718f33203c/proxy/kiro_headers.go)
- [account_failover.go](https://github.com/mxuanvan02/OmniProxy/blob/1269ac5a130f755a8709d14b3a362d718f33203c/proxy/account_failover.go)
- [cooldown_class.go](https://github.com/mxuanvan02/OmniProxy/blob/1269ac5a130f755a8709d14b3a362d718f33203c/pool/cooldown_class.go)
- [4982fae fingerprint diff](https://github.com/mxuanvan02/OmniProxy/commit/4982fae1897f78d502ec6fdfc5ec5fbbd65334a7)

## 4. Yaocool/kiro-proxy

**仓库：** <https://github.com/Yaocool/kiro-proxy>  
**HEAD：** `7bac2dd8a2884ac0c05c9dd8c1f7ff86813769bf`，2026-09-16

### 近期核心提交

- `04fd7a1`，2026-09-07：增加 Kiro API key runtime 支持；
- `63e70aa`，2026-09-14：更新 upstream client identities；
- `7bac2dd`，2026-09-16：模型 token limits。

### 身份 profile 的事实

`crates/kproxy-kiro/src/identity.rs` 把这些参数集中管理：

- IDE version `1.0.437`；
- CLI version `2.21.4`；
- IDE SDK、Node、CLI SDK、API version、Rust version；
- IDE User-Agent 与 `x-amz-user-agent`；
- CLI streaming/management 两类 service identity；
- IDE machineId 注入。

`crates/kproxy-core/src/account.rs` 的账号结构同时保存：

- account ID、email、label；
- `machine_id`；
- `profile_arn`；
- `upstream_user_id`；
- `region`；
- auth method；
- access/refresh/client credentials；
- usage/subscription；
- tags、created_at、credit exhausted。

### region、endpoint、API key 的事实

`crates/kproxy-kiro/src/endpoint.rs`：

- credential region 进入 endpoint URL；
- API key/GovCloud 强制使用 runtime；
- profile ARN region 对非 GovCloud 账号可作为 API region 依据；
- GovCloud 不允许被 BuilderId placeholder ARN 带入 commercial partition；
- endpoint cache 按 `(account id, purpose)` 保存 preferred/disabled endpoint；
- generation 与 models 有分别的 endpoint purpose。

### 优点、缺口与适用性

**优点：**

- identity profile 比多数项目更集中、更容易审计；
- 近期 commit 直接修订客户端版本和 service identity；
- 账号结构明确保存 `machine_id`，适合做每号持久化；
- API key、GovCloud、runtime endpoint 的分流清晰。

**缺口：**

- IDE 的 OS/Node/SDK/version 是统一 profile 常量，不是每账号独立；
- CLI UA 没有 machineId；
- `machine_id` 字段独立不等于 TLS/连接池/出口独立；
- 版本基线需要跟随官方客户端更新，不能只更新字段名。

**对当前项目的参考：** 如果当前项目要补“账号级 identity profile”，这是近期最直接的结构参考；必须保留“协议允许共享版本 profile、账号级 machineId”两层语义，不能把所有 UA 字段都伪装成账号独立。

源码链接：

- [identity.rs](https://github.com/Yaocool/kiro-proxy/blob/7bac2dd8a2884ac0c05c9dd8c1f7ff86813769bf/crates/kproxy-kiro/src/identity.rs)
- [account.rs](https://github.com/Yaocool/kiro-proxy/blob/7bac2dd8a2884ac0c05c9dd8c1f7ff86813769bf/crates/kproxy-core/src/account.rs)
- [endpoint.rs](https://github.com/Yaocool/kiro-proxy/blob/7bac2dd8a2884ac0c05c9dd8c1f7ff86813769bf/crates/kproxy-kiro/src/endpoint.rs)
- [63e70aa identity update](https://github.com/Yaocool/kiro-proxy/commit/63e70aa)

## 5. TsinHzl/kiro2cc-proxy

**仓库：** <https://github.com/TsinHzl/kiro2cc-proxy>  
**HEAD：** `c7fa17986c1e31337d25b01fa3eaec30fb1a2074`，2026-09-20

### 近期核心身份提交

- `8dcff4e`，2026-09-15：BuilderId 添加失败回滚；
- `ea612b4`，2026-09-15：profileArn 缺失改为首即禁用并区分原因；
- `5686031`，2026-09-15：BuilderId/Social 缺 profileArn 时按类型注入固定 ARN；
- `caa4ed1`，2026-09-15：添加/加载账号自动补全 profileArn 并持久化；
- `67d6b61`，2026-09-15：account-level thinking adaptive。

9 月 20 日的最新提交主要是 quota display/admin UI；不能把 UI 提交误称为身份调度改进，但 9 月 15 日的 profileArn 提交确实是核心路径。

### profileArn 与 machineId 事实

该项目的 credentials 模型已经保存：

- `profileArn`；
- `authMethod`；
- `region`、`authRegion`、`apiRegion`；
- `machineId`；
- token/client/endpoint/proxy 等字段。

其 profileArn 策略有明确分层：

- Social：固定 Social ARN；
- BuilderId/IdC：固定 BuilderId placeholder ARN；
- External IdP/未知类型：不伪造 ARN；
- 缺失 ARN 的企业 IdC 可以触发 `ProfileArnMissing`，首个请求即禁用并故障转移；
- 添加/加载账号时自动补全并持久化；
- BuilderId 添加失败会回滚，避免半成品账号残留。

### 关键矛盾

这个项目最能说明“profileArn 不等于每号唯一”：

- Social 和 BuilderId 使用固定值是协议兼容策略；
- External IdP 保留缺失，说明真实 ARN 不能凭空构造；
- 要求每号强制不同 ARN 会破坏上游认证语义。

**对当前项目的参考：** 适合借鉴 profileArn 生命周期、持久化时机和禁用原因；不应照搬“所有账号都补一个唯一 ARN”的错误目标。

源码链接：

- [credentials.rs](https://github.com/TsinHzl/kiro2cc-proxy/blob/c7fa17986c1e31337d25b01fa3eaec30fb1a2074/src/kiro/model/credentials.rs)
- [provider.rs](https://github.com/TsinHzl/kiro2cc-proxy/blob/c7fa17986c1e31337d25b01fa3eaec30fb1a2074/src/kiro/provider.rs)
- [token_manager.rs](https://github.com/TsinHzl/kiro2cc-proxy/blob/c7fa17986c1e31337d25b01fa3eaec30fb1a2074/src/kiro/token_manager.rs)
- [caa4ed1](https://github.com/TsinHzl/kiro2cc-proxy/commit/caa4ed1)

## 6. ZyphrZero/kiro.rs

**仓库：** <https://github.com/ZyphrZero/kiro.rs>  
**HEAD：** `be0c04219d9d1b93b7fe5c3d7b9e7c9cf0d05863`，2026-09-17

### 近期核心提交

- `3219d1c`，2026-09-17：honor credential region for inference；
- `3194bb2`，2026-09-17：usage requests without profile ARN fallback；
- `f413e7d`，2026-09-17：tool call name 兼容；
- `19b7f4b`，2026-09-07：session affinity/cache 相关。

### 事实

该 fork 的近期变化直接验证了两个容易混淆的边界：

1. **credential region 必须进入 inference URL/Host。** 只把 region 存在 credentials 中但仍固定打到全局 host，会导致 foreign-region profile 403。
2. **usage/models/profile 路径需要允许无 ARN fallback。** 带 ARN 的请求遇到 `Improperly formed request` 或 `Invalid profileArn` 时，才回退到无 ARN 形态；无 ARN 请求本身不能无限重复同一错误。

### 优点、缺口与适用性

**优点：**

- 近期提交直接触及账号 region 与 profileArn 的上游兼容边界；
- 对 usage/profile 辅助通道的 fallback 条件比“所有 403 都换号”更精确；
- session affinity 能降低上下文在账号间漂移。

**缺口：**

- 仍不是完整 per-account runtime fingerprint；
- machineId/UA/OS/SDK 不能自动视为独立；
- 主要是单机 fork 的 credential/endpoint 语义修复，不等同于当前 v167 的多 region berserk 调度。

**对当前项目的参考：** 适合校验 `apiRegion`、profileless usage/models fallback 的边界；不应把“回退到无 ARN”解释为“所有账号都无需 profileArn”。

源码链接：

- [3219d1c](https://github.com/ZyphrZero/kiro.rs/commit/3219d1c)
- [3194bb2](https://github.com/ZyphrZero/kiro.rs/commit/3194bb2)
- [src/kiro/endpoint/mod.rs](https://github.com/ZyphrZero/kiro.rs/blob/be0c04219d9d1b93b7fe5c3d7b9e7c9cf0d05863/src/kiro/endpoint/mod.rs)
- [src/kiro/token_manager.rs](https://github.com/ZyphrZero/kiro.rs/blob/be0c04219d9d1b93b7fe5c3d7b9e7c9cf0d05863/src/kiro/token_manager.rs)

## 7. justlovemaki/AIClient2API

**仓库：** <https://github.com/justlovemaki/AIClient2API>  
**HEAD：** `b12e25ea2840c880dd60f2ab4c45bc5bbfd10fab`，2026-09-16

### 近期核心提交

- `b0e3176`，2026-09-11：Builder ID 请求路由重构；
- `b12e25e`，2026-09-16：Kiro auth/profileArn 重构与 3.5.0；
- 这两个提交都改了实际 provider/auth 代码，不只是版本文案。

### 事实

`src/providers/claude/kiro-profile.js`：

- 无有效 `profileArn` 的 BuilderId 不再生成固定 placeholder；
- profileless BuilderId 请求直接路由到 CodeWhisperer；
- 有真实 profileArn 的账号保留正常 q.* 路由；
- `shouldDiscoverKiroProfile()` 认为 BuilderId 与 Enterprise IdC 共享 OIDC 形态，不能只凭 authMethod 判断是否可 discovery；
- discovery 失败时保留 profileless fallback。

`src/providers/claude/claude-kiro.js`：

- credentials 中保存 `profileArn`、`region`、`idcRegion`、`uuid`；
- `generateMachineIdFromConfig()` 优先级是 `uuid > profileArn > clientId > fallback`；
- IDE ListAvailableProfiles 和 usage limits 发送账号计算出的 machineId；
- 但 generation 的 CLI-style UA 是固定的 `aws-sdk-rust... AmazonQ-For-CLI`，不含账号 machineId；
- per-node proxy/TLS sidecar 可以按 `uuid` 绑定。

### 优点、缺口与适用性

**优点：**

- BuilderId “固定 placeholder ARN” 的风险被识别并移除；
- profile discovery 和 request routing 解耦；
- 账号级 UUID 可参与 machineId 派生；
- 近期 auth 路由是真实核心代码更新。

**缺口：**

- fallback 依赖 `uuid/profileArn/clientId`，不是统一持久化的完整 identity profile；
- CLI User-Agent 仍共享；
- 生成请求没有像 IDE discovery/usage 路径那样完整注入账号 machineId；
- 该项目是多 provider gateway，Kiro account scheduler 不如当前项目/kiro-provider 专门。

**对当前项目的参考：** profileless BuilderId 的路由分流很有价值；不要把 request-local routing fallback 当作账号身份补全。

源码链接：

- [kiro-profile.js](https://github.com/justlovemaki/AIClient2API/blob/b12e25ea2840c880dd60f2ab4c45bc5bbfd10fab/src/providers/claude/kiro-profile.js)
- [claude-kiro.js](https://github.com/justlovemaki/AIClient2API/blob/b12e25ea2840c880dd60f2ab4c45bc5bbfd10fab/src/providers/claude/claude-kiro.js)
- [b0e3176](https://github.com/justlovemaki/AIClient2API/commit/b0e3176)
- [b12e25e](https://github.com/justlovemaki/AIClient2API/commit/b12e25e)

## 8. suprim-corp/suprim-gateway

**仓库：** <https://github.com/suprim-corp/suprim-gateway>  
**HEAD：** `f941a8146c69c2d954a07d70616b49325bb2561e`，2026-09-13

### 近期核心提交

`f941a81`：

- 把 Kiro upstream 的 402、429、503 合并到 `ROTATE_ACCOUNT_STATUSES`；
- 402 Payment Required/额度耗尽不再直接返回客户端，而是换下一个账号；
- 新增对应测试。

### 身份事实

`StoredAccount` 包含：

- name、profileArn、authType；
- clientId/clientSecret；
- access/refresh token、expiresAt；
- region、apiRegion；
- provider、projectId。

但 `KiroMachineId` 是从宿主机的 IOPlatformUUID、`/etc/machine-id` 或 Windows MachineGuid 读取，再缓存一个 host-level SHA-256。多个账号同一 gateway 进程共享 machineId。

`KiroUserAgent` 固定为 Kiro IDE 1.0.228、Node 22.22.0、SDK 1.0.44/1.0.0；版本 profile 是全局常量，不是账号级。

### 调度事实

`KiroUpstreamDispatcher`：

- OAuth/social 与 AWS SSO/API key 采用不同 endpoint 顺序；
- 每个账号先按 endpoint fallback；
- 全 endpoint 403 后才 refresh token；
- 402/429/503 直接换账号；
- 账号轮换有 90 秒预算；
- streaming 先开启 heartbeat，再把错误转为 SSE event。

### 优点、缺口与适用性

**优点：** 402 quota 直接换号的语义简单且可测试；endpoint/auth family 分流清楚。  
**缺口：** machineId、UA、TLS 仍是 gateway-level/全局；没有 region × account × round 的 berserk；cooldown/quality 状态比当前 v167 简单。

**对当前项目的参考：** 只适合借鉴 402 的分类和“额度耗尽换号”，不适合借鉴其身份隔离方案。

源码链接：

- [KiroUpstreamDispatcher.java](https://github.com/suprim-corp/suprim-gateway/blob/f941a8146c69c2d954a07d70616b49325bb2561e/src/main/java/dev/suprim/gateway/proxy/kiro/KiroUpstreamDispatcher.java)
- [KiroMachineId.java](https://github.com/suprim-corp/suprim-gateway/blob/f941a8146c69c2d954a07d70616b49325bb2561e/src/main/java/dev/suprim/gateway/provider/kiro/KiroMachineId.java)
- [KiroUserAgent.java](https://github.com/suprim-corp/suprim-gateway/blob/f941a8146c69c2d954a07d70616b49325bb2561e/src/main/java/dev/suprim/gateway/provider/kiro/KiroUserAgent.java)
- [f941a81](https://github.com/suprim-corp/suprim-gateway/commit/f941a81)

## 9. mikeyobrien/pi-provider-kiro

**仓库：** <https://github.com/mikeyobrien/pi-provider-kiro>  
**HEAD：** `0a023d52497e844cf51292b6bd2c0a8c74420ac2`，2026-09-14

### 近期核心提交

- `ef72fc8`，2026-09-14：清理 first-token timeout timer；
- `872eea0`，2026-09-14：可选 Kiro cache-read 估算；
- `3981dd0`，2026-09-14：可选 credit value 估算；
- `c5c10b7`，2026-09-12：保持模型 catalog version dots。

### 事实

该项目是 Pi provider/extension，不是多账号网关。它的近期价值在：

- stream first-token timeout 的资源清理；
- provider 报告 cache counters 时优先使用 wire truth；
- 没有 wire cache counters 时，按 conversation、context shrink、idle expiry 做保守估算；
- 估算结果带 `cacheEstimated`，把推测与上游真实计数区分。

region/profileArn：

- `resolveApiRegion()` 进入 runtime endpoint；
- model cache 按 region 保存；
- credential profileArn 可投影到模型；
- API key 与 OAuth credential 分开读取。

### 缺口与适用性

- 没有账号池、账号级 machineId 派生、账号级 UA/TLS；
- 近期更新主要是 stream/usage，不是账号调度；
- 适合参考“估算值必须显式标记、wire truth 优先”和 timer/stream 生命周期，不适合做多账号 identity 基线。

源码链接：

- [src/index.ts](https://github.com/mikeyobrien/pi-provider-kiro/blob/0a023d52497e844cf51292b6bd2c0a8c74420ac2/src/index.ts)
- [src/stream.ts](https://github.com/mikeyobrien/pi-provider-kiro/blob/0a023d52497e844cf51292b6bd2c0a8c74420ac2/src/stream.ts)
- [src/cache-estimator.ts](https://github.com/mikeyobrien/pi-provider-kiro/blob/0a023d52497e844cf51292b6bd2c0a8c74420ac2/src/cache-estimator.ts)
- [ef72fc8](https://github.com/mikeyobrien/pi-provider-kiro/commit/ef72fc8)
- [872eea0](https://github.com/mikeyobrien/pi-provider-kiro/commit/872eea0)

## 四、当前项目 v0.0.167 在这些项目中的位置

当前项目的近期核心提交：

- `4c7790b`：external pool quality-aware scheduling；
- `e6c5e35`：endpoint 内置化和轮换开关；
- `144ac56`：endpoint rotation 独立总开关；
- `d04de88`：429 berserk rotation 与普通 region rotation；
- `e6d6397`：本地账号 berserk 状态机；
- `60eb117`：容量变更保留 warmup；
- `3e5774b`：查询账号时 hydrate credential email；
- `6287c50`：protocol interop 与 external pool hardening。

### 与外部项目相比的强项

1. 失败分类更接近“真正可恢复的上游原因”，而不是把所有 429 都当成同一类。
2. region rotation 与 berserk 分离，普通模式不会被放大成笛卡尔积。
3. berserk 预算按账号数、region 数、轮数扩展，并保留下游 committed 保护。
4. 外部池质量调度有 EWMA、Redis、probation、recovery ramp，不只看最近一次失败。
5. 已有较细的 profileArn discovery identity key，能避免账号替换后复用旧负面缓存。

### 与外部项目相比的弱项

1. machineId 仍有全局覆盖、无 ID shared fallback、重启不持久化等边界。
2. CLI User-Agent 仍无 machineId。
3. OS/Node/SDK/Kiro/TLS 仍主要是全局 profile，而不是账号级。
4. profileArn 的固定/真实/缺失语义需要继续按认证类型治理。
5. region rotation 主要作用于 inference；models/profile/MCP 辅助路径不一定使用同一矩阵。

## 五、跨项目对比矩阵

| 维度 | 当前 v167 | kiro-lb | kiro-provider | OmniProxy | Yaocool | TsinHzl | ZyphrZero | AIClient2API | suprim |
|---|---|---|---|---|---|---|---|---|---|
| 持久化账号 ID | 有 | 有/路径或 hash | SQLite UUID/ID | UUID | `acc_...` | numeric ID | 有 | UUID/节点 | name/client |
| access/refresh/client credentials | 有 | 有 | 有 | 有 | 有 | 有 | 有 | 有 | 有 |
| 账号级 machineId | 条件独立 | 主机级 | 未形成完整方案 | token 派生 | 明确保存 | credentials 派生 | 需看 fork 配置 | uuid/profile/client 派生 | 主机级 |
| IDE UA 带 machineId | 是 | 否/主机 fingerprint | 未完整实现 | 是 | 是 | 是 | 视路径 | discovery/usage 有 | 是 |
| CLI UA 带 machineId | 否 | 否 | 否 | ksk 用固定 CLI UA | 否 | 否 | 否 | 否 | 否 |
| OS/Node/SDK 账号级 | 否 | 否 | 否 | 否 | 否 | 否 | 否 | 否 | 否 |
| profileArn discovery | 有 singleflight/cache | 有 | 有/严格 region | 多区域 retry/cache | 有 | 自动补全/禁用 | fallback | discovery + profileless route | 有 |
| 固定 ARN 语义 | 按协议 | 账号状态加载 | 账号记录 | Kiro 分支 | 需按类型 | 明确固定 | fork 规则 | 不再制造 placeholder | 真实/存储 |
| auth/API region 分离 | 有 | 有 | `region/oidcRegion` | 有 regionForAccount | 有 | 有 | 有 | `region/idcRegion` | `region/apiRegion` |
| region rotation | 有 | endpoint fallback | 无同等矩阵 | endpoint fallback | 有 endpoint cache | 有基础 region | 有 credential region | 路由分流 | endpoint order |
| 429/402/403 分类 | 细 | 细 | health/recovery | 很细 | 基础 | profile missing 分类 | usage fallback | 403 route | 402/429/503 |
| session affinity/replay | 有部分 | endpoint/cache affinity | 强 | cache sticky | endpoint cache | session/context | 有部分 | provider pool | 无强 replay |
| per-account proxy | 有 | auth/provider 级 | 需配置 | 有 | 有 | 有 | 有 | 有/TLS sidecar | ProxyChain |
| 分布式质量状态 | Redis | 否 | SQLite/CAS | 进程 pool | 否 | storage/CAS | 否 | 否 | 否 |
| 近期核心维护 | 2026-09-20 | 2026-09-18 | 2026-09-18 | 2026-09-18 | 2026-09-16 | 2026-09-15/20 | 2026-09-17 | 2026-09-16 | 2026-09-13 |

## 六、事实冲突与如何解释

### 冲突 1：machineId 有的按账号派生，有的按主机派生

- 当前 v167、OmniProxy、Yaocool/TsinHzl 倾向账号凭据或账号记录派生；
- kiro-lb 使用 hostname + username；
- suprim-gateway 读取宿主机硬件/系统 machine ID。

这不是谁“代码写错”这么简单，而是目标不同：

- 主机派生适合模拟“一个真实客户端安装”；
- 账号派生适合让多账号池在同一网关上保持身份隔离。

如果目标是每账号独立，主机派生方案不能直接采用。

### 冲突 2：profileArn 有的固定，有的拒绝伪造

- TsinHzl 明确为 Social/BuilderId 补固定 ARN；
- AIClient2API 删除 BuilderId placeholder，改走无 ARN CodeWhisperer；
- ZyphrZero 对 usage/profile 允许带 ARN失败后无 ARN fallback；
- External IdP 项目通常要求真实 ARN。

事实结论是：**profileArn 的正确性依赖认证类型和上游表面，不能统一用“每号唯一”或“每号必有”作为规则。**

### 冲突 3：region 是 auth region 还是 API region

- kiro-provider 把 `region` 和 `oidcRegion` 分开；
- 当前 v167 明确 auth region 不随 region rotation 改变；
- ZyphrZero 近期修复了 credential region 进入 inference Host；
- suprim/某些项目把 region 直接当作所有 endpoint 的共同字段。

如果不拆分，SSO token refresh 可能被错误打到 API region，或者 profile 在 eu-central-1 时仍被打到 us-east-1。

### 冲突 4：活跃提交不等于身份隔离完成

- pi-provider-kiro 近期维护很活跃，但主要改 stream/usage；
- OmniProxy 的 fingerprint commit 主要是 Antigravity；
- suprim-gateway 近期确有 Kiro 402 rotation，但 machineId 仍是 host-level；
- TsinHzl 近期 UI 提交很多，但真正身份核心改动集中在 2026-09-15。

因此“最近两周有提交”只能证明项目活跃，不能直接证明它解决了本次目标。

### 冲突 5：健康状态与指纹是两套东西

一个项目可以：

- 很会判断 429/402/403；
- 很会换账号；
- 很会记 quota；

但仍然让所有账号共享同一套 UA/OS/TLS。反过来，一个项目可以让每账号 machineId 不同，却没有正确的 cooldown/region/session 调度。评估时不能把两者混成一项。

## 七、按目标排序的参考价值

### A. 账号身份/指纹

1. **Yaocool/kiro-proxy**：identity profile 组织、版本基线、账号 machineId、API key/GovCloud 分流；
2. **TsinHzl/kiro2cc-proxy**：profileArn 补全/持久化/禁用原因；
3. **当前 v167**：machineId 派生、profile discovery key、IDE headers；
4. **OmniProxy**：账号级 proxy 与 token-derived machineId；
5. **AIClient2API**：UUID/profile/clientId 派生，但 CLI path 仍共享；
6. **kiro-lb、suprim-gateway**：主机级 fingerprint，不适合作为每号独立基线。

### B. 上游 endpoint、region 和 failover

1. **当前 v167**：region rotation + berserk + Retry-After/risk-control 分层；
2. **kiro-lb**：endpoint affinity、cooldown、quota headroom；
3. **ZyphrZero**：credential region 和 profileless usage fallback；
4. **OmniProxy**：transport/SSE/model unavailable 分类；
5. **suprim-gateway**：402/429/503 rotation；
6. **Yaocool**：endpoint cache、API key/GovCloud runtime；
7. **AIClient2API**：profileless BuilderId route。

### C. 账号容量、会话和并发

1. **sunerpy/kiro-provider**：per-account queue/lease、least queued、sticky/replay；
2. **当前 v167**：credential capacity、warmup 保留、local/external attempt budget；
3. **kiro-lb**：account health、quota headroom、cache sticky；
4. **OmniProxy**：pool recovery waves、model lock、transport recovery；
5. 其他项目以基础轮换或 endpoint fallback 为主。

### D. 观测与审计

1. **当前 v167**：attempt chain、credential id、region、failure kind、external quality；
2. **kiro-provider**：audit hash、CAS generation、last sync/fail counts；
3. **kiro-lb**：dashboard 状态、quota/rate observations；
4. **OmniProxy**：pool dump、cooldown reason、account health；
5. **pi-provider-kiro**：`cacheEstimated` 这类“估算与真实值分离”值得借鉴。

## 八、针对当前项目的综合建议

以下是基于上述事实的设计建议，不代表本次已修改代码：

### 1. 建立单独的 `AccountIdentityProfile`

每个账号至少持久化：

```text
credential_id
machine_id
machine_id_source
profile_arn_state
auth_method/provider
auth_region
api_region
endpoint
proxy_resource_id
ua_profile_version
tls_profile_id
identity_revision
```

`machine_id_source` 必须能区分：

```text
explicit
derived_refresh_token
derived_api_key
persisted_fallback
global_override
```

这样才能识别“看起来有 machineId，实际是全局覆盖”。

### 2. 把 profileArn 当作协议状态，不当作通用指纹

每个账号记录：

```text
real_discovered
refresh_response
fixed_protocol_arn
profileless_allowed
missing_required
```

调度时根据认证类型和 endpoint 选择，而不是简单判断 `profileArn != None`。

### 3. 将账号身份和上游调度日志绑定

每次上游 attempt 记录：

```text
request_id
credential_id
machine_id_hash
profile_arn_hash
auth_region
api_region
region_override
endpoint
host
proxy_resource_id
ua_profile_version
failure_kind
retry_after
downstream_committed
```

必须禁止把 token、client secret、完整 profileArn 原文写入普通日志。

### 4. 继承 v167 的错误分类，但补上项目间已经验证的类别

最少区分：

```text
RiskControl429
RetryAfter429
Plain429
408/Timeout
5xx
402Quota
401/403Auth
ProfileArnMissing
ModelUnavailable
TransportTimeout
SSETruncated
InvalidRequest
DownstreamCommitted
```

其中 `TransportTimeout`、`SSETruncated` 不应自动把账号永久 cooldown；`ProfileArnMissing` 也不应和 token 过期混在一起。

### 5. 继承 kiro-provider 的 per-account capacity/session 语义

region rotation 不应绕过账号 capacity。每个 attempt 仍要绑定：

```text
account context
token snapshot
machine/profile identity snapshot
region override
capacity lease
session affinity
```

不能只在 URL 层切 region，却把 session、warmup、quota 统计写到错误账号。

### 6. 把“真实上游参数”和“估算参数”分开

参考 pi-provider-kiro：

- 上游真的返回的 usage/cache/profile 信息优先；
- 估算值要带 `estimated` 标记；
- 不要把本地推断的账号归属当作上游证明；
- 不要把最近一次成功请求的 Host/UA 推断成所有后续请求都会使用。

## 九、最终结论

### 哪些项目最值得参考

- **身份 profile：** Yaocool/kiro-proxy；
- **profileArn 生命周期：** TsinHzl/kiro2cc-proxy + ZyphrZero/kiro.rs；
- **账号容量/会话：** sunerpy/kiro-provider；
- **endpoint/account health：** minpeter/kiro-lb；
- **transport/error classification：** OmniProxy；
- **402 quota rotation：** suprim-gateway；
- **当前综合调度基线：** v0.0.167。

### 哪些方案不能直接照搬

- 不要照搬 kiro-lb/suprim 的主机级 machineId 来做每号独立；
- 不要把所有 profileArn 都强制生成为每号唯一；
- 不要把 Antigravity fingerprint commit 直接当作 Kiro fingerprint 事实；
- 不要把近期 UI、README、release commit 当作核心调度实现；
- 不要把“能换号”误认为“账号指纹已经独立”；
- 不要把“有 account record”误认为“UA/TLS/出口已经按账号隔离”。

### 对当前 v0.0.167 的判定

**v0.0.167 是当前更好的调度版本，但不是完整账号指纹版本。**

它应该作为调度底座继续使用；账号独立性部分应吸收 Yaocool 的 identity profile、TsinHzl/ZyphrZero 的 profileArn 生命周期、kiro-provider 的账号上下文/容量租约，同时保留当前版本对 Retry-After、风控 429、region rotation、berserk 和下游 committed 的保护。

## 十、证据索引

### 当前项目

- [v0.0.161 tag](https://github.com/2ue/kiro.rs/tree/v0.0.161)
- [v0.0.167 tag](https://github.com/2ue/kiro.rs/tree/v0.0.167)
- [region rotation / berserk](../src/kiro/retry_pipeline.rs)
- [inference scheduler](../src/kiro/provider.rs)
- [machineId](../src/kiro/machine_id.rs)
- [credential model](../src/kiro/model/credentials.rs)
- [external pool](../src/external_pool.rs)

### 近期项目

- [kiro-lb](https://github.com/minpeter/kiro-lb)
- [kiro-provider](https://github.com/sunerpy/kiro-provider)
- [OmniProxy](https://github.com/mxuanvan02/OmniProxy)
- [kiro-proxy](https://github.com/Yaocool/kiro-proxy)
- [kiro2cc-proxy](https://github.com/TsinHzl/kiro2cc-proxy)
- [ZyphrZero/kiro.rs](https://github.com/ZyphrZero/kiro.rs)
- [AIClient2API](https://github.com/justlovemaki/AIClient2API)
- [suprim-gateway](https://github.com/suprim-corp/suprim-gateway)
- [pi-provider-kiro](https://github.com/mikeyobrien/pi-provider-kiro)

