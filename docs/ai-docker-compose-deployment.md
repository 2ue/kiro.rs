# Kiro.rs Docker Compose 部署文档（给 AI 执行）

本文档用于让 AI 或自动化脚本按 Docker Compose 方式部署 Kiro.rs。所有解释使用中文；JSON 字段名、环境变量名、命令参数保持真实名称，便于直接复制执行。

## 1. 部署目标

部署后提供这些入口：

| 入口 | 说明 |
| --- | --- |
| `http://服务器地址:8990/v1` | 默认 high-cache Anthropic 兼容接口 |
| `http://服务器地址:8990/cc/v1` | Claude Code 兼容接口，保持 `/cc` 独立 usage 上报策略 |
| `http://服务器地址:8990/ha/v1` | high-cache 接口，默认只改写 input 上报 |
| `http://服务器地址:8990/na/v1` | no-cache usage 上报接口，默认只保留真实上游 cache usage |
| `http://服务器地址:8990/admin` | 管理后台 |

当前版本以 PgSQL + Redis 为必需依赖：

| 组件 | 用途 |
| --- | --- |
| PgSQL | 运行配置、凭据、凭据运行态、凭据统计、usage 记录、模型价格 |
| Redis | 会话绑定、临时冷却、本地限流、并发 lease、跨实例 Token 刷新锁、余额查询缓存 |

## 2. 前置条件

目标机器需要具备：

- Docker
- Docker Compose v2（命令是 `docker compose`）
- 能访问镜像仓库 `ghcr.io/2ue/kiro-rs`
- 能访问 Kiro 上游服务

检查命令：

```bash
docker --version
docker compose version
```

## 3. 目录结构

建议部署目录：

```text
/opt/kiro-rs/
├── docker-compose.yml
└── config/
    ├── config.json
    └── credentials.json
```

创建目录：

```bash
mkdir -p /opt/kiro-rs/config
cd /opt/kiro-rs
```

## 4. docker-compose.yml

在 `/opt/kiro-rs/docker-compose.yml` 写入：

```yaml
services:
  kiro-rs-postgres:
    image: postgres:18-alpine
    container_name: kiro-rs-postgres
    environment:
      POSTGRES_DB: ${KIRO_RS_POSTGRES_DB:-kiro_rs}
      POSTGRES_USER: ${KIRO_RS_POSTGRES_USER:-kiro_rs}
      POSTGRES_PASSWORD: ${KIRO_RS_POSTGRES_PASSWORD:-change-me}
    volumes:
      - kiro-rs-postgres-data:/var/lib/postgresql
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U ${KIRO_RS_POSTGRES_USER:-kiro_rs} -d ${KIRO_RS_POSTGRES_DB:-kiro_rs}"]
      interval: 10s
      timeout: 5s
      retries: 10
    restart: unless-stopped

  kiro-rs-redis:
    image: redis:7-alpine
    container_name: kiro-rs-redis
    command: ["redis-server", "--appendonly", "yes"]
    volumes:
      - kiro-rs-redis-data:/data
    healthcheck:
      test: ["CMD", "redis-cli", "ping"]
      interval: 10s
      timeout: 5s
      retries: 10
    restart: unless-stopped

  kiro-rs:
    image: ${KIRO_RS_IMAGE:-ghcr.io/2ue/kiro-rs}:${KIRO_RS_VERSION:-latest}
    container_name: kiro-rs
    depends_on:
      kiro-rs-postgres:
        condition: service_healthy
      kiro-rs-redis:
        condition: service_healthy
    environment:
      KIRO_RS_POSTGRES_URL: postgres://${KIRO_RS_POSTGRES_USER:-kiro_rs}:${KIRO_RS_POSTGRES_PASSWORD:-change-me}@kiro-rs-postgres:5432/${KIRO_RS_POSTGRES_DB:-kiro_rs}
      KIRO_RS_REDIS_URL: redis://kiro-rs-redis:6379/0
    ports:
      - "${KIRO_RS_PORT:-8990}:8990"
    volumes:
      - ./config:/app/config
    extra_hosts:
      - "host.docker.internal:host-gateway"
    restart: unless-stopped

volumes:
  kiro-rs-postgres-data:
  kiro-rs-redis-data:
```

