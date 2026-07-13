# kiro-rs

一个用 Rust 编写的 Anthropic Claude API 兼容代理服务，将 Anthropic API 请求转换为 Kiro API 请求。

## 免责声明

本项目仅供研究使用, Use at your own risk, 使用本项目所导致的任何后果由使用人承担, 与本项目无关。
本项目与 AWS/KIRO/Anthropic/Claude 等官方无关, 本项目不代表官方立场。

## 注意！

运行时默认使用 rustls；默认 feature 构建和官方 Docker 镜像同时包含 rustls 与 native-tls，可通过 `config.json` 的 `tlsBackend` 切换。只有显式使用 `--no-default-features` 构建时才不包含 native-tls。
如果遇到请求报错, 尤其是无法刷新 token, 或者是直接返回 error request, 请尝试切换 tls 后端为 `native-tls`, 一般即可解决。

**Write Failed/会话卡死**: 如果遇到持续的 Write File / Write Failed 并导致会话不可用，通常与输出过长被截断有关，可尝试调低输出相关 token 上限。

## 功能特性

- **Anthropic API 兼容**: 完整支持 Anthropic Claude API 格式
- **流式响应**: 支持 SSE (Server-Sent Events) 流式输出
- **Token 自动刷新**: 自动管理和刷新 OAuth Token
- **多凭据支持**: 支持配置多个凭据，按优先级自动故障转移
- **负载均衡**: 支持 `priority`（按优先级）和 `balanced`（均衡分配）两种模式
- **智能重试**: 单凭据最多重试 3 次，单请求最多重试 9 次
- **数据库持久化**: 运行配置、凭据、凭据运行态、usage 记录和模型价格使用 PgSQL；会话绑定、冷却、限流、并发 lease、刷新锁和余额缓存使用 Redis
- **Thinking 模式**: 支持 Claude 的 extended thinking 功能
- **工具调用**: 完整支持 function calling / tool use
- **WebSearch**: 内置 WebSearch 工具转换逻辑
- **多模型支持**: 支持 Sonnet、Opus、Haiku 系列模型
- **Admin 管理**: 可选的 Web 管理界面和 API，支持凭据管理、余额查询等
- **多级 Region 配置**: 支持全局和凭据级别的 Auth Region / API Region 配置
- **凭据级代理**: 支持为每个凭据单独配置 HTTP/SOCKS5 代理，优先级：凭据代理 > 全局代理 > 无代理

---

