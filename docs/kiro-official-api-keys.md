# Kiro 官方 API Key 管理

账号页的“官方 Key 管理”支持给 OAuth / JSON 导入账号新增官方 `ksk_...` 密钥、撤销密钥和导出文件。批量自动跳过以 API Key 导入的账号，管理接口也会拒绝这些账号创建、保存或删除 Key。

## 使用

1. 在账号页勾选一个或多个账号，打开“官方 Key 管理”。未勾选时使用全部账号。
2. 单个模式可选择来源账号、填写密钥名称；允许给同一账号新增多个 Key。批量逐个创建，名称追加账号 ID，单条失败继续，可停止后续创建。
3. 创建成功后，完整 Key、官方 Key ID、名称、服务区域、Profile ARN 和创建时间保存在独立的 `credential_kiro_api_keys` 表中，关联来源账号。刷新页面、关闭再打开或重启服务后可以重新读取。
4. 管理列表显示邮箱、密钥名称、服务区域、创建时间和保存状态；完整明文默认折叠，可以展开。单个模式只导出当前账号，批量模式导出当前范围。
5. 点击“删除 Key”后确认，服务先调用官方 `DeleteApiKey` 撤销，再删除本地关联记录。官方已经不存在的 Key 视为撤销完成。上游失败保留本地记录；官方撤销成功但本地清理失败时，可以重试删除。

来源账号始终保留原 OAuth 认证：生成结果不会写入来源账号 `kiroApiKey`、`authMethod` 或 `api_key_hash`，也不会自动加入调度账号池。此功能不读取或回填历史官方 Key。

## 导出格式

默认导出 `.txt`，每行一个完整 Key。还可以选择：

- 邮箱 + Key：TXT，以制表符分隔。
- Key + 服务区域：TXT，每行 `ksk_完整密钥----区域`。
- CSV：账号 ID、邮箱、Key、密钥名称、服务区域、创建时间，包含 UTF-8 BOM。
- JSON：完整 Key 元数据。

纯 Key、Key + 区域和 JSON 可被现有账号导入器识别。纯 Key 不携带区域；需要保留区域时选择 Key + 服务区域或 JSON。

## 管理接口

全部沿用 Admin API 认证，包含明文的成功响应设置 `Cache-Control: no-store`。

- `POST /api/admin/credentials/kiro-api-key`：`{credentialId,label}`，创建并保存一个 Key。
- `POST /api/admin/credentials/kiro-api-keys/batch`：`{credentialIds,labelPrefix}`，串行批量创建；ID 去重，单次最多 500 个，省略 ID 使用全部账号。
- `POST /api/admin/credentials/kiro-api-keys/list`：`{credentialIds,offset}`，返回已保存关联记录，每页最多 500 条，仅列出 OAuth 来源账号。
- `POST /api/admin/credentials/kiro-api-keys/save`：重新保存创建结果，不发起官方创建请求。
- `POST /api/admin/credentials/kiro-api-keys/delete`：`{credentialId,keyId}`，根据本地关联记录撤销官方 Key，再移除记录。不能删除其他账号的关联记录。

创建结果包含 `credentialId`、`email`、`keyId`、`keyPrefix`、`apiKey`、`label`、`region`、`profileArn`、`createdAt`、`persisted` 和可选 `warning`。

## 故障边界

创建前先检查独立存储可用性，失败时不发起官方创建。创建完成但存储失败时仍返回完整 Key，标记 `persisted=false` 并显示原因；页面提供“重试保存”和导出，不能自动重复创建。上游缺少官方 Key ID 时同样保留完整明文供导出，但不能伪造 ID 进行删除。

管理存储操作具有 5 秒超时。创建和删除不进入推理的调度选中、冷却、禁用、失败计数或并发占用路径；错误只影响对应管理请求。审计异步尽力写入，仅记录名称、区域、保存状态或批量数量，不写入完整 Key。

控制面参考 `../kiro-manager-lite/src/main/kiroApiKey.ts`：`POST https://management.{region}.kiro.dev/`，OAuth Bearer token，`x-amz-target: KiroControlPlaneBearerService.CreateApiKey / DeleteApiKey`，请求体含 `profileArn`。服务区域归并为 `us-east-1` 或 `eu-central-1`，删除使用创建时保存的区域和 Profile ARN。

401 至多刷新一次，明确的 Profile ARN 拒绝允许换候选；网络错误、超时、429/5xx、成功响应解析失败均不会自动重发创建请求。新增表为纯 schema 扩展，没有历史数据回填。迁移关闭且表不存在时，管理接口返回存储不可用，不导致核心服务启动失败。

## 验证

后端定向测试覆盖完整值解析、Profile ARN、401 刷新、Profile 拒绝切换、429 不重试、官方删除及已不存在场景、Key 导入账号拒绝、调度状态不变，以及真实 PostgreSQL 下跨连接读取、幂等保存、账号隔离、重复删除和来源 OAuth 字段保持不变。存储覆盖 idc、social、external_idp 和 oauth 各类来源，拒绝 api_key 类型。

前端测试覆盖默认纯 Key、多种格式、导入往返、API Key 账号识别、批量失败继续和停止保留结果。所有官方请求测试使用模拟上游，不操作真实官方账号。

本次验证：Rust 1.92.0 下 5 项官方 Key 定向测试和 PostgreSQL dashboard/storage 发布 smoke 全部通过；Clippy 基线检查通过（740，基线上限 849），无默认特性的 all-targets 编译通过。前端现有 78 项测试全部通过。浏览器在独立的本地模拟页面中验证了批量跳过、单个追加、刷新读取、默认纯 Key 文件内容、删除确认、保存失败提示及重试保存；控制台没有错误。浏览器模拟不代表真实官方控制面已实测。