PostgreSQL 18 官方镜像默认使用版本化的数据目录，compose 中挂载父目录
`/var/lib/postgresql`，不要继续把数据卷直接挂到旧路径
`/var/lib/postgresql/data`。如果你已经用 PostgreSQL 16 跑过生产数据，
不要直接把同一个旧数据卷换成 18 镜像启动；需要先备份并通过 dump/restore
或官方 pg_upgrade 流程完成大版本升级。

可配置环境变量：

| 变量名 | 默认值 | 控制什么 |
| --- | --- | --- |
| `KIRO_RS_IMAGE` | `ghcr.io/2ue/kiro-rs` | 控制使用哪个镜像仓库。一般不需要改。 |
| `KIRO_RS_VERSION` | `latest` | 控制镜像版本。生产建议固定为具体版本，例如 `0.0.19`。 |
| `KIRO_RS_PORT` | `8990` | 控制宿主机暴露端口。容器内端口固定是 `8990`。 |
| `KIRO_RS_POSTGRES_DB` | `kiro_rs` | 控制 PgSQL 数据库名。 |
| `KIRO_RS_POSTGRES_USER` | `kiro_rs` | 控制 PgSQL 用户名。 |
| `KIRO_RS_POSTGRES_PASSWORD` | `change-me` | 控制 PgSQL 密码，生产必须改成强密码。 |

固定版本启动示例：

```bash
KIRO_RS_VERSION=0.0.19 KIRO_RS_POSTGRES_PASSWORD='替换成强密码' docker compose up -d
```

如果宿主机想用 `9022` 端口：

```bash
KIRO_RS_PORT=9022 KIRO_RS_VERSION=0.0.19 KIRO_RS_POSTGRES_PASSWORD='替换成强密码' docker compose up -d
```

## 5. config.json

在 `/opt/kiro-rs/config/config.json` 写入下面内容，并按实际情况修改密钥：

```json
{
  "postgres": {
    "url": "postgres://kiro_rs:change-me@kiro-rs-postgres:5432/kiro_rs"
  },
  "redis": {
    "url": "redis://kiro-rs-redis:6379/0"
  },
  "host": "0.0.0.0",
  "port": 8990,
  "apiKey": "sk-kiro-rs-change-me",
  "adminApiKey": "sk-admin-change-me",
  "payloadGuardEnabled": true,
  "payloadGuardMaxBytes": 460800,
  "payloadGuardTrimHistory": true
}
```

关键说明：

- Docker 部署时 `host` 必须是 `0.0.0.0`，否则宿主机端口映射后可能访问不到服务。
- Compose 会通过 `KIRO_RS_POSTGRES_URL` 和 `KIRO_RS_REDIS_URL` 注入数据库连接地址；文件里的 `postgres.url` 和 `redis.url` 也可以保留，主要用于本地或非 Compose 场景。
- 未写出的配置会使用内置默认值。首次启动导入 PgSQL 后，可以在后台配置页热更新调度、payload 防护、高缓存模拟和路径级 usage 上报策略。
- 首次启动时，如果 PgSQL 没有运行配置或凭据，服务会从 `config.json` 和 `credentials.json` 导入。
- 导入后运行配置、凭据状态、Token 刷新结果、失败计数、预热状态、调度统计、usage 记录、模型价格都以 PgSQL 为准。
- 会话粘性绑定、同会话软失败计数、上游瞬态错误冷却、本地 RPM 限流、单凭据并发 lease 和跨实例 Token 刷新锁都以 Redis 为准。

## 6. credentials.json

首次启动前，在 `/opt/kiro-rs/config/credentials.json` 写入凭据。可以是单个对象，也可以是数组。

单个 OAuth 凭据示例：

