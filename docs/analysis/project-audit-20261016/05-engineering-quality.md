# 05 工程质量：测试、CI、代码结构与仓库卫生

## E1（P2）9 个长期失败的测试掩盖回归

### 现状

`cargo test --all-targets -- --test-threads=1` 在 0.0.193 和 0.0.194 上都有以下 9 个失败（0.0.193 还多一个 `l2_failing_pool_is_probationed_then_recovers_and_regains_traffic`，0.0.194 已通过）：

| 测试 | 类别 |
|---|---|
| `anthropic::handlers::tests::normalized_external_direct_policy_skips_raw_preparse_without_raw_pool` | handlers |
| `external_pool::tests::external_pool_no_response_headers_becomes_client_timeout_without_raw_body` | 外部池超时 |
| `external_pool::tests::external_pool_retry_send_timeout_uses_remaining_dispatch_deadline` | 外部池超时 |
| `external_pool::tests::external_pool_slow_upstream_status_keeps_status_and_error_body_fragment` | 外部池超时 |
| `external_pool::tests::external_pool_stream_keepalive_emits_ping_during_silent_gap_before_output` | 外部池超时 |
| `external_pool::tests::legacy_zero_external_wait_reaches_a_bounded_final_error` | 外部池超时 |
| `kiro::token_manager::manager::tests::postgres_add_credential_is_atomic_with_initial_runtime_state` | PG |
| `kiro::token_manager::manager::tests::postgres_admin_capacity_update_defers_runtime_patch_during_recovery` | PG |
| `...::refresh_cluster_tests::token_refresh_two_manager_pg_cas_fences_stale_rotating_and_non_rotating_results_for_five_rounds` | PG |

另有几个时序敏感的测试在机器负载高时会偶发失败：`postgres_migration_compresses_second_rollup_buckets_to_hours`、`redis_usage_writer_burst_keeps_scheduler_latency_bounded_for_three_rounds`、`production_postgres_only_usage_never_materializes_redis_for_five_rounds` 等。全量测试和构建并发运行时，失败数从 9 涨到 15。

### 影响

每次都要人工逐条比对"新增失败"，判断成本高，也容易误判。CI 在 PR 上跑全量，这些失败会让 PR 永远是红的，或者被习惯性忽略。

### 方案

1. 逐个定位根因并分成三类：
   - 本地环境相关（比如依赖特定代理、网络或端口）：用 `#[ignore = "需要 X"]` 加独立的 opt-in 环境变量。
   - 测试本身断言过时：修正断言。
   - 真实缺陷：修代码。
2. 时序敏感的测试改为等待条件、指数退避、宽松上界，不再依赖固定 sleep。
3. 目标：在干净环境下全量测试 0 失败。达成后在 CI 上"失败即阻断"。
4. 过渡期在仓库里维护 `scripts/ci/known-test-failures.txt`，CI 只在出现清单之外的失败时报错，清单只减不增。

## E2（P2）release 流水线不跑全量测试

### 现状

- `build.yaml` 只在 `pull_request → main` 和 `workflow_call` 时触发（第 4–6 行）。
- `docker-build.yaml` 以 `profile: release` 调用它，release 档只做 `cargo check --no-default-features` 和一个 PG smoke 测试（第 120、188、192 行）。
- 本仓库的发版习惯是直接在 main 上提交并打 tag，不走 PR，所以全量测试在发版前**从来没有在 CI 上跑过**，完全依赖本地。

### 方案

1. 增加 `push: branches: [main]` 触发 full 档（不阻塞发版，作为发版前的信号）。
2. release 档改为跑全量测试，排除 E1 的已知清单，失败则阻断镜像构建。CI 时长约增加 25 分钟（以 0.0.194 本地串行 1615s 为参考，CI 机器可能更慢）。
3. 或者保持 release 档快速，但要求打 tag 的提交必须已有一次成功的 full 档运行（manifest job 前做检查）。

## E3（P3）超大文件与超长函数

### 现状（main@317a8b1）

| 文件 | 行数 |
|---|---|
| `src/storage/postgres.rs` | 20477 |
| `src/external_pool.rs` | 16069 |
| `src/kiro/provider.rs` | 14772（其中约 7000 行测试夹在中间） |
| `src/kiro/token_manager/manager.rs` | 14110 |
| `src/anthropic/handlers.rs` | 12993 |
| `src/storage/redis_cache.rs` | 12249（内嵌几十段 Lua） |
| `src/admin/service.rs` | 9913 |
| `src/anthropic/payload_guard.rs` | 9458 |
| `src/anthropic/stream.rs` | 8985 |

函数层面：

