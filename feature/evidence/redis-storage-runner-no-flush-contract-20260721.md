# Redis Storage Runner No-FLUSH 合同

Date: 2026-07-21

Scope: `run-token-refresh-cluster-validation.mjs` 与 `run-multi-instance-redis-coordination-validation.mjs` 的 Redis cleanup 安全边界。该证据只证明 runner 层不再用 `FLUSHDB` 兜底清空调用方 Redis database；不证明对应真实 storage dynamic 在最终 frozen candidate 上已重新执行。

## 结论

两个 runner 过去的清理口径是：

- 启动前要求 Redis DB 为空；
- 如果测试后发现 DB 有残留 key，则在 runner cleanup 中执行 `FLUSHDB`；
- 最后报告 `redisDatabaseFlushed`/`databaseFlushed`。

这在“调用方确认为隔离空 DB”的前提下曾可接受，但与当前用户要求的“只启动/使用一套当前项目隔离 PG/Redis，即使本地有其他同项目也不得互相干扰”不够一致。即使 runner 已经检查 DB 为空，`FLUSHDB` 仍是整库破坏性动作；一旦 URL/DB 误配，损害面超过本轮测试 owned namespace。

本轮改为：

- 启动前仍要求 DB1..15 且 `DBSIZE=0`，非空则在 Cargo 前 fail closed。
- 测试正文仍依赖 Rust fixture 自己的随机 `kiro_rs:test:<uuid>` namespace 和 `delete_pattern_bounded("*")` 清理。
- runner cleanup 只停止自有 child process group 和删除 temp root。
- runner cleanup 不再执行 `FLUSHDB`/`FLUSHALL`。
- 如果测试后 Redis DB 仍有残留，runner 返回 `redisDatabaseEmpty=false` / `databaseEmpty=false`、`residualKeyCount=<n>`，并让门禁失败；由调用方按真实 owned 资源边界人工处理。

## 本轮测试命令与结果

```bash
node --test \
  feature/tests/run-token-refresh-cluster-validation.contract.test.mjs \
  feature/tests/run-multi-instance-redis-coordination-validation.contract.test.mjs
```

Result: `19 pass / 1 skip / 0 fail`.

说明：

- skip 是 `KIRO_MULTI_INSTANCE_CONTRACT_NONEMPTY_REDIS_URL` 的 live nonempty Redis opt-in，未提供时明确 skip，不计产品 pass。
- 合同覆盖缺 URL、隔离标志、DB0、`9022` 早拒绝且不调用 Cargo。
- 合同覆盖 source 中无 Docker/protected listener inspection。
- 新增断言覆盖两个 runner 均不含 `FLUSHDB`/`FLUSHALL`，并报告 `residualKeyCount`。

```bash
node --check feature/tests/run-token-refresh-cluster-validation.mjs
node --check feature/tests/run-multi-instance-redis-coordination-validation.mjs
```

Result: pass.

```bash
rg -n "FLUSHDB|FLUSHALL" \
  feature/tests/run-token-refresh-cluster-validation.mjs \
  feature/tests/run-multi-instance-redis-coordination-validation.mjs \
  feature/tests/run-token-refresh-cluster-validation.contract.test.mjs \
  feature/tests/run-multi-instance-redis-coordination-validation.contract.test.mjs
```

Result: no matches.

```bash
git diff --check -- \
  feature/tests/run-token-refresh-cluster-validation.mjs \
  feature/tests/run-multi-instance-redis-coordination-validation.mjs \
  feature/tests/run-token-refresh-cluster-validation.contract.test.mjs \
  feature/tests/run-multi-instance-redis-coordination-validation.contract.test.mjs
```

Result: pass.

本轮没有运行 Cargo、没有启动 Docker、没有连接 Redis live fixture、没有生成 `target/`。

## 未关闭项

- 未重新执行 token-refresh cluster dynamic；该门禁仍依赖 caller-owned loopback PostgreSQL、loopback Redis DB1..15 和 scoped Cargo wrapper。
- 未重新执行 multi-instance Redis coordination dynamic；该门禁仍依赖 caller-owned loopback Redis DB1..15 和 scoped Cargo wrapper。
- 这两个 runner 仍是源码级 Rust test runners，会通过 `run-cargo-scoped.sh` 运行 Cargo；最终动态执行时仍必须遵守 scoped target 清理和 final inventory。
