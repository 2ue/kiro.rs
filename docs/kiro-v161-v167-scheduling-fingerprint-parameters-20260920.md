# Kiro v0.0.161 与 v0.0.167：账号身份、调度指纹参数与上游调用差异

**分析日期：** 2026-09-20  
**比较范围：** `v0.0.161`（`31c947b76c8ab39891796f43b91f2b2afd2c0814`，2026-09-02）到 `v0.0.167`（`35ce08e32cc2222e6acc0a8c48cbc088b954943c`，2026-09-20）  
**变更规模：** 97 个文件，新增 16,070 行，删除 550 行  
**本地源码：** 当前仓库 `src/kiro/`、`src/model/`、`src/storage/`  
**说明：** 本文只分析账号独立性、运行时指纹参数、参数计算/持久化、以及调度会使用的上游请求参数。协议转换、下游格式适配不作为本次结论依据。

## 一、先给结论

### 1. v0.0.167 的调度能力明显强于 v0.0.161

167 新增或强化了以下与“账号身份被选中后如何调用上游”直接相关的能力：

- 单次尝试可以使用 `region_override`，把同一个账号切换到另一个 API region；
- 普通 region 轮换与本地 berserk 模式分离；
- berserk 模式按 `账号 × region × 轮次` 遍历，而不是只在同一个账号上重复；
- 普通 429、408、5xx、超时等瞬态错误与风控型 429、带 `Retry-After` 的 429 分开处理；
- `getUsageLimits` 通过 `isEmailRequired=true` 请求上游账号邮箱，并在本地邮箱为空时写回；
- 外部池增加 EWMA 失败率、TTFT、总延迟、probation/recovery、Redis 分布式质量状态和 top-K weighted random；
- 修改账号容量时保留原有 warmup 进度，避免一次管理操作把账号重新打回冷启动。

因此，如果问题是“哪个版本更适合多账号生产调度”，答案是 **v0.0.167**。

### 2. 但 v0.0.167 仍然不是“完整的每号独立运行时指纹”

当前实现不能据此宣称每个账号拥有完全独立的指纹。事实是：

- OAuth/API key 账号通常可以得到独立的 `machineId`；
- 但全局 `config.machineId` 会覆盖多个没有账号级 machineId 的账号；
- 没有 `credentials.id` 的账号会共用同一个 fallback bucket；
- fallback machineId 只在进程内缓存，重启后会变化；
- IDE User-Agent 带 `machineId`，CLI User-Agent 不带 `machineId`；
- OS、Node、Kiro、SDK、语言版本由全局配置或常量生成，不是每账号独立；
- TLS/HTTP client 不是按账号构造完整独立的运行时指纹；
- 代理可以按账号配置，但相同代理 URL 会复用同一类 client/连接资源；
- `profileArn` 是上游归属参数，不等于“每号唯一指纹”；某些认证类型必须使用固定或共享 ARN。

所以，若验收标准是“每个账号号段都必须有独立 machineId、独立 UA、独立 OS/Node/SDK、独立 TLS/连接画像、独立出口”，**161 和 167 都不完全满足**。167 只是把账号身份带入调度和上游请求的覆盖面做得更大。

## 二、账号字段基线：哪些在两个版本都存在

`KiroCredentials` 的基础账号模型在 161 已经存在，不能把下面字段全部写成 167 新增。字段位于 `src/kiro/model/credentials.rs`：

| 参数类别 | 字段 | v0.0.161 | v0.0.167 | 是否账号级 |
|---|---|---:|---:|---|
| 账号主键 | `id` | 已有 | 已有 | 是，但可能缺失 |
| 访问授权 | `accessToken` | 已有 | 已有 | 是 |
| 刷新授权 | `refreshToken` | 已有 | 已有 | 是 |
| 上游归属 | `profileArn` | 已有 | 已有 | 是，但不一定每号唯一 |
| 认证类型 | `authMethod` | 已有 | 已有 | 是 |
| 提供方 | `provider` | 已有 | 已有 | 是 |
| OIDC 客户端 | `clientId`、`clientSecret` | 已有 | 已有 | 是 |
| OAuth/OIDC 端点 | `tokenEndpoint`、`issuerUrl`、`scopes` | 已有 | 已有 | 是 |
| 区域 | `region`、`authRegion`、`apiRegion` | 已有 | 已有 | 是，语义不同 |
| 设备标识 | `machineId` | 已有 | 已有 | 条件独立 |
| 运营信息 | `email`、`subscriptionTitle` | 已有 | 已有 | 是，但邮箱补全路径有变化 |
| 网络 | `proxyUrl`、代理账号密码、`proxyResourceId` | 已有 | 已有 | 可以账号级覆盖 |
| 上游表面 | `endpoint` | 已有 | 已有 | 可以账号级指定 |
| 调度限制 | `rpm`、并发、`priority`、支持模型 | 已有 | 已有 | 是 |

