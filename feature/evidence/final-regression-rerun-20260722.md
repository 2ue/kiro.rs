# 2026-07-22 当前工作树回归复测证据

Date: 2026-07-22

Status: `focused-regression-pass / release-gates-still-require-final-inventory-and-version-step`

## 范围与安全边界

本轮复测针对用户补充的 P0/P1 风险重新验证：Claude Code transcript 泄漏、tool hash 形态、thinking/output_config 转发、body 清理/压缩字节语义、prompt 总开关独立性、高并发低 RPM / Redis scheduler degraded / runtime quarantine、business Redis 与 usage/observability Redis 故障域隔离，以及错误突发下内部重试/Redis 写放大。

执行边界：

- 未执行 Docker 验证。
- 未压测、重启或修改既有 `127.0.0.1:9022` 服务；相关 runner 只按数值排除 9022，未探测/触碰该 listener。
- PostgreSQL 使用当前项目 loopback 连接创建 caller-owned 临时数据库；每个数据库独占命名，跑完删除。
- Redis 使用 loopback 空 DB 或自有 prefix；runner 不使用 `FLUSHDB`，只清理 owned namespace。
- Raw report 只在 `/tmp` 下临时存在；文档只保留脱敏摘要和 SHA-256。

冻结候选：

- Git HEAD: `401473ca1649997bdeccf4468e3add1bdb187248`
- Frozen `kiro-rs` binary SHA-256: `31b8c4749201b0f7666b63a9c268c0b75e21f6c1600b18c77bf39a7c6c249c2e`
- Claude Code CLI: `2.1.197`
- Release build batch also ran `cargo fmt --all -- --check` and `git diff --check`.
- Scoped target cleanup for release build: `scope=long-session-release-20260722 size_kib=798496 removed=true reservation_released=true`

## 结果汇总

| 验证项 | 结果 | 覆盖点 |
| --- | --- | --- |
| Runtime quarantine storage matrix | PASS: 3 outer × 6 exact = 18/18 | PostgreSQL pool pressure 不假 quarantine、pending mutation FIFO/revision replay、generation fence、Redis finite queue deadline、degraded waiter fail-closed、cancelled waiter release |
| Scheduler Redis chaos | PASS: 3 outer × 8 exact = 24/24 | affinity latency 不污染 capacity、50/500ms latency、连续 timeout breaker、300 lease release、disconnect/reconnect、usage writer + scheduler 联合故障、cancel / commit-unknown cleanup |
| Business/observability Redis fault domain | PASS: source contract 37 pass / 9 live-signal skips / 0 fail；product 3/3 exact | usage/Admin 只用 observability Redis；scheduler/external/runtime-event/health 只用 business Redis；observability 慢/断开不影响 scheduler；business fault fail-closed 且不伪装 `AllDisabled` |
| SchedulerRedisDegraded external takeover | PASS: enabled 3 clean rounds × (5 degraded + 5 recovery)；disabled 1 clean round × (5 degraded + 5 recovery) | enabled 时 500ms Redis degraded 请求进入 external 并恢复 local；disabled 时不打 local/external upstream、公开 429 脱敏且恢复 local；runner 不用 Docker/Cargo、不探测 `9022` |
| Protocol contamination source contracts | PASS: 30/30 | `user Continue`、`Tool results provided`、`function_results`、`*Hash<8hex>` 等泄漏形态；Hash-shaped text 不作为任意内部工具名；marker-free raw body byte-identical |
| Prompt/UI control contracts | PASS | Rust/UI/Admin UI 默认 task-quality prompt byte-equal 且无内部 marker；两套 UI 的 prompt master 与 bodyConversion 控制面独立 |
| Real Claude CLI thinking capture | PASS: 30 message requests = 6 efforts × 5 rounds | Claude CLI 原始请求体包含 `thinking: {type:"adaptive"}`；`output_config.effort` absent→`high`，`low/medium/high/xhigh/max` 均按原值出现 |
| Real kiro.rs thinking wire | PASS: 60/60 = 2 endpoints × 6 efforts × 5 rounds | `additionalModelRequestFields.output_config.effort` 在 CLI/IDE 两入口均保持 `low/medium/high/xhigh/max`，`max` 未被截断为 `high`；当前 OutputConfig schema 下 final Kiro wire 不注入未广告的 `thinking` 字段 |
| Real Claude CLI long session | PASS: 5 sessions × 20 tool cycles | 110 CLI turns、105 `--continue` turns、100 tool turns、50 Bash / 50 Read、210 inference hits、100 tool_use/tool_result pairs、leakMatches=0、unknown upstream requests=0 |
| Real Claude CLI bare invoke | PASS: 20 cases | 15 negative text cases 不升级为工具；5 structured ToolUseEvent cases 正常工具执行；25 inference hits、5 tool_use/tool_result pairs、unknown upstream requests=0 |
| Rust reasoning/body focused tests | PASS: 8/8 exact | provider max effort no truncation/no invented thinking、provider endpoint/compression byte-exact、raw reasoning accepted/rejected forms、clean raw body zero-copy 100 rounds、raw sanitizer clean body byte-identical、prompt master disables auto thinking、tool-result continuation keeps thinking signal |

