# 任务 s7：管理 API 与控制台（阶段 7）

## 目标

落实设计 `docs/claude-pool-refactor/08-管理API与控制台.md`。后端补齐路由、账号覆盖、模型 listed、配置导出导入的管理端点；前端删除无后端支持的页面，新增路由页，账号页改造，API/hooks/types 按领域拆分。前置：阶段 3/4/5 已完成（名字已是 `accounts`、`routes[]`、`AccountOverrides`、`listed`）。

## 先读

- 设计 08 章全文；03 章第 4 节（路由定义字段）、第 5 节（分层）
- 后端：`src/admin/router.rs`（现有端点）、`src/admin/service.rs`、`src/admin/types.rs`、`src/admin/route_scenarios.rs`、`src/model/routes.rs`（阶段 4 产物）、`src/model/runtime.rs`
- 前端：`ui/src/app/router.tsx`、`ui/src/types/ui.ts`（导航）、`ui/src/api/credentials.ts`（663 行，名不副实，含全部 API）、`ui/src/hooks/use-credentials.ts`、`ui/src/types/api.ts`（3237 行）、`ui/src/features/{accounts,cache-policies,prompt-injection,runtime,route-scenarios,models,usage,overview,security,audit}/`、`ui/src/components/{ui,patterns,charts}/`
- 前端约束：只用 `@/components/ui`（Radix/shadcn 封装）与 `@/components/patterns`，不用原生 `<button>`/`<select>`/`<dialog>`；图标用 lucide；显示约定见 11 章术语与 08 章 2.2 节（精确时间 `10/25 21:15:05`、key 中间省略、不用色块显示数字）

## 允许改动的文件

`src/admin/**`、`src/model/routes.rs`（只加序列化/校验辅助）、`src/storage/postgres.rs`（只加配置导出导入所需的读写函数）、`src/main.rs`、`ui/**`（`ui/node_modules` 除外）。不改调度、整形、入口链路。

## A. 后端端点

### 删除
`/usage-dashboard/overview/external`（并入 `/usage-dashboard/overview`，返回里不再有 local/external 分区）。

### 新增

| 端点 | 作用 |
|---|---|
| `GET /routes` | `RoutesConfig` 全量（含内置四条） |
| `PUT /routes` | 全量替换，走 `RuntimeConfigStore::update`，校验：前缀归一化去重、内置不可删、`enabled` 等；返回归一化后的全量 |
| `GET /routes/effective?prefix=/cc` | 该路由最终生效的 `ResolvedCacheRoutePolicy`、`RouteFeatureSet`、`ResolvedInjections`、`accountRule`、`modelRule`，以及每个字段来源层（`builtin_default` / `global_template` / `route`） |
| `GET /accounts/{id}/effective` | 账号合并后的调度与传输参数（`AccountOverrides` 覆盖全局），每字段标来源（`global` / `account`） |
| `POST /accounts/{id}/models/promote` | 阶段 4 已实现，若无则补：账号 `supported_models` 中不在目录的模型加入目录（`listed=false`，`source=account_discovery`） |
| `PATCH /model-capabilities/{model}/listed` | 设置 `listed` |
| `GET /config/export?include_secrets=true|false` | 新系统配置导出：`{formatVersion, exportedAt, accounts[], runtimeConfig（去掉 postgres/redis/observabilityRedis/host/port）, routeScenarios[], modelPricing[], manualModels[], requestApiKeys[]}`；`include_secrets=false` 时账号 `apiKey`、`requestApiKeys[].apiKey`、`adminApiKey` 替换为 `"<redacted>"` |
| `POST /config/import?dry_run=true|false` | 自然键（账号 `name`、路由场景 `name`、模型 `model`、密钥 `name`）全量覆盖，单事务；`dry_run=true` 返回差异报告（新增/覆盖/删除的条目列表）；含 redacted 值的包拒绝非 dry_run；成功后发 `events:runtime_config_changed` 与 `events:account_data_changed` |

### 修改
- `GET/PUT /config/runtime`：响应与请求体里**不再包含** `routes`、`cachePolicy.pathOverrides`、`reportedUsage.pathOverrides`、`promptInjection.pathOverrides`、`routeFeatures.pathOverrides`（这些由 `/routes` 管）；保留全局模板默认值与其他字段。`UpdateRuntimeConfigRequest` 的字段只剩阶段 2 后的 Config 字段（去掉 Kiro 必填项后应全部是 `Option`）。
- 账号创建/更新请求：加 `tags`、`proxyUrl`、`costMultiplier`、`overrides`（阶段 4 若已加则确认），`proxyUrl` 返回时掩码用户名密码。
- 审计日志：`/routes` PUT、`/config/import` apply、`listed` 变更写审计。

## B. 前端

### 删除
`ui/src/features/{credentials,inventory,proxies,public-pool}/`（阶段 5 若已删则跳过）、`ui/src/api/{inventory.ts,public-pool.ts}`、`ui/src/hooks/use-inventory.ts`、`ui/src/lib/credential-*`、`ui/src/lib/kiro-api-keys.ts`、`ui/src/components/credential-tags-field.tsx`、`ui/tests/{credential-*,kiro-api-keys}.test.ts`、`features/models/subscription-model-lists.tsx`、`features/runtime/version-endpoint-section.tsx`；router 与 `types/ui.ts` 导航同步删 `/pool`、`credentials`、`inventory`、`validation`、`proxies`。