这张表的重点是：**167 的主要价值不是重新发明账号字段，而是让已有字段更多地参与实际请求选择、上游地址和故障恢复。**

## 三、machineId：计算、优先级、持久化与真实独立性

### 3.1 两个版本的核心计算规则没有语义变化

`src/kiro/machine_id.rs` 在 161 和 167 的核心逻辑一致，优先级如下：

```text
凭据级 machineId（格式合法）
    > 全局 config.machineId（格式合法）
    > API key 账号：SHA256("KiroAPIKey/" + kiroApiKey)
    > OAuth 账号：SHA256("KotlinNativeAPI/" + refreshToken)
    > fallback：SHA256("KiroFallback/" + 随机 UUID)
```

实现位置：

- [src/kiro/machine_id.rs](../src/kiro/machine_id.rs) 45-86：派生优先级；
- 同文件 88-109：fallback 缓存与重启行为。

UUID 格式的账号级 `machineId` 会去掉连字符并重复一次，规范化为 64 个十六进制字符；已经是 64 位十六进制的值直接保留。

### 3.2 正常账号什么时候可以做到独立

- 每个 OAuth 账号拥有不同 `refreshToken` 时，派生结果通常不同；
- 每个 API key 账号拥有不同 `kiroApiKey` 时，派生结果通常不同；
- 管理员显式为每个账号填入不同的合法 `machineId` 时，账号级值优先；
- 已有凭据 ID 和刷新材料的账号，跨重启通常保持稳定，因为派生材料本身持久化。

这解释了为什么“多数正常账号”看起来有独立 machineId，但不能据此推导“所有账号、所有请求表面都独立”。

### 3.3 三个会破坏账号独立性的边界

#### 全局 machineId 覆盖

`config.machineId` 位于账号级 machineId 之后。如果多个账号没有自己的合法 `machineId`，它们会共享全局值。多账号模式下设置全局 machineId，会把多个账号压成一个设备身份。

#### 没有 ID 的账号共享 fallback bucket

fallback 缓存类型是：

```rust
HashMap<Option<u64>, String>
```

也就是说，`credentials.id == None` 的所有账号共享 `None` 这个 key。正常导入流程通常会分配 ID，但“账号文件直接导入、外部凭据临时路径、数据迁移不完整”等场景不能假设一定有 ID。

#### fallback 不跨重启持久化

缺少 `kiroApiKey` 和 `refreshToken` 时，系统使用随机 UUID 派生 fallback machineId，并只存进进程内缓存。进程重启后会产生新值，上游看到的设备身份会变化。

### 3.4 machineId 不是完整 runtime fingerprint

machineId 只影响构造出来的部分 IDE 标识。它不自动隔离：

- OS 字符串；
- Node 版本；
- Kiro 版本；
- SDK 版本；
- API service label；
- TLS backend；
- HTTP/2/HTTP/1 连接特征；
- 代理出口 IP；
- DNS/连接池；
- CLI 端点的 User-Agent。

因此，“每号一个 machineId”只是最低限度的账号标签，不等于每号一个完整运行时指纹。

## 四、profileArn：上游归属参数，不应简单等同于每号唯一

### 4.1 161 已有 discovery key，167 没有改变其核心语义

167 的 `profile_arn_discovery_key` 继续使用 SHA-256 做 identity key，原始 secret 不直接放入缓存键。关键输入包括：

- 本地凭据数字 ID；
- `refreshToken`，否则使用 API key，再否则使用当前 access token；
- secret 类型（`refresh_token`、`kiro_api_key`、`access_token`）；
- `authMethod`；
- `provider`；
- `clientId`；
- `tokenEndpoint`；
- `apiRegion`；
- `endpoint`；
- 当前 `machineId`。

