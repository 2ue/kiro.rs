# P1 - 114 升级后 schema 未迁移导致备用池/总览异常

## 现象

生产从 113 升级到 114 后：

- 备用池页面表现为数据不见或加载异常；
- Dashboard 页面显示“总览加载失败 / error returned from database”；
- app 容器健康检查仍为 healthy，容易误判为业务数据丢失。

## 生产证据

已在生产目标上复核：

- 运行镜像版本为 `0.0.114`，revision 为 `18b286efa47759b95b581f76a465a2bd9cb02983`。
- 挂载配置中 `postgres.migrateOnStart=false`，Compose 初始没有环境变量覆盖。
- 114 代码运行路径需要的新列不存在时会报错，例如：
  - `external_upstream_pools.revision`
  - `usage_records.rollup_active`
  - `model_capabilities_sync_status.reasoning_fields`
- 备用池数据没有删除。迁移前已查到 `external_upstream_pools` 中仍有 12 条记录，其中 11 条 active、3 条 enabled active。

## 根因

`PostgresStore::connect` 旧逻辑只在 `postgres.migrateOnStart=true` 时执行迁移；如果挂载配置显式关闭迁移，当前二进制会在旧 schema 上继续启动，并在后续 admin/API/usage 查询中才触发 SQL 错误。

这造成两个问题：

1. 健康检查只证明进程和依赖可连，不证明 schema 与二进制兼容。
2. 数据仍在，但查询新列失败，表现成“备用池丢失”或 Dashboard 加载失败。

## 复现方法

本地复现不需要真实生产数据：

1. 准备一个旧 schema 或在隔离测试 schema 中删除 `external_upstream_pools.revision`。
2. 配置 `postgres.migrateOnStart=false`。
3. 启动旧逻辑二进制。
4. 访问 `/api/admin/external-pools` 或写入/查询 usage，会得到数据库列不存在错误。

新增测试：

- `required_postgres_schema_columns_cover_known_upgrade_breakers`
- `required_postgres_schema_missing_columns_reports_table_and_column`
- `postgres_schema_compatibility_check_rejects_missing_upgrade_column`

## 修复方案

### 1. 启动兼容检查

`PostgresStore::connect` 在可选迁移之后无条件执行轻量 schema compatibility check：

- 只查 `information_schema.columns`；
- 不扫描业务数据；
- 缺表/缺列时直接拒绝启动；
- 错误信息明确提示设置 `KIRO_RS_POSTGRES_MIGRATE_ON_START=true` 或 `postgres.migrateOnStart=true`。

这样其它现网实例如果仍挂载旧配置关闭迁移，不会再以 healthy 状态带旧 schema 提供服务。

### 2. 部署模板显式开启迁移

`docker-compose.database.yml` 增加：

```yaml
KIRO_RS_POSTGRES_MIGRATE_ON_START: ${KIRO_RS_POSTGRES_MIGRATE_ON_START:-true}
```

部署文档和 README 同步说明生产升级必须开启启动迁移，除非已通过其它维护流程完成当前镜像要求的 schema 迁移。

### 3. Dashboard 降重

生产中 schema 修复后，`/api/admin/usage-dashboard` 仍可能因为一次性聚合所有窗口 breakdown 和外部池逐池拆分接近或超过 5s。

修复：

- 后端总接口只返回基础窗口、series、top；
- 高成本 breakdown 和 external pool billing by pool 继续使用已有按选中窗口的拆分接口；
- 旧 admin-ui 改为拆分加载，避免一个慢分片拖垮整个页面。

## 验证

- 生产手动迁移后：
  - 关键列存在；
  - `/api/admin/external-pools` 返回 200；
  - 备用池数据数量仍在；
  - Dashboard 拆分接口 `/windows`、`/series`、`/top` 返回 200。
- 本地：
  - Rust schema guard 单测通过；
  - admin-ui tsc/build 通过；
  - 新 UI tsc/build 通过。

