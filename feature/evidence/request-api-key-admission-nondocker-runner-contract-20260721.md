# Request API Key Admission Multi-Instance 非 Docker Runner 合同

Date: 2026-07-21

Scope: `feature/tests/request-api-key-admission-multi-instance.mjs` 验证程序的运行时安全边界。该证据只证明 runner 已从 Docker/PostgreSQL/Redis/Toxiproxy 自管理模式改为 caller-owned PostgreSQL/Redis + 本地 Node Redis proxy；不证明最终 frozen candidate 的多实例 admission 产品动态门禁已通过。

## 结论

`request-api-key-admission-multi-instance.mjs` 已完成 runner 层安全改造：

- 要求 `KIRO_RS_BINARY` 和 `KIRO_VALIDATION_ARTIFACT_DIR` 为仓库外绝对真实路径，由 `runtime-validation-paths.mjs` 统一 fail closed。
- 要求 `KIRO_REQUEST_ADMISSION_POSTGRES_URL_TEMPLATE` 使用 loopback PostgreSQL，包含且只包含一个 `{database}` placeholder。
- 要求 `KIRO_REQUEST_ADMISSION_POSTGRES_DATABASES` 数量等于 `KIRO_REQUEST_ADMISSION_ROUNDS`，且每个名称匹配 `kiro_request_admission_*`；runner 不创建 database。
- 要求 `KIRO_REQUEST_ADMISSION_REDIS_URL` 使用 loopback Redis DB1..15，拒绝 DB0、auth、query、fragment 和 `9022`。
- 要求 `KIRO_REQUEST_ADMISSION_REDIS_PREFIX` 为调用方临时 owned prefix，拒绝 `kiro_rs:local`。
- 使用 `feature/tests/redis-chaos-proxy.mjs` 替代 Docker Toxiproxy；latency toxic 走 `/proxies/redis/toxics`，`reset_peer` cell 映射为 proxy disabled。
- 不启动 Docker、不创建 PostgreSQL database、不 `FLUSHDB`/`FLUSHALL`、不调用 Cargo、不使用 `host.docker.internal`、不探测已有 `9022` listener。
- 两个 kiro.rs 服务子进程使用最小环境，不继承调用方整份 `process.env`。
- 每个 outer round 两个实例共享同一个 caller-owned database 和同一个 round-specific Redis `keyPrefix`。
- 结束只扫描并删除 `${KIRO_REQUEST_ADMISSION_REDIS_PREFIX}:*` owned keys，不清空 Redis database。

## 本轮测试命令与结果

```bash
node --check feature/tests/request-api-key-admission-multi-instance.mjs
node --test feature/tests/request-api-key-admission-multi-instance.contract.test.mjs
```

Result: `5/5` pass.

覆盖项：

- JavaScript syntax。
- validate-only 接受 caller-owned loopback PG/Redis，输出 `dockerUsed=false`、`cargoUsed=false`、`createsPostgresDatabase=false`、`flushesRedisDatabase=false`、`usesDockerToxiproxy=false`。
- database list 数量必须等于 rounds，并拒绝非 `kiro_request_admission_*` 名称。
- 早拒绝非 loopback PostgreSQL/Redis、`9022`、Redis DB0 和共享 `kiro_rs:local` prefix。
- source contract 禁止 Docker/Cargo 调用、`CREATE DATABASE`、`FLUSHDB/FLUSHALL`、`host.docker.internal`、`listenerSnapshot` 和 `...process.env` 继承，并要求使用 `redis-chaos-proxy.mjs`。

```bash
node --test \
  feature/tests/request-api-key-admission-multi-instance.contract.test.mjs \
  feature/tests/aws-api-key-region-lifecycle.contract.test.mjs \
  feature/tests/runtime-validation-paths.test.mjs \
  feature/tests/external-takeover-scheduler-degraded-nondocker.contract.test.mjs \
  feature/tests/scheduler-fairness-sticky-race.contract.test.mjs \
  feature/tests/strict-local-first-routing.contract.test.mjs
```

Result: `41/41` pass.

该合批确认 request-admission、F06、runtime path、external takeover、E01/E02 和 E05 runner 合同兼容。测试不启动 Docker、不运行 Cargo、不启动 kiro.rs 服务、不触碰 `9022`。

```bash
git diff --check -- \
  feature/tests/request-api-key-admission-multi-instance.mjs \
  feature/tests/request-api-key-admission-multi-instance.contract.test.mjs
```

Result: pass.

## 动态执行前置条件

完整动态门禁仍需要调用方提供：

```bash
KIRO_RS_BINARY=/abs/outside/repo/frozen/kiro-rs \
KIRO_VALIDATION_ARTIFACT_DIR=/abs/outside/repo/artifacts \
KIRO_REQUEST_ADMISSION_POSTGRES_URL_TEMPLATE='postgres://...@127.0.0.1:<pg-port>/{database}' \
KIRO_REQUEST_ADMISSION_POSTGRES_DATABASES='kiro_request_admission_run_01,kiro_request_admission_run_02,kiro_request_admission_run_03' \
KIRO_REQUEST_ADMISSION_REDIS_URL='redis://127.0.0.1:<redis-port>/<nonzero-db-1-15>' \
KIRO_REQUEST_ADMISSION_REDIS_PREFIX='kiro_rs:request_admission:<unique-owned-prefix>' \
KIRO_REQUEST_ADMISSION_ROUNDS=3 \
node feature/tests/request-api-key-admission-multi-instance.mjs
```

默认旧门禁为 5 rounds；如果使用默认值，调用方必须提供 5 个预创建空 PostgreSQL databases。runner 只启动 fake Kiro upstream、local Redis chaos proxy 和两个临时 kiro.rs 服务实例。调用方必须在 runner 外部创建并最终 drop 独占 PostgreSQL databases，并在提取脱敏摘要后删除仓库外 artifact root。

## 未关闭项

- 未运行完整 request-admission multi-instance dynamic service gate；因此 request API key admission 的最终 release gate 仍 pending。
- 未绑定最终 frozen release candidate SHA。
- 旧 2026-07-16 证据仍是 provisional/debug/run-history；它证明行为方向，但不能替代当前非 Docker runner 的 frozen dynamic。
- 仍需与 shared attempt budget、provider/external/stream faults、usage attribution、L3/L5 和真实 Claude CLI 场景联合验收。