### 拆分
`ui/src/api/credentials.ts` → `api/{accounts,routes,runtime,security,route-scenarios,models,usage-dashboard,config-transfer}.ts`；`hooks/use-credentials.ts` → `hooks/{use-accounts,use-routes,use-runtime-config,use-security,use-models}.ts`；`types/api.ts` → `types/{accounts,routes,runtime,security,usage,models,common}.ts` 并在 `types/api.ts` 里 re-export 以减少改动面。删除所有引用已删后端功能的类型（`Credential*`、`Inventory*`、`Proxy*`、`PublicPool*`、`Kiro*`、`SubscriptionModelList*`）。

### 新增页面 `/routes`（`features/routes/`）
- 列表：每条路由一行：前缀、名称、启用开关、缓存策略类型、注入项数、开启的功能数、允许账号数（`accountRule` 为 allow_all 时显示“全部”）、操作（编辑、复制为新路由、删除；内置路由删除按钮禁用并提示）。
- 编辑页（整页，不是抽屉；路由 `/routes/:prefix`，根路由用 `/routes/_root`）：五个 Tab：
  1. **缓存与 usage**：复用 `features/cache-policies/` 现有表单组件（把它改成接收单条路由 patch 的受控组件），保留六种策略的专属参数区与 reportedUsage 字段表。
  2. **提示词注入**：复用 `features/prompt-injection/route-injection-editor.tsx`。
  3. **路由功能**：阶段 3 的 8 个功能各一个开关卡片，开启后展开参数（参数表单从 `runtime-sections.tsx` 对应节搬过来：payload、requestCompat、imageProcessing、missingMaxTokens）。
  4. **账号规则**：三态 mode + 账号多选（从 `/accounts` 列表取，显示 name 与 id）。
  5. **模型规则**：三态 mode + 规则列表（支持尾部 `*`），右侧显示“当前可用账号支持的模型”供点选。
  - 顶部“查看生效配置”按钮调 `/routes/effective` 用抽屉（Sheet）展示每字段来源。
- 原 `/cache-policies` 与 `/prompt-injection` 顶层导航改为“策略模板”一个入口（`features/policy-templates/`），只编辑全局六个缓存模板默认值、reportedUsage 默认、注入默认开关与文案；不再有按路径的表。

### 改造 `/accounts`（原 external-pools）
- 表单删 `requestBodyMode`、`rawModelMode`、`preservePath`（阶段 3/5 若已删则确认）；新增 `tags`（标签输入）、`proxyUrl`、`costMultiplier`、折叠区“覆盖全局参数”（`AccountOverrides` 的 9 个 `Option` 字段，未设置显示全局值占位）。
- 列表加标签筛选；详情加“查看生效参数”（调 `/accounts/{id}/effective`）。
- 模型发现结果旁加“加入目录”按钮（调 `/accounts/{id}/models/promote`）。

### 改造其他页
- `/runtime`：只剩分节：请求准入、账号池（容量/重试/质量/自动禁用/冷却/超时/uplift/诊断，字段名用去前缀后的 `accounts.*`）、模型解析、代理与 TLS、审计保留、usage 保留。删 loadBalancing、cooldown、scheduler、warmup、capacity、compat、versionEndpoint 节；`payload`/`requestCompat` 节的内容已搬到路由功能 Tab，这里只保留“全局默认参数”说明与跳转。
- `/overview`：删 local 分区；展示报告量、上游原始量、报告估值、原始估值、账号成本（`account_cost_usd` 之和）、差额。
- `/usage`：列名 `credential` → `account`；详情三份 usage 与四种 cost 不变；新增账号成本列。
- `/models`：加 `listed` 开关列；加“从账号发现加入目录”入口。
- `/security`：请求密钥加 `allowedRoutes` 多选（若后端 `RequestApiKeyPolicy.allowed_routes` 阶段 4 未加，这里一起加：`src/model/config.rs` 加 `allowed_routes: Vec<String>`，鉴权中间件在路由解析后检查，不在列表内返回 403 `permission_error`）。
- `/route-scenarios`：删本地列，只剩账号规则快照。
- 新增 `/settings/transfer`（`features/config-transfer/`）：导出按钮（含/不含密钥）、导入：选文件 → dry-run 报告（表格：类型/自然键/动作）→ 确认应用。

### 显示约定
时间 `MM/DD HH:mm:ss`；key、邮箱中间省略（现有 `middle-truncate`）；数字不加背景色块；空状态用 `EmptyState`；所有按钮用 `Button`，弹窗用 `Dialog`/`Sheet`。

## 测试

- 后端：`/routes` 校验与 effective 来源标注的单测；`/config/export` 与 `/config/import?dry_run=true` 的 round-trip 单测（导出 → 导入 dry-run 应为“全部无变化”）；`allowed_routes` 403 测试。
- 前端：`pnpm --dir ui check && pnpm --dir ui build`；`ui/tests/` 为纯函数测试（vitest 若未配置则用现有方式），为 `routes` 的前缀归一化/排序显示、`account-rules` 三态文案各加一个。
- 浏览器走查由我在验收时做，你不需要起服务。

## 自检

```
cargo fmt --all
cargo build -q --bins
cargo test --bin claude-pool -q -- admin
cargo test --bin claude-pool -q -- golden_usage
cargo test --bin claude-pool -q 2>&1 | tail -30
pnpm --dir ui check && pnpm --dir ui build
grep -rn "credentials\|Credential\|inventory\|Inventory\|public-pool\|PublicPool\|proxy-resources" ui/src --include=*.ts --include=*.tsx | grep -v "request_api_key\|requestApiKey\|RequestApiKey" | head   # 应为空
test ! -d ui/src/features/credentials && test -d ui/src/features/routes && echo "pages ok"
```
