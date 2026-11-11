# Kiro 官方协议操作与参考项目对照

Status: `Protocol design / reference`

Last reviewed: 2026-11-10 Asia/Shanghai

## 1. 范围

本文只描述协议和数据合同：

- 凭证导入、导出和字段归一化；
- Kiro 官方 Token 刷新请求；
- 账号身份、额度、模型和 Profile 查询请求；
- refresh token 轮换后的状态更新；
- Region、Profile ARN、认证类型和协议端点的绑定；
- 同账号协议端点 fallback 与跨账号调度的边界。

本文不描述：

- UI 页面、按钮、交互流程；
- 导入或查询的触发时机、定时周期和后台任务；
- 批量导入并发、队列、节流和刷新策略；
- 客户端版本升级、服务二进制升级或版本稳定性判断。

## 2. 结论

1. **导入/导出 JSON 不是 Kiro 官方协议。** 它们是管理器之间交换凭证的本地数据格式；真正的官方协议从 Token 刷新、`getUsageLimits`、`GetUserInfo`、`GetUserUsageAndLimits`、`ListAvailableProfiles` 和 `ListAvailableModels` 开始。
2. **当前项目已经覆盖主要官方 operation，但没有统一的跨项目兼容协议层。** 当前 Rust 侧有 Social、IdC/Builder ID、External IdP 刷新，Portal/REST 额度查询、Profile、models 和 MCP；Portal CBOR 已接入非 API Key usage 查询。
3. **当前项目的 `endpoint` 是协议实现选择，不是账号池。** 当前按 `credential.endpoint > config.defaultEndpoint` 选择一个 `ide` 或 `cli` 实现；同账号协议端点候选链尚未落地。
4. **端点 fallback 必须绑定当前账号的完整身份上下文。** 不需要新增“账号 × 端点”关系表，但每次候选切换都必须复用同一账号的 token、Profile ARN、Machine ID、认证 Region、API Region、认证类型和代理。
5. **换账号和换端点是两层不同操作。** 同账号端点候选耗尽后，才允许账号调度器选择下一个账号；新账号必须从自己的首选端点重新开始。
6. **refresh token 可能轮换。** 刷新响应带新 `refreshToken` 时必须持久化新值；没有新值时沿用旧值。保存失败时不能把新 token 当作已发布状态。
7. **Profile ARN 和 Region 不能用一个默认值覆盖所有认证类型。** API Key 不使用 IDE/Profile 语义；Builder ID 的额度查询需要保留兼容占位 `profileArn`；Enterprise/External IdP 需要真实可用的 Profile ARN。

## 3. 协议分层

```text
本地文件协议
  -> 解析、字段别名、认证类型推断、敏感字段恢复
  -> 形成统一 CredentialEnvelope

官方认证协议
  -> Social refreshToken
  -> IdC / Builder ID OIDC token
  -> External IdP OAuth refresh_token

官方数据协议
  -> REST getUsageLimits
  -> REST/CBOR GetUserInfo
  -> CBOR GetUserUsageAndLimits
  -> REST ListAvailableProfiles
  -> REST/JSON ListAvailableModels

本地调度协议
  -> 同账号协议端点候选
  -> 同账号 Region/host 候选
  -> 跨账号 failover
```

最后一层是当前服务自己的调度，不是 Kiro 提供的账号调度 API。它只能改变本地选择顺序，不能改变上游 token 的归属。

## 4. 统一凭证模型

当前项目和三个参考项目最终都需要表达以下字段。字段名可以不同，协议含义相同。