实现位置：

- [src/kiro/provider.rs](../src/kiro/provider.rs) 8240-8314。

该 key 用于 per-credential singleflight、resolved cache、negative backoff、bounded LRU 及账号替换后的状态隔离。它是“发现 profileArn 的缓存身份”，不是发送给上游的完整身份。

### 4.2 profileArn 的真实语义有认证类型差异

- Social/BuilderId 可能使用固定或共享协议 ARN；
- Enterprise/External IdP 通常需要真实租户归属 ARN；
- API key 的 profileArn 语义可能与 OAuth 账号不同；
- 没有真实 ARN 时，不能为了“每号唯一”随意拼造一个 ARN。

因此，要求“每个号的 profileArn 必须唯一”本身可能与上游认证协议冲突。正确要求应当是：

1. 账号真实拥有的 ARN 必须只绑定到该账号；
2. 固定 ARN 只能用于协议明确要求共享值的认证类型；
3. External IdP 缺少真实 ARN 时，应记录为 profile 缺失或走协议允许的无 ARN 路径，而不是伪造唯一 ARN。

## 五、IDE 与 CLI 上游请求中的身份/指纹参数

### 5.1 IDE 端点：machineId 会进入 User-Agent

当前 `src/kiro/endpoint/ide.rs` 生成：

```text
x-amz-user-agent:
aws-sdk-js/1.0.34 KiroIDE-{kiroVersion}-{machineId}

user-agent:
aws-sdk-js/1.0.34
ua/2.1
os/{systemVersion}
lang/js
md/nodejs#{nodeVersion}
api/codewhispererstreaming#1.0.34
m/E
KiroIDE-{kiroVersion}-{machineId}
```

请求还会使用：

- `Authorization: Bearer {accessToken}`；
- `host: q.{apiRegion}.amazonaws.com`；
- `amz-sdk-invocation-id`：每次请求随机 UUID，不是账号稳定身份；
- `amz-sdk-request: attempt=1; max=3`；
- `x-amzn-codewhisperer-optout`；
- `x-amzn-kiro-agent-mode`；
- API key 时的 `tokentype: API_KEY`；
- External IdP 时的 `TokenType: EXTERNAL_IDP`。

IDE inference 请求体会在根对象注入 `profileArn`（如果解析得到）；MCP 和 models 请求还会在 header 中发送 `x-amzn-kiro-profile-arn`。

### 5.2 CLI 端点：没有 machineId

当前 `src/kiro/endpoint/cli.rs` 的生成身份是：

```text
x-amz-user-agent:
aws-sdk-rust/1.3.15 ua/2.1
api/codewhispererstreaming/0.1.16551
os/{systemVersion}
lang/rust/1.92.0
m/F
app/AmazonQ-For-CLI

user-agent:
aws-sdk-rust/1.3.15 ua/2.1
api/codewhispererstreaming/0.1.16551
os/{systemVersion}
lang/rust/1.92.0
md/appVersion-{kiroVersion}
app/AmazonQ-For-CLI
```

CLI management 请求使用 `codewhispererruntime/0.1.16551`，并带 `m/F,C`。这些字符串没有账号级 machineId。

因此，同一个账号池即使在 IDE 路径有账号级 machineId，切到 CLI 路径后仍会共享一套 CLI 运行时身份。CLI 的账号区分更多依赖：

- Bearer token；
- API key 的 `tokentype`；
- External IdP 的 `TokenType`；
- `profileArn`；
- Host/region；
- 账号级代理。

### 5.3 版本参数是全局/常量，不是账号级

`systemVersion`、`nodeVersion`、`kiroVersion` 从全局 `Config` 或固定常量读取。它们不会因为调度切换账号而自动变化。也就是说，账号 A 和账号 B 在 IDE 路径可能只有 machineId、token、profileArn、region、proxy 等差异，OS/Node/SDK/Kiro 版本仍相同。

## 六、region、endpoint 与 token refresh 的分离

### 6.1 161 的有效 API region

在 161 的旧语义中，API 请求主要使用凭据的 `effective_api_region()`，再回退到全局配置。API region 与 auth region 的区别已经存在，但没有请求级覆盖。