## 关键事实

### 1. `output_config.effort` 没有被截断成 `high`

真实 Claude CLI capture：

- absent → CLI 发出 `output_config.effort="high"`；
- low → `low`；
- medium → `medium`；
- high → `high`；
- xhigh → `xhigh`；
- max → `max`。

真实 `kiro.rs -> fake Kiro upstream` capture：

- CLI endpoint: absent/low/medium/high/xhigh/max 分别转成 wire effort `high/low/medium/high/xhigh/max`；
- IDE endpoint: 同样保持 `high/low/medium/high/xhigh/max`；
- `max` 5/5 × 2 endpoints 均保持 `max`，没有降级为 `high`。

### 2. 当前 Kiro upstream wire 不传 `thinking.type=adaptive` 是设计行为，不是本轮发现的丢字段

本项目的最终 Kiro wire 行为是：

```json
{
  "additionalModelRequestFields": {
    "output_config": { "effort": "max" }
  }
}
```

在当前 model-discovery 广告 `output_config.effort` schema 时，converter 将 Anthropic/Claude Code 的 `thinking: {type:"adaptive"}` 和 `output_config.effort` 收敛为 Kiro 侧的 `additionalModelRequestFields.output_config.effort`。源码合同明确要求“不 invent unadvertised thinking field”。如果上游 discovery 将能力路径广告为 `reasoning`，代码支持写 `additionalModelRequestFields.reasoning.effort`；当前 OutputConfig path 下不写 `thinking`。

对应源码合同：

- `src/anthropic/converter/model.rs` 的 `build_additional_model_request_fields()` 在 `KiroReasoningFieldPath::OutputConfig` 下只写 `output_config`，`thinking: None`。
- `src/kiro/provider.rs` 的 `provider_sends_converter_max_effort_without_inventing_thinking_for_five_rounds` 断言 final wire 为 `{"output_config":{"effort":"max"}}`，并断言不生成未广告 `thinking`。

因此结论是：`max` 没被截断；`thinking.type=adaptive` 不出现在 final Kiro wire 是当前 Kiro schema 映射策略。若以后官方 schema 明确要求 `thinking` 字段，本项目应通过 discovery path / schema 切换，而不是无条件注入。

### 3. Transcript/hash 泄漏的已知和相邻形态本轮均未复现

覆盖形态包括：

- `user Continue`
- `user Tool results provided`
- `Tool results:`
- `<function_results>` / `</function_results>`
- `<function_calls>` / `<invoke name=...>`
- `bashHashd1e9567d` / `readHash9b9a8d05`
- 任意 `NameHash[0-9a-f]{8}` 形态