- `handle_non_stream_request` 约 1500 行，`create_sse_stream` 的 unfold 闭包约 800 行。
- `handle_stream_request` 有 27 个位置参数，`AppState` 有 40 多个扁平字段。
- `call_api_with_retry` 单个函数约 2000 行。

### 方案（按收益排序，每步独立可发版）

1. **测试外移**：`provider.rs` 的内嵌测试搬到 `provider_tests.rs`，沿用 manager 的 `#[path]` 方式。零行为风险。
2. **Lua 脚本外置**：`redis_cache.rs` 的脚本移到 `src/storage/lua/*.lua`，用 `include_str!` 加载，并给脚本加单元测试。
3. **参数对象化**：`handle_stream_request` / `post_messages_inner` 的参数收拢成 `StreamRequestContext` 结构体。
4. **按领域拆 postgres.rs**：credentials / runtime_state / usage / external_pools / migrations 各一个模块，对外接口不变。
5. 每一步都要求：全量测试结果和 clippy 计数不变，diff 只做移动不改逻辑，便于 review。

## E4（P3）重复代码与重复定义

| 问题 | 位置 | 方案 |
|---|---|---|
| 五套路由前缀逐行复制 | `src/anthropic/router.rs:279-426` | 用一个函数接收前缀生成 Router |
| `post_messages_*` / `count_tokens_*` 薄包装重复 | `handlers.rs` | 用宏或泛型提取器合并 |
| `credential_runtime_mutations`、`credential_stats_delta_batches` 的 DDL 各定义两次 | `postgres.rs:12661/13295`、`12692/13263` | 删除重复的那一份（DDL 都是 `IF NOT EXISTS`，删除前确认列定义一致） |
| 刷新退避常量两处定义 | `manager.rs:1078-1082` 与 `redis_cache.rs:723-724` | 提到共享模块，Lua 通过 ARGV 传入 |
| `envelope::request_id()` 在同一请求里多次生成 | `handlers.rs` | 入口生成一次，贯穿 usage 与回落日志，便于关联排查 |
| `clear_credential` 是空实现 | `prompt_cache.rs:938` | 删除，或者明确注释为什么不需要 |

## E5（P3）两套前端双写维护

### 现状

- `admin-ui/`（Tailwind v3，挂 `/admin`）是旧版，`ui/`（shadcn + Tailwind v4，挂 `/ui`）是现行版。
- `api/credentials.ts`、`api/usage.ts`、`lib/storage.ts`、`hooks/use-auto-refresh.ts` 两边几乎平行复制，最近的提交两边一起改。
- Dockerfile 两套都构建，release 构建两个 `dist` 都要存在，否则 rust-embed 编译失败。
- 另有远端分支 `feat/console-ui`（8 个提交、2.8 万行），是第三套独立控制台，还在开发中。

### 方案（需用户决策）

1. 定一个 `/admin` 的下线日期，期间只修 bug、不加功能。
2. 下线时 `/admin` 改为 302 跳转到 `/ui`，删除 `admin-ui/` 和对应的 rust-embed、Dockerfile 步骤。
3. 明确 `feat/console-ui` 和 `ui/` 的关系，避免出现第三套双写。

## E6（P3）仓库卫生

### 现状

- 跟踪文件 968 个，其中文档和证据约 600 个：`docs/plantree` 123、`docs/archive` 104、`feature/evidence` 111、`feature/issues` 109、`feature/tests` 62。
- 不该入库的文件：
  - `.claude/settings.local.json`、`.vscode/settings.json`：个人配置。
  - `.cargo/config.toml`：写死了 `target-dir=/tmp/kiro-rs-rust-analyzer-target` 和 `linker=/usr/bin/cc`，都是开发机专属。
  - `feature/evidence/real-account-tests-20260901/credentials-snapshot-20260901.json`：220 个账号的运行时快照，没有 token，但有账号 ID 和订阅信息。
- 根目录 `package.json` 是 `npm init` 生成的空壳，测试脚本是 `exit 1`。
- `.gitignore` 残留已不存在目录的规则：`admin-ui-daisy`、`inventory-system`、`catch-test`。
- 根目录零散文档：`CLOUDFLARE_TIMEOUT_FIX.md`、`STREAMING_KEEPALIVE.md`。

### 方案

1. 个人配置文件 `git rm --cached` 后加入 `.gitignore`。`.cargo/config.toml` 只保留与机器无关的 `RUST_MIN_STACK`，其余改为本地覆盖文件 `.cargo/config.local.toml`（不入库）。
2. 账号快照删除或脱敏。
3. 删除空壳 `package.json` 和过时的 `.gitignore` 规则。
4. 根目录零散文档移到 `docs/archive/`。
5. 体积治理（可选）：`feature/` 下的历史证据归档成发布附件或单独仓库，主仓库只保留索引。