典型地址：

```text
IDE inference: https://q.{api_region}.amazonaws.com/generateAssistantResponse
IDE MCP:       https://q.{api_region}.amazonaws.com/mcp
CLI inference: https://runtime.{api_region}.kiro.dev/
CLI MCP:       https://q.{api_region}.amazonaws.com/mcp
CLI management:https://management.{api_region}.kiro.dev/
```

### 6.2 167 新增 `region_override`

167 给 `RequestContext` 增加：

```rust
region_override: Option<&str>
```

优先级变成：

```text
本次 region_override
> credentials.api_region
> profileArn 内嵌 region
> 全局 config.effective_api_region()
```

实现位置：

- [src/kiro/endpoint/mod.rs](../src/kiro/endpoint/mod.rs) 277-310；
- [src/kiro/endpoint/ide.rs](../src/kiro/endpoint/ide.rs) 32-49；
- [src/kiro/endpoint/cli.rs](../src/kiro/endpoint/cli.rs) 31-49。

重要限制：

- `region_override` 只影响 API 请求地址和 Host；
- 不改变 token refresh 的 auth region；
- models/profile/MCP 等辅助请求目前多数使用 `None`，只有 inference 主路径显式使用 rotation region；
- 同一次原地 retry 会保留同一个 region，避免一次请求的第一次和第二次尝试落到不同上游。

### 6.3 167 的 region 轮换矩阵

内置候选为：

```text
us-east-1
eu-central-1
```

如果账号 region 是 `eu-*`，顺序为：

```text
eu-central-1 -> us-east-1
```

否则：

```text
us-east-1 -> eu-central-1
```

配置开关：

```text
kiroUpstreamRegionRotationEnabled
localBerserkModeEnabled
localBerserkMaxRounds
localBerserkRoundDelayMs
```

berserk 模式的实际尝试空间是：

```text
账号 × region × rounds
```

硬上限为 2,000 次。顺序是先把当前 region 下的账号轮完，再切换 region；所有 region 轮完后才进入下一轮。这个变化不是“新增账号身份字段”，但它改变了同一个账号的上游 Host/region 组合，因而属于调度时使用的指纹参数变化。

## 七、167 的 429/瞬态分类如何影响账号身份使用

167 在 `src/kiro/retry_pipeline.rs` 中明确区分：

```text
风控型 429                  -> 原有风控/冷却，不进入 berserk
带 Retry-After 的 429       -> 遵守上游时间，不进入 berserk
普通 429、408、5xx、超时     -> 可按账号/region 继续尝试
```

在 `src/kiro/provider.rs` 约 12841-12957：

- berserk 仅在模式开启、错误类型是 `RateLimit`、且没有 `Retry-After` 时生效；
- berserk 普通 429 不写全局 cooldown，避免本次请求内刚轮换出的账号被全局状态立即隐藏；
- 风控型 429 仍走 `report_risk_controlled_outcome_deferred`；
- 带 `Retry-After` 的 429 继续走既有冷却与故障转移；
- 400、401、403、其他请求错误不进入账号 × region 矩阵；
- 下游一旦 committed，预算再大也停止重试。

这使得 167 在“使用哪个账号、哪个 region、何时停止”上比 161 更精确，但它不会自动让账号的 UA/TLS/OS 变成独立。

## 八、上游接口与关键参数变化

| 上游接口/路径 | 161 | 167 | 变化性质 |
|---|---|---|---|
| Social token refresh | `prod.{authRegion}.auth.desktop.kiro.dev/refreshToken` | 同 | 核心协议未变 |
| AWS OIDC refresh | `oidc.{authRegion}.amazonaws.com/token` | 同 | region rotation 不改 auth region |
| External IdP refresh | 账号 `tokenEndpoint` | 同 | 账号级 endpoint 仍独立 |
| IDE inference | `q.{apiRegion}.amazonaws.com/generateAssistantResponse` | 增加请求级 `region_override` | 167 新增 API region 覆盖 |
| CLI inference | `runtime.{apiRegion}.kiro.dev/` | 增加请求级 `region_override` | 167 新增 API region 覆盖 |
| MCP | `q.{apiRegion}.amazonaws.com/mcp` | 主体不变；辅助路径通常不跟随 rotation | 需注意路径不完全同构 |
| ListAvailableModels | 带 `origin`、可选 `profileArn`、`nextToken` | 核心参数不变 | 167 没有把它全面纳入 region 矩阵 |
| ListAvailableProfiles | 用 access token 做 profile discovery | 核心 discovery 语义不变 | key/cache 仍按账号身份隔离 |
| getUsageLimits | `origin=AI_EDITOR&resourceType=AGENTIC_REQUEST` | 增加 `isEmailRequired=true` | 167 新增账号邮箱回传要求 |
| getUsageLimits 响应 | 用量、订阅等 | 新增解析 `userInfo.email` | 167 可补齐本地 email |
| setUserPreference/overage | 使用账号 API region、profileArn | 仍使用账号 API region | 不应误用 auth region |