| 逻辑字段 | 用途 | Social | IdC / Builder ID | External IdP | API Key |
| --- | --- | --- | --- | --- | --- |
| `accessToken` | 数据面 Bearer token | 可有 | 可有 | 可有 | API key 直接作为凭证，通常不使用 OAuth access token |
| `refreshToken` | 获取新 access token | 必填 | 必填 | 必填 | 不使用 |
| `clientId` | OIDC 或外部 IdP 客户端 | 不需要 | 必填 | 通常需要 | 不使用 |
| `clientSecret` | OIDC 客户端密钥 | 不需要 | 通常需要 | 视 IdP 而定 | 不使用 |
| `authMethod` | 选择刷新协议 | `social` | `idc` | `external_idp` | `api_key` |
| `provider` / `idp` | 上游身份提供方 | Github/Google | BuilderId/Enterprise | Microsoft/Enterprise 等 | Kiro API Key |
| `region` | 兼容旧格式的区域字段 | 可有 | 可有 | 可有 | 常用于 API host |
| `authRegion` | OIDC 或认证服务区域 | 通常不使用 | 必须可推导 | IdP 自己的区域/端点 | 不使用 |
| `apiRegion` | `q.<region>.amazonaws.com` 等数据面区域 | 可有 | 可有 | 可有 | 可有 |
| `profileArn` | Enterprise/Profile 数据面身份 | 通常不使用 | Builder ID 可为占位；Enterprise 需真实值 | 通常需真实值 | 不使用 |
| `machineId` | Kiro 请求头中的设备身份 | 需要稳定值 | 需要稳定值 | 需要稳定值 | 按实现需要 |
| `endpoint` | 本地协议实现首选项 | `ide` 常见 | `ide` 常见 | `ide` 常见 | `cli` 常见 |

统一模型建议叫 `CredentialEnvelope`，不要直接把外部项目的 `Account` 结构当作内部存储结构。`Account` 往往还包含订阅、额度、标签、代理和运行状态，这些不是认证请求的必要字段。

## 5. 导入和导出数据协议

### 5.1 当前项目的导出

当前 Admin 导出入口为：

```text
GET /api/admin/credentials/export?format=json
GET /api/admin/credentials/export?format=backup-json
GET /api/admin/credentials/export?format=jsonl
```

实现位置：

- `src/admin/handlers.rs:1579-1630`；
- `src/admin/service.rs:5808-5899`；
- `src/kiro/token_manager/manager.rs:14445-14468`。

当前格式：

1. `json`：顶层 JSON 数组，每项是 `KiroCredentials`；
2. `backup-json`：

   ```json
   {
     "format": "kiro-rs-credentials-backup",
     "exportedAt": "<RFC3339>",
     "credentials": [
       {
         "id": 1,
         "authMethod": "social",
         "refreshToken": "<redacted>",
         "region": "us-east-1",
         "endpoint": "ide"
       }
     ]
   }
   ```

3. `jsonl`：每行一个 `KiroCredentials` JSON 对象。

导出的是当前凭据配置快照，不包含调度器内部运行状态。OAuth 凭据会带 `refreshToken`；API Key 凭据使用 `kiroApiKey`，不应被强行转换成 OAuth 字段。

### 5.2 当前项目的导入归一化

前端解析入口为 `admin-ui/src/lib/credential-import.ts:119-276`。它支持：

- 单对象、数组、JSONL；
- `{ "accounts": [...] }`；
- `{ "credentials": [...] }`；
- `{ "data": { "credentials": [...] } }`；
- 嵌套 `credentials` 对象；
- camelCase 与 snake_case；
- `expiresAt` / `expired` 的秒或毫秒时间戳；
- `authMethod`、`provider`、`profileArn`、`tokenEndpoint`、`issuerUrl`、`scope(s)`；
- `ksk_...|region` 和纯 API Key 行文本。

归一化规则：

```text
kiroApiKey 存在                      -> api_key
否则 authMethod=external_idp         -> external_idp
否则 clientId + clientSecret 存在    -> idc
否则                                 -> social
```

这里的 `social` 是缺省推断值，不代表已经向 Kiro 验证成功。导入协议适配器只负责形成 `CredentialEnvelope`；是否能刷新、Profile 是否匹配、额度是否可读属于官方协议验证结果。

### 5.3 参考项目格式对照

| 项目 | 主导出形态 | 可接受导入形态 | 关键兼容点 |
| --- | --- | --- | --- |
| 当前 `kiro.rs` | `KiroCredentials[]`、`backup-json`、JSONL | 数组、包装对象、嵌套凭据、API Key 文本 | 字段别名多，内部字段最完整 |
| Kiro-Go | `{version, exportedAt, accounts:[...]}` 账号包 | 单对象、数组、`accounts` 包装、文本 API Key | `credentials` 结构和扁平字段都存在 |
| Kiro-account-manager | 完整账号包、JSON/TXT/CSV/剪贴板 | refresh token、JSON、CSV、卡密行 | `idp`、`provider`、`clientId`/`clientSecret` 组合 |
| kiro-manager-lite | `{app, version, exportedAt, accounts, groups}` 或精简 OIDC JSON | 完整备份、精简 JSON、CSV/TXT、嵌套 token 原文 | 完整备份用于本地恢复，精简凭证用于协议验证 |