```json
{
  "refreshToken": "替换成你的 refresh token",
  "expiresAt": "2026-12-31T00:00:00Z",
  "authMethod": "social",
  "priority": 0,
  "email": "account@example.com"
}
```

多凭据示例：

```json
[
  {
    "id": 1,
    "refreshToken": "替换成第一个 refresh token",
    "expiresAt": "2026-12-31T00:00:00Z",
    "authMethod": "social",
    "priority": 0,
    "email": "account1@example.com"
  },
  {
    "id": 2,
    "kiroApiKey": "ksk_xxxxxxxx",
    "authMethod": "api_key",
    "priority": 10,
    "email": "account2@example.com"
  }
]
```

数据库已有凭据后，服务启动不依赖 `credentials.json`。之后建议通过管理后台新增、禁用、删除或导出凭据。

## 7. 配置项中文说明

### 数据库和缓存

| 字段名 | 建议值 | 控制什么 |
| --- | --- | --- |
| `postgres.url` | Compose 自动注入 | 控制 PgSQL 连接地址。服务必须能连接 PgSQL 才能启动。 |
| `postgres.maxConnections` | `10` | 控制 PgSQL 连接池最大连接数。 |
| `postgres.migrateOnStart` | `true` | 控制启动时是否自动创建或升级数据库表。生产建议保持开启。 |
| `redis.url` | Compose 自动注入 | 控制 Redis 连接地址。服务必须能连接 Redis 才能启动；会话绑定、临时冷却、限流、并发占用、刷新锁和余额缓存都写入 Redis。 |
| `redis.keyPrefix` | `kiro_rs:prod` | 控制 Redis key 前缀，用于和同一个 Redis 中的其他业务隔离。 |

### 基础监听和认证

| 字段名 | 建议值 | 控制什么 |
| --- | --- | --- |
| `host` | Docker 中用 `0.0.0.0` | 控制服务监听地址。 |
| `port` | `8990` | 控制容器内服务端口。 |
| `apiKey` | 强随机字符串 | 控制调用 `/v1`、`/cc/v1`、`/ha/v1`、`/na/v1` API 时使用的客户端密钥。 |
| `adminApiKey` | 强随机字符串 | 控制管理后台和 `/api/admin/*` 的认证密钥。 |
| `tlsBackend` | `rustls` | 控制 HTTP 客户端 TLS 实现。Docker 镜像推荐 `rustls`。 |

### Kiro 上游环境模拟

| 字段名 | 建议值 | 控制什么 |
| --- | --- | --- |
| `region` | `us-east-1` | 控制默认 Kiro 区域。未单独配置 `authRegion` / `apiRegion` 时会用它。 |
| `authRegion` | 可不填 | 控制刷新 OAuth / IdC Token 的区域。 |
| `apiRegion` | 可不填 | 控制请求 Kiro API 的区域。 |
| `kiroVersion` | `0.11.107` | 控制请求上游时模拟的 Kiro IDE 版本。 |
| `nodeVersion` | `22.22.0` | 控制请求上游时模拟的 Node 版本。 |
| `machineId` | 可不填 | 控制全局机器 ID。通常建议让系统根据每个凭据自动派生。 |
| `systemVersion` | 可不填 | 控制请求上游时模拟的系统版本。 |

### 凭据调度

| 字段名 | 建议值 | 控制什么 |
| --- | --- | --- |
| `loadBalancingMode` | `priority` 或 `balanced` | 控制多凭据调度方式。 |
| `credentialRpm` | `null` | 控制每个凭据的本地请求速率限制。`null` 或 `0` 表示关闭。 |
| `credentialMaxConcurrentRequests` | `0` | 控制每个凭据最多同时处理多少个请求。`0` 表示不限制。 |
| `credentialTransientCooldownSecs` | `10` | 控制上游临时错误但没有 `Retry-After` 时，单个凭据临时冷却多少秒。 |
| `credentialMaxCooldownSecs` | `300` | 控制临时冷却最长秒数。 |
| `credentialDispatchMaxWaitSecs` | `120` | 控制单个请求最多排队等待凭据可调度多久。`0` 表示不限制。 |
| `credentialInFlightLeaseMaxSecs` | `900` | 控制单个并发占用超过多久未活跃时自动释放。 |
| `credentialWarmupRequests` | `3` | 控制新凭据预热剩余请求数。预热不会伪造成功次数，只降低被选中的概率。 |
| `credentialWarmupSelectionPercent` | `5` | 控制 `balanced` 模式下预热凭据参与真实请求调度的概率百分比。 |
| `defaultEndpoint` | `ide` | 控制凭据未单独指定 `endpoint` 时走哪个 Kiro 端点。 |

