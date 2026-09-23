# Kiro v0.0.161 与 v0.0.167：账号身份补全和上游调度差异

**分析日期：** 2026-09-20  
**比较范围：** `v0.0.161`（2026-09-02）与 `v0.0.167`（2026-09-20）  
**分析重点：** 账号信息补全、账号独立性、上游接口身份、Region/Endpoint、Token refresh、重试、故障转移和多账号调度  
**明确排除：** 协议转换层；`sub2api-kiro` 及相关项目不作为参考

## 结论

### 哪个版本更好

如果目标是多账号生产调度和上游故障恢复，`v0.0.167` 明显优于 `v0.0.161`。主要优势包括：

- API Region rotation；
- 本地账号 × Region 狂暴轮换；
- 普通瞬态重试中的 Region fallback；
- 风控型 429、带 `Retry-After` 的 429、普通 429 的分类；
- 外部账号池质量感知调度；
- 质量 probation、recovery ramp 和 Redis 质量状态；
- warmup 状态修复；
- 使用额度查询中的上游 email 补全与持久化。

### 是否已经做到“每个账号完全独立”

没有完全做到。更准确的判断是：

```text
逻辑账号身份：基本独立
认证材料：基本独立
账号级调度状态：v0.0.167 明显更完整
machineId：大多数账号独立，但存在全局覆盖和 fallback 边界
profileArn：按认证类型处理，不保证每个账号唯一
运行时指纹：没有完整做到账号级独立
网络出口：取决于代理配置，不自动保证
```

`v0.0.161` 已经具备完整的基础账号字段和 machineId 机制。不能把“基础身份字段”写成 `167` 才新增；`167` 的重点是补全 email 和升级调度。

## 一、版本和代码范围

本地仓库当前状态：

```text
v0.0.161: 31c947b76c8ab39891796f43b91f2b2afd2c0814
v0.0.167: 35ce08e32cc2222e6acc0a8c48cbc088b954943c
```

`v0.0.161..v0.0.167` 的总体变化：

```text
97 files changed
16070 insertions
550 deletions
```

与本报告直接相关的主要提交：

```text
4c7790b feat(external-pool): quality-aware scheduling for external accounts
5d49a6e test(external-pool): L2 real-scheduling suite for quality-aware scheduling
e6d6397 feat(kiro): 本地账号狂暴轮换配置与轮换状态机
d04de88 feat(kiro): 429 狂暴轮换与普通模式 region 轮换接入重试管线
144ac56 feat(kiro): 端点轮换改为独立总开关，与狂暴模式完全正交
e6c5e35 feat(kiro): 端点内置化 + 两个管理前端暴露轮换/狂暴开关
5c6375f fix(kiro): 修复狂暴模式被尝试预算截断、Retry-After 429 误入轮换
60eb117 fix: preserve credential warmup across capacity updates
6287c50 feat: harden protocol interop and external pool scheduling
3e5774b fix(kiro): hydrate credential emails during account queries
```

## 二、账号静态字段差异

`v0.0.161` 和 `v0.0.167` 的 `KiroCredentials` 基础身份字段没有实质差异。两版本都具备：

| 字段/能力 | 161 | 167 | 结论 |
|---|---:|---:|---|
| `id` | 有 | 有 | 账号数据库身份已存在 |
| `accessToken` | 有 | 有 | 账号级认证材料 |
| `refreshToken` | 有 | 有 | OAuth 账号级认证材料 |
| `kiroApiKey` | 有 | 有 | API Key 账号级认证材料 |
| `profileArn` | 有 | 有 | 不是 167 首次新增 |
| `expiresAt` | 有 | 有 | Token 生命周期 |
| `authMethod` | 有 | 有 | social/idc/external_idp/api_key 等 |
| `provider` | 有 | 有 | BuilderId/Enterprise/External IdP 等 |
| `clientId` / `clientSecret` | 有 | 有 | IdC/OIDC |
| `tokenEndpoint` | 有 | 有 | External IdP refresh |
| `issuerUrl` / `scopes` | 有 | 有 | External IdP 导入和刷新 |
| `region` | 有 | 有 | Token 刷新相关 |
| `authRegion` | 有 | 有 | Token 刷新区域 |
| `apiRegion` | 有 | 有 | API 请求区域 |
| `machineId` | 有 | 有 | 两版本机制基本相同 |
| `email` | 有 | 有 | 167 增加上游补全 |
| `subscriptionTitle` | 有 | 有 | 161 已有同步逻辑 |
| `proxyUrl` 及代理认证 | 有 | 有 | 可配置账号级代理 |
| `endpoint` | 有 | 有 | IDE/CLI 路径区分 |
| `supportedModels` | 有 | 有 | 账号级模型调度限制 |
| RPM/并发/优先级/冷却状态 | 有 | 更完整 | 167 调度行为增强 |