- [开始](#开始)
  - [1. 编译](#1-编译)
  - [2. 最小配置](#2-最小配置)
  - [3. 启动](#3-启动)
  - [4. 验证](#4-验证)
  - [Docker](#docker)
- [前端开发预览](docs/frontend-dev-environment.md)
- [配置详解](#配置详解)
  - [config.json](#configjson)
  - [credentials.json](#credentialsjson)
  - [Region 配置](#region-配置)
  - [代理配置](#代理配置)
  - [认证方式](#认证方式)
  - [环境变量](#环境变量)
- [API 端点](#api-端点)
  - [标准端点 (/v1)](#标准端点-v1)
  - [Claude Code 兼容端点 (/cc/v1)](#claude-code-兼容端点-ccv1)
  - [Thinking 模式](#thinking-模式)
  - [工具调用](#工具调用)
- [本地 Claude Code CLI 测试](docs/claude-code-cli-local-testing.md)
- [模型映射](#模型映射)
- [Admin（可选）](#admin可选)
- [注意事项](#注意事项)
- [项目结构](#项目结构)
- [技术栈](#技术栈)
- [License](#license)
- [致谢](#致谢)

## 开始

### 1. 编译

> PS: 如果不想编辑可以直接前往 Release 下载二进制文件

CI、Docker 和发布构建固定使用 Node.js `22.23.0`、pnpm `11.11.0` 与 Rust `1.92.0`。

> **发布/嵌入式构建前置步骤**：从干净 checkout 构建二进制时，必须先生成新旧两套前端的 `dist`：
> ```bash
> npm install --global pnpm@11.11.0
> pnpm --dir admin-ui install --frozen-lockfile
> pnpm --dir admin-ui build
> pnpm --dir ui install --frozen-lockfile
> pnpm --dir ui build
> ```
>
> 日常前端开发不要靠重新构建 Rust 二进制看效果，直接使用 Vite 热更新入口，见 [前端开发预览](docs/frontend-dev-environment.md)。

```bash
rustup toolchain install 1.92.0
cargo +1.92.0 build --release --locked
```

默认构建同时支持 `rustls` 和 `native-tls`，运行时仍默认选择 `rustls`。仅需要 rustls 的自定义构建可以使用 `cargo +1.92.0 build --release --locked --no-default-features`；该二进制不能把 `tlsBackend` 切换为 `native-tls`。

### 构建门禁

PR、main 分支和发布 tag 共用同一套质量门禁。门禁构建 `admin-ui` 与 `ui`，检查 Rust 格式和 Clippy 告警基线，使用真实 PgSQL/Redis 分别执行默认 feature 与无默认 feature 测试，并用默认 feature 生成 release 二进制。发布 tag 必须严格等于 `v` 加 `Cargo.toml` 中的版本，例如 Cargo 版本 `0.0.101` 对应 `v0.0.101`。

本地执行存储集成测试时必须显式提供测试实例；测试会在 PgSQL 中创建临时 schema，并在 Redis 中使用随机 key prefix：

```bash
export KIRO_RS_TEST_POSTGRES_URL='postgres://user:password@127.0.0.1:5432/kiro_rs_test'
export KIRO_RS_TEST_REDIS_URL='redis://127.0.0.1:6379/0'
cargo +1.92.0 test --locked --all-targets --no-default-features
```

CI 用 [scripts/ci/clippy-baseline.json](scripts/ci/clippy-baseline.json) 锁定现有 Clippy 债务，新增或增加任何 lint/file 告警桶都会失败。清理告警后使用固定 Rust 版本执行 `node scripts/ci/check-clippy-baseline.mjs --update`，把基线同步下调。

### 2. 最小配置

创建 `config.json`：

```json
{
   "postgres": {
      "url": "postgres://kiro_rs:kiro_rs_dev_password@127.0.0.1:25432/kiro_rs"
   },
   "redis": {
      "url": "redis://127.0.0.1:26379/0"
   },
   "host": "127.0.0.1",
   "port": 8990,
   "apiKey": "sk-kiro-rs-qazWSXedcRFV123456",
   "apiKeys": [],
   "region": "us-east-1"
}
```
> PS: 如果你需要 Web 管理面板, 请注意配置 `adminApiKey`
> PgSQL 和 Redis 为必需依赖。首次启动时会把 `config.json` 和 `credentials.json` 导入 PgSQL；之后运行配置、凭据状态、Token 刷新结果、失败计数、预热状态、统计和 usage 记录都以数据库为准。会话粘性、临时冷却、本地限流、并发占用和跨实例 Token 刷新锁以 Redis 为准。

创建 `credentials.json`（从 Kiro IDE 等中获取凭证信息）：
> PS: 可以前往 Web 管理面板配置跳过本步骤
> 如果你对凭据地域有疑惑, 请查看 [Region 配置](#region-配置)

Social 认证：
```json
{
   "refreshToken": "你的刷新token",
   "expiresAt": "2025-12-31T02:32:45.144Z",
   "authMethod": "social"
}
```

IdC 认证：
```json
{
   "refreshToken": "你的刷新token",
   "expiresAt": "2025-12-31T02:32:45.144Z",
   "authMethod": "idc",
   "clientId": "你的clientId",
   "clientSecret": "你的clientSecret"
}
```

### 3. 启动

```bash
./target/release/kiro-rs
```

或指定配置文件路径：

```bash
./target/release/kiro-rs -c /path/to/config.json --credentials /path/to/credentials.json
```

本地开发时，仓库里的 `config.json` 当前监听 `127.0.0.1:9022`。这个端口只作为后端 API 使用；前端页面使用 Vite 热更新地址预览。

### 4. 验证

```bash
curl http://127.0.0.1:8990/v1/messages \
  -H "Content-Type: application/json" \
  -H "x-api-key: sk-kiro-rs-qazWSXedcRFV123456" \
  -d '{
    "model": "claude-sonnet-4-20250514",
    "max_tokens": 1024,
    "stream": true,
    "messages": [
      {"role": "user", "content": "Hello, Claude!"}
    ]
  }'
```

### Docker

完整部署请使用不会覆盖旧文件的 `docker-compose.database.yml`，它包含当前服务、PgSQL 和 Redis：

```bash
docker compose -f docker-compose.database.yml up -d
```

需要将首次导入用的 `config.json` 和 `credentials.json` 挂载到容器中，具体参见 `docker-compose.database.yml`。

使用已发布镜像部署：

```bash
docker compose -f docker-compose.deploy.yml up -d
```

部署版默认使用 `ghcr.io/2ue/kiro-rs:latest`，可通过环境变量固定版本：

```bash
KIRO_RS_VERSION=0.0.5 docker compose -f docker-compose.deploy.yml up -d
```

如需改用 Docker Hub 镜像，可通过 `KIRO_RS_IMAGE` 覆盖镜像仓库。

容器端口固定为 `8990`，请确保挂载的 `config/config.json` 中 `host` 配置为 `0.0.0.0`，否则宿主机端口映射后可能无法访问服务。

## 配置详解

### config.json

| 字段 | 类型 | 默认值 | 描述 |
|------|------|--------|------|
| `host` | string | `127.0.0.1` | 服务监听地址 |
| `port` | number | `8080` | 服务监听端口 |
| `apiKey` | string | - | 主客户端 API Key（用于调用 `/v1`、`/cc/v1`、`/ha/v1`、`/na/v1`，历史兼容字段） |
| `apiKeys` | string[] | `[]` | 额外客户端 API Key 列表；实际生效集合为 `apiKey + apiKeys` 去空去重，管理后台可新增、替换、删除 |
| `region` | string | `us-east-1` | AWS 区域 |
| `authRegion` | string | - | Auth Region（用于 Token 刷新），未配置时回退到 region |
| `apiRegion` | string | - | API Region（用于 API 请求），未配置时回退到 region |
| `kiroVersion` | string | `0.11.107` | Kiro 版本号 |
| `machineId` | string | - | 自定义机器码（64位十六进制），不定义则自动生成 |
| `systemVersion` | string | 随机 | 系统版本标识 |
| `nodeVersion` | string | `22.22.0` | Node.js 版本标识 |
| `tlsBackend` | string | `rustls` | TLS 后端：`rustls` 或 `native-tls` |
| `countTokensApiUrl` | string | - | 外部 count_tokens API 地址 |
| `countTokensApiKey` | string | - | 外部 count_tokens API 密钥 |
| `countTokensAuthType` | string | `x-api-key` | 外部 API 认证类型：`x-api-key` 或 `bearer` |
| `proxyUrl` | string | - | HTTP/SOCKS5 代理地址 |
| `proxyUsername` | string | - | 代理用户名 |
| `proxyPassword` | string | - | 代理密码 |
| `adminApiKey` | string | - | Admin API 密钥，配置后启用凭据管理 API 和 Web 管理界面 |
| `postgres.url` | string | 必填 | PgSQL 连接地址。服务启动必须能连接；首次启动可从配置文件导入运行配置和凭据 |
| `postgres.maxConnections` | number | `10` | PgSQL 连接池最大连接数 |
| `postgres.migrateOnStart` | boolean | `true` | 启动时是否自动创建/升级数据库表；生产建议保持开启 |
| `postgres.compressUsageRollupsOnStart` | boolean | `false` | 启动时是否执行历史 usage rollup 小桶压缩；生产默认关闭，建议低峰期显式开启一次或使用单独维护窗口处理 |
| `redis.url` | string | 必填 | Redis 连接地址，用于会话绑定、临时冷却、本地限流、并发 lease、跨实例 Token 刷新锁和余额缓存 |
| `redis.keyPrefix` | string | `kiro_rs:local` | Redis key 前缀，用于和同一个 Redis 中的其他业务隔离 |
| `loadBalancingMode` | string | `priority` | 负载均衡模式：`priority`（按优先级）或 `balanced`（均衡分配） |
| `credentialRpm` | number/null | `null` | 单凭据本地 RPM 限速；`null` 或 `0` 表示关闭。开启后会优先分流到其他可用凭据 |
| `credentialMaxConcurrentRequests` | number | `0` | 单凭据最大并发请求数；`0` 表示不限制。开启后同一凭据达到并发上限时，新请求会优先换其他可用凭据 |
| `credentialTransientCooldownSecs` | number | `10` | 上游 408/429/5xx 且没有 Retry-After 时的凭据临时冷却秒数；只有存在其他可用凭据时才冷却当前凭据 |
| `credentialMaxCooldownSecs` | number | `300` | 单凭据临时冷却上限，用于限制 Retry-After 的影响范围 |
| `credentialDispatchMaxWaitSecs` | number | `120` | 单个请求等待凭据可调度的最长秒数；`0` 表示不限制。超时后返回本地调度限流错误，避免请求长期挂起 |
| `credentialInFlightLeaseMaxSecs` | number | `900` | 单个并发占用超过多久未活跃时自动释放；`0` 表示关闭。用于兜底异常路径导致的并发槽长期占用 |
| `credentialWarmupRequests` | number | `3` | 新增凭据默认预热次数；预热只通过真实业务请求成功递减，不伪造 success_count |
| `credentialWarmupSelectionPercent` | number | `5` | balanced 模式下预热凭据参与真实业务请求调度的概率百分比 |
| `compression.enabled` | boolean | `false` | 是否启用上游请求压缩；默认关闭 |
| `compression.whitespaceCompression` | boolean | `true` | 启用 compression 后是否只做 JSON whitespace 压缩；默认只开启该低风险压缩 |
| `payloadGuardEnabled` | boolean | `true` | 是否启用发送 Kiro 上游前的最终 payload 防护 |
| `payloadGuardMode` | string | `on_too_long` | payload 防护触发模式。`on_too_long` 首次请求只做协议修复、不按大小预裁剪；仅在上游返回输入过长/请求体过大后按预算裁剪并重试一次。`preemptive` 保持旧行为，发送前超过预算即裁剪 |
| `payloadGuardMaxBytes` | number | `460800` | 本地 payload 经验预算，按最终发送的 Kiro JSON body 字节数计算；它不是模型上下文上限。`0` 表示不按大小整形或裁剪，但仍执行 payload 协议修复 |
| `payloadGuardTrimHistory` | boolean | `true` | payload 超出本地预算时是否允许裁剪最旧历史；关闭后只做协议修复，仍超预算会标记后透传给 Kiro |
| `payloadShaping.enabled` | boolean | `true` | 超出本地预算时是否先执行低风险内容整形 |
| `payloadShaping.truncateHistoricalToolResults` | boolean | `true` | 是否对历史 `tool_result` 做头尾保留截断；当前合法 `tool_result` 不受影响 |
| `payloadShaping.historicalToolResultMaxChars` | number | `8000` | 单个普通历史 `tool_result` 最多保留字符数 |
| `payloadShaping.discardHistoricalThinking` | boolean | `true` | 是否移除旧 assistant 历史中的 `<thinking>` 块 |
| `payloadShaping.compressToolDefinitions` | boolean | `true` | 是否压缩工具描述和 JSON Schema 注释字段；不会删除 `type`、`properties`、`required`、`enum` |
| `payloadShaping.toolDefinitionsBudgetBytes` | number | `20000` | 当前 tools 定义超过多少 JSON 字节后开始压缩描述和注释；`0` 表示关闭工具定义预算压缩 |
| `payloadShaping.webFetchTrimEnabled` | boolean | `true` | 是否对历史 WebFetch 内容移除 data image、重复行和明显噪声 |
| `payloadShaping.webFetchBodyMaxChars` | number | `12000` | 历史 WebFetch 正文去噪后的字符预算 |
| `payloadShaping.fitCurrentPayloadToBudget` | boolean | `false` | 是否在历史裁剪后仍超预算时自动启用当前 tool_result、当前文本、当前 document 和当前图片兜底裁剪 |
| `payloadShaping.truncateCurrentToolResults` | boolean | `false` | 是否允许在仍超预算时截断当前合法 `tool_result`；默认关闭 |
| `payloadShaping.currentToolResultMaxChars` | number | `80000` | 单个当前 `tool_result` 的头尾保留字符预算 |
| `payloadShaping.truncateCurrentUserContent` | boolean | `false` | 是否允许在仍超预算时截断当前 user content；包含 document 标签时会保留文档块并只裁文档外侧文本 |
| `payloadShaping.currentUserContentMaxChars` | number | `120000` | 当前纯文本 user content 的头尾保留字符预算 |
| `payloadShaping.truncateCurrentDocuments` | boolean | `false` | 是否允许在仍超预算时截断当前 `<document>` 块正文，并保留 document 标签 |
| `payloadShaping.currentDocumentMaxChars` | number | `80000` | 单个当前 document 正文的头尾保留字符预算 |
| `payloadShaping.truncateCurrentImages` | boolean | `false` | 是否允许在仍超预算时丢弃当前图片；图片不会本地重编码压缩 |
| `payloadShaping.currentImagesMaxBytes` | number | `180000` | 当前 images 数组允许保留的 JSON 字节预算 |
| `payloadShaping.oversizedImageHandling` | string | `drop-with-placeholder` | 单张图片超过上游 5 MB 限制时的处理方式：`drop-with-placeholder` 移除图片并给模型占位说明；`reject` 直接返回 400 |
| `compatProfile` | string | `claude-code` | 兼容 profile：`claude-code` 优先真实 Claude Code CLI 可用性；`anthropic-strict` 减少代理改写和调试特征；`debug` 等同 `claude-code` 但默认暴露代理 warning |
| `kiroAgentModeStrategy` | string | `vibe` | Kiro IDE `x-amzn-kiro-agent-mode` 策略：`vibe` 保持当前成功链路，`spec` 强制规格模式，`auto` 按账号协议自动判定 |
| `extractThinking` | boolean | `true` | 非流式响应的 thinking 块提取。启用后 `<thinking>` 标签会被解析为独立的 `thinking` 内容块 |
| `promptCacheTargetReadRatio` | number | `0.98` | `/v1/messages`、`/cc/v1/messages`、`/ha/v1/messages` high-cache 的目标 cache read 中心比例；`/na/v1/messages` 默认是 no-cache，不进入本地缓存模拟 |
| `promptCacheTokenScale` | number | `1.6` | `/v1/messages`、`/cc/v1/messages`、`/ha/v1/messages` high-cache 模拟专用的 total input 放大倍数，只影响本地模拟 cache usage |
| `promptCacheMaxSimulatedInputTokens` | number | `300000` | `/v1/messages`、`/cc/v1/messages`、`/ha/v1/messages` high-cache 模拟 total input 的上限；触顶时会做确定性 soft-cap 抖动 |
| `promptCacheCapJitterMinTokens` | number | `12000` | high-cache 触顶 soft-cap 的最小扣减 token |
| `promptCacheCapJitterMaxTokens` | number | `24000` | high-cache 触顶 soft-cap 的最大扣减 token |
| `promptCacheScaleMinInputTokens` | number | `20000` | 基础输入达到该门槛后才启用 high-cache token scale，避免短测试请求被放大 |
| `reportedUsage.default` | object | input/output 原始值，cache read/write 保留计算值 | 控制所有路径的默认 usage 上报方式，只影响响应和后台 usage record，不影响 reader 计算、本地缓存 tracker 或上游请求 |
| `reportedUsage.pathOverrides` | object | `/cc`、`/ha` | 按路径前缀独立覆盖默认 usage 上报策略，最长前缀优先；例如 `/cc` 会匹配 `/cc/v1/messages`，`/ha` 不会继承 `/cc` 的 writer 配置 |
| `reportedUsage.*.finalCacheReadMaxTokens` | number | `700000` | 每个路径策略最终上报的 `cache_read_input_tokens` 上限；在 input 差值转入 cache read 后执行，0 表示关闭 |
| `reportedUsage.*.finalCacheReadJitterMinTokens` | number | `0` | 最终读取缓存上限的确定性扣减下限 |
| `reportedUsage.*.finalCacheReadJitterMaxTokens` | number | `0` | 最终读取缓存上限的确定性扣减上限 |
| `reportedUsage.*.finalOutputGuardEnabled` | boolean | `true` | 是否启用最终输出限制。关闭后不执行 output 百分比放大和最终上限裁剪，只保留 `output` 字段自身的改写结果 |
| `reportedUsage.*.outputUpliftMinTokens` | number | `1000` | `output_tokens` 完成字段改写后，大于该阈值才进入百分比放大；等于阈值不放大 |
| `reportedUsage.*.outputUpliftPercent` | number | `50` | `output_tokens` 超过阈值后的放大百分比；0 表示关闭放大，最大 200 |
| `reportedUsage.*.finalOutputMaxTokens` | number | `200000` | 放大后的 `output_tokens` 最终上限；0 表示关闭最终上限 |
| `reportedUsage.*.finalOutputJitterMinTokens` | number | `5000` | 最终输出上限的确定性扣减下限 |
| `reportedUsage.*.finalOutputJitterMaxTokens` | number | `12000` | 最终输出上限的确定性扣减上限，避免稳定撞到模型或展示硬上限 |
| `usageRecordLimit` | number | `5000` | 内存中保留的最近 usage 记录数量；完整 usage 记录写入 PgSQL |
| `highCacheThreshold` | number | `10000` | Admin 统计高缓存请求的 cache read 阈值 |
| `defaultEndpoint` | string | `ide` | 默认 Kiro 端点。凭据未显式指定 `endpoint` 时使用。当前支持：`ide`、`cli` |
| `exposeProxyWarnings` | boolean | `false` | 是否通过 `x-kiro-rs-warnings` 暴露代理侧兜底改写。`anthropic-strict` 下会强制关闭 |

最小配置示例：

```json
{
   "postgres": {
      "url": "postgres://kiro_rs:kiro_rs_dev_password@127.0.0.1:25432/kiro_rs"
   },
   "redis": {
      "url": "redis://127.0.0.1:26379/0"
   },
   "host": "127.0.0.1",
   "port": 8990,
   "apiKey": "sk-kiro-rs-qazWSXedcRFV123456",
   "apiKeys": [],
   "adminApiKey": "sk-admin-your-secret-key",
   "payloadGuardEnabled": true,
   "payloadGuardMode": "on_too_long",
   "payloadGuardMaxBytes": 460800,
   "payloadGuardTrimHistory": true,
   "payloadShaping": {
      "enabled": true,
      "truncateHistoricalToolResults": true,
      "historicalToolResultMaxChars": 8000,
      "discardHistoricalThinking": true,
      "compressToolDefinitions": true,
      "toolDefinitionsBudgetBytes": 20000,
      "webFetchTrimEnabled": true,
      "webFetchBodyMaxChars": 12000,
      "fitCurrentPayloadToBudget": false,
      "truncateCurrentToolResults": false,
      "currentToolResultMaxChars": 80000,
      "truncateCurrentUserContent": false,
      "currentUserContentMaxChars": 120000,
      "truncateCurrentDocuments": false,
      "currentDocumentMaxChars": 80000,
      "truncateCurrentImages": false,
      "currentImagesMaxBytes": 180000,
      "oversizedImageHandling": "drop-with-placeholder"
   }
}
```

未写出的字段会使用内置默认值。首次启动导入 PgSQL 后，也可以在后台配置页热更新调度、payload 防护、本地模拟缓存和路径级 usage 上报策略。

`payloadShaping` 默认不会截断当前 user message、当前合法 `tool_result`、当前 PDF/document 或当前图片。如果显式打开 `fitCurrentPayloadToBudget` 或具体当前内容截断项，服务会在历史整形和旧历史裁剪后仍超出 `payloadGuardMaxBytes` 时，按最终序列化后的 Kiro JSON body 字节数循环收缩当前内容，直到低于配置预算或没有可继续处理的内容。若仍超出预算，服务会记录 `still_oversized=true` 并继续请求 Kiro，让上游返回真实错误。

缓存模式由路径固定选择：

- `/v1/messages`：high-cache。即使请求没有显式 `cache_control`，也会按稳定前缀建立本地缓存；如果上游 metadata 返回的 cache read/write 都是 0，会用本地缓存 usage 补足 cache 字段。
- `/cc/v1/messages`：high-cache，与 `/v1/messages` 使用同一套底层缓存模拟；默认只通过 `reportedUsage.pathOverrides["/cc"]` 改写下游 input 和 cache write 上报。
- `/ha/v1/messages`：high-cache，与 `/v1/messages` 使用同一套底层缓存模拟；默认只通过 `reportedUsage.pathOverrides["/ha"]` 改写下游 input 上报。后续如果要改 writer，需要单独改 `/ha` 覆盖项。
- `/na/v1/messages`：no-cache 路由；不进入本地缓存模拟，响应和后台记录直接使用原始 usage。

本地模拟缓存按实际解析后的上游模型判断 prompt cache 能力和最小缓存长度。Anthropic prompt caching 支持 active Claude 模型；Haiku 不是无缓存模型，但 Haiku 4.5 的最小可缓存长度是 4096 tokens，Haiku 3.5 是 2048 tokens。低于模型最小长度时本地不会模拟 cache creation/read。

路径级 usage 上报策略支持这些字段：

- `input`：控制 `input_tokens`。使用 `sample-max` 时会采样到 `maxTokens` 以内；`moveDeltaToCacheRead` 为 true 时，减少的 input 差值会加入 `cache_read_input_tokens`。
- `output`：控制 `output_tokens`。默认建议 `raw`，也就是直接使用上游返回的原始输出。
- `cacheRead`：控制 `cache_read_input_tokens`。默认建议 `preserve`。
- `cacheCreation`：控制 `cache_creation_input_tokens`。`/cc` 默认使用 `sample-target`，`targetTokens` 为 `3000`，`normalMaxMultiplier` 为 `1.2`。

每个路径策略还会在所有字段改写完成后应用 `finalCacheReadMaxTokens`，默认把最终 `cache_read_input_tokens` 限制在 `700000` 以内。`finalCacheReadJitterMinTokens` / `finalCacheReadJitterMaxTokens` 可让触顶值在上限以下确定性波动；守护只会向下裁剪，不会抬高原本较小的读取缓存值。

### credentials.json

支持单对象格式（向后兼容）或数组格式（多凭据）。

#### 字段说明

| 字段             | 类型     | 描述                                          |
|----------------|--------|---------------------------------------------|
| `id`           | number | 凭据唯一 ID（可选，仅用于 Admin API 管理；手写文件可不填）        |
| `accessToken`  | string | OAuth 访问令牌（可选，可自动刷新）                        |
| `refreshToken` | string | OAuth 刷新令牌                                  |
| `profileArn`   | string | AWS Profile ARN（可选，登录时返回）                   |
| `expiresAt`    | string | Token 过期时间 (RFC3339)                        |
| `authMethod`   | string | 认证方式：`social` 或 `idc`                       |
| `clientId`     | string | IdC 登录的客户端 ID（IdC 认证必填）                     |
| `clientSecret` | string | IdC 登录的客户端密钥（IdC 认证必填）                      |
| `priority`     | number | 凭据优先级，数字越小越优先，默认为 0                         |
| `region`       | string | 凭据级 Auth Region, 兼容字段                       |
| `authRegion`   | string | 凭据级 Auth Region，用于 Token 刷新, 未配置时回退到 region |
| `apiRegion`    | string | 凭据级 API Region，用于 API 请求                    |
| `machineId`    | string | 凭据级机器码（64位十六进制）                             |
| `email`        | string | 用户邮箱（可选，从 API 获取）                           |
| `proxyUrl`     | string | 凭据级代理 URL（可选，特殊值 `direct` 表示不使用代理）       |
| `proxyUsername`| string | 凭据级代理用户名（可选）                                |
| `proxyPassword`| string | 凭据级代理密码（可选）                                 |
| `endpoint`     | string | 凭据级端点名称（可选，未配置时使用 `config.defaultEndpoint`）|

说明：
- IdC / Builder-ID / IAM 在本项目里属于同一种登录方式，配置时统一使用 `authMethod: "idc"`
- 为兼容旧配置，`builder-id` / `iam` 仍可被识别，但会按 `idc` 处理

#### 单凭据格式（旧格式，向后兼容）

```json
{
   "accessToken": "请求token，一般有效期一小时，可选",
   "refreshToken": "刷新token，一般有效期7-30天不等",
   "profileArn": "arn:aws:codewhisperer:us-east-1:111112222233:profile/QWER1QAZSDFGH",
   "expiresAt": "2025-12-31T02:32:45.144Z",
   "authMethod": "social",
   "clientId": "IdC 登录需要",
   "clientSecret": "IdC 登录需要"
}
```

#### 多凭据格式（支持故障转移和自动回写）

```json
[
   {
      "refreshToken": "第一个凭据的刷新token",
      "expiresAt": "2025-12-31T02:32:45.144Z",
      "authMethod": "social",
      "priority": 0
   },
   {
      "refreshToken": "第二个凭据的刷新token",
      "expiresAt": "2025-12-31T02:32:45.144Z",
      "authMethod": "idc",
      "clientId": "xxxxxxxxx",
      "clientSecret": "xxxxxxxxx",
      "region": "us-east-2",
      "priority": 1,
      "proxyUrl": "socks5://proxy.example.com:1080",
      "proxyUsername": "user",
      "proxyPassword": "pass"
   },
   {
      "refreshToken": "第三个凭据（显式不走代理）",
      "expiresAt": "2025-12-31T02:32:45.144Z",
      "authMethod": "social",
      "priority": 2,
      "proxyUrl": "direct"
   }
]
```

多凭据特性：
- 按 `priority` 字段排序，数字越小优先级越高（默认为 0）
- 单凭据最多重试 3 次，单请求最多重试 9 次
- 自动故障转移到下一个可用凭据
- 多凭据格式下 Token 刷新后自动回写到源文件

### Region 配置

支持多级 Region 配置，分别控制 Token 刷新和 API 请求使用的区域。

**Auth Region**（Token 刷新）优先级：
`凭据.authRegion` > `凭据.region` > `config.authRegion` > `config.region`

**API Region**（API 请求）优先级：
`凭据.apiRegion` > `config.apiRegion` > `config.region`

### 代理配置

支持全局代理和凭据级代理，凭据级代理会覆盖该凭据产生的所有出站连接（API 请求、Token 刷新、额度查询）。

**代理优先级**：`凭据.proxyUrl` > `config.proxyUrl` > 无代理

| 凭据 `proxyUrl` 值 | 行为 |
|---|---|
| 具体 URL（如 `http://proxy:8080`、`socks5://proxy:1080`） | 使用凭据指定的代理 |
| `direct` | 显式不使用代理（即使全局配置了代理） |
| 未配置（留空） | 回退到全局代理配置 |

凭据级代理示例：

```json
[
   {
      "refreshToken": "凭据A：使用自己的代理",
      "authMethod": "social",
      "proxyUrl": "socks5://proxy-a.example.com:1080",
      "proxyUsername": "user_a",
      "proxyPassword": "pass_a"
   },
   {
      "refreshToken": "凭据B：显式不走代理（直连）",
      "authMethod": "social",
      "proxyUrl": "direct"
   },
   {
      "refreshToken": "凭据C：使用全局代理（或直连，取决于 config.json）",
      "authMethod": "social"
   }
]
```

### 认证方式

客户端请求本服务时，支持两种认证方式：

1. **x-api-key Header**
   ```
   x-api-key: sk-your-api-key
   ```

2. **Authorization Bearer**
   ```
   Authorization: Bearer sk-your-api-key
   ```

### 环境变量

服务运行时主要使用配置文件和启动参数。以下环境变量可选：

```bash
RUST_LOG=debug ./target/release/kiro-rs
```

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `RUST_LOG` | `info` | 日志级别，例如 `debug` / `info` |
| `KIRO_API_KEY` | - | 自动导入一个最高优先级的 Kiro API Key 凭据并写入 PgSQL，可用于不准备 `credentials.json` 的场景 |
| `KIRO_RS_IMAGE` | `ghcr.io/2ue/kiro-rs` | `docker-compose.deploy.yml` 使用的镜像仓库 |
| `KIRO_RS_VERSION` | `latest` | `docker-compose.deploy.yml` 使用的镜像 tag |
| `KIRO_RS_PORT` | `8990` | Docker 部署时映射到宿主机的端口 |
| `KIRO_ADMIN_UI_MODE` | debug: `redirect`; release: `embedded` | 旧版 `/admin` 的服务模式：`embedded` / `redirect` / `proxy` / `filesystem` / `disabled` |
| `KIRO_ADMIN_UI_DIR` | `admin-ui/dist` | `/admin` 使用 `filesystem` 模式时读取的构建目录 |
| `KIRO_ADMIN_UI_DEV_SERVER` | debug: `http://127.0.0.1:9025/admin` | `/admin` 使用 `redirect` 或 `proxy` 时指向的 Vite 服务 |
| `KIRO_NEW_UI_MODE` / `KIRO_UI_MODE` | debug: `redirect`; release: `embedded` | 新版 `/ui` 的服务模式：`embedded` / `redirect` / `proxy` / `filesystem` / `disabled` |
| `KIRO_NEW_UI_DIR` / `KIRO_UI_DIR` | `ui/dist` | `/ui` 使用 `filesystem` 模式时读取的构建目录 |
| `KIRO_NEW_UI_DEV_SERVER` / `KIRO_UI_DEV_SERVER` | debug: `http://127.0.0.1:9023/ui` | `/ui` 使用 `redirect` 或 `proxy` 时指向的 Vite 服务 |

生产默认使用 `embedded`，前端构建产物编进后端二进制，部署仍是单服务。debug 构建默认不嵌入前端 dist，后端 `/admin` 和 `/ui` 会重定向到对应 Vite 服务；开发环境统一使用 Vite 热更新，前端通过 `/api` 代理到后端 API。

本地开发预览地址：

| 前端 | 命令 | 浏览器地址 | 用途 |
|------|------|------------|------|
| 旧版 Admin UI | `bash scripts/dev-ui.sh admin` | `http://127.0.0.1:9025/admin/` | 旧版管理入口 |
| 新版 UI | `bash scripts/dev-ui.sh ui` | `http://127.0.0.1:9023/ui/runtime` | 当前主要开发入口 |

后端 API 示例：

```bash
# 当前本地 config.json 监听 9022；config.example.json 的默认示例是 8990
./target/release/kiro-rs -c config.json --credentials credentials.json
```

前端默认把 `/api` 代理到 `http://127.0.0.1:9022`。如果后端不是 9022，用 `VITE_API_PROXY_TARGET` 覆盖：

```bash
VITE_API_PROXY_TARGET=http://127.0.0.1:8990 bash scripts/dev-ui.sh ui
```

日常前端开发可以直接打开上表里的 Vite 地址；如果访问 debug 后端的 `/admin` 或 `/ui`，后端也会自动重定向到对应 Vite 地址。release 二进制仍默认使用 embedded 页面。

## API 端点

### 标准端点 (/v1)

| 端点 | 方法 | 描述 |
|------|------|------|
| `/v1/models` | GET | 获取可用模型列表 |
| `/v1/messages` | POST | 创建消息（对话，固定 high-cache 本地 usage 模拟） |
| `/v1/messages/count_tokens` | POST | 估算 Token 数量 |

### No-Cache 端点 (/na/v1)

| 端点 | 方法 | 描述 |
|------|------|------|
| `/na/v1/models` | GET | 获取可用模型列表 |
| `/na/v1/messages` | POST | 创建消息（对话；默认不进入本地缓存模拟，直接使用原始 usage） |
| `/na/v1/messages/count_tokens` | POST | 估算 Token 数量 |

### Claude Code 兼容端点 (/cc/v1)

| 端点 | 方法 | 描述 |
|------|------|------|
| `/cc/v1/models` | GET | 获取可用模型列表 |
| `/cc/v1/messages` | POST | 创建消息（实时流式返回，最终 `message_delta.usage` 修正 token 用量） |
| `/cc/v1/messages/count_tokens` | POST | 估算 Token 数量（与 `/v1` 相同） |

> **`/cc/v1/messages` 与 `/v1/messages` 的区别**：
> - `/v1/messages`：实时流式返回，`message_start` 中的 `input_tokens` 是估算值
> - `/cc/v1/messages`：实时流式返回，`message_start` 先给估算用量；最终 `message_delta.usage` 会优先使用上游 metadata，缺失时用 `contextUsageEvent` 和模型输入窗口换算后修正
> - 上游长时间无内容时仍会发送 `ping` 事件保活

### Thinking 模式

支持 Claude 的 extended thinking 功能：

```json
{
  "model": "claude-sonnet-4-20250514",
  "max_tokens": 16000,
  "thinking": {
    "type": "enabled",
    "budget_tokens": 10000
  },
  "messages": [...]
}
```

### 工具调用

完整支持 Anthropic 的 tool use 功能：

```json
{
  "model": "claude-sonnet-4-20250514",
  "max_tokens": 1024,
  "tools": [
    {
      "name": "get_weather",
      "description": "获取指定城市的天气",
      "input_schema": {
        "type": "object",
        "properties": {
          "city": {"type": "string"}
        },
        "required": ["city"]
      }
    }
  ],
  "messages": [...]
}
```

## 模型映射

| Anthropic 模型 | Kiro 模型 |
|----------------|-----------|
| `*sonnet*` | `claude-sonnet-4.5` |
| `*opus*`（含 4.5/4-5） | `claude-opus-4.5` |
| `*opus*`（其他） | `claude-opus-4.6` |
| `*haiku*` | `claude-haiku-4.5` |

## Admin（可选）

当 `config.json` 配置了非空 `adminApiKey` 时，会启用：

- **Admin API（认证同 API Key）**
  - `GET /api/admin/credentials` - 获取所有凭据状态
  - `POST /api/admin/credentials` - 添加新凭据
  - `DELETE /api/admin/credentials/:id` - 删除凭据
  - `POST /api/admin/credentials/:id/disabled` - 设置凭据禁用状态
  - `POST /api/admin/credentials/:id/priority` - 设置凭据优先级
  - `POST /api/admin/credentials/:id/warmup` - 设置凭据预热次数
  - `POST /api/admin/credentials/:id/in-flight/clear` - 清理凭据并发占用
  - `POST /api/admin/credentials/:id/reset` - 重置失败计数
  - `POST /api/admin/credentials/:id/refresh` - 强制刷新 Token
  - `GET /api/admin/credentials/:id/balance` - 获取凭据余额
  - `POST /api/admin/credentials/:id/test` - 测试指定凭据的模型调用
  - `GET /api/admin/usage-records-paged` - 分页查询 Usage 记录
  - `POST /api/admin/usage-records/clear` - 软删除当前 Usage 展示记录
  - `GET /api/admin/model-pricing` - 获取模型价格目录
  - `POST /api/admin/model-pricing/sync` - 手动同步模型价格目录
  - `GET /api/admin/audit-logs` - 分页查询后台审计日志

- **Admin UI**
  - `GET /admin` - 访问旧版管理页面。日常开发看 Vite 地址；该路由默认服务 embedded 发布产物。
  - `GET /ui` - 访问新版管理页面。日常开发看 Vite 地址；该路由默认服务 embedded 发布产物。

## 注意事项

1. **凭证安全**: 请妥善保管首次导入用的 `credentials.json` 和 PgSQL 数据库，不要提交到版本控制
2. **Token 刷新**: 服务会自动刷新过期的 Token，无需手动干预
3. **WebSearch 工具**: 当 `tools` 列表仅包含一个 `web_search` 工具时，会走内置 WebSearch 转换逻辑

## 项目结构

```
kiro-rs/
├── src/
│   ├── main.rs                 # 程序入口
│   ├── http_client.rs          # HTTP 客户端构建
│   ├── token.rs                # Token 计算模块
│   ├── debug.rs                # 调试工具
│   ├── test.rs                 # 测试
│   ├── model/                  # 配置和参数模型
│   │   ├── config.rs           # 应用配置
│   │   └── arg.rs              # 命令行参数
│   ├── anthropic/              # Anthropic API 兼容层
│   │   ├── router.rs           # 路由配置
│   │   ├── handlers.rs         # 请求处理器
│   │   ├── middleware.rs       # 认证中间件
│   │   ├── types.rs            # 类型定义
│   │   ├── converter.rs        # 协议转换器
│   │   ├── stream.rs           # 流式响应处理
│   │   └── websearch.rs        # WebSearch 工具处理
│   ├── kiro/                   # Kiro API 客户端
│   │   ├── provider.rs         # API 提供者
│   │   ├── token_manager.rs    # Token 管理
│   │   ├── machine_id.rs       # 设备指纹生成
│   │   ├── model/              # 数据模型
│   │   │   ├── credentials.rs  # OAuth 凭证
│   │   │   ├── events/         # 响应事件类型
│   │   │   ├── requests/       # 请求类型
│   │   │   ├── common/         # 共享类型
│   │   │   ├── token_refresh.rs # Token 刷新模型
│   │   │   └── usage_limits.rs # 使用额度模型
│   │   └── parser/             # AWS Event Stream 解析器
│   │       ├── decoder.rs      # 流式解码器
│   │       ├── frame.rs        # 帧解析
│   │       ├── header.rs       # 头部解析
│   │       ├── error.rs        # 错误类型
│   │       └── crc.rs          # CRC 校验
│   ├── admin/                  # Admin API 模块
│   │   ├── router.rs           # 路由配置
│   │   ├── handlers.rs         # 请求处理器
│   │   ├── service.rs          # 业务逻辑服务
│   │   ├── types.rs            # 类型定义
│   │   ├── middleware.rs       # 认证中间件
│   │   └── error.rs            # 错误处理
│   ├── admin_ui/               # Admin UI 静态文件服务
│   │   └── router.rs           # embedded / redirect / proxy / filesystem 路由
│   ├── storage/                # PgSQL 和 Redis 存储
│   │   ├── postgres.rs         # PgSQL 表结构和读写
│   │   └── redis_cache.rs      # Redis 缓存和调度运行态
│   └── common/                 # 公共模块
│       └── auth.rs             # 认证工具函数
├── ui/                         # UI 前端工程（开发用 Vite，发布产物可嵌入二进制）
├── tools/                      # 辅助工具
├── Cargo.toml                  # 项目配置
├── config.example.json         # 配置示例
├── docker-compose.yml          # 旧版单服务 Docker Compose 配置
├── docker-compose.local-infra.yml # 本地 PgSQL/Redis 测试依赖
├── docker-compose.database.yml # 服务 + PgSQL + Redis 部署配置
└── Dockerfile                  # Docker 构建文件
```

## 技术栈

- **Web 框架**: [Axum](https://github.com/tokio-rs/axum) 0.8
- **异步运行时**: [Tokio](https://tokio.rs/)
- **HTTP 客户端**: [Reqwest](https://github.com/seanmonstar/reqwest)
- **序列化**: [Serde](https://serde.rs/)
- **日志**: [tracing](https://github.com/tokio-rs/tracing)
- **命令行**: [Clap](https://github.com/clap-rs/clap)
- **数据库**: [SQLx](https://github.com/launchbadge/sqlx) + PostgreSQL
- **缓存**: [Redis](https://redis.io/)

## License

MIT

## 致谢

本项目的实现离不开前辈的努力:  
 - [kiro2api](https://github.com/caidaoli/kiro2api)
 - [proxycast](https://github.com/aiclientproxy/proxycast)

本项目部分逻辑参考了以上的项目, 再次由衷的感谢!