设计结论：新增兼容层应采用“输入适配器 -> CredentialEnvelope”，不要让每个参考项目的账号结构渗透到 Token Manager。输出也应区分：

- `native`：当前项目可无损恢复的凭据字段；
- `compatibility`：面向 Kiro-Go/KAM/Lite 的扁平字段；
- `redacted`：只含标识、认证类型、Region、Profile 摘要和 token 指纹，不含可用凭据。

## 6. Token 刷新协议

### 6.1 Social / GitHub / Google

```http
POST https://prod.<auth-region>.auth.desktop.kiro.dev/refreshToken
Content-Type: application/json
User-Agent: KiroIDE-<version>-<machine-id>
```

```json
{
  "refreshToken": "<refresh-token>"
}
```

响应核心字段：

```json
{
  "accessToken": "<access-token>",
  "refreshToken": "<new-refresh-token-if-rotated>",
  "profileArn": "<optional-profile-arn>",
  "expiresIn": 3600
}
```

当前 Rust 实现还会发送 `host`、`Accept`、`Connection` 等兼容头，并按认证 Region 构造候选 host。

### 6.2 IdC / Builder ID

```http
POST https://oidc.<region>.amazonaws.com/token
Content-Type: application/json
```

```json
{
  "clientId": "<client-id>",
  "clientSecret": "<client-secret>",
  "refreshToken": "<refresh-token>",
  "grantType": "refresh_token"
}
```

请求通常需要 AWS SDK/Kiro 兼容头：`x-amz-user-agent`、`user-agent`、`host`、`amz-sdk-invocation-id` 和 `amz-sdk-request`。

### 6.3 External IdP

```http
POST <tokenEndpoint>
Content-Type: application/x-www-form-urlencoded
```

```text
client_id=<client-id>&grant_type=refresh_token&refresh_token=<refresh-token>&scope=<scope>
```

响应使用 OAuth 命名：`access_token`、`refresh_token`、`expires_in`、`scope`。没有新 `refresh_token` 时沿用输入值。

### 6.4 轮换持久化合同

刷新结果必须按以下字段级规则处理：

```text
accessToken       <- 响应 accessToken
refreshToken      <- 响应 refreshToken；缺失时保留旧值
expiresAt         <- now + expiresIn
profileArn        <- 响应 profileArn（非空时更新）
refreshTokenHash  <- 原始 refresh token 的不可逆指纹，用于重复检测
```

推荐的发布顺序：

1. 验证响应字段和认证类型；
2. 将新凭据写入持久化存储，并带 revision/generation；
3. 持久化成功后更新内存池；
4. 持久化失败时保留错误结果，并把旋转后的 token 作为受控诊断返回值，而不是静默丢弃。

这不是定时策略，而是单次刷新操作的状态一致性合同。Kiro-Go、Kiro-account-manager 和 kiro-manager-lite 都处理了“响应返回新 refresh token”这一事实；当前项目已有新 token 更新逻辑，但兼容导入层和多实例发布仍应共用同一合同。

## 7. 账号信息和能力查询协议

### 7.1 REST `GetUsageLimits`

```http
GET https://q.<api-region>.amazonaws.com/getUsageLimits
    ?origin=AI_EDITOR
    &resourceType=AGENTIC_REQUEST
    &isEmailRequired=true
    &profileArn=<optional-or-required>
Authorization: Bearer <access-token>
Accept: application/json
```

当前项目在 `src/kiro/token_manager/refresh.rs:1093-1170` 构造该请求。返回通常包含：

- `usageBreakdownList`：基础额度、试用额度、奖励额度、超额字段；
- `subscriptionInfo`；
- `nextDateReset`；
- `overageConfiguration` / `overageSettings`；
- 部分实现会嵌套 `userInfo`。

`profileArn` 规则：

- Enterprise/External IdP：优先使用真实 ARN；
- Builder ID：保留官方兼容占位 ARN，不能因为“不是企业账号”而无条件删除；
- API Key：使用 `tokentype: API_KEY`，不注入 IDE/Profile ARN。

### 7.2 `GetUserInfo`

不同实现使用两种可见传输形态：

**REST/JSON 形态（Kiro-Go）：**