相关模型位于 [`src/kiro/model/credentials.rs`](../src/kiro/model/credentials.rs)。

### 关键判断

```text
161 已经具备账号级基础身份；
167 主要加强账号信息补全、失败状态和调度路径；
不能把 profileArn、machineId、token、region 字段全部算成 167 新增。
```

## 三、machineId 行为和独立性

当前两版本的 machineId 生成逻辑基本一致，优先级为：

```text
凭据级 machineId
> 全局 config.machineId
> API Key: SHA256("KiroAPIKey/" + kiroApiKey)
> OAuth: SHA256("KotlinNativeAPI/" + refreshToken)
> fallback: SHA256("KiroFallback/" + UUID)
```

实现位置：[`src/kiro/machine_id.rs`](../src/kiro/machine_id.rs)。

### 已满足的部分

- OAuth 账号通常由各自 `refreshToken` 派生不同 machineId；
- API Key 账号通常由各自 API key 派生不同 machineId；
- 账号缺少 machineId 时，启动加载阶段会生成；
- 正常账号加载路径会将新生成的 machineId 持久化到 PgSQL；
- IDE User-Agent 会带 machineId；
- usage limits 请求会带 machineId；
- profile ARN discovery identity key 会包含 machineId。

### 当前不能声称“绝对独立”的部分

1. **全局 machineId 可以覆盖多个账号**

   如果全局 `config.machineId` 配置合法，则没有凭据级 machineId 的账号会共用该值。

2. **fallback machineId 只保证进程内稳定**

   没有 refresh token、API key 或凭据级 machineId 的账号，fallback 基于随机 UUID，代码注释明确说明“不持久化”。重启后可能变化。

3. **无数据库 ID 的账号存在共享 fallback 桶**

   fallback cache 使用 `Option<u64>` 作为 key；多个 `id == None` 的账号可能共享一个 fallback 值。

4. **machineId 不是完整 runtime fingerprint**

   当前未按账号独立管理：

   - OS 类型和版本；
   - Node/Rust 版本；
   - AWS SDK 版本；
   - Kiro/CLI 版本；
   - TLS ClientHello；
   - HTTP header 顺序和客户端连接行为；
   - 浏览器注册指纹；
   - 网络出口。

5. **CLI User-Agent 不含 machineId**

   IDE 请求包含：

   ```text
   KiroIDE-{kiroVersion}-{machineId}
   ```

   CLI 请求主要使用固定的 AWS SDK/Rust/Amazon Q CLI runtime identity，账号仍由 token、region、endpoint 和代理区分，但 machineId 不直接进入 CLI User-Agent。

### machineId 结论

```text
161 和 167 的 machineId 基础机制基本相同；
167 没有通过 machineId 新建完整账号指纹系统；
167 的优势主要来自账号调度，而不是 machineId 设计本身。
```

## 四、profileArn 和 profile discovery

### profile discovery 不是 167 首次新增

`v0.0.161` 已经存在按账号身份 hash 隔离 profile ARN discovery 状态的逻辑。当前 key 会结构化组合：

- credential numeric ID；
- refresh token / API key / access token；
- secret kind；
- auth method；
- provider；
- client ID；
- token endpoint；
- API region；
- endpoint；
- machineId。

实现位置：[`src/kiro/provider.rs`](../src/kiro/provider.rs) 的 `profile_arn_discovery_key`。

该设计可防止：

- 删除账号后 numeric ID 复用时继承旧状态；
- token 更换后继承旧的 negative backoff；
- 不同认证方式共用 discovery 状态；
- 不同 endpoint/region 共用 discovery 状态；
- 不同 machineId 共用 discovery 状态。