结果：

- Source contracts: 30/30 pass。
- Long session 5×20：`leakMatches=0`。
- Tool mapping 在 upstream wire 内仍可使用 request-local hash 名，但 Claude CLI 可见输出恢复为公开工具名 `Bash` / `Read`，未泄漏 hash。

### 4. Body 清理/压缩不会无条件改写 clean body

本轮直接验证：

- `raw_reasoning_protocol_accepts_supported_forms_without_changing_bytes_for_five_rounds`
- `clean_anthropic_raw_body_is_zero_copy_and_byte_identical_for_one_hundred_rounds`
- `raw_request_sanitization_keeps_clean_body_byte_identical`
- `provider_sends_endpoint_and_compression_bytes_exactly_for_five_rounds`

结论：

- clean body 路径不会先 DOM parse 再重序列化；
- marker-free body 保持 byte-identical；
- provider compression enabled/disabled 的 final bytes 和 SHA 与期望完全一致；
- reasoning 控制字段校验先做 bounded scan；非法/歧义控制 fail closed，合法控制不改写原始字节。

### 5. Prompt 总开关与 bodyConversion 已分离，但总开关仍是“提示词注入总门”

本轮 `prompt-control-independence.mjs` 通过，说明两套 UI 不再把 prompt master 操作误写到 `bodyConversion`。但当前产品语义仍是：

- `promptSteering.enabled=false` 时，不注入 language/task/custom/tool_choice/thinking/chunked-write prompt。
- body conversion / native reasoning / raw protocol validation 不依赖这个开关。

这是当前实现的显式合同，不是 body 处理总开关。若产品希望“语言增强”和“tool_choice/thinking/chunked-write prompt”拆成多个总开关，应作为 UX/配置设计变更另立 issue，而不能在本轮回归里静默改变语义。

### 6. Redis scheduler / usage / runtime fault 组合本轮未再复现假全禁用或内部 spin

关键结果：

- Runtime quarantine storage: 18/18 pass。
- Scheduler Redis chaos: 24/24 pass；DB7 清空；50ms 正常、500ms 有界 fail-closed/recovery；wrongtype/disconnect recover；usage writer + scheduler 联合故障 recover without spin or false disable。
- Business/observability fault domain: 3/3 product exact pass；observability latency/disconnect 不影响 business scheduler；business fault 不被记录为 `AllDisabled`。
- SchedulerRedisDegraded external takeover: enabled 3 个 clean-DB 轮次通过，每轮 5 个 degraded 请求均 HTTP 200 并命中 external，5 个 recovery 请求均回到 local；disabled 1 个 clean-DB 轮次通过，5 个 degraded 请求均 429 且 local/external hits 为 0，恢复后 5/5 local 200。

这覆盖了用户描述的“并发高、RPM不高、Redis/usage 干扰导致 scheduler degraded、账号假禁用、最终外部池接管”的核心链路。

注意：external takeover 初始 `OUTER_ROUNDS=3` 同库连跑曾出现 runner 假红，原因是 PostgreSQL 持久 runtime config 复用了上一轮端口，导致健康检查命中错误端口；当前有效证据使用每轮前 drop/create 独占临时库的 clean round，避免把 runner 隔离问题误判为产品请求路径问题。

## 命令与证据摘录

### Runtime quarantine

```text
KIRO_RS_TEST_POSTGRES_URL=<loopback-postgres-current-project>
KIRO_RS_TEST_REDIS_URL=redis://127.0.0.1:26379/0
KIRO_RUNTIME_QUARANTINE_STORAGE_SCOPE=runtime-quarantine-storage-20260722-r1
feature/tests/run-runtime-quarantine-storage-validation.sh
```

Result: 3 outer × 6 exact, 18/18 pass.

Cleanup: `size_kib=1711220 removed=true reservation_released=true`.