```http
POST https://q.<region>.amazonaws.com/GetUserInfo
Authorization: Bearer <access-token>
Content-Type: application/json
```

```json
{ "origin": "KIRO_IDE" }
```

**CBOR 门户形态（KAM/Lite）：**

```http
POST https://app.kiro.dev/GetUserInfo
Accept: application/cbor
Content-Type: application/cbor
smithy-protocol: rpc-v2-cbor
Authorization: Bearer <access-token>
Cookie: Idp=<idp>; AccessToken=<access-token>
```

CBOR body 是同等语义的编码对象 `{ "origin": "KIRO_IDE" }`。返回字段通常是 `email`、`userId`、`idp` 和 `status`。

当前 Rust 已实现 Portal CBOR `GetUserInfo`，并在 `GetUserUsageAndLimits` 缺少 email 时作为补充查询；没有单独的管理员 `GetUserInfo` 查询入口。`getUsageLimits` REST response 的 userInfo 与导入字段仍是其他 email 来源。

### 7.3 CBOR `GetUserUsageAndLimits`

```http
POST https://app.kiro.dev/GetUserUsageAndLimits
Accept: application/cbor
Content-Type: application/cbor
smithy-protocol: rpc-v2-cbor
Authorization: Bearer <access-token>
Cookie: Idp=<idp>; AccessToken=<access-token>
```

CBOR body：

```json
{
  "isEmailRequired": true,
  "origin": "KIRO_IDE"
}
```

当前 Rust 的非 API Key usage 路径先调用 Portal CBOR，并将结果归一化为本地 usage 结构。IdC/External IdP 只有 Portal 返回认证或权限错误时才回退 REST；API Key 直接走 REST。REST 使用账号 effective API Region，Portal 使用固定 operation base，两者都没有 Region fallback。

### 7.4 `ListAvailableProfiles`

```http
POST https://codewhisperer.<region>.amazonaws.com/ListAvailableProfiles
Content-Type: application/json
Authorization: Bearer <access-token>
```

body：

```json
{}
```

响应中的 `profiles[].arn` 是 Enterprise/External IdP 可用的真实 Profile ARN。Builder ID 和 Social 账号可能返回 403 或空集合，这应被解释为“不提供 Profile 列表”，不能直接等价为 token 无效。

当前项目只为缺少真实 ARN 的 External IdP 做单 Region Profile 发现，并对 403/空列表使用兼容 fallback；不做多 Region 查询。Kiro-Go/Lite 还会按候选 Region 查询并缓存成功 ARN。

### 7.5 `ListAvailableModels`

IDE 端点通常使用：

```http
GET https://q.<region>.amazonaws.com/ListAvailableModels
    ?origin=AI_EDITOR
    &maxResults=50
    &profileArn=<optional-or-required>
Authorization: Bearer <access-token>
```

CLI 端点使用 AWS JSON 协议：

```http
POST https://codewhisperer.<region>.amazonaws.com/
Content-Type: application/x-amz-json-1.0
X-Amz-Target: AmazonCodeWhispererService.ListAvailableModels
Authorization: Bearer <access-token>
```

返回 `models` 和可选 `nextToken`。当前 Rust 已支持 IDE/CLI 两套模型发现请求、Profile 注入、API Key `tokentype` 和分页 token 防循环；模型发现是辅助数据协议，不应被当作推理端点本身。

### 7.6 WebSearch MCP `tools/call`

WebSearch 是独立的 JSON-RPC 请求，不复用 inference 的 AWS JSON Content-Type：

```http
POST https://q.<api-region>.amazonaws.com/mcp
Content-Type: application/json
Authorization: Bearer <access-token>
```

CLI 账号也使用 MCP 的 `application/json`，即使 CLI inference 使用 `application/x-amz-json-1.0`。API Key 不发送 OAuth/Profile header 并带 `tokentype: API_KEY`。MCP 的 Profile 规则与模型列表不同：IdC/Builder ID MCP 请求需要 Builder ID placeholder；模型列表仍按可选 Profile 规则省略 placeholder。IDE、CLI 和兼容 endpoint decorators 使用 MCP 专用 resolver。

当前 JSON-RPC body：

```json
{
  "id": "web_search_tooluse_<random>_<timestamp>_<random>",
  "jsonrpc": "2.0",
  "method": "tools/call",
  "params": {
    "name": "web_search",
    "arguments": { "query": "<search text>" }
  }
}
```