同时已有：

- per-credential singleflight；
- resolved cache；
- negative backoff；
- bounded LRU；
- auxiliary attempt 不占 inference budget；
- 已有真实 profile ARN 时跳过 discovery。

### profileArn 不能简单当成“每账号唯一指纹”

要区分：

- **真实 profileArn**：可能代表真实租户、组织或账号；
- **协议固定 profileArn**：只是为了满足上游请求格式，不等于账号唯一身份。

Social、BuilderId 等认证类型可能按上游协议使用固定或共享 ARN；External IdP 缺少真实 ARN 时不应随意伪造一个“每号不同”的 ARN，否则可能破坏上游归属关系。

## 五、167 新增的账号信息补全

这是两个版本在“账号信息补全”上的最明确差异。

### usage limits 请求

`v0.0.167` 将：

```text
/getUsageLimits?origin=AI_EDITOR&resourceType=AGENTIC_REQUEST
```

改为：

```text
/getUsageLimits?origin=AI_EDITOR&resourceType=AGENTIC_REQUEST&isEmailRequired=true
```

### 响应模型

新增读取：

```json
{
  "userInfo": {
    "email": "user@example.com"
  }
}
```

### 持久化规则

调用 `get_usage_limits_for` 时：

- 从上游读取 `userInfo.email`；
- 仅在本地 email 为空时写回；
- 不覆盖管理员手工填写的 email；
- 写入失败只告警，不影响本次额度查询；
- 成功后发布 credentials changed 事件。

这条路径是 Admin 账号额度/信息查询路径，不是每次 inference 请求自动补全。

### subscriptionTitle

`subscriptionTitle` 的基本持久化逻辑在 `v0.0.161` 已存在，因此不能全部视为 167 新增。

## 六、上游接口和调度差异

### 1. API Region rotation

`v0.0.167` 新增独立开关：

```text
kiroUpstreamRegionRotationEnabled
```

固定候选：

```text
us-east-1
eu-central-1
```

规则：

- `eu-*` 账号优先 `eu-central-1`；
- 其他账号优先 `us-east-1`；
- 当前 API 请求失败后可以切换另一个 Region；
- 只覆盖 API Region；
- 不改变 token 刷新的 Auth Region；
- models/profile/MCP 等辅助路径不自动等同于 inference 的 Region 轮换矩阵；
- 默认关闭。

端点区域解析优先级：

```text
本次尝试 region_override
> credentials.apiRegion
> profileArn 内嵌 Region
> 全局 API Region
```

### 2. Local berserk account × Region rotation

`v0.0.167` 新增：

```text
localBerserkModeEnabled
localBerserkMaxRounds
localBerserkRoundDelayMs
```

策略顺序：

```text
Region 1:
  account 1 → account 2 → ... → account N

Region 2:
  account 1 → account 2 → ... → account N

下一轮:
  重新从 Region 1 开始
```

总预算：

```text
账号数 × Region 数 × 轮数
```

硬上限为 2000 次。

### 3. 429/瞬态错误分类

167 明确区分：

| 错误类型 | 167 的处理 |
|---|---|
| 风控型 429 | 进入风险控制/冷却，不进入狂暴矩阵 |
| 带 `Retry-After` 的 429 | 遵守上游等待时间，不打满账号池 |
| 普通 429 | 狂暴模式下可切账号/Region |
| 408 | 普通瞬态重试，狂暴模式可参与轮换 |
| 5xx | 普通瞬态重试，狂暴模式可参与轮换 |

### 4. 普通模式 Region fallback

Region rotation 与 berserk mode 是两个独立开关。即使狂暴模式关闭，只打开 Region rotation，普通瞬态重试仍可以切换 API Region。

### 5. 外部账号池质量感知调度

167 新增：

- 失败率 EWMA；
- TTFT EWMA；
- 总耗时 EWMA；
- sample count；
- quality TTL；
- top-K weighted random；
- probation；
- recovery ramp；
- Redis 分布式质量状态；
- 被动真实流量采样；
- 质量采样任务并发上限。

调度原则：