### Reasoning/body focused Rust tests

```text
feature/tests/run-cargo-scoped.sh reasoning-body-focused-20260722 -- ...
```

Result: 8 exact tests pass.

Cleanup: `size_kib=1710860 removed=true reservation_released=true`.

### Real Claude CLI thinking capture

```text
node feature/tests/thinking-effort-claude-cli-capture.mjs
```

Result: `observation_complete`, total message requests `30`, unknown requests `0`, invalid JSON `0`, cleanup true.

### Real Kiro thinking wire

```text
KIRO_RS_BINARY=<frozen-sha-31b8c...>
KIRO_CLAUDE_BINARY=<canonical Claude package binary>
KIRO_PSQL_BINARY=<temporary Node pg psql-wrapper>
KIRO_THINKING_WIRE_DATABASE_OWNER=codex_20260722
node feature/tests/thinking-effort-kiro-wire.mjs
```

Result: `pass`, total cases `60`, inference hits `60`, discovery hits `2`, protocol violations `0`.

Report SHA-256: `df9a2fe3e07a41fd9df5cd8716ab6270d8902e3a09f1c9f0a749fff7487170a3`.

### Real Claude CLI long session

```text
KIRO_RS_BINARY=<frozen-sha-31b8c...>
KIRO_LONG_SESSION_TOOL_CYCLES=20
node feature/tests/claude-cli-long-session-continue.mjs
```

Result: `pass`, gateQualified `true`.

Totals:

- sessions: `5`
- CLI turns: `110`
- continue turns: `105`
- tool turns: `100`
- Bash / Read: `50 / 50`
- inference hits: `210`
- tool_use / tool_result: `100 / 100`
- leakMatches: `0`
- fakeUnknownRequests: `0`

Report SHA-256: `2342ef2f3c66ed84ecbeb45fb9cad471a0307e05f0de7a0fbeb85cdc289df7f7`.

### Real Claude CLI bare invoke

```text
KIRO_RS_BINARY=<frozen-sha-31b8c...>
node feature/tests/bare-invoke-claude-cli.mjs
```

Result: `pass`.

Totals:

- cases: `20`
- negative text cases: `15`
- structured cases: `5`
- inference hits: `25`
- tool_use / tool_result: `5 / 5`
- fakeUnknownRequests: `0`

Report SHA-256: `cc8ce4446006d071e75ccc89594af04518138e05a0b428725af087855443989d`.

### Redis fault-domain

```text
node --test feature/tests/run-redis-fault-domain-product-validation.contract.test.mjs
KIRO_REDIS_FAULT_DOMAIN_BUSINESS_URL=redis://127.0.0.1:26379/8
KIRO_REDIS_FAULT_DOMAIN_OBSERVABILITY_URL=redis://127.0.0.1:50892/2
KIRO_RS_TEST_REDIS_ISOLATED=1
node feature/tests/run-redis-fault-domain-product-validation.mjs
```

Contract: 46 tests = 37 pass / 9 live-signal skips / 0 fail.

Product: 3 outer × 1 exact × 3 internal rounds, pass.

### Scheduler Redis chaos

```text
KIRO_SCHEDULER_CHAOS_REDIS_DIRECT_URL=redis://127.0.0.1:26379/7
KIRO_RS_TEST_REDIS_ISOLATED=1
node feature/tests/run-scheduler-redis-chaos-validation.mjs
```

Result: 3 outer × 8 exact = 24/24 pass; cleanup databaseEmpty true.

### Post-document-update gates

After updating the final issue summaries and release report:

```text
node feature/tests/check-feature-docs.mjs
git diff --check
node --test \
  feature/tests/protocol-marker-inventory-source-contract.test.mjs \
  feature/tests/protocol-contamination-source-contract.test.mjs \
  feature/tests/thinking-effort-kiro-wire-contract.test.mjs \
  feature/tests/runtime-validation-paths.test.mjs \
  feature/tests/thinking-effort-claude-cli-capture-signal.test.mjs
node feature/tests/prompt-default-parity.mjs
node feature/tests/prompt-control-independence.mjs
node feature/tests/cost-format-contract.mjs
node feature/tests/request-api-key-id-contract.mjs
node feature/tests/mcp-attempt-channel-contract.mjs
node --test \
  feature/tests/run-redis-fault-domain-product-validation.contract.test.mjs \
  feature/tests/run-scheduler-redis-chaos-validation.contract.test.mjs \
  feature/tests/run-token-refresh-cluster-validation.contract.test.mjs \
  feature/tests/run-multi-instance-redis-coordination-validation.contract.test.mjs
```

Results:

- feature docs: 47 issue documents and 114 relative links pass;
- `git diff --check`: pass;
- protocol/thinking/runtime path contracts: 30/30 pass;
- prompt/UI/cost/request-key/MCP contracts: pass;
- Redis/scheduler/storage runner contracts: 94 tests = 72 pass / 22 explicit live-fixture skips / 0 fail.

Final artifact inventory was rerun:

```text
node feature/tests/inventory-build-artifacts.mjs --gate
```

Result: fail, because an existing live `kiro-rs` process PID 84264 references the repository root `target/`:

```text
targets=1 reservations=0 target_processes=1 blockers=2
target classification=unmanaged-repo-cargo-target size_kib=933260
target-process pid=84264 classification=kiro-runtime
```

Process evidence:

```text
PID 84264 COMMAND ./target/release/kiro-rs -c config.json --credentials credentials.json
LISTEN 127.0.0.1:9022
txt /Users/yuanfeijie/Desktop/procode/kiro.rs/target/release/kiro-rs
stdout/stderr /Users/yuanfeijie/Desktop/procode/kiro.rs/target/local-verify/kiro-rs-9022.log
```

This is not a scoped validation target leak. The target was not deleted because it is still referenced by a live process. Disk after cleanup: filesystem free space about `98GiB`; repo `target/` about `911MiB`.

## 清理状态

本轮创建的以下临时对象已在记录本证据后删除；复核未发现这些路径继续存在：

- frozen candidate root `/tmp/kiro-long-session-candidate.pbCgoR`
- report roots:
  - `/tmp/kiro-long-session-artifacts.seedHs`
  - `/tmp/kiro-bare-invoke-artifacts.AXUxeq`
  - `/tmp/kiro-thinking-wire-artifacts.ZiJSNA`（失败的 Volta shim 尝试）
  - `/tmp/kiro-thinking-wire-artifacts.CsRitR`
- temporary pg client `/tmp/kiro-pgclient.zOQeyV`
- temporary psql wrapper `/tmp/kiro-psql-wrapper.dIkU0r`
- caller-owned PostgreSQL databases:
  - `kiro_long_session_codex_20260722`
  - `kiro_bare_invoke_codex_20260722`
  - `kiro_thinking_wire_codex_20260722_cli`
  - `kiro_thinking_wire_codex_20260722_ide`

删除边界：只删除本轮明确创建的 caller-owned 对象，不清理 live `9022` 服务、不清理未知 Docker 资源、不清理用户/其他分支产物。

## 限制

- 本轮没有执行 Docker validation。
- 本轮没有跑 5×100 长会话；2026-07-20 旧 frozen 证据有 5×100 pass，本轮当前 candidate 做了 5×20。
- Real upstream validation 使用 fake Kiro upstream 捕获协议 wire，避免消耗真实官方账号；它证明本项目 final wire 的字段映射，不证明官方服务在所有生产模型/region 上的运行质量。
- UI browser smoke、旧版本升级 smoke、最终 build artifact inventory 和版本发布仍需作为发版前最后门禁执行；Docker 动态验证按用户当前要求豁免，不作为 pass 记入。