当前项目从最后一条 user 消息抽取 query，最多发送 200 个字符；`allowed_domains` 最多三项时可能追加 `site:` 提示，结果再应用 allowed/blocked 域名过滤。Manager Lite 使用相同 JSON-RPC method/name/argument 形状，但直接 trim query，不附加 allowed-domain 提示。

HTTP `400` 是协议/请求层响应。当前本地错误 `invalid_tool_input` 只是 `InvalidRequest` 的映射，不是 Kiro 的响应正文，也不能单独证明 query 格式错误。普通 MCP 400 按请求错误处理。

本次 `req_01evUz3cpf9N45dQzkiZf8Xu` 的事故后只读配置指向 `ide`、`q.us-east-1`，账号为 IdC/Builder ID，Profile ARN 是 Builder ID placeholder；Region 与 ARN Region 一致。项目根目录 JSON 中的 KIRO FREE IdC 账号已完成官方 MCP endpoint A/B：相同版本、SDK、Authorization 和语义 JSON-RPC body 下，省略 `x-amzn-kiro-profile-arn` 返回 `400 profileArn is required for this request.`，加入 Builder ID placeholder 后返回 `200`。生产账号配置与失败条件一致，故此次错误定位为缺失必需 Profile header；usage 记录没有保存生产请求原始 header/body，无法还原其逐字节报文。CLI Content-Type 差异不适用于这条路径。

## 8. 参考项目的协议实现差异

| 能力 | 当前 `kiro.rs` | Kiro-Go | Kiro-account-manager | kiro-manager-lite |
| --- | --- | --- | --- | --- |
| OAuth 刷新 | Social、IdC、External IdP | Social、IdC、External IdP | Social、IdC、External IdP | Social、IdC |
| 新 refresh token | 已接收并更新 | 接收、持久化、保存旧 token 指纹 | 无新值沿用旧值 | 无新值沿用旧值，并同步 IDE token |
| REST usage | `getUsageLimits` | `getUsageLimits` | 支持 REST | 支持 REST |
| CBOR usage | 非 API Key 优先 Portal CBOR；IdC/External IdP 认证拒绝时回 REST | 非主路径 | `GetUserUsageAndLimits` | `GetUserUsageAndLimits`，401/403 回 REST |
| UserInfo | Portal CBOR 在 usage 缺 email 时补查；无独立管理查询 | REST `GetUserInfo` | CBOR `GetUserInfo` | CBOR `GetUserInfo` |
| Profile discovery | External IdP 有限支持 | 多 Region 查询并缓存 | Enterprise 查询 | Enterprise 查询并写回 ARN |
| Model discovery | IDE/CLI 两实现，支持分页 | REST `ListAvailableModels` | REST/CBOR 组合 | REST 查询 |
| 导入验证合同 | 归一化后由服务验证 | refresh 后保存 | refresh + usage 验证 | 精简凭证进入 refresh + usage 链 |
| 端点 fallback | inference、models、MCP 已有同账号候选；usage/Profile 不复用该候选链 | OAuth IDE/Q 链；API Key 固定 CLI runtime | 主要是 usage REST 主备 | 主要是 usage/Profile 候选 |

可直接借鉴的协议事实：

- Kiro-Go 的 refresh 结果先落盘再发布到运行时；
- Kiro-Go 的 `rotatedRefreshToken` 能在持久化失败时帮助调用方接住轮换值；
- KAM/Lite 同时保留 REST 与 CBOR usage 适配器，并把错误码解释为认证类型/协议能力差异；
- Lite 的 `profileArn` 候选按账号类型区分，成功的真实 ARN 写回账号；
- 所有项目都把 API Key 与 OAuth 刷新分成不同分支。

不能直接照搬的部分：

- Kiro-Go 的 `auto`/Q 端点链不能加入当前 Rust，除非对应端点实现、请求体、流式事件和错误分类都已注册；
- KAM 的 REST 403 主备切换是额度查询专用语义，不等同于推理请求的协议端点 fallback；
- CBOR 门户的 `Cookie: Idp=...` 不能迁移到当前 REST 请求；
- 参考项目的账号轮询、冷却和粘性属于本地调度，不是 Kiro 官方协议字段。

