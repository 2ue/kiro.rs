# AWS API Key + Region 非 Docker Runner 合同

Date: 2026-07-21

Scope: F06 `aws-api-key-region-lifecycle.mjs` 验证程序的运行时安全边界。该证据只证明 runner 不再依赖 Docker/隐式 Cargo/root target/破坏性 Redis 操作；不证明最终 frozen candidate 的 F06 产品动态门禁已通过。

## 结论

`feature/tests/aws-api-key-region-lifecycle.mjs` 已从旧的 Docker-managed PostgreSQL/Redis runner 改为 caller-owned runtime runner：

- 要求 `KIRO_RS_BINARY` 和 `KIRO_VALIDATION_ARTIFACT_DIR` 为仓库外绝对真实路径，由 `runtime-validation-paths.mjs` 统一 fail closed。
- 要求 `KIRO_F06_POSTGRES_URL` 指向 loopback、端口非 `9022`、database 名称匹配 `kiro_f06_*`。
- 要求 `KIRO_F06_REDIS_URL` 指向 loopback Redis DB1..15，拒绝 DB0、auth、query、fragment 和 `9022`。
- 要求 `KIRO_F06_REDIS_PREFIX` 为调用方临时 owned prefix，拒绝 `kiro_rs:local`。
- 不启动 Docker、不创建 PostgreSQL database、不 `FLUSHDB`/`FLUSHALL`、不调用 Cargo、不探测已有 `9022` listener。
- 服务子进程使用最小环境，只传入 `RUST_LOG`、`KIRO_API_KEY`、`KIRO_RS_HOST`、`KIRO_RS_PORT`，不继承 caller `process.env` 中的 PG/Redis/secret 变量。
- Redis 清理只扫描和删除 `${KIRO_F06_REDIS_PREFIX}:*`，不清空调用方 database。
- PostgreSQL 查询改为调用本机 `psql`，用于动态 gate 时只检查/读取 caller-owned database；runner 自身不创建或删除 database。

## 本轮测试命令与结果

```bash
node --check feature/tests/aws-api-key-region-lifecycle.mjs
```

Result: pass.

```bash
node --test feature/tests/aws-api-key-region-lifecycle.contract.test.mjs
```

Result: `6/6` pass.

覆盖项：

- JavaScript syntax。
- validate-only 接受 caller-owned loopback PostgreSQL/Redis，输出 `dockerUsed=false`、`cargoUsed=false`、`createsPostgresDatabase=false`、`flushesRedisDatabase=false`。
- 早拒绝 PostgreSQL/Redis `9022`。
- 早拒绝 Redis DB0 和共享 `kiro_rs:local` prefix。
- 早拒绝非 loopback dependency 和非 `kiro_f06_*` database。
- source contract 禁止 Docker/Cargo 调用、`CREATE DATABASE`、`FLUSHDB/FLUSHALL`、`...process.env` 继承。

```bash
node --test \
  feature/tests/aws-api-key-region-lifecycle.contract.test.mjs \
  feature/tests/runtime-validation-paths.test.mjs \
  feature/tests/external-takeover-scheduler-degraded-nondocker.contract.test.mjs \
  feature/tests/scheduler-fairness-sticky-race.contract.test.mjs \
  feature/tests/strict-local-first-routing.contract.test.mjs
```

Result: `36/36` pass.

该合批确认 F06 与现有 runtime path、external takeover、E01/E02、E05 runner 合同兼容。测试不启动 Docker、不运行 Cargo、不启动 kiro.rs 服务、不触碰 `9022`。

```bash
git diff --check -- \
  feature/tests/aws-api-key-region-lifecycle.mjs \
  feature/tests/aws-api-key-region-lifecycle.contract.test.mjs
```

Result: pass.

## 动态执行前置条件

完整 F06 动态门禁仍需要调用方提供：

```bash
KIRO_RS_BINARY=/abs/outside/repo/frozen/kiro-rs \
KIRO_VALIDATION_ARTIFACT_DIR=/abs/outside/repo/artifacts \
KIRO_F06_POSTGRES_URL='postgres://...@127.0.0.1:<pg-port>/kiro_f06_<owned_empty_db>' \
KIRO_F06_REDIS_URL='redis://127.0.0.1:<redis-port>/<nonzero-db-1-15>' \
KIRO_F06_REDIS_PREFIX='kiro_rs:f06:<unique-owned-prefix>' \
KIRO_F06_ROUNDS=3 \
node feature/tests/aws-api-key-region-lifecycle.mjs
```

调用方必须在 runner 外部创建并最终 drop 独占 PostgreSQL database，并在提取脱敏摘要后删除仓库外 artifact root。runner 只清理自己的临时文件、子进程和 owned Redis prefix。

## 未关闭项

- 未运行完整 F06 dynamic service gate；因此 CRD-001 仍是 `pending`。
- 未绑定最终 frozen release candidate SHA。
- 未完成两套 UI browser import/export warning gate。
- 未完成多实例同时重复导入的 auxiliary admission gate。
- 未覆盖真实 AWS/Kiro 官方 upstream；F06 核心 lifecycle 仍以 fake upstream 验证 region Host、Bearer 和 `tokentype=API_KEY`。