### 邮箱补全的边界

167 的邮箱补全位于 Admin/usage limits 路径，不是每次 inference 自动补全：

- 上游返回 `userInfo.email`；
- 仅当本地 email 为空时写回；
- 不覆盖管理员手工 email；
- 持久化失败只记录 warning，不影响当前查询；
- 成功后发布 credentials changed 事件。

实现位置：

- [src/kiro/token_manager/refresh.rs](../src/kiro/token_manager/refresh.rs) 912-994；
- [src/kiro/model/usage_limits.rs](../src/kiro/model/usage_limits.rs)；
- [src/kiro/token_manager/manager.rs](../src/kiro/token_manager/manager.rs) 12020-12095。

## 九、167 新增的外部池质量调度是否属于指纹变化

commit `4c7790b` 的 `externalPoolQualityAwareSchedulingEnabled` 引入：

- failure-rate EWMA；
- TTFT EWMA；
- total latency EWMA；
- sample count、quality TTL；
- top-K weighted random；
- probation、recovery ramp；
- Redis distributed quality state；
- passive real traffic sampling；
- sampling concurrency limit；
- 仅在同一 priority 内调整质量排序；
- 错误惩罚高于延迟惩罚；
- 样本不足时回退旧 selector；
- 所有池都降级时不把候选集错误清空；
- 无候选时显式返回 503；
- 默认关闭。

它改变的是“账号被选中的概率”和“健康状态如何影响调度”，不是 HTTP 指纹本身。要把它纳入账号独立性审计，应该观察：

```text
实际选中 credential id
实际 machineId
实际 profileArn
实际 apiRegion / region_override
实际 proxy resource
实际 Host / User-Agent
```

只看质量分或池名，不能证明上游看到的是独立账号。

## 十、账号独立性矩阵

| 维度 | v0.0.161 | v0.0.167 | 结论 |
|---|---|---|---|
| 本地 credential `id` | 有 | 有 | 正常导入时可独立；无 ID 仍有边界 |
| access token | 账号级 | 账号级 | 独立 |
| refresh token/API key | 账号级 | 账号级 | machineId 的主要派生材料 |
| profileArn | 账号级/可能固定共享 | 同 | 必须遵循认证类型，不能强行每号唯一 |
| authMethod/provider | 账号级 | 账号级 | 影响 refresh 和 TokenType |
| tokenEndpoint/issuer/scopes | 账号级 | 账号级 | External IdP 可独立 |
| apiRegion | 账号级 | 账号级 + 请求级覆盖 | 167 更强 |
| authRegion | 账号级/全局回退 | 不被 region rotation 改写 | 167 更安全 |
| endpoint | 可账号级 | 可账号级 + rotation 对 API 地址生效 | 167 更灵活 |
| machineId | 条件独立 | 计算规则未变 | 不是完整独立指纹 |
| fallback machineId | 进程内 | 进程内 | 重启变化；无 ID 账号共享 |
| IDE User-Agent | 带 machineId | 带 machineId | 可看到账号级差异 |
| CLI User-Agent | 不带 machineId | 不带 machineId | 仍共享 CLI 身份 |
| OS/Node/Kiro/SDK 版本 | 全局/常量 | 全局/常量 | 非账号级 |
| `amz-sdk-invocation-id` | 每请求随机 | 每请求随机 | 请求追踪 ID，不是账号指纹 |
| proxy URL/resource | 可账号级 | 可账号级 | 相同代理仍共享出口/连接特征 |
| TLS backend/client | 非完整账号级 | 非完整账号级 | 不能宣称每号 TLS 独立 |
| endpoint Host | 账号 region | 账号 region 或 rotation region | 167 增加组合维度 |
| 调度健康状态 | 基础冷却/失败 | region rotation、berserk、质量调度 | 167 明显更强 |
| email/subscription | 已有存储 | usage limits 可补齐 email | 167 补全链路更完整 |