## 9. 端点、Region 与账号绑定设计

### 9.1 三种“端点”必须分开

| 名称 | 示例 | 作用 | 绑定账号 |
| --- | --- | --- | --- |
| 协议端点 | `ide`、`cli`、未来的 `codewhisperer`/`amazonq` | 决定 URL、body、headers、事件协议 | 运行时绑定当前账号 |
| API Region/host | `q.us-east-1.amazonaws.com` | 决定数据面区域 | 由当前账号 Region 构造 |
| 认证端点 | `prod.<region>.auth.desktop.kiro.dev`、`oidc.<region>.amazonaws.com` | 决定 refresh 协议 | 由认证类型和 auth region 构造 |

`runtime.<region>.kiro.dev` 是 IDE 实现内部的 host 变体，不能自动当成独立协议端点。

### 9.2 同账号协议候选

建议的本地路由快照：

```text
credential.endpoint
    > config.preferredEndpoint
    > config.defaultEndpoint
```

候选资格必须同时满足：

1. endpoint 已注册；
2. 认证类型和 Profile 语义兼容；
3. 能用当前账号 Region 构造有效 host；
4. 当前请求类型有该 endpoint 的 URL/body/header 实现；
5. 当前请求尚未提交下游语义输出。

候选切换只替换 endpoint-specific 的请求构造，不替换：

- access token / API key；
- refresh token；
- machine ID；
- profile ARN；
- auth region / API region 的账号上下文；
- 账号代理和请求归属。

### 9.3 跨账号调度

跨账号调度由当前服务的账号池负责：

```text
账号 A
  -> A 的首选 endpoint
  -> A 的兼容 endpoint 候选
  -> A 候选耗尽
账号 B
  -> B 的首选 endpoint
  -> B 的兼容 endpoint 候选
```

账号 B 不能继承账号 A 的 endpoint 游标、Profile ARN、Region override 或 token。每一次真实发送都必须归因到 `(credential_id, endpoint, api_region)`。

### 9.4 失败分类

| 上游结果 | 同账号换协议端点 | 跨账号 | 处理理由 |
| --- | --- | --- | --- |
| DNS、连接、TLS 建连失败 | 可尝试 | 可尝试 | 请求体尚未发送 |
| 408/425/5xx | 仅首语义输出前且预算允许 | 可尝试 | 瞬态故障 |
| 429 | 默认不跨 endpoint | 按账号策略 | 可能是账号额度或风控，而非 host 故障 |
| 400 malformed/schema/tool | 不尝试 | 不因换账号解决 | 请求本身错误 |
| 明确 endpoint unavailable 的 400/404 | 可尝试 | 按账号策略 | 必须由结构化错误白名单确认 |
| 401/402/403、额度耗尽 | 不尝试 | 可尝试 | 认证或账号额度问题 |
| Profile ARN/Region 不匹配 | 先修正账号上下文 | 通常不换号 | 盲切 endpoint 无法修复身份绑定 |
| 已写出首个语义事件 | 不尝试 | 不尝试 | 禁止重放同一请求 |

每一跳都使用同一个推理发送预算。不能把“账号重试上限”和“endpoint 候选数量”分别计算后相乘，造成隐式无限重试。

## 10. 推荐的实现边界

### 10.1 新增协议适配接口

```rust
trait KiroProtocolOperations {
    async fn refresh_token(&self, credential: &CredentialEnvelope) -> Result<RefreshResult>;
    async fn get_usage_limits(&self, ctx: &AccountProtocolContext) -> Result<UsageSnapshot>;
    async fn get_user_info(&self, ctx: &AccountProtocolContext) -> Result<UserInfo>;
    async fn get_user_usage_and_limits(&self, ctx: &AccountProtocolContext) -> Result<UsageSnapshot>;
    async fn list_available_profiles(&self, ctx: &AccountProtocolContext) -> Result<Vec<Profile>>;
    async fn list_available_models(&self, ctx: &AccountProtocolContext) -> Result<ModelPage>;
}
```

实际代码可按当前 Rust 模块拆成多个 trait；关键是每个官方 operation 有独立请求/响应模型，不能用一个“账号刷新”函数包住所有差异。

### 10.2 新增导入适配器

```text
KiroRsAdapter       -> KiroCredentials
KiroGoAdapter       -> CredentialEnvelope
KAMAdapter          -> CredentialEnvelope
KiroLiteAdapter     -> CredentialEnvelope
ApiKeyLineAdapter   -> CredentialEnvelope
```

