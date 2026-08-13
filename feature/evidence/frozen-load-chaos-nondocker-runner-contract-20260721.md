# Frozen Load/Chaos 非 Docker Runner 合同

Date: 2026-07-21

Scope: `feature/tests/frozen-load-chaos-runner.mjs` 的 L3/L4/L5 验证程序安全边界。该证据只证明 runner 已脱离 Docker/隐式 Cargo/root target/破坏性 Redis 操作；不证明最终 frozen candidate 的 L3/L4/L5 产品动态门禁已通过。

## 结论

`frozen-load-chaos-runner.mjs` 已从旧的自管理 Docker PostgreSQL/Redis runner 改为 caller-owned runtime runner：

- 使用统一 `KIRO_RS_BINARY` 与 `KIRO_VALIDATION_ARTIFACT_DIR`，要求仓库外绝对真实路径，并拒绝直接使用 `target/debug` 或 `target/release` 产物。
- 新增 `KIRO_LOADTEST_BINARY`，同样要求仓库外复制出的冻结 `kiro_loadtest` binary，拒绝 direct Cargo target 产物。
- 要求 `KIRO_LOAD_CHAOS_POSTGRES_URL_TEMPLATE` 指向 loopback PostgreSQL，并包含且只包含一个 `{database}` placeholder。
- 要求 `KIRO_LOAD_CHAOS_POSTGRES_DATABASES` 提供预创建 caller-owned database 列表，名称必须匹配 `kiro_load_chaos_*`。
  - L3 需要 3 个 database。
  - L4 需要 6 个 database。
  - L5 需要 1 个 database。
- 要求 `KIRO_LOAD_CHAOS_REDIS_URL` 指向 loopback Redis DB1..15，拒绝 DB0、auth、query、fragment 和 `9022`。
- 要求 `KIRO_LOAD_CHAOS_REDIS_PREFIX` 为 caller-owned temporary prefix，拒绝共享 `kiro_rs:local`。
- 不启动 Docker、不创建 PostgreSQL database、不 drop PostgreSQL database、不 `FLUSHDB`/`FLUSHALL` Redis、不调用 Cargo、不探测已有 `9022` listener。
- fake upstream、kiro.rs proxy 与 loadtest 子进程均使用最小环境，不继承调用方整份 `process.env`。
- Redis 清理只通过 RESP `SCAN`/`DEL` 删除本次 `${KIRO_LOAD_CHAOS_REDIS_PREFIX}:db:<database>:*` keys，不清空调用方 Redis database。
- 动态执行时 raw runtime root 位于 `KIRO_VALIDATION_ARTIFACT_DIR/runtime/` 下；调用方在提取脱敏摘要后负责删除仓库外 artifact root。

## 本轮测试命令与结果

```bash
node --test feature/tests/frozen-load-chaos-runner.contract.test.mjs
```

Result: `6/6` pass.

覆盖项：

- JavaScript syntax。
- validate-only 接受 L3 caller-owned loopback PG/Redis，输出 `dockerUsed=false`、`cargoUsed=false`、`createsPostgresDatabase=false`、`dropsPostgresDatabase=false`、`flushesRedisDatabase=false`、`inheritedProcessEnvironment=false`。
- tier 与 database 数量强绑定：L3=3、L4=6、L5=1。
- 早拒绝非 loopback PostgreSQL、PostgreSQL `9022`、非 `kiro_load_chaos_*` database、Redis DB0、Redis auth 和共享 `kiro_rs:local` prefix。
- 早拒绝仓库外但仍位于 `target/release` 的 direct Cargo loadtest binary。
- source contract 禁止 Docker/Cargo 调用、`CREATE DATABASE`、`DROP DATABASE`、`FLUSHDB/FLUSHALL` 和 `...process.env` 继承。

```bash
node --test \
  feature/tests/frozen-load-chaos-runner.contract.test.mjs \
  feature/tests/runtime-validation-paths.test.mjs
```

Result: `15/15` pass.

该合批确认 load/chaos runner 也纳入共享 runtime path fail-closed 合同：所有 runtime runners 都必须使用仓库外冻结 binary 与 owned artifact root，且不得检查既有 `9022` listener。

```bash
git diff --check -- \
  feature/tests/frozen-load-chaos-runner.mjs \
  feature/tests/frozen-load-chaos-runner.contract.test.mjs \
  feature/tests/runtime-validation-paths.test.mjs
```

Result: pass.

本轮没有启动 Docker、没有运行 Cargo、没有启动 kiro.rs 服务、没有生成 `target/`。

## 动态执行前置条件

完整 L3/L4/L5 动态门禁仍需要调用方提供同一冻结候选：

```bash
KIRO_RS_BINARY=/abs/outside/repo/frozen/kiro-rs \
KIRO_LOADTEST_BINARY=/abs/outside/repo/frozen/kiro_loadtest \
KIRO_VALIDATION_ARTIFACT_DIR=/abs/outside/repo/artifacts \
KIRO_LOAD_CHAOS_POSTGRES_URL_TEMPLATE='postgres://...@127.0.0.1:<pg-port>/{database}' \
KIRO_LOAD_CHAOS_POSTGRES_DATABASES='kiro_load_chaos_run_01,kiro_load_chaos_run_02,kiro_load_chaos_run_03' \
KIRO_LOAD_CHAOS_REDIS_URL='redis://127.0.0.1:<redis-port>/<nonzero-db-1-15>' \
KIRO_LOAD_CHAOS_REDIS_PREFIX='kiro_rs:load_chaos:<unique-owned-prefix>' \
node feature/tests/frozen-load-chaos-runner.mjs --tier l3
```

L4 需要 6 个预创建空 database；L5 需要 1 个。调用方必须在 runner 外部创建并最终 drop 独占 PostgreSQL databases，并在提取脱敏摘要后删除仓库外 artifact root。runner 只停止自己的 fake upstream、临时 kiro.rs 服务、loadtest 子进程和 owned Redis prefix。

## 未关闭项

- 未用改造后的 runner 重跑最终 frozen candidate L3/L4/L5 dynamic；历史 r8 仍是产品行为参考，不能替代当前非 Docker runner 的最终候选动态门禁。
- 未绑定最终 release candidate SHA。
- 未覆盖真实 Kiro upstream、native MCP/search/image/agent、真实 Claude Code CLI fault recovery、两实例 fault/fallback、UI/browser、upgrade 和 final inventory。
