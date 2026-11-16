# 08 管理 API 与控制台

## 1. 管理 API

前缀 `/api/admin`，鉴权中间件不变。

### 1.1 删除

`/credentials*`（全部约 40 条）、`/inventory*`、`/proxy-resources*`、`/subscription-model-lists*`、`/config/kiro-version/*`、`/config/load-balancing`、`/public-pool`、`/api/public/pool`、`/model-capabilities/sync`、`/usage-dashboard/accounts`（Kiro 账号看板）、`/usage-dashboard/overview/local`、`/credentials/{id}/diagnostics`。

### 1.2 改名

| 现有 | 新 |
|---|---|
| `/external-pools` 及全部子路径 | `/accounts` |
| `/external-pools/supported-models/discover` | `/accounts/supported-models/discover` |
| `/usage-dashboard/external-pool-billing` | `/usage-dashboard/account-billing` |
| `/usage-dashboard/external-pool-risk` | `/usage-dashboard/account-risk` |
| `/usage-dashboard/overview/external` | `/usage-dashboard/overview`（合并，只剩一个分区） |

### 1.3 新增

| 端点 | 作用 |
|---|---|
| `GET/PUT /routes` | 路由定义列表的读写（替代分散在 `/config/runtime` 里的五张 pathOverrides 表） |
| `GET /routes/{prefix}/effective` | 该路由的最终生效配置及每个字段来自哪一层（global / route 默认 / route 显式） |
| `POST /accounts/{id}/models/promote` | 把账号发现的模型加入目录 |
| `POST /config/import-package`、`GET /config/export-package` | 配置包导入导出（`feat/config-package` 已有，扩展为新结构） |
| `GET /accounts/{id}/effective` | 账号最终生效的调度与传输参数及来源层 |

### 1.4 `/config/runtime` 的契约变化

`RuntimeConfigResponse` / `UpdateRuntimeConfigRequest`（`admin/types.rs:1897-2338`）里五个非 Option 的 Kiro 必填字段（`credentialRpm`、`credentialTransientCooldownSecs`、`credentialMaxCooldownSecs`、`credentialWarmupRequests`、`compressionEnabled`）删除；`externalPools` 字段改 `accounts`；`cachePolicy.pathOverrides`、`reportedUsage.pathOverrides`、`promptInjection.pathOverrides`、`routeFeatures.pathOverrides`、`definedCacheRoutes` 从这个接口移出到 `/routes`。前端的 runtime 页只保存自己的 schema。

## 2. 控制台（`ui/`）

### 2.1 导航

| 路由 | 页面 | 变化 |
|---|---|---|
| `/overview` | 总览 | 删 local 分区；展示账号池用量、报告量、原始量、账号成本、差额 |
| `/accounts` | 账号 | 原 `/external-pools`。列表、创建/编辑、测试、模型发现、清冷却、清自动禁用；表单删 `requestBodyMode`、`rawModelMode`、`preservePath`，新增 `tags`、`proxyUrl`、`costMultiplier`、覆盖参数折叠区 |
| `/routes` | 路由 | **新页面**。每条路由一行：前缀、名称、启用、缓存策略类型、注入项数、功能开关数、允许账号数。编辑页分五个分区：缓存与 usage（复用现有 `cache-policies/` 表单）、提示词注入（复用 `prompt-injection/`）、路由功能、账号规则、模型规则。原 `/cache-policies` 与 `/prompt-injection` 两个独立页面降为路由编辑页的分区，顶层导航保留"策略模板"入口用于编辑全局模板默认值 |
| `/route-scenarios` | 路由场景 | 删本地列 |
| `/usage` | 用量 | 列名 `credential` → `account`；详情里 raw / shaped / reported 三份 usage 与四种 cost 不变 |
| `/account-risk` | 账号风险 | 原 `/external-pool-risk` |
| `/models` | 模型 | 删订阅等级列表与"按账号同步"；新增 `listed` 开关、"从账号发现结果加入目录" |
| `/runtime` | 运行配置 | 只剩：准入、账号池（容量、重试、质量、自动禁用、冷却、超时、uplift、诊断）、请求兼容、payload、模型映射、代理与 TLS、审计保留、usage 保留 |
| `/security` | 密钥 | 新增 `allowedRoutes` |
| `/audit` | 审计 | 不变 |
| 删除 | `/credentials`、`/inventory`、`/proxies`、`/pool`、`/validation` | |

### 2.2 显示约定（C11）

精确时间 `10/25 21:15:05`；key 中间省略；不用色块显示数字；mock 数据符合真实规则。沿用现有组件库（`ui/src/components/ui`、`patterns`），不引入新的 UI 框架。

### 2.3 代码整理

`api/credentials.ts`（663 行）、`hooks/use-credentials.ts`、`types/api.ts`（3237 行）按 accounts / routes / runtime / security / usage / models 拆分；`lib/credential-*`、`lib/kiro-api-keys.ts`、`components/credential-tags-field.tsx` 删除。`ui/tests/` 中涉及凭据页的测试删除，其余更新字段名。
