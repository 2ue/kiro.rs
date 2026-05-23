# Kiro.rs Docker Compose 部署文档（给 AI 执行）

本文档用于让 AI 或自动化脚本按 Docker Compose 方式部署 Kiro.rs。所有解释使用中文；JSON 字段名、环境变量名、命令参数保持真实名称，便于直接复制执行。

## 1. 目标

部署后提供这些入口：

- Anthropic 兼容接口：`http://服务器地址:8990/v1`
- Claude Code 兼容接口：`http://服务器地址:8990/cc/v1`
- 真实 cache usage 上报接口：`http://服务器地址:8990/na/v1`
- 管理后台：`http://服务器地址:8990/admin`

默认 Docker 镜像：

```text
ghcr.io/2ue/kiro-rs:latest
```

如果要固定版本，推荐使用最新发布版本，例如：

```text
ghcr.io/2ue/kiro-rs:0.0.17
```

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
  kiro-rs:
    image: ${KIRO_RS_IMAGE:-ghcr.io/2ue/kiro-rs}:${KIRO_RS_VERSION:-latest}
    container_name: kiro-rs
    restart: unless-stopped
    ports:
      - "${KIRO_RS_PORT:-8990}:8990"
    volumes:
      - ./config:/app/config
    extra_hosts:
      - "host.docker.internal:host-gateway"
    healthcheck:
      test: ["CMD-SHELL", "nc -z 127.0.0.1 8990 || exit 1"]
      interval: 30s
      timeout: 5s
      retries: 3
      start_period: 20s