### 上游请求体防护

| 字段名 | 建议值 | 控制什么 |
| --- | --- | --- |
| `payloadGuardEnabled` | `true` | 是否在发送 Kiro 上游前按最终 JSON 字节数检查请求体。 |
| `payloadGuardMaxBytes` | `460800` | Kiro 上游请求 JSON body 最大字节数。 |
| `payloadGuardTrimHistory` | `true` | 请求体超限时是否裁剪最旧历史；关闭后只做协议修复，仍超限会直接返回客户端错误。 |

### 路径缓存行为

路径缓存模式是固定的，不再通过配置切换：

| 路径 | 行为 |
| --- | --- |
| `/v1/messages` | 高缓存模式。 |
| `/cc/v1/messages` | 高缓存模式，默认只由 `/cc` 路径覆盖项改写下游 input 和 cache write 上报。 |
| `/ha/v1/messages` | 高缓存模式，默认只由 `/ha` 路径覆盖项改写下游 input 上报。 |
| `/na/v1/messages` | 高缓存路由，默认由 `/na` 路径覆盖项关闭本地模拟 cache usage 补足，只保留真实上游 cache usage。 |

### 高缓存模拟和 usage 上报

| 字段名 | 建议值 | 控制什么 |
| --- | --- | --- |
| `promptCacheTargetReadRatio` | `0.98` | 控制高缓存模拟中 cache read 的目标比例中心。实际会自然浮动。 |
| `promptCacheTokenScale` | `1.6` | 控制高缓存模拟时 total input 的放大倍数。 |
| `promptCacheMaxSimulatedInputTokens` | `300000` | 控制模拟 total input 的上限。 |
| `promptCacheCapJitterMinTokens` | `12000` | 控制触顶时 soft-cap 最小扣减值。 |
| `promptCacheCapJitterMaxTokens` | `24000` | 控制触顶时 soft-cap 最大扣减值。 |
| `promptCacheScaleMinInputTokens` | `20000` | 控制基础输入达到多少 token 后才启用放大。 |
| `reportedUsage.default` | input/output 原始值，cache read/write 保留计算值 | 控制所有路径的默认 input、output、cache read、cache write 上报方式。 |
| `reportedUsage.pathOverrides` | `/na`、`/cc`、`/ha` | 控制路径前缀覆盖策略。每个前缀独立配置，最长前缀优先。 |
| `mode: "preserve"` | 默认用于 cache read/write | 保留本地 high-cache 计算后的字段值。 |
| `mode: "raw"` | 默认用于 input/output | 使用请求和上游响应的原始字段值，不使用本地 high-cache 放大后的值。 |
| `mode: "sample-max"` | input 可用 | 把字段采样到 `maxTokens` 以内，数值自然浮动，不固定到上限。 |
| `mode: "sample-target"` | cache write 可用 | 按 `targetTokens` 和 `normalMaxMultiplier` 生成自然分布。 |
| `moveDeltaToCacheRead` | input 建议 `true` | input 被压低的差值转入 cache read，只改变下游上报外观。 |

### Usage 记录和模型价格

| 字段名 | 建议值 | 控制什么 |
| --- | --- | --- |
| `usageRecordLimit` | `5000` | 控制内存中最多保留多少条最近请求记录；完整 usage 记录写入 PgSQL。 |
| `highCacheThreshold` | `10000` | 控制后台统计中“高缓存请求”的判定阈值。 |

