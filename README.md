# claude-pool

Claude Code 协议（Anthropic Messages）账号池调度网关。账号通过 `baseUrl + key` 接入，路由决定账号范围、模型限制、usage 整形和可选请求处理策略。

## 功能特性

- **账号池调度**：优先级、并发 lease、容量排队、失败换号、首输出前重试、冷却、自动禁用与可选质量评分。
- **统一路由**：最长前缀匹配；路由和账号双向放行；路由策略与账号传输覆盖分层配置。
- **六种 usage 策略**：`no_cache`、`current_high_cache`、`prefix_tool`、`stable_segment_cache`、`prefix_tool_plus`、`bounded_high_cache`；记录 raw / shaped / reported 用量与成本。
- **模型限制与映射**：目录管理、账号模型发现、路由模型规则、账号映射；模型目录只展示 `listed` 且路由可用的条目。
- **下游密钥与额度**：独立密钥、允许路由、RPM、并发、排队和用量额度。
- **管理控制台**：账号、路由、策略模板、模型、用量、风险、运行配置、密钥与审计。
- **多实例协调**：PostgreSQL 保存配置和记录，Redis 协调 lease、运行态与热加载；运行中协调服务异常时可使用进程内 lease。
- **配置迁移**：管理 API 导出导入新系统配置包；CLI 从旧版网关数据库导出、预览迁移并导入。见[旧版迁移](#从旧版网关迁移)。

协议默认透传。模型映射、鉴权、必要 beta 兼容、usage 改写和错误脱敏按系统契约执行；缺省参数补全、thinking 处理、历史清洗、图片处理、payload 裁剪等功能需要显式开启。

## 快速开始

### 编译

工具链：Rust **1.92.0**、Node.js **22.23.0**、pnpm **11.11.0**。发布二进制嵌入 `ui/dist`，先构建前端：

```bash
pnpm --dir ui install --frozen-lockfile
pnpm --dir ui check
pnpm --dir ui build
cargo build --release --locked --bin claude-pool
```

默认产物位于 `target/release/claude-pool`；如果本地 `.cargo/config.toml` 配置了共享 target，使用该目录下的 `release/claude-pool`。下文用 `cargo run --release --locked --bin claude-pool --` 启动，兼容两种目录布局。发布前核对 CLI 版本，预期输出 `claude-pool 1.0.0`：

```bash
cargo run --release --locked --bin claude-pool -- --version
```

### 准备数据库与最小配置

服务启动需要 PostgreSQL 和 Redis；可用仓库的本地依赖编排启动：

```bash
docker compose -f docker-compose.local-infra.yml up -d
```

在仓库根新建 `config.json`：

```json
{
  "host": "127.0.0.1",
  "port": 8990,
  "postgres": {
    "url": "postgres://claude_pool:claude_pool_dev_password@127.0.0.1:25432/claude_pool"
  },
  "redis": { "url": "redis://127.0.0.1:26379/0" },
  "apiKey": "sk-downstream-change-me",
  "adminApiKey": "sk-admin-change-me"
}
```

更完整的示例见 [config.example.json](config.example.json)。首次启动把文件中的运行配置写入 PostgreSQL，后续在控制台或管理 API 修改；修改文件不会覆盖数据库已有运行配置。监听地址及存储连接可用部署环境变量覆盖。

### 启动并接入账号

```bash
cargo run --release --locked --bin claude-pool -- -c config.json
```

打开 `http://127.0.0.1:8990/ui`，使用 `adminApiKey` 登录，在账号页填写上游地址和密钥。也可通过管理 API 创建：

```bash
curl --fail-with-body http://127.0.0.1:8990/api/admin/accounts \
  -H 'content-type: application/json' \
  -H 'x-api-key: sk-admin-change-me' \
  -d '{
    "name": "primary",
    "baseUrl": "https://api.anthropic.com",
    "apiKey": "replace-with-upstream-key",
    "authType": "x_api_key",
    "supportedModels": ["claude-sonnet-4-5"],
    "maxConcurrentRequests": 10
  }'
```

`baseUrl` 末尾是 `/v1` 时追加 `/messages`，否则追加 `/v1/messages`。支持 `bearer` 和 `x_api_key` 鉴权；按上游要求选择。账号保存在数据库中，`config.json` 的 `accounts` 对象是调度默认值。

### 验证

```bash
curl --fail-with-body http://127.0.0.1:8990/healthz
curl --fail-with-body http://127.0.0.1:8990/readyz
curl --fail-with-body http://127.0.0.1:8990/v1/messages \
  -H 'content-type: application/json' \
  -H 'anthropic-version: 2023-06-01' \
  -H 'x-api-key: sk-downstream-change-me' \
  -d '{
    "model": "claude-sonnet-4-5",
    "max_tokens": 128,
    "messages": [{"role": "user", "content": "回复 ok"}]
  }'
```

模型名应与接入账号实际支持的模型一致。Claude Code 的 `ANTHROPIC_BASE_URL` 可设置为 `http://127.0.0.1:8990/cc`，鉴权使用下游密钥。

### Docker

把配置放在 `config/config.json`；容器监听设置为 `0.0.0.0:8990`。使用完整编排时，数据库连接由 `CLAUDE_POOL_POSTGRES_URL`、`CLAUDE_POOL_REDIS_URL` 覆盖：

```bash
mkdir -p config logs
cp config.example.json config/config.json
# 编辑 config/config.json 中的下游密钥与管理密钥
export CLAUDE_POOL_POSTGRES_PASSWORD='replace-with-database-password'
export CLAUDE_POOL_VERSION=1.0.0
docker compose -f docker-compose.database.yml up -d
```

自建镜像：

```bash
docker build -t claude-pool:1.0.0 .
CLAUDE_POOL_IMAGE=claude-pool CLAUDE_POOL_VERSION=1.0.0 \
  docker compose -f docker-compose.database.yml up -d
```

## 分支与发布

`claude-pool` 维护在仓库 `2ue/kiro.rs` 的 **`claude-main`** 分支上，是一条独立主线：只单向从 `main` 合入通用修复，永不合回 `main`。版本号、tag、镜像都与 `main` 隔离：

| 项 | `claude-main` | `main`（kiro-rs） |
|---|---|---|
| 包名 / 二进制 | `claude-pool` | `kiro-rs` |
| 版本来源 | `Cargo.toml`（当前 `1.0.0`） | `Cargo.toml` |
| 发布 tag | `claude-vMAJOR.MINOR.PATCH` | `vMAJOR.MINOR.PATCH` |
| 镜像 | `ghcr.io/2ue/claude-pool`、`docker.io/<user>/claude-pool` | `ghcr.io/2ue/kiro-rs` |
| 质量门禁 | 对 `claude-main` 的 push / PR 跑全量门禁 | 对 `main` |

发布步骤（在 `claude-main` 的 worktree 内执行）：

```bash
# 1. 顺延版本号：Cargo.toml、Cargo.lock、package.json、ui/package.json 四处一致
# 2. 提交
git commit -am "chore(release): 1.0.1"
# 3. 推送分支并打 tag，tag 必须严格等于 claude-v + Cargo 版本
git push origin claude-main
git tag claude-v1.0.1 && git push origin claude-v1.0.1
```

tag 推送后 `Publish Claude Pool Docker Images` 工作流会校验 tag 与 Cargo 版本一致、运行 release 档质量门禁、构建 amd64 与 arm64 镜像并推送多架构 manifest；`latest` 只在 `claude-v*` 命名空间内按最高版本更新。

`docker-compose.yml` 和 `docker-compose.deploy.yml` 连接已有基础设施；`docker-compose.database.yml` 启动应用、PostgreSQL 和 Redis；`docker-compose.local-infra.yml` 只启动本地开发依赖。详见 [Docker 部署](docs/ai-docker-compose-deployment.md)。

## 核心概念

### 账号

一个 `baseUrl + apiKey` 上游接入点，账号记录保存在 PostgreSQL。
账号保存优先级、并发上限、支持模型和允许路由。
`overrides` 中的超时与 usage 放大参数覆盖全局账号默认值，未设置的字段继承全局。

### 路由

一个入口前缀与其完整行为配置，例如 `/cc` 或 `/dfcache/team`。
请求按最长前缀匹配，未知自定义前缀返回 404。
路由 `accountRule` 和账号 `routeRules` 都放行，账号才进入候选；模型限制同时生效。

### 策略

路由的 `cachePolicy` 和 `reportedUsage` 决定用量模拟、字段整形、写入频次、TTL 与上下限。
全局保存六种策略模板，路由选择模板并设置参数。
账号 `usageProjectionMode=pass_through` 可跳过整形；需要严格上下限时使用 `bounded_high_cache`。

### 路由功能

`features` 控制补 `max_tokens`、reasoning 兼容、thinking 名称/触发、历史清洗、payload 和图片处理。
这些请求处理默认关闭，按需在全局默认值或单条路由中显式开启。
全局默认位于 `routeFeatures.defaults`，路由通过 `routes[].features` 覆盖对应字段。

### 下游密钥

调用者使用的 key 与上游账号密钥分开管理。
密钥只控制鉴权、准入、额度和 `allowedRoutes`，限制逐层收紧。
密钥不携带 usage 策略或请求改写配置；这些行为由命中的路由决定。

完整字段与配置合并规则见 [03 领域模型与配置分层](docs/claude-pool-refactor/03-领域模型与配置分层.md)。

## 配置

### config.json 字段

下表按当前 `Config` 与 [配置示例](config.example.json) 整理。示例只显式写常用字段；未写的字段使用代码默认值。

| 字段 | 示例 / 默认 | 作用 |
|---|---|---|
| `host`、`port` | 示例 `0.0.0.0`、`8990`；代码默认 `127.0.0.1`、`8080` | 监听地址与端口 |
| `postgres.url` | 示例本地 `25432` | 配置、账号、用量、模型、审计的权威存储；启动必需 |
| `postgres.maxConnections`、`usageMaxConnections` | `10`、`4` | 业务与 usage 独立连接池 |
| `postgres.migrateOnStart`、`compressUsageRollupsOnStart` | `true`、`false` | schema 初始化与历史 rollup 压缩开关 |
| `redis.url`、`keyPrefix` | 示例本地 `26379`；前缀默认 `claude_pool:local` | 调度协调与热加载；同集群实例使用相同前缀 |
| `observabilityRedis.url`、`keyPrefix` | `null`、`claude_pool:observability` | 可选观测 Redis；必须与业务 Redis 属于不同服务进程；未配置则使用 PostgreSQL/本机观测 |
| `apiKey`、`apiKeys` | 一个主密钥及附加数组 | 下游鉴权密钥集合 |
| `requestApiKeyPolicies` | `[]` | 管理密钥对象：启用、名称、准入、额度、`allowedRoutes` |
| `adminApiKey` | 示例 `sk-admin-your-secret-key` | 非空时启用管理 API |
| `requestAdmission` | 示例 RPM `300`、并发 `32`、队列 `64`、等待 `1000ms` | 单实例下游密钥准入；各实例独立计数 |
| `tlsBackend` | `rustls` | 默认 TLS 实现 |
| `proxyUrl`、`proxyUsername`、`proxyPassword` | 未设置 | 全局出口代理；账号可覆盖代理 URL |
| `streamPreOutputHoldSecs` | `15` | 首输出前静默保留窗口，范围 `0–60`，`0` 表示持续保留到首输出 |
| `inferenceUpstreamMaxAttempts` | `4` | 单次下游请求的上游调用总预算 |
| `promptInjection` | 示例四项均关闭 | `custom`、`identityPrompt`、`languageConstraint`、`taskQuality` 的默认开关和文本；`applyToCountTokens` 控制估算时是否应用 |
| `routeFeatures.defaults` | `{}` | 可选路由功能默认参数；空对象关闭所有功能 |
| `compatProfile` | `claude-code` | 兼容配置画像 |
| `modelResolutionMode`、`modelMapping` | `compatible`、默认规则 | 内部模型解析与映射；账号的外发模型映射独立配置 |
| `promptCacheTargetReadRatio`、`promptCacheTokenScale` | `0.98`、`1.6` | 高缓存模拟的读取目标与输入倍率 |
| `promptCacheMaxSimulatedInputTokens`、`promptCacheScaleMinInputTokens` | `300000`、`20000` | 模拟上限与倍率生效门槛 |
| `promptCacheCapJitterMinTokens`、`promptCacheCapJitterMaxTokens` | `12000`、`24000` | 模拟上限的确定性浮动 |
| `promptCacheCreationControl` | 默认频次配置 | 写入频率、增量、窗口预算等 |
| `promptCacheMaxEntriesPerAccount`、`promptCacheMaxEntriesGlobal` | `200`、`20000` | 单会话 scope 与全局的前缀追踪条目上限；scope 由会话和路由命名空间组成，换号继续共享 |
| `promptCacheEntryTtlSecs`、`promptCacheEstimatedBytesLimit` | `86400`、`268435456` | 追踪条目 TTL 与估算内存限制 |
| `cachePolicy`、`reportedUsage` | 默认模板 | 全局六种策略模板和字段整形默认值；路由负责挂载 |
| `routes` | 示例四条内置路由 | 路由前缀、策略、功能、注入、账号和模型规则 |
| `accounts` | 示例启用、等待 `60s`、队列 `64`、`wait` | 全局账号池容量、重试、质量、自动禁用、冷却、传输、usage 放大等默认值 |
| `activeRouteScenarioId` | 未设置 | 最近完整应用的路由场景标识 |
| `usageRecordLimit`、`highCacheThreshold` | `5000`、`10000` | 内存记录上限与管理端高缓存判定阈值 |
| `auditLogRetentionEnabled`、`auditLogRetentionDays` | `false`、`15` | 审计自动清理默认关闭 |
| `runtimeConfigMigrationVersion` | `2` | 内部配置迁移标记 |

账号池常用默认值为全局并发 `512`、排队 `64`、调度等待 `60s`、跨账号尝试 `3`、请求超时 `180s`、流空闲超时 `180s`、首输出超时 `120s`。质量评分和自动禁用默认关闭。详见 [04 调度](docs/claude-pool-refactor/04-调度.md)。

### usage 策略

| `cacheType` | 用途 |
|---|---|
| `no_cache` | 关闭本地缓存模拟 |
| `current_high_cache` | 沿用高缓存模拟、字段整形与成本下限行为 |
| `prefix_tool` | 前缀与工具上下文缓存策略 |
| `stable_segment_cache` | 稳定片段缓存策略 |
| `prefix_tool_plus` | 四字段倍率、浮动、写入频次和计费下限 |
| `bounded_high_cache` | 对最终用量执行严格上下限收口 |

`no_cache` 控制模拟策略；需要账号完全保留上游 usage 时，设置 `usageProjectionMode=pass_through`。`current_high_cache` 的既有成本下限行为可能突破 input 上限；需要严格上下限时选择 `bounded_high_cache`。具体顺序与行为见 [05 usage 整形](docs/claude-pool-refactor/05-usage整形与缓存策略.md)。

### 环境变量

| 变量 | 作用 |
|---|---|
| `RUST_LOG` | 日志等级，默认 `info` |
| `CLAUDE_POOL_HOST`、`CLAUDE_POOL_PORT` | 覆盖监听地址、端口；Compose 中 `PORT` 也用于宿主机映射 |
| `CLAUDE_POOL_POSTGRES_URL` | 覆盖 PostgreSQL 连接 |
| `CLAUDE_POOL_POSTGRES_MAX_CONNECTIONS`、`CLAUDE_POOL_POSTGRES_USAGE_MAX_CONNECTIONS` | 覆盖两类连接池容量 |
| `CLAUDE_POOL_POSTGRES_MIGRATE_ON_START` | 控制启动 schema 迁移 |
| `CLAUDE_POOL_POSTGRES_COMPRESS_USAGE_ROLLUPS_ON_START` | 控制历史 rollup 压缩，默认关闭 |
| `CLAUDE_POOL_REDIS_URL` | 覆盖业务 Redis 连接 |
| `CLAUDE_POOL_OBSERVABILITY_REDIS_URL`、`CLAUDE_POOL_OBSERVABILITY_REDIS_KEY_PREFIX` | 可选独立观测 Redis |
| `CLAUDE_POOL_UI_MODE`、`CLAUDE_POOL_UI_DIR` | UI 模式及 filesystem 目录；模式为 `embedded` / `redirect` / `proxy` / `filesystem` / `disabled` |
| `CLAUDE_POOL_UI_DEV_SERVER`、`CLAUDE_POOL_UI_EXTERNAL_URL` | UI 开发服务地址；debug 默认指向 `http://127.0.0.1:9023/ui` |
| `CLAUDE_POOL_NEW_UI_*` | 对应 UI 变量的优先读取形式 |
| `CLAUDE_POOL_CAPTURE_DIR` | 诊断采集目录，默认 `logs/llm-capture` |
| `CLAUDE_POOL_HTTP_DRAIN_SECS` | 优雅退出时 HTTP 排空等待，默认 `45s` |
| `CLAUDE_POOL_IMAGE`、`CLAUDE_POOL_VERSION` | Compose 镜像仓库与 tag，默认 `ghcr.io/2ue/claude-pool:latest` |
| `CLAUDE_POOL_POSTGRES_DB`、`CLAUDE_POOL_POSTGRES_USER`、`CLAUDE_POOL_POSTGRES_PASSWORD` | Compose 数据库名、用户、密码 |
| `CLAUDE_POOL_POSTGRES_PORT`、`CLAUDE_POOL_REDIS_PORT` | 本地依赖宿主端口，默认 `25432`、`26379` |

### 路由定义示例

在控制台编辑路由，或把下面对象加入 `routes` 数组；四条内置路由继续保留：

```json
{
  "prefix": "/dfcache/team",
  "name": "团队严格边界策略",
  "enabled": true,
  "cachePolicy": { "cacheType": "bounded_high_cache", "routeNamespace": true },
  "features": { "thinkingModelName": false, "historySanitize": false },
  "accountRule": { "mode": "allow_list", "rules": ["1", "2"] },
  "modelRule": { "mode": "allow_list", "rules": ["claude-sonnet-*"] }
}
```

请求地址为 `/dfcache/team/v1/messages`。`accountRule.rules` 写实际账号 ID；`allow_list` 的空列表拒绝全部，`deny_list` 的空列表允许全部。模型限制与账号 `supportedModels` 取交集；账号列表为空表示不限。模型解析、映射与目录管理见 [06](docs/claude-pool-refactor/06-模型目录映射与限制.md)。

## API 端点

`{prefix}` 表示空字符串、`/cc`、`/ha`、`/na` 或已配置的路由前缀。推理与模型接口支持 `x-api-key` 或 `Authorization: Bearer` 下游鉴权。

| 方法 | 路径 | 作用 |
|---|---|---|
| POST | `{prefix}/v1/messages` | Anthropic Messages；支持 SSE、thinking、工具调用和服务端工具透传 |
| POST | `{prefix}/v1/messages/count_tokens` | 本地 token 估算，按该路由的注入项与图片处理配置计算；不占账号额度 |
| GET | `{prefix}/v1/models` | 返回目录与该路由可用账号模型的交集 |
| GET | `{prefix}/v1/models/{model_id}` | 单个模型详情 |
| GET / HEAD | `/api/hello`、`/cc/api/hello`、`/ha/api/hello`、`/na/api/hello`、`/dfcache/{name}/api/hello` | 无需鉴权的 Claude Code 预连接探测 |
| GET | `/healthz` | 进程存活检查 |
| GET | `/readyz` | PostgreSQL、Redis、事件订阅与调度就绪检查 |

`count_tokens` 的完整路径在 `/messages` 下。自定义路由必须先定义，未知前缀返回 404。

| 内置前缀 | 默认 `cacheType` | 默认 reportedUsage |
|---|---|---|
| `""`，即 `/v1` | `current_high_cache` | 继承默认字段策略 |
| `/cc` | `current_high_cache` | input `sample-max=96`；creation `sample-target=3000`，倍率 `1.2` |
| `/ha` | `current_high_cache` | input `sample-max=96` |
| `/na` | `no_cache` | 继承默认字段策略 |

这些差异来自路由定义，修改路由策略即可改变。流保活使用 SSE 注释帧，详见[流式保活与代理超时](docs/streaming-keepalive.md)。

账号池错误出口遵循 [11 章 3.10](docs/claude-pool-refactor/11-实施说明与关键逻辑契约.md#310-错误分类与对外映射)：

| 条件 | HTTP | `error.type` | 处理 |
|---|---|---|---|
| 上游限流且重试耗尽 | 429 | `rate_limit_error` | 固定公开文案 |
| 上游 400 且不可重试 | 400 | `invalid_request_error` | 图片、payload、上下文或通用固定文案 |
| 容量不足、排队超时、全局并发满 | 529 | `overloaded_error` | 固定公开文案 |
| 其余账号上游错误 | 502 | `api_error` | 固定公开文案 |

账号池出口错误附 `x-error-id`，原始上游错误只进入内部记录和日志。入口另有鉴权 401、规则/模型拒绝 404、请求体过大 413、准入限流 429；对下游输出开始后的失败通过流式 error 事件结束。

## 管理 API 与控制台

控制台入口 `/ui`；管理 API 前缀 `/api/admin`，使用 `adminApiKey`，支持与下游相同的两种头部鉴权方式。

| 能力 | 主要端点（省略 `/api/admin`） |
|---|---|
| 账号 | `GET/POST /accounts`、`PUT/DELETE /accounts/{id}`、测试、模型发现、清冷却与自动禁用 |
| 路由 | `GET/PUT /routes`、`GET /routes/effective?prefix=%2Fcc` |
| 生效配置 | `GET /accounts/{id}/effective` |
| 模型 | `/model-capabilities`、`/model-capabilities/manual`、`PATCH /model-capabilities/{model}/listed`、`POST /accounts/{id}/models/promote` |
| 配置包 | `GET /config/export`、`POST /config/import` |
| 运行配置与密钥 | `GET/PUT /config/runtime`、`/security/request-keys` |
| 用量与审计 | `/usage-records-paged`、`/usage-dashboard/overview`、`/audit-logs` |
| 清理与诊断 | `/usage-records/cleanup/*`、`/diagnostics/capture/*` |

控制台支持路由场景快照、raw / shaped / reported 用量与成本查看。自动删除类功能默认关闭。管理模块说明见 [08](docs/claude-pool-refactor/08-管理API与控制台.md)；开发入口见[前端开发环境](docs/frontend-dev-environment.md)。

## 从旧版网关迁移

使用新二进制直接读取旧 PostgreSQL，导出账号、运行配置、下游密钥、路由场景、价格和手工模型目录；usage 历史不迁移。完整旧版本名称、字段映射和行为差异见 [07 章第 4 节](docs/claude-pool-refactor/07-存储与迁移.md)。

```bash
cargo run --release --locked --bin claude-pool -- export-legacy \
  --from-postgres 'postgres://reader:password@old-db/legacy' \
  --out legacy.json

# 默认 dry-run，先检查丢弃字段、容量、功能开关与映射变化
cargo run --release --locked --bin claude-pool -- -c config.json import-legacy legacy.json

# 显式 apply 后，单事务全量覆盖目标配置
cargo run --release --locked --bin claude-pool -- -c config.json import-legacy legacy.json --apply
```

导出以只读快照读取旧库，文件只允许当前用户读写且不覆盖已有文件。`--redact-secrets` 生成的脱敏包只用于检查，不能 apply。正式导入需要新库和 Redis；目标实例应使用独立数据库与 Redis 前缀。`--apply` 会全量覆盖目标配置，执行前备份目标数据库，并按 dry-run 报告核对覆盖范围。

导入保留目标机器的存储与监听配置，保留旧账号 `usageProjectionMode`，按报告审查容量、注入、原字节透传及新默认值变化。新系统之间迁移使用管理 API 配置包接口。

## 测试

本项目是 bin 包，单测指定二进制：

```bash
cargo fmt --all
cargo test --bin claude-pool -q
cargo test --bin claude-pool -- account::golden_tests
pnpm --dir ui check
pnpm --dir ui build
```

存储集成测试读取 `CLAUDE_POOL_TEST_POSTGRES_URL`、`CLAUDE_POOL_TEST_REDIS_URL`。未设置时相关测试自动跳过；需要强制验证的 CI 另设 `CLAUDE_POOL_REQUIRE_STORAGE_TESTS=1`。使用专门的测试数据库和 Redis，PostgreSQL 测试按 schema 隔离，Redis 测试按随机前缀隔离。

```bash
CLAUDE_POOL_TEST_POSTGRES_URL='postgres://claude_pool:password@127.0.0.1:25432/claude_pool_test' \
CLAUDE_POOL_TEST_REDIS_URL='redis://127.0.0.1:26379/15' \
CLAUDE_POOL_REQUIRE_STORAGE_TESTS=1 \
  cargo test --bin claude-pool -q
```

`tests/golden/usage/` 保存 48 个固定种子的用量快照，覆盖六种策略 × 流式/非流式 × 有无读取证据 × 有无 1h 写入。普通运行只比较预期输出；发布验收不设置 `GOLDEN_UPDATE`。压测工具为 `pool_loadtest`，先用 `cargo run --bin pool_loadtest -- --help` 查看参数。

## 项目结构

以下路径以当前实现为准，设计目录目标见 [11 章第 2 节](docs/claude-pool-refactor/11-实施说明与关键逻辑契约.md#2-目标目录结构)。

```text
src/
├── main.rs                       装配、启动、健康检查、后台任务
├── model/
│   ├── config.rs                 配置类型、默认值、校验
│   ├── runtime.rs                RuntimeConfigStore 与热加载
│   ├── routes.rs                 路由定义、预编译与合并
│   ├── prefix_tool_plus_policy.rs plus 策略配置
│   └── prompt_injection/         注入与路由功能配置
├── anthropic/
│   ├── router.rs、middleware.rs   协议路由、鉴权与路由限制
│   ├── handlers.rs、handlers/    请求入口与路由功能处理
│   ├── body_patch.rs             定点改写白名单
│   ├── conversation_id.rs        稳定会话标识
│   ├── cache.rs、prompt_cache.rs、prompt_cache_creation_control.rs
│   ├── bounded_shaping.rs、prefix_tool_plus_shaping.rs  usage 算法
│   └── usage.rs、pricing.rs、model_capabilities.rs      用量与目录
├── account/
│   ├── mod.rs                    账号、调度、lease、传输、流处理与投影编排
│   ├── body_pipeline.rs、model_pipeline.rs、retry_pipeline.rs
│   ├── usage_projection.rs、early_commit.rs
│   └── tests.rs、golden_tests.rs
├── storage/                      PostgreSQL、Redis、账号事件与演示数据
├── admin/                        管理 API、配置包、路由场景与诊断入口
├── migration/                    旧配置包导出、映射、差异报告与导入
├── common/、diagnostics/         公共工具与采集
├── ui/                           静态页面服务
├── http_client.rs、token.rs      HTTP 客户端与 token 估算
└── bin/pool_loadtest.rs          压测工具
ui/                              React 控制台
tests/golden/usage/               usage 快照
docs/                            部署、开发说明与设计记录
```

## 技术栈

Rust 2024、Axum 0.8、Tokio、Reqwest、Serde、tracing、Clap、SQLx / PostgreSQL、Redis；前端使用 React 18、TypeScript、Vite 5、Tailwind CSS 4、Radix UI 与 TanStack Query。Docker 使用多阶段构建，支持 amd64 / arm64；CI 校验格式、既有 Clippy 基线、前端构建与 Rust 测试。

## License

[MIT](LICENSE)