- 质量只在同一优先级层内调整；
- 错误率惩罚高于延迟惩罚；
- 样本不足时回退 legacy selector；
- 所有池同时变差时不清空候选集；
- 普通 transient failure 不再直接升级成 pool-level cooldown；
- 有候选但未选出 pool 时返回明确 503，避免无限空转。

开关默认关闭：

```text
externalPoolQualityAwareSchedulingEnabled = false
```

### 6. warmup 状态修复

167 修复单账号 RPM/并发容量变更时误重置 warmup 的行为：

- 容量变更推进 runtime generation；
- 不重置账号已有 warmup 进度；
- 新账号仍按默认 warmup 初始化；
- 全局运行时容量变更仍可能按全局语义重置。

## 七、GitHub 同类项目对照

检索时间：2026-09-20  
重点窗口：2026-09-06 至 2026-09-20  
以下项目按直接相关性筛选；明确排除 `sub2api-kiro`。

### 近期活跃项目

#### `ZyphrZero/kiro.rs`

近期提交重点：

- `3219d1c`（2026-09-17）：inference 请求遵循 credential region；
- `3194bb2`（2026-09-17）：profileArn 不兼容时增加无 ARN fallback；
- `f413e7d`（2026-09-17）：修复 bare namespaced tool call。

与本报告最相关的参考：

- credential region 纳入 IDE/CLI/MCP/API URL 和 Host header；
- API region 与认证 region 的优先级分开；
- usage/model/profile 控制面针对 403、`Improperly formed request`、`Invalid profileArn` 做无 ARN回退；
- 避免无 ARN 请求重复触发同一个 fallback。

参考：

- <https://github.com/ZyphrZero/kiro.rs>
- <https://github.com/ZyphrZero/kiro.rs/commit/3219d1c>
- <https://github.com/ZyphrZero/kiro.rs/commit/3194bb2>

#### `Yaocool/kiro-proxy`

近期提交 `63e70aa`（2026-09-14）新增统一上游身份模块 `identity.rs`，集中管理：

- IDE/CLI 版本；
- SDK 版本；
- Node/Rust 版本；
- API 版本；
- Streaming 与 Management service；
- IDE User-Agent；
- CLI User-Agent；
- IDE machineId 注入。

其账号模型还包括：

- account ID；
- email；
- label；
- machine_id；
- profile_arn；
- upstream_user_id；
- usage/subscription；
- region；
- auth method；
- credentials。

值得借鉴的是“统一 identity profile”方向；但其 OS、SDK、版本仍是全局常量，CLI 也不是完整的账号级 runtime fingerprint。

参考：

- <https://github.com/Yaocool/kiro-proxy>
- <https://github.com/Yaocool/kiro-proxy/commit/63e70aa>

#### `TsinHzl/kiro2cc-proxy`

近期提交（2026-09-15 至 2026-09-20）重点是：

- 添加账号时自动补全 profileArn；
- 加载存量账号时自动补全并持久化；
- 按 Social、BuilderId、External IdP 区分 profileArn；
- External IdP 缺真实 ARN 时不随意填固定 ARN；
- 缺 profileArn 时区分禁用原因；
- 修复 BuilderId `Invalid profileArn` 后的回滚。

参考价值集中在 profileArn 生命周期和控制面一致性，不是质量调度。

参考：

- <https://github.com/TsinHzl/kiro2cc-proxy>
- <https://github.com/TsinHzl/kiro2cc-proxy/commit/caa4ed1>

#### `minpeter/kiro-lb`

近期提交关注：

- tool-call integrity；
- streaming connection reuse；
- generation failover；
- active load、credit usage、idle time；
- account health、concurrency lease、cooldown、account exclusion。

它适合参考：

- 账号池 lease；
- 失败账号排除；
- failover 期间避免重复选中同一账号；
- pool score explanation。

但没有当前 167 的 Redis 质量 EWMA、probation、recovery ramp 体系。

参考：

- <https://github.com/minpeter/kiro-lb>
- <https://github.com/minpeter/kiro-lb/commit/3a05ed7>

#### `d-kuro/kirocc`

近期重点偏：

- Kiro CLI credentials；
- 模型和协议兼容；
- 错误处理；
- CLI 行为对齐。