```

可选环境变量：

| 变量名 | 默认值 | 控制什么 |
| --- | --- | --- |
| `KIRO_RS_IMAGE` | `ghcr.io/2ue/kiro-rs` | 控制使用哪个镜像仓库。一般不需要改。 |
| `KIRO_RS_VERSION` | `latest` | 控制镜像版本。生产建议固定为具体版本，例如 `0.0.17`。 |
| `KIRO_RS_PORT` | `8990` | 控制宿主机暴露端口。容器内端口固定是 `8990`。 |

固定版本启动示例：

```bash
KIRO_RS_VERSION=0.0.17 docker compose up -d
```

如果宿主机想用 `9022` 端口：

```bash
KIRO_RS_PORT=9022 KIRO_RS_VERSION=0.0.17 docker compose up -d
```

## 5. config.json

在 `/opt/kiro-rs/config/config.json` 写入下面内容，并按实际情况修改密钥和凭据策略：

```json
{
  "host": "0.0.0.0",
  "port": 8990,
  "apiKey": "sk-kiro-rs-change-me",
  "tlsBackend": "rustls",
  "region": "us-east-1",
  "kiroVersion": "0.11.107",
  "nodeVersion": "22.22.0",
  "adminApiKey": "sk-admin-change-me",
  "loadBalancingMode": "priority",
  "credentialRpm": null,
  "credentialMaxConcurrentRequests": 0,
  "credentialTransientCooldownSecs": 10,
  "credentialMaxCooldownSecs": 300,
  "credentialDispatchMaxWaitSecs": 120,
  "credentialInFlightLeaseMaxSecs": 900,
  "credentialWarmupRequests": 3,
  "credentialWarmupSelectionPercent": 5,
  "credentialsPersist": true,
  "credentialStatsPersist": true,
  "compression": {
    "enabled": false,
    "whitespaceCompression": true
  },
  "compatProfile": "claude-code",
  "extractThinking": true,
  "promptCacheTargetReadRatio": 0.98,
  "promptCacheTokenScale": 1.6,
  "promptCacheMaxSimulatedInputTokens": 300000,
  "promptCacheCapJitterMinTokens": 12000,
  "promptCacheCapJitterMaxTokens": 24000,
  "promptCacheScaleMinInputTokens": 20000,
  "reportedUsage": {
    "default": {
      "enabled": true,
      "input": { "mode": "preserve" },
      "output": { "mode": "preserve" },
      "cacheRead": { "mode": "preserve" },
      "cacheCreation": { "mode": "preserve" }
    },
    "pathOverrides": {
      "/na": {
        "enabled": false
      },
      "/cc": {
        "enabled": true,
        "input": {
          "mode": "sample-max",
          "maxTokens": 96,
          "moveDeltaToCacheRead": true
        },
        "cacheCreation": {
          "mode": "sample-target",
          "targetTokens": 3000,
          "normalMaxMultiplier": 1.1
        }
      },
      "/ha": {
        "enabled": true,
        "input": {
          "mode": "sample-max",
          "maxTokens": 96,
          "moveDeltaToCacheRead": true
        }
      }
    }
  },
  "usageRecordLimit": 5000,
  "usageRecordPersist": true,
  "highCacheThreshold": 10000,
  "defaultEndpoint": "ide",
  "exposeProxyWarnings": false
}
```

重要：Docker 部署时 `host` 必须是 `0.0.0.0`，否则容器内服务只监听容器自己的 `127.0.0.1`，宿主机端口映射后可能访问不到。

## 6. config.json 配置项说明

### 基础监听和认证

| 字段名 | 建议值 | 控制什么 |
| --- | --- | --- |
| `host` | Docker 中用 `0.0.0.0` | 控制服务监听地址。Docker 部署必须监听所有网卡。 |
| `port` | `8990` | 控制容器内服务端口。Compose 已把宿主机端口映射到容器 `8990`。 |
| `apiKey` | 自己生成的强随机字符串 | 控制调用 `/v1`、`/cc/v1`、`/na/v1` API 时使用的客户端密钥。客户端请求要带 `Authorization: Bearer <apiKey>` 或 `x-api-key: <apiKey>`。 |
| `adminApiKey` | 自己生成的强随机字符串 | 控制管理后台和 `/api/admin/*` 的认证密钥。打开 `/admin` 后需要输入它。 |
| `tlsBackend` | `rustls` | 控制 HTTP 客户端 TLS 实现。Docker 镜像推荐 `rustls`。 |

密钥生成示例：

```bash
openssl rand -hex 32
```

### Kiro 上游环境模拟

| 字段名 | 建议值 | 控制什么 |
| --- | --- | --- |
| `region` | `us-east-1` | 控制默认 Kiro 区域。未单独配置 `authRegion` / `apiRegion` 时会用它。 |
| `authRegion` | 可不填 | 控制刷新 OAuth / IdC Token 的区域。只在需要和 API 区域分开时配置。 |
| `apiRegion` | 可不填 | 控制请求 Kiro API 的区域。只在需要和刷新区域分开时配置。 |
| `kiroVersion` | `0.11.107` | 控制请求上游时模拟的 Kiro IDE 版本。一般保持默认。 |
| `nodeVersion` | `22.22.0` | 控制请求上游时模拟的 Node 版本。一般保持默认。 |
| `machineId` | 可不填 | 控制全局机器 ID。通常建议让系统根据每个凭据自动派生，避免所有账号共用同一个机器 ID。 |
| `systemVersion` | 可不填 | 控制请求上游时模拟的系统版本。未填会使用内置默认值。 |

### 代理

| 字段名 | 建议值 | 控制什么 |
| --- | --- | --- |
| `proxyUrl` | 可不填 | 控制全局代理地址。支持 `http://`、`https://`、`socks5://`。 |
| `proxyUsername` | 可不填 | 控制全局代理用户名。 |
| `proxyPassword` | 可不填 | 控制全局代理密码。 |

如果某个凭据需要单独代理，可以在 `credentials.json` 里给该凭据配置 `proxyUrl`、`proxyUsername`、`proxyPassword`。如果某个凭据要明确不走全局代理，可以把该凭据的 `proxyUrl` 设置为 `direct`。

### 凭据调度

| 字段名 | 建议值 | 控制什么 |
| --- | --- | --- |
| `loadBalancingMode` | `priority` 或 `balanced` | 控制多凭据调度方式。`priority` 按优先级优先使用；`balanced` 会参考统计和预热状态做均衡选择。 |
| `credentialRpm` | `null` | 控制每个凭据的本地请求速率限制。`null` 或 `0` 表示关闭；大于 0 表示单个凭据每分钟最多请求次数。 |
| `credentialMaxConcurrentRequests` | `0` | 控制每个凭据最多同时处理多少个请求。`0` 表示不限制；大于 0 时，同一凭据占满后会优先把新请求分配给其他可用凭据。 |
| `credentialTransientCooldownSecs` | `10` | 控制上游临时错误但没有 `Retry-After` 时，单个凭据临时冷却多少秒。 |
| `credentialMaxCooldownSecs` | `300` | 控制临时冷却最长秒数，防止上游 `Retry-After` 过长导致账号长期不用。 |
| `credentialDispatchMaxWaitSecs` | `120` | 控制单个请求最多排队等待凭据可调度多久。`0` 表示不限制；超过后返回本地调度限流错误，避免客户端一直挂起。 |
| `credentialInFlightLeaseMaxSecs` | `900` | 控制单个并发占用超过多久未活跃时自动释放。`0` 表示关闭；用于兜底异常路径导致的账号并发槽长期占用。 |
| `credentialWarmupRequests` | `3` | 控制新凭据预热剩余请求数。预热不会伪造成功次数，只降低被选中的概率。 |
| `credentialWarmupSelectionPercent` | `5` | 控制 `balanced` 模式下预热凭据参与真实请求调度的概率百分比。 |
| `credentialsPersist` | `true` | 控制是否把 Token 刷新、禁用状态、优先级等变更写回 `credentials.json`。生产建议开启。 |
| `credentialStatsPersist` | `true` | 控制是否持久化账号成功次数、最后使用时间等调度统计。生产建议开启。 |
| `defaultEndpoint` | `ide` | 控制凭据未单独指定 `endpoint` 时走哪个 Kiro 端点。通常保持 `ide`。 |

### 上游请求压缩

| 字段名 | 建议值 | 控制什么 |
| --- | --- | --- |
| `compression.enabled` | `false` | 控制是否启用请求压缩。默认关闭，避免改变请求内容太多。 |
| `compression.whitespaceCompression` | `true` | 控制启用压缩时是否只做空白压缩。当前推荐只使用这个低风险压缩。 |

### 兼容模式

| 字段名 | 建议值 | 控制什么 |
| --- | --- | --- |
| `compatProfile` | `claude-code` | 控制 Anthropic 兼容策略。`claude-code` 适合 Claude Code CLI；`anthropic-strict` 尽量减少代理改写；`debug` 便于排查。 |
| `extractThinking` | `true` | 控制是否把非流式响应中的 `<thinking>...</thinking>` 提取成 Anthropic thinking 内容块。 |
| `exposeProxyWarnings` | `false` | 控制是否通过响应头 `x-kiro-rs-warnings` 暴露代理侧隐式改写统计。排查时可开启。 |

### 缓存路径行为

路径缓存模式是固定的，不再通过配置切换：

| 路径 | 行为 |
| --- | --- |
| `/v1/messages` | 高缓存模式。 |
| `/cc/v1/messages` | 高缓存模式，底层计算同 `/v1`；默认只由 `/cc` 路径覆盖项改写下游 input 和 cache write 上报。 |
| `/ha/v1/messages` | 高缓存模式，底层计算同 `/v1`；默认只由 `/ha` 路径覆盖项改写下游 input 上报。 |
| `/na/v1/messages` | 高缓存路由；默认由 `/na` 路径覆盖项关闭本地模拟 cache usage 补足，只保留真实上游 cache usage。 |

### 高缓存模拟

这些配置只影响本地 high-cache usage 模拟和下游 usage 展示，不改变上游真实请求内容。

| 字段名 | 建议值 | 控制什么 |
| --- | --- | --- |
| `promptCacheTargetReadRatio` | `0.98` | 控制高缓存模拟中 cache read 的目标比例中心。实际会自然浮动，不会每次精确等于 98%。 |
| `promptCacheTokenScale` | `1.6` | 控制高缓存模拟时 total input 的放大倍数。只影响 usage 模拟，不影响上游真实 metadata。 |
| `promptCacheMaxSimulatedInputTokens` | `300000` | 控制模拟 total input 的上限。 |
| `promptCacheCapJitterMinTokens` | `12000` | 控制触顶时 soft-cap 最小扣减值，避免每次固定卡在上限。 |
| `promptCacheCapJitterMaxTokens` | `24000` | 控制触顶时 soft-cap 最大扣减值。 |
| `promptCacheScaleMinInputTokens` | `20000` | 控制基础输入达到多少 token 后才启用放大，避免短请求被放大。 |

### 路径级下游 usage 上报

`reportedUsage` 只影响返回给下游和写入 usage record 的上报值，不影响本地 reader 计算、prompt-cache tracker、上游请求。配置先使用 `default`，再用 `pathOverrides` 按路径前缀做最长匹配覆盖；例如 `/cc` 会匹配 `/cc/v1/messages`。

| 字段名 | 建议值 | 控制什么 |
| --- | --- | --- |
| `reportedUsage.default` | 原样上报 | 控制所有路径的默认 input、output、cache read、cache write 上报方式。 |
| `reportedUsage.pathOverrides` | `/na`、`/cc`、`/ha` | 控制路径前缀覆盖策略。每个前缀独立配置，最长前缀优先。 |
| `mode: "preserve"` | 默认 | 原样上报该字段。 |
| `mode: "sample-max"` | input 可用 | 把字段采样到 `maxTokens` 以内，数值自然浮动，不固定到上限。 |
| `mode: "sample-target"` | cache write 可用 | 按 `targetTokens` 和 `normalMaxMultiplier` 生成自然分布。 |
| `moveDeltaToCacheRead` | input 建议 `true` | input 被压低的差值转入 cache read，只改变下游上报外观。 |

### Usage 记录和后台统计

| 字段名 | 建议值 | 控制什么 |
| --- | --- | --- |
| `usageRecordLimit` | `5000` | 控制内存中最多保留多少条请求级 usage 记录。 |
| `usageRecordPersist` | `true` | 控制是否把 usage 记录追加写入 `kiro_usage_records.jsonl`。 |
| `highCacheThreshold` | `10000` | 控制后台统计中“高缓存请求”的判定阈值。cache read 大于等于该值会被统计为高缓存请求。 |

### 模型价格

模型价格目录会在服务启动时自动初始化，也可以在后台手动同步。

| 行为 | 说明 |
| --- | --- |
| 启动自动同步 | 从公开价格目录同步项目关注的模型价格。 |
| 同步失败 | 不影响请求、不影响调度，只在后台价格状态里显示错误。 |
| 计费用途 | 只用于 usage record、凭据卡片、后台统计的估算费用展示。 |
| 展示单位 | 后台价格表使用 `$xx/M`，表示每百万 token 的美元价格。 |

## 7. credentials.json

`credentials.json` 放在 `/opt/kiro-rs/config/credentials.json`。支持数组格式，生产建议使用数组格式，因为数组格式支持 Token 刷新后回写、禁用状态回写、优先级保存。

### OAuth / Social 凭据示例

```json
[
  {
    "refreshToken": "你的-refresh-token",
    "authMethod": "social",
    "priority": 0,
    "endpoint": "ide"
  }
]
```

字段说明：

| 字段名 | 控制什么 |
| --- | --- |
| `refreshToken` | 控制 OAuth 刷新令牌。必须是真实完整 token，不能是被省略号截断的值。 |
| `authMethod` | 控制认证方式。Social 账号填 `social`。 |
| `priority` | 控制优先级。数字越小优先级越高。 |
| `endpoint` | 控制该凭据走哪个 Kiro 端点。通常填 `ide` 或不填。 |
| `disabled` | 控制凭据是否禁用。`true` 表示不参与调度。 |
| `machineId` | 控制该凭据独立机器 ID。可不填，让系统自动派生。 |
| `email` | 控制后台展示邮箱。可不填。 |
| `proxyUrl` | 控制该凭据专用代理。可填 `direct` 表示强制直连。 |

### IdC 凭据示例

```json
[
  {
    "refreshToken": "你的-refresh-token",
    "authMethod": "idc",
    "clientId": "你的-client-id",
    "clientSecret": "你的-client-secret",
    "region": "us-east-1",
    "priority": 1
  }
]
```

IdC 额外字段说明：

| 字段名 | 控制什么 |
| --- | --- |
| `clientId` | 控制 IdC token 刷新使用的客户端 ID。 |
| `clientSecret` | 控制 IdC token 刷新使用的客户端密钥。 |
| `region` | 控制该凭据默认区域。 |
| `authRegion` | 控制该凭据 token 刷新区。未填时回退到 `region` 或全局配置。 |
| `apiRegion` | 控制该凭据 Kiro API 请求区。未填时回退到全局配置。 |

### API Key 凭据示例

```json
[
  {
    "kiroApiKey": "ksk_xxxxxxxxxxxxxxxxx",
    "authMethod": "api_key",
    "priority": 0
  }
]
```

API Key 字段说明：

| 字段名 | 控制什么 |
| --- | --- |
| `kiroApiKey` | 控制 Kiro API Key。API Key 凭据不需要 `refreshToken`。 |
| `authMethod` | API Key 凭据填 `api_key`。 |
| `priority` | 控制优先级。数字越小优先级越高。 |

### 多凭据示例

```json
[
  {
    "refreshToken": "账号A-refresh-token",
    "authMethod": "social",
    "priority": 0,
    "email": "account-a@example.com"
  },
  {
    "refreshToken": "账号B-refresh-token",
    "authMethod": "idc",
    "clientId": "账号B-client-id",
    "clientSecret": "账号B-client-secret",
    "region": "us-east-1",
    "priority": 1,
    "email": "account-b@example.com"
  },
  {
    "kiroApiKey": "ksk_xxxxxxxxxxxxxxxxx",
    "authMethod": "api_key",
    "priority": 2,
    "email": "api-key-account"
  }
]
```

## 8. 启动

在 `/opt/kiro-rs` 执行：

```bash
docker compose up -d
```

查看容器状态：

```bash
docker compose ps
```

查看日志：

```bash
docker compose logs -f kiro-rs
```

## 9. 验证

### 验证 API 可用

把下面的 `sk-kiro-rs-change-me` 替换成 `config.json` 里的 `apiKey`：

```bash
curl -sS \
  -H 'Authorization: Bearer sk-kiro-rs-change-me' \
  http://127.0.0.1:8990/v1/models
```

预期返回 JSON 模型列表。

### 验证 `/cc/v1/models`

```bash
curl -sS \
  -H 'Authorization: Bearer sk-kiro-rs-change-me' \
  http://127.0.0.1:8990/cc/v1/models
```

预期返回 JSON 模型列表。

### 验证管理后台

浏览器打开：

```text
http://服务器地址:8990/admin
```

输入 `config.json` 里的 `adminApiKey`。

### 验证模型价格状态

把下面的 `sk-admin-change-me` 替换成 `config.json` 里的 `adminApiKey`：

```bash
curl -sS \
  -H 'x-api-key: sk-admin-change-me' \
  http://127.0.0.1:8990/api/admin/model-pricing
```

预期：

- `available` 为 `true`
- `modelCount` 为 `6`
- `source` 通常为 `litellm`，如果同步失败则可能是内置价格目录

## 10. 客户端配置

### Claude Code 使用 `/cc/v1`

建议 Claude Code 类客户端使用：

```text
base_url = http://服务器地址:8990/cc/v1
api_key = config.json 里的 apiKey
```

### 普通 Anthropic 兼容客户端使用 `/v1`

```text
base_url = http://服务器地址:8990/v1
api_key = config.json 里的 apiKey
```

### 真实 cache usage 上报路径

如果需要底层仍按高缓存计算，但下游只看真实上游 cache usage：

```text
base_url = http://服务器地址:8990/na/v1
api_key = config.json 里的 apiKey
```

## 11. 管理后台能力

后台地址：

```text
http://服务器地址:8990/admin
```

后台能力：

- 查看凭据状态
- 添加、禁用、删除凭据
- 测试单个凭据模型调用
- 批量导入凭据
- 导出凭据，支持 `json`、`backup-json`、`jsonl`
- 查看 usage 记录
- 查看估算费用和模型计价覆盖
- 手动同步模型价格
- 修改部分运行时配置

注意：导出凭据包含完整 `refreshToken`、`kiroApiKey`、代理密码等敏感字段，导出文件必须按密钥文件处理。

## 12. 升级

如果使用 `latest`：

```bash
cd /opt/kiro-rs
docker compose pull
docker compose up -d
```

如果固定版本，例如升级到 `0.0.17`：

```bash
cd /opt/kiro-rs
KIRO_RS_VERSION=0.0.17 docker compose pull
KIRO_RS_VERSION=0.0.17 docker compose up -d
```

建议生产使用固定版本，避免 `latest` 自动变化导致行为不确定。

## 13. 停止和重启

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

## 14. 数据文件

这些文件都在 `/opt/kiro-rs/config` 中：

| 文件名 | 控制什么 |
| --- | --- |
| `config.json` | 服务配置、API Key、Admin Key、调度策略、缓存模拟策略。 |
| `credentials.json` | 凭据列表。开启 `credentialsPersist` 后，Token 刷新和禁用状态会写回这里。 |
| `kiro_stats.json` | 凭据调度统计缓存。开启 `credentialStatsPersist` 后生成。 |
| `kiro_usage_records.jsonl` | 请求级 usage 记录。开启 `usageRecordPersist` 后生成。 |
| `kiro_balance_cache.json` | 后台余额查询缓存。 |

备份时至少备份：

```text
config/config.json
config/credentials.json
```

如果想保留历史统计，也备份：

```text
config/kiro_stats.json
config/kiro_usage_records.jsonl
```

## 15. 常见问题

### 端口能监听但宿主机访问不到

检查 `config.json`：

```json
"host": "0.0.0.0"
```

Docker 部署不能使用 `127.0.0.1` 作为容器内监听地址。

### 返回 401

检查客户端传入的密钥是否等于 `config.json` 里的 `apiKey`。

正确 header 示例：

```bash
Authorization: Bearer sk-kiro-rs-change-me
```

或：

```bash
x-api-key: sk-kiro-rs-change-me
```

### 管理后台无法登录

检查输入的是否是 `adminApiKey`，不是 `apiKey`。

### 添加 OAuth 凭据失败

检查 `refreshToken` 是否完整。被截断、有省略号、长度过短的 token 会被拒绝。

### 只有一个启用账号时一直 429

这通常是上游临时限流。当前版本会避免把“临时排除唯一可用凭据”误报成“所有凭据禁用”，并尽量在错误日志中带上凭据标识。建议：

- 在后台查看该凭据是否冷却、限流或禁用
- 等待冷却结束
- 不要短时间重复验活或余额刷新

### 模型价格同步失败

模型价格只用于统计展示，不影响调度和请求。失败时服务会继续使用当前价格目录或内置价格目录。可以在后台“价格”页面手动同步。

### `/v1`、`/cc/v1`、`/na/v1` 应该选哪个

| 路径 | 使用场景 |
| --- | --- |
| `/cc/v1` | Claude Code CLI 或类似客户端。 |
| `/v1` | 普通 Anthropic 兼容客户端，默认高缓存模拟。 |
| `/na/v1` | 想只上报真实上游 cache usage 时使用。 |

## 16. 最小可用部署命令汇总

```bash
mkdir -p /opt/kiro-rs/config
cd /opt/kiro-rs

cat > docker-compose.yml <<'YAML'
services:
  kiro-rs:
    image: ${KIRO_RS_IMAGE:-ghcr.io/2ue/kiro-rs}:${KIRO_RS_VERSION:-0.0.17}
    container_name: kiro-rs
    restart: unless-stopped
    ports:
      - "${KIRO_RS_PORT:-8990}:8990"
    volumes:
      - ./config:/app/config
    extra_hosts:
      - "host.docker.internal:host-gateway"
YAML
```

继续写入 `config/config.json` 和 `config/credentials.json` 后启动：

```bash
docker compose up -d
docker compose logs -f kiro-rs
```