## 十一、哪个版本更好

### 如果评价“生产调度稳定性”

选 **v0.0.167**：

- 账号耗尽时能切 region；
- 普通 429 与风控/Retry-After 分开；
- berserk 的尝试预算不会被默认 1..10 的预算截断；
- 同轮换账号不退避，跨轮次才退避；
- 保留 committed 防重复输出；
- 外部池可基于真实流量质量调整；
- warmup 不会被普通容量修改误清零。

### 如果评价“每个账号是否拥有完全独立的运行时指纹”

**两者都不合格，167 也没有完成这个目标。**

167 改善的是：

```text
账号选择 -> 账号 token/profileArn/machineId -> region/Host -> 上游重试
```

但没有完成：

```text
账号 -> 独立 OS/Node/SDK/Kiro 版本
账号 -> 独立 CLI machineId
账号 -> 独立 TLS/HTTP client/连接池
账号 -> 独立且持久化的 fallback identity
账号 -> 自动独立网络出口
```

### 最终判断

如果只能二选一，**使用 v0.0.167 作为调度基线**；如果验收目标是“账号信补全和指纹独立”，必须在 167 之上继续补齐账号级 identity profile，不能把 167 现有的 machineId 派生直接当作完成。

## 十二、针对目标要求的验收建议

以下是分析结论，不代表本次已修改业务代码：

1. 每个账号必须有稳定的持久化 identity record，至少包含 `credential id`、`machineId`、`profileArn state`、`authRegion`、`apiRegion`、`endpoint`、`proxy resource`、`fingerprint version`。
2. 多账号模式禁用全局 machineId 覆盖，除非显式选择“所有账号共享设备身份”。
3. fallback machineId 必须落库；不能使用 `id == None` 的共享 bucket。
4. CLI 路径需要明确记录是否允许共享官方 CLI User-Agent；如果要求每号独立，CLI 也要有协议合法的账号级差异字段。
5. 将 `osVersion`、`nodeVersion`、`kiroVersion`、SDK/service version 标记为“模拟客户端 profile”，不要误标为账号独立字段。
6. `profileArn` 按认证类型校验：真实 ARN 优先，固定 ARN 仅限协议明确允许的类型，External IdP 缺失时不可伪造。
7. 维持 auth region 与 API region 分离；region rotation 只能覆盖 API Host，不得改变 token refresh endpoint。
8. 记录每次请求最终使用的 `credential id`、`machineId hash`、`profileArn hash`、Host、region、proxy id、UA profile version 和失败分类，便于证明“每号独立”而不是凭配置推断。
9. 对相同 proxy、相同 TLS backend、相同 Host 的账号做审计，避免账面字段独立但网络层完全相同。
10. 对 berserk、Retry-After、风控型 429、400/403、下游 committed 分别做计数，不能用一个“重试次数”替代整个上游调度事实。

## 十三、源码证据索引

- [machineId 计算与 fallback](../src/kiro/machine_id.rs)
- [KiroCredentials 字段定义](../src/kiro/model/credentials.rs)
- [IDE 上游 headers、Host、profileArn 注入](../src/kiro/endpoint/ide.rs)
- [CLI 上游 headers、Host、management 请求](../src/kiro/endpoint/cli.rs)
- [RequestContext 与 region_override](../src/kiro/endpoint/mod.rs)
- [region rotation / berserk 规则](../src/kiro/retry_pipeline.rs)
- [inference 账号选择、region 覆盖、429 分类](../src/kiro/provider.rs)
- [usage limits、email 参数和上游调用](../src/kiro/token_manager/refresh.rs)
- [email 写回、subscription 写回、warmup 保留](../src/kiro/token_manager/manager.rs)
- [167 外部池质量调度](../src/external_pool.rs)