适配器只负责：字段映射、别名、时间戳、认证类型推断、嵌套对象展开和敏感字段完整性。它不负责替代官方上游调用。

### 10.3 统一响应归一化

REST、CBOR 和 CLI JSON 的字段命名不同，内部应归一化为：

```text
UserInfo        { email, user_id, idp, status }
UsageSnapshot   { subscription, breakdowns, current, limit, remaining, reset_at, overage }
Profile         { arn, region, source }
ModelPage       { models, next_token }
RefreshResult   { access_token, refresh_token, expires_at, profile_arn }
```

保留 `raw_protocol` 和 `operation` 元数据用于诊断，但禁止保存原始 token、完整 Authorization、Cookie 或 Client Secret。

## 11. 验收合同

### 导入导出

- 当前 `json`、`backup-json`、`jsonl` 可以无损回读 OAuth、External IdP、API Key 的必要字段；
- Kiro-Go 的 `accounts` 包装、KAM 的扁平 JSON/CSV、Lite 的完整备份和精简 JSON 都能形成同一 `CredentialEnvelope`；
- API Key 不会被当成 OAuth refresh token；
- snake_case/camelCase、秒/毫秒时间戳和嵌套 `credentials` 的映射结果一致；
- 导出/日志/错误中不出现真实 token 或 secret。

### 官方操作

- Social、IdC、External IdP 三条刷新请求的 URL、body、Content-Type 和关键 headers 正确；
- 新 refresh token 被更新，缺失时保留旧值；
- `getUsageLimits` 正确处理 Builder ID 占位 Profile ARN、Enterprise 真实 ARN 和 API Key token type；
- `GetUserInfo` REST/CBOR 两种响应能归一化；
- CBOR `GetUserUsageAndLimits` 的 401/403 能按认证能力回退 REST；
- `ListAvailableProfiles` 的 403 对 Builder ID/Social 表示“不支持 Profile 列表”，不直接禁用账号；
- `ListAvailableModels` 的 REST/CLI 分页不会重复消费相同 `nextToken`。

### 调度

- 同账号 endpoint fallback 不改变 token、Profile、machine ID、Region 和代理；
- 换账号后 endpoint 游标从新账号首选值重新开始；
- 401/402/403 和额度耗尽不在同账号盲切协议端点；
- 下游已产生首个语义事件后不重放；
- endpoint、Region、账号三层 attempt 都计入同一发送预算。

## 12. 依据

当前项目：

- `src/admin/service.rs:5808-5899`：导出 JSON、backup JSON、JSONL；
- `src/admin/types.rs:965-1105`：导入字段合同；
- `admin-ui/src/lib/credential-import.ts:119-276`：导入归一化；
- `src/kiro/token_manager/refresh.rs:627-1090`：三类 Token 刷新；
- `src/kiro/token_manager/refresh.rs:1093-1170`：`getUsageLimits`；
- `src/kiro/provider.rs:10771-10889`：`ListAvailableProfiles`；
- `src/kiro/provider.rs:12020-12120`：`ListAvailableModels`；
- `src/kiro/endpoint/mod.rs:14-18`：当前 `ide`/`cli` 注册；
- `src/kiro/endpoint/ide.rs:95-215`、`src/kiro/endpoint/cli.rs:145-214`：两种协议实现。

参考项目最新代码：

- Kiro-Go：`/tmp/kiro-go-latest/proxy/handler.go`、`/tmp/kiro-go-latest/proxy/kiro_api.go`、`/tmp/kiro-go-latest/proxy/kiro.go`；
- Kiro-account-manager：`/tmp/kam-latest/Kiro-account-manager/src/main/index.ts`、`src/main/kiroAuthSync.ts`；
- kiro-manager-lite：`/Users/yuanfeijie/Desktop/procode/kiro-manager-lite/src/main/kiroApi.ts`、`src/main/accountService.ts`、`src/renderer/src/utils/transfer.ts`；
- `kiro-rs-mata`：`/Users/yuanfeijie/Desktop/procode/kiro-rs-mata/src/kiro/provider.rs` 的同账号协议端点候选链。

本文件不记录任何真实 refresh token、access token、API Key、Client Secret 或本地凭据内容。