账号独立 runtime fingerprint 和复杂质量调度不是其主要方向。

参考：

- <https://github.com/d-kuro/kirocc>

### 历史知名项目和派生项目

#### `hank9999/kiro.rs`

可作为早期 Rust Kiro 多账号、machineId、API key、token refresh 和基础调度的历史对照。当前报告时间窗内没有同等强度的核心调度更新。

#### `jwadow/kiro-gateway`

可参考 model discovery、endpoint migration、streaming tool parsing 和错误分类。当前不是最近两周的主要调度证据。

#### `dwgx/KiroStudio`

可参考 credentials 持久化、region probe、retry budget、account scheduling 和诊断工具，但最近两周没有与本题同等强度的核心账号调度变化。

## 八、账号独立性检查表

| 检查项 | 结论 |
|---|---|
| 每账号独立数据库 ID | 已满足 |
| 每账号独立 access token | 已满足，前提是导入源凭据不同 |
| 每账号独立 refresh token/API key | 已满足，前提是源凭据不同 |
| 每账号独立 auth method/provider | 已满足 |
| 每账号独立 client ID/token endpoint | 有条件满足 |
| 每账号独立 API Region | 已满足，可由 retry 覆盖 |
| 每账号独立 Auth Region | 已满足 |
| 每账号独立 endpoint IDE/CLI | 已满足 |
| 每账号独立 proxy 配置 | 已满足 |
| 每账号独立 machineId | 有条件满足 |
| machineId 跨重启稳定 | 正常落库账号满足，fallback 路径不完全满足 |
| 全局 machineId 不覆盖账号 | 未满足 |
| 无 ID 账号不共享 fallback | 未满足 |
| 每账号独立真实 profileArn | 有条件满足 |
| Social/BuilderId profileArn 每号唯一 | 不满足，可能按协议共享 |
| External IdP 缺 ARN 时不伪造 | 基本满足 |
| 每账号独立 IDE User-Agent | 部分满足 |
| 每账号独立 CLI User-Agent | 未满足 |
| 每账号独立 OS/Node/SDK/Kiro 版本 | 未满足 |
| 每账号独立 TLS/HTTP fingerprint | 未满足 |
| 每账号独立网络出口 | 取决于代理配置 |
| profile discovery 状态按身份隔离 | 已满足，161 已有 |
| 账号失败/cooldown/RPM/concurrency 隔离 | 167 更完整 |
| 外部池质量隔离 | 167 新增，默认关闭 |

## 九、最终建议

### 版本选择

生产多账号调度选择：

```text
v0.0.167
```

### 不能做的错误推断

```text
错误：161 没有账号级身份字段。
正确：161 已有完整基础字段，167 主要增强补全和调度。

错误：profile discovery 是 167 首次新增。
正确：161 已有按身份 hash 隔离 discovery；167 继续在新调度中使用。

错误：只要 machineId 不同，就代表完整账号指纹独立。
正确：machineId 只是一个字段，CLI、TLS、SDK、网络出口仍可能共享。

错误：给每个账号随便生成不同 profileArn 就能实现独立。
正确：profileArn 必须遵循认证类型和上游归属，协议占位 ARN 不等于账号唯一身份。
```

### 若要实现严格的“每账号独立身份”

至少还需要：

1. 多账号模式下禁止全局 machineId 作为默认身份，或仅允许显式单账号模式使用；
2. fallback machineId 基于稳定、持久化的账号材料生成；
3. 拒绝多个 `id == None` 账号共享 fallback；
4. 建立统一的 per-account upstream identity profile；
5. IDE、CLI、usage limits、MCP、management 统一使用同一身份上下文；
6. 明确区分真实 profileArn 和协议占位 ARN；
7. 对账号级代理、Region、endpoint、token endpoint 做启动时完整性审计；
8. 增加账号身份完整度检查和管理端展示；
9. 如确实要求网络层隔离，为每个账号配置独立代理出口，并单独评估 TLS/HTTP client fingerprint。

## 十、验证状态

本报告只做源码和 GitHub 分析，没有修改业务代码，也没有读取未跟踪的凭据文件。

本轮未运行 Rust 测试。
