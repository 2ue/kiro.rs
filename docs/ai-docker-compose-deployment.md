# Docker Compose 部署

`claude-pool` 需要 PostgreSQL 和 Redis。首次启动从 `config/config.json` 初始化运行配置，之后配置与账号以 PostgreSQL 为准；发布容器在 `/ui` 提供嵌入的管理控制台。

## 选择编排文件

| 文件 | 服务 | 用途 |
|---|---|---|
| `docker-compose.yml` | `claude-pool` | 连接已有数据库与 Redis |
| `docker-compose.deploy.yml` | `claude-pool` | 连接已有依赖，带 `/readyz` 健康检查、日志轮转和退出等待 |
| `docker-compose.database.yml` | `claude-pool-app`、`claude-pool-postgres`、`claude-pool-redis` | 完整部署，数据库和 Redis 使用命名卷 |
| `docker-compose.local-infra.yml` | `claude-pool-postgres-local`、`claude-pool-redis-local` | 只启动本地开发依赖 |

## 完整部署

在仓库根准备目录与配置：

```bash
mkdir -p config logs
cp config.example.json config/config.json
```

编辑 `config/config.json`，至少设置监听地址和两类下游密钥：

```json
{
  "host": "0.0.0.0",
  "port": 8990,
  "apiKey": "sk-downstream-change-me",
  "adminApiKey": "sk-admin-change-me"
}
```

完整编排自动给应用传入数据库和 Redis URL。数据库默认库名、用户名均为 `claude_pool`；用环境变量设置密码，并固定镜像版本：

```bash
export CLAUDE_POOL_POSTGRES_PASSWORD='replace-with-database-password'
export CLAUDE_POOL_VERSION=1.0.0
docker compose -f docker-compose.database.yml up -d
docker compose -f docker-compose.database.yml ps
docker compose -f docker-compose.database.yml logs --tail=100 claude-pool-app
```

`CLAUDE_POOL_IMAGE` 默认 `ghcr.io/2ue/claude-pool`，`CLAUDE_POOL_PORT` 默认 `8990`，`CLAUDE_POOL_VERSION` 默认 `latest`。部署自建镜像时：

```bash
docker build -t claude-pool:1.0.0 .
CLAUDE_POOL_IMAGE=claude-pool CLAUDE_POOL_VERSION=1.0.0 \
  docker compose -f docker-compose.database.yml up -d
```

浏览器打开 `http://服务器地址:8990/ui`，使用配置中的 `adminApiKey` 登录。账号页填写上游 `baseUrl`、`apiKey` 和鉴权类型；可发现支持模型、加入模型目录，再在路由页选择策略与允许账号。

## 验证服务

```bash
curl --fail-with-body http://127.0.0.1:8990/healthz
curl --fail-with-body http://127.0.0.1:8990/readyz
curl --fail-with-body http://127.0.0.1:8990/v1/models \
  -H 'x-api-key: sk-downstream-change-me'
curl --fail-with-body http://127.0.0.1:8990/v1/messages \
  -H 'content-type: application/json' \
  -H 'anthropic-version: 2023-06-01' \
  -H 'x-api-key: sk-downstream-change-me' \
  -d '{"model":"claude-sonnet-4-5","max_tokens":128,"messages":[{"role":"user","content":"回复 ok"}]}'
```

模型必须由已启用账号接受，并被当前路由允许。`/healthz` 检查进程存活，`/readyz` 检查依赖、事件订阅与调度就绪。

## 已有 PostgreSQL 与 Redis

使用 `docker-compose.deploy.yml` 时，在 `config/config.json` 填入可从容器访问的连接地址：

```json
{
  "host": "0.0.0.0",
  "port": 8990,
  "postgres": { "url": "postgres://claude_pool:password@database.example:5432/claude_pool" },
  "redis": { "url": "redis://redis.example:6379/0", "keyPrefix": "claude_pool:production" },
  "apiKey": "sk-downstream-change-me",
  "adminApiKey": "sk-admin-change-me"
}
```

```bash
docker compose -f docker-compose.deploy.yml up -d
```

容器内的 `127.0.0.1` 指向容器自身。连接宿主机可使用编排已配置的 `host.docker.internal`；连接其他 Compose 服务使用服务名。`CLAUDE_POOL_POSTGRES_URL`、`CLAUDE_POOL_REDIS_URL` 可覆盖配置文件，但需要在应用的 `environment` 中显式传入；仅在宿主 shell 导出变量不会自动传给容器。

多实例共享 PostgreSQL 和业务 Redis 前缀，实现账号 lease 与配置热加载。下游密钥的 RPM、并发和等待队列按实例计数；需要统一限额时在入口网关限流或按实例分配预算。

## 存储与备份

| 位置 | 内容 |
|---|---|
| PostgreSQL `runtime_config` | 运行配置、路由与下游密钥 |
| PostgreSQL `accounts` | 账号、模型与路由规则、覆盖参数 |
| PostgreSQL `usage_records` 及 rollup 表 | raw / shaped / reported 用量、成本和统计 |
| PostgreSQL `model_capabilities`、`model_pricing` | 模型目录与价格 |
| PostgreSQL `route_scenarios`、`admin_audit_logs` | 场景与审计 |
| Redis | 账号 lease、队列、冷却、质量与配置事件 |
| `config/`、`logs/` | 启动配置与诊断采集 |

备份数据库与部署配置。控制台配置导出用于账号和运行配置迁移，不包含 usage 历史；数据库备份用于完整恢复。观测 Redis 为可选独立服务，不能与业务 Redis 使用同一进程；未配置时观测功能使用 PostgreSQL 和进程内状态。

PostgreSQL 18 镜像的数据卷挂在 `/var/lib/postgresql`；本地 PostgreSQL 16 镜像使用 `/var/lib/postgresql/data`。数据库主版本升级使用 dump/restore 等对应升级流程，不能直接复用另一主版本的数据目录。

## 重启与升级

固定镜像 tag 后拉取并重建应用：

```bash
docker compose -f docker-compose.database.yml pull claude-pool-app
docker compose -f docker-compose.database.yml up -d claude-pool-app
docker compose -f docker-compose.database.yml logs --tail=100 claude-pool-app
```

```bash
docker compose -f docker-compose.database.yml restart claude-pool-app
docker compose -f docker-compose.database.yml down
```

`down` 保留命名卷。应用运行配置通过控制台修改；文件负责首次初始化与机器本地部署参数。完整编排升级时继续使用原数据库密码。

部署编排的 `CLAUDE_POOL_HTTP_DRAIN_SECS` 默认 `45`，配合 `stop_grace_period: 60s` 给进行中的流与后台任务留出退出时间。流式代理设置见[流式保活与代理超时](streaming-keepalive.md)。