模型价格目录会在服务启动时自动初始化，也可以在后台手动同步。价格只用于统计和展示，价格同步失败不会影响凭据调度和模型调用。

## 8. 启动和检查

启动：

```bash
cd /opt/kiro-rs
KIRO_RS_VERSION=0.0.19 KIRO_RS_POSTGRES_PASSWORD='替换成强密码' docker compose up -d
```

查看状态：

```bash
docker compose ps
docker compose logs -f kiro-rs
```

验证模型列表：

```bash
curl http://127.0.0.1:8990/v1/models \
  -H 'x-api-key: sk-kiro-rs-change-me'
```

验证消息接口：

```bash
curl http://127.0.0.1:8990/v1/messages \
  -H 'content-type: application/json' \
  -H 'x-api-key: sk-kiro-rs-change-me' \
  -d '{
    "model": "claude-sonnet-4-5",
    "max_tokens": 128,
    "messages": [{"role": "user", "content": "hello"}]
  }'
```

## 9. 管理后台

浏览器打开：

```text
http://服务器地址:8990/admin
```

输入 `adminApiKey` 登录。后台可管理凭据、测试凭据模型调用、查看余额、查看 usage 记录、修改运行配置、同步模型价格和导出凭据。

## 10. 数据持久化和备份

当前版本以 PgSQL + Redis 为持久化核心：

| 位置 | 控制什么 |
| --- | --- |
| PgSQL `runtime_config` | 运行配置。首次可从 `config.json` 导入，之后后台修改写入这里。 |
| PgSQL `credentials` | 凭据列表、禁用状态、刷新后的 Token、优先级等。首次可从 `credentials.json` 导入。 |
| PgSQL `credential_runtime_state` | 凭据失败计数、刷新失败计数、禁用原因、预热剩余次数。 |
| PgSQL `credential_stats` | 凭据成功次数、最后使用时间等调度统计。 |
| PgSQL `usage_records` | 请求级 usage 记录、错误详情、模型计价结果。 |
| PgSQL `model_pricing` | 模型价格同步结果。 |
| Redis | 会话粘性绑定、同会话软失败计数、临时冷却、本地限流、并发 lease、Token 刷新锁、余额查询缓存。 |
| `config/config.json` | 首次导入和数据库连接配置。 |
| `config/credentials.json` | 首次导入凭据文件；数据库已有凭据后服务启动不依赖它。 |

备份时至少备份 PgSQL 数据卷。如果还依赖文件做首次导入，也备份：

```text
config/config.json
config/credentials.json
```

## 11. 停止、重启、升级

停止：

```bash
cd /opt/kiro-rs
docker compose down
```

重启：

```bash
cd /opt/kiro-rs
docker compose restart kiro-rs
```

升级版本：

```bash
cd /opt/kiro-rs
KIRO_RS_VERSION=0.0.19 KIRO_RS_POSTGRES_PASSWORD='替换成强密码' docker compose pull
KIRO_RS_VERSION=0.0.19 KIRO_RS_POSTGRES_PASSWORD='替换成强密码' docker compose up -d
```

## 12. 常见问题

### 服务启动失败并提示连接 PgSQL 或 Redis 失败

先看容器状态：

```bash
docker compose ps
docker compose logs kiro-rs-postgres
docker compose logs kiro-rs-redis
docker compose logs kiro-rs
```

确认 `KIRO_RS_POSTGRES_PASSWORD` 在启动命令、数据库容器和应用容器中一致。

### 修改 config.json 后为什么不生效

首次导入后，运行配置以 PgSQL `runtime_config` 为准。后续请在管理后台修改配置；如果要重新从文件导入，需要清空数据库中的运行配置，谨慎操作。

### 修改 credentials.json 后为什么不生效

首次导入后，凭据以 PgSQL `credentials` 为准。后续请在管理后台新增、删除、禁用或导出凭据；如果要重新从文件导入，需要清空数据库中的凭据，谨慎操作。
