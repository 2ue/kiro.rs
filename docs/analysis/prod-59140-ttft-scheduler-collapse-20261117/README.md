# 59140 首字极慢与"账号连续报错"：现象、根因、修改、未完成工作与验证方案

本文是 2026-11-17 这次排查的完整记录，目标是让没有参与本次会话的人也能完整理解、复核并继续推进。所有时间都是北京时间（UTC+8）。

- **涉及的服务**：本机 `152.53.242.87:59140`，容器 `kiro-rs-forced-59140-app`，镜像 `ghcr.io/2ue/kiro-rs-forced:0.0.257`，OCI revision `705622dd`，部署目录 `/root/docker-compose/kiro-rs-forced-59140`。运行配置保存在该实例 PG 的 `runtime_config` 表里，不在 `config.json`。
- **结论截止**：文中数据截至 2026-11-17 15:48。**本次没有对线上做任何写操作**：没有改配置、没有重启、没有部署。线上只做了只读的 SQL 查询、读日志、管理端只读接口、perf 采样和 `/proc` 采样。
- main 与 forced-main 共有问题的代码层说明见 `docs/analysis/scheduler-model-support-hot-path-and-signature-replay-20261117.md`（main 提交 `33a4c2d7`，已合并进 forced-main）。本文覆盖完整事件，以及 forced-main 独有的问题。
- 仓库里另有一份未跟踪的文档 `docs/analysis/prod-59140-huaqiangbei-429-ttft-root-cause-20261117.md`，出自其他会话，从 sub2api 上游一侧分析到本服务。第八节对它逐条做了核对，只能作参考，不能全部采信。

---

## 一、一句话结论

1. **首字极慢的根因在代码里，main 也有**：调度每做一次全池扫描，就要为每个账号重新展开一遍订阅模型列表；每个请求要做好几次这样的扫描，而且都持有全局 `entries` 锁。59140 有约 850 个启用账号，全部共用一张 33 个模型的 `free` 列表，一次扫描约 85–89 ms，调度吞吐被锁死在每秒约 6 个。解开入口限流后，到达量超过这个上限，排队无界增长：调度等待 50–180 秒，客户端约 120 秒超时断开。锁竞争还让异步运行时的 worker 线程阻塞，所有 Redis、PG、HTTP 操作都晚 1–2 秒才被处理，进一步引发超时、熔断和 Token 刷新协调失败。
2. **配置变化是触发条件，不是根因**：
   - 账号从约 476 个增加到 940 个，单次扫描开销翻倍，吞吐上限减半；
   - 解除主 Key 入口限流（RPM 300、并发 32）后，到达量不再被挡在入口，直接压到串行瓶颈上；
   - 11-16 18:02 重新开启的外部池让每个请求多一次全池扫描。近 3 小时它一次都没用上，却占了约 36.6% 的 CPU。
3. **"账号连续报错"（`THINKING_SIGNATURE_INVALID`）是另一个问题**：
   - 它不影响账号健康度，账号不会因此被禁用或冷却；
   - 会话历史里一旦有无效签名，之后每一轮都会先失败一次再重试成功；会话又粘在同一个账号上，所以看起来是"某账号连续报错"；
   - 无效签名有两类来源：其他渠道带来的外来签名（main 和 forced 共有），以及 forced-main 的身份防护改写了思考文本却保留原签名（forced 独有）。两类都已经在代码里修复。
4. **已完成**：五项代码修改，全部只在本地分支提交，未推送、未部署。第一批的单测已通过；签名相关单测、端到端压测、真实上游验证都还没做（第十节）。

---

## 二、现象

### 2.1 每分钟延迟（完整表见 `evidence/latency-per-minute.txt`）

| 时段 | 每分钟请求 | 调度耗时 P50（各分钟中位数的中位 / 范围） | 流式首字 P50（中位 / 范围） | 客户端断开 |
|---|---:|---|---|---:|
| 09:30–10:55 | 274 | 36 ms / 33–44 ms | 2.3 s / 1.4–3.5 s | 0 |
| 10:55–11:39 | 182 | 58 ms / 37–491 ms | 2.9 s / 1.7–73.8 s | 12 |
| 11:40–12:40 | 268 | 6.6 s / 0.7–20.5 s | 14.2 s / 3.5–36.0 s | 11 |
| 12:40–13:40 | 236 | 52.2 s / 6.3–106.8 s | 67.3 s / 32.3–107.1 s | 1579 |
| 13:40–14:40 | 223 | 62.3 s / 46.3–107.6 s | 80.9 s / 58.4–132.7 s | 2004 |
| 14:40–15:26 | 178 | 60.7 s / 14.5–175.9 s | 73.1 s / 18.4–241.6 s | 2003 |
| 15:26–15:48 | 137 | 70.4 s / 17.1–185.5 s | 80.1 s / 19.5–209.9 s | **2657（共 3010 个请求，成功只有 351 个）** |

- 调度耗时取自 `usage_records.data.latencyTrace.credentialDispatchElapsedMs`，计时只覆盖 `acquire_context_for_dispatch_with_version` 这一个函数（`provider.rs`）。
- 单条样本：调度 208.6 秒，上游响应头 2.3 秒，首字 211.4 秒；只有 1 次尝试，没有刷新 Token。
- 13:20–14:00 的客户端断开：2020 个，耗时中位 123.6 秒、P90 127.7 秒，调度中位 98.6 秒，其中 2018 个一个字都没收到。说明下游约 120 秒超时放弃；而且此时调度往往已经完成，上游的号和额度已经被占用。
- 有些分钟记录数骤降，甚至整分钟缺失（例如 12:28、12:43、12:55）。这与运行时阻塞、用量写入超时有关，不是没有流量。

### 2.2 运行时现场（只读采样）

| 时间 | 观测 | 数值 |
|---|---|---|
| 13:18 | `docker stats` | 59140 CPU 约 120%，内存 2.79G；主机负载 3.4（8 核） |
| 13:19 | `/api/admin/credentials/summary` | 总数 940，可用 853，禁用 87，在途 142，**排队 0**，额度耗尽 82 |
| 13:19 | `perf stat` 10 秒 | 1.066 核；上下文切换 845 次/秒；**futex 257 次/秒**；sched_yield 67 次/秒 |
| 13:19 | `/proc/<pid>/task/*/stat` 每 3 ms 采样一次，15 秒共 4340 次 | 同一时刻在运行的线程数：0 个占 **0%**，1 个占 50.8%，2 个占 34.9%，3 个占 9.7%，4 个及以上 4.6% |
| 13:2x | `/proc/<pid>/task/*/wchan` 每 10 ms 采样，10 秒 | 主运行时的 tokio 线程平均 10.6 个在 `futex_wait_queue`、1.37 个在运行，**没有一个停在 `do_epoll_wait`**；用量写入和存储任务两个辅助运行时各有 1 个线程正常停在 epoll |
| 13:20 | `/readyz` 6 次 | `runtimeLagMs` 312–2431 ms；接口本身耗时 0.008–4.1 秒，其中 1 次返回 503（`not_ready`、`redis:false`）；14:5x 有一次 10 秒内无响应 |
| 13:2x | 缺页 / 换出 | 主缺页 3 次 / 10 秒，`VmSwap` 94MB：排除内存换出导致卡顿 |

**怎么读这些数字**：CPU 约等于 1 核，任何时刻至少有 1 个线程在跑，futex 很少，没有线程停在 epoll——这是一段串行临界区被 CPU 占满、其余 worker 在同步锁上排队的典型特征。健康的 tokio 运行时总会有一个空闲 worker 停在 `epoll_wait` 上轮询 I/O；现在没有，所以 I/O 完成通知只能等 worker 偶尔做维护时才被处理，表现为 `runtimeLagMs` 达到秒级。

### 2.3 日志（约 13:07–13:17 的 10 分钟）

- `/v1/messages` 请求 3510 个（每分钟约 351 个到达）。
- Social Token 刷新 376 次；`token refresh failed` 145 次（之后的 10 分钟：协调失败 294 次、协调限流 63 次、403 认证 20 次、协调超时 14 次、持久化 10 次）。
- "已根据 Redis 通知同步凭据快照 reason=token_refreshed" 31 次；"已从 PgSQL 加载 940 条凭据统计" 33 次；"优先级变更后切换凭据" 90 次。
- "Redis 调度协调暂不可用，单实例按本地并发与 RPM 账本继续调度" 54 次；多种 Redis 存储任务超时；`slow statement: SELECT now() = statement_timestamp()` 29 次（空语句也报慢，说明问题在应用侧）。
- "同步存储操作耗时较长 operation=重放 PgSQL 凭据运行态 mutation" 26 次，单次 0.3–5 秒；"从 PgSQL 一致性重新加载凭据和运行态"单次 0.1–5 秒，多次超过 5 秒上限。
- "强制模型收敛：上游判定输入过长且服务端退避已用尽，按临时过载返回" 21 次（见第七节）。

### 2.4 账号与模型

- 启用账号里 848 个是 `KIRO FREE`，4 个没有等级标题，**全都没有自带模型目录**，全部按订阅等级 `free` 的列表判断。
- 该列表是 11-16 16:25 手工设置的 33 个模型（`source: manual`）：auto、claude-3-5-haiku-20241022、claude-3-5-sonnet-20240620、claude-3-5-sonnet-20241022、claude-3-7-sonnet-20250219、claude-fable-5、claude-fable-5-1、claude-fable-5-2、claude-haiku-4-5、claude-haiku-4-5-20251001、claude-haiku-4.5、claude-haiku-5-5、claude-opus-4-1-20250805、claude-opus-4-20250514、claude-opus-4-5、claude-opus-4-5-20251101、claude-opus-4-6、claude-opus-4-7、claude-opus-4-8、claude-opus-5、claude-opus-5-5、claude-sonnet-4-20250514、claude-sonnet-4-5、claude-sonnet-4-5-20250929、claude-sonnet-4-6、claude-sonnet-5、claude-sonnet-5-5、deepseek-3.2、minimax-m2.5、minimax-m2.1、glm-5、qwen3-coder-next、claude-sonnet-4.5。
- 13:3x 前 30 分钟的样本里，上游模型 100% 是 `claude-haiku-4.5`：强制映射把 claude-opus-5、claude-sonnet-5-5、claude-opus-5-5 等全部映射到它。
- 外部池：`externalPoolsEnabled=true`，路由白名单为 `/cc`、`/dfcache/dfv1`、`/dfcache/waijiea`、`/ha`、`/na`、`/v1`，`externalPoolCapacityMode=wait`，`externalPoolLocalRescueMaxWaitSecs=5`，表中有 4 个池。**近 3 小时 40734 个请求中，走外部池的为 0**（`local_success` 37925 个，`local_error_no_fallback` 2796 个）。

---

## 三、时间线（`evidence/audit-config-timeline.txt`）

| 时间 | 事件 |
|---|---|
| 11-16 12:50 | 关闭外部池 `externalPoolsEnabled=false` |
| 11-16 16:25 | `free` 等级模型列表手工设为上述 33 个模型 |
| 11-16 18:02 | 重新开启外部池 `externalPoolsEnabled=true` |
| 11-17 10:55:16 | 负载均衡模式切换为 `balanced` |
| 11-17 11:04:20–11:49 | 多批次导入账号（大量批次部分或全部失败）。到约 13:20，近 3 小时新增 464 个，其中 461 个启用；未删除的账号共 940 个 |
| 11-17 11:39:36 | `update_request_api_key`：取消主 Key 的入口限额（根据上述其他会话文档，原为 RPM 300、并发 32；本会话未单独核对旧值） |
| 11-17 11:40 | 调度 P50 从 224 ms 立即跳到 13.3 秒（变化发生在下一条配置改动之前） |
| 11-17 11:43:50 | 运行配置：`credentialMaxConcurrentRequests` 0→1；`dispatchGlobalMaxConcurrentRequests` 512→1000；`dispatchMaxQueuedRequests` 30→50；`kiroUpstreamStreamIdleTimeoutSecs` 180→300；默认 `requestAdmission` 由 RPM 300 / 并发 32 / 队列 64 / 1000 ms 全部改为 0 |
| 11-17 11:43:58 | `credentialRpm` 100→10 |
| 11-17 13:04–13:06 | 新建 3 个请求 Key |
| 约 13:2x | 出现 59142、59143 两个 forced 0.0.257 实例（15:26 时显示已运行 2 小时，不是本会话创建的） |
| 11-17 15:26 起 | 59140 客户端断开比例达到 80–95% |

关于 11:43 这批改动：所有账号都有各自的并发覆盖值，或者实际使用的上限远高于在途数，所以 `credentialMaxConcurrentRequests`、`credentialRpm`、全局上限这几项与本次变慢**没有因果关系**。整个过程中在途只有约 142 个、排队 0。

---

## 四、根因分析（第一性原理）

### 4.1 串行瓶颈从哪里来

1. 每个 `/v1/messages` 请求至少做以下几次全池扫描，每次对所有启用账号调用 `credential_is_usable_for_model`，最终落到 `model_is_supported_by_list`：
   - 入口检查 `local_pool_route_state_cached_for_route`；
   - 外部池前置检查 `local_pool_route_state_fresh_for_route`（开启外部池后才有；容量满时还会每 50 ms 重扫一次）；
   - 选号 `select_next_credential_excluding`（外加 `prefer_catalog_listed`）；
   - 会话粘性检查（`get_bound_credential`、`should_migrate_sticky_to_better_tier`、`credential_accepts_session_binding`）。
2. 旧版 `model_is_supported_by_list` 每次都把 33 个模型逐个克隆、规范化（O(M²) 去重），再为每个模型生成多个变体字符串（`format!`），对单个账号要做数百次堆分配。实测 848 个账号扫一遍需要 83–89 ms。
3. 这些扫描都持有调度器全局同步锁 `entries`（`parking_lot::Mutex`），选号那次还在全局门锁 `selection_reservation_gate`（`tokio::sync::Mutex`）之内，**本质上是串行的**。按 perf 估算，单个请求光模型判断就要在锁内花约 95 ms，吞吐上限约每秒 6 个。线上 `/readyz` 实测 `dispatchRatePerSec` 为 4–8，每分钟完成量 300–400，正好吻合。
4. 等待 `entries` 锁的线程是 tokio worker 本身（同步锁直接阻塞线程），worker 被占满，I/O 驱动无人轮询，运行时延迟达到秒级（2.2 节）。于是：
   - Redis 调度协调的 75–500 ms 预算频繁超时、熔断打开；
   - Token 刷新的分布式协调失败（每 10 分钟约 300 次），需要刷新的账号又被排除，调度重试，额外再扫描；
   - PG 用量写入超时；
   - 已经开始的流的转发和心跳也变慢。
5. 解除入口限流前，入口每分钟最多放进 300 个、在途最多 32 个，正好压在这个天花板附近（11:34–11:39 的调度 P50 已经从 36 ms 涨到 127–224 ms）。解除之后到达量超过天花板，排队无界增长，等待时间从秒级涨到百秒级。客户端约 120 秒断开后还会重试，进一步推高到达量。

### 4.2 证据：用逐字节一致的二进制做符号化 perf

- 线上二进制是 strip 过的。在 `rust:1.92.0-alpine3.23` 中，用与 Dockerfile 相同的设置（`CARGO_PROFILE_RELEASE_LTO=false`、`CODEGEN_UNITS=16`、在 `/app` 下构建、同样的 `ui/dist`），对 `705622dd` 执行 `cargo rustc --release --bin kiro-rs -- -C strip=none`。产物的 `.text` 段与线上二进制**逐字节相同**（34533266 字节），符号可以直接用来解析线上采样的地址。
- 13:23 采集在 CPU 上的样本（`perf record -F 299 --call-graph dwarf,16384`，15 秒，6944 个栈），结论如下（完整列表见 `evidence/perf-cpu-top.txt`）：

| 函数（含子调用） | 占比 |
|---|---:|
| `model_support::model_is_supported_by_list` | **53.6%** |
| `support_match_variants` / `claude_model_variants` | 49.9% / 41.6% |
| `MultiTokenManager::compute_local_pool_route_state`（入口与外部池前置检查） | **36.6%** |
| `alloc::fmt::format::format_inner` | 32.7% |
| `memcpy`（自身） | 20.8% |
| `select_next_credential_excluding` | 18.1% |
| `token::count_all_tokens_local`（其中 `forced_backoff::fit_to_budget` 8.6%） | 10.5% |
| `<Config as Clone>::clone`（整份配置深拷贝） | 0.4% |
| `reload_credentials_from_postgres` | 0.3% |

- 13:31 采集线程切出时的栈（`sched:sched_switch`）：能完整展开的 3428 个栈里，2239 个落在 `model_is_supported_by_list` 内，说明线程大多是在执行这段代码时被调度切走的。

### 4.3 各因素的权重与归属

| 因素 | 权重 | 归属 | 处理 |
|---|---|---|---|
| 模型判断每次重新展开列表、全池扫描在锁内串行 | **主因**（CPU 54%，决定吞吐上限） | main 与 forced 共有 | 已修（`ae0ce8ab`） |
| 账号翻倍、解除入口限流 | 触发条件 | 配置 | 修复后不再敏感；见第九节 |
| 外部池前置检查多一次扫描 | 主要放大（CPU 36.6%，包含在上一行的 54% 之内） | 共有代码 + 59140 配置 | 修复后扫描很便宜；修复前可以关闭外部池临时缓解 |
| Token 刷新通知自触发全池重载 | 次要放大（CPU 0.3%，但周期性占锁、加重 PG 压力） | 共有 | 已修（`bea2ead9`） |
| 强制映射窗口适配二次方开销、在 worker 上同步执行 | 次要（CPU 8.6%，单次约 0.45 秒，期间占住 worker） | **forced 独有** | 已修（`fc2bc87d`） |
| 调度等待缺少整体期限（`credentialDispatchMaxWaitSecs=5` 管不到门锁等待） | 让排队可以无限长 | 共有 | 未修，列为后续 |

---

## 五、修改清单（只在本地，未推送、未部署）

### 5.1 提交与分支

- **main**（基于 origin/main `66c85a7b`，本地 `main` 已快进到 `33a4c2d7`）：
  - `ae0ce8ab` perf(scheduler): cache model-support expansions used by every pool scan
  - `bea2ead9` fix(runtime-events): stop full credential reloads on every token refresh
  - `4bb325f1` fix(provider): stop re-sending thinking signatures a conversation already had rejected
  - `33a4c2d7` docs(analysis): 共有问题文档
- **forced-main**（基于 `678e75e2` 0.0.259）：
  - `67e70bd3` 合并 main（ae0ce8ab、bea2ead9）
  - `fc2bc87d` perf(forced): fit long histories without re-counting every dropped turn
  - `889009bf` 合并 main（4bb325f1）
  - `23c5ce87` fix(forced): give rewritten visible thinking a signature that replays
  - `60974e2e` 合并 main（33a4c2d7 文档）
  - 本文档所在的提交
- 辅助分支：`fix/scheduler-dispatch-hot-path`（指向 `bea2ead9`）、`fix/thinking-signature-replay`（指向 `33a4c2d7`）。工作树 `/root/code/kiro.rs-wt-sched-perf` 检出在后者。

### 5.2 各项改动

1. **`ae0ce8ab` 模型判断缓存**（main，`src/model/model_support.rs`）：在线程内按内容缓存模型列表的展开结果，命中时逐项比较原始列表，内容变了就不会用旧结果；同时缓存候选模型的变体；`subscription_tier_key` 记住上一次的标题。判断语义与旧实现逐条等价。一次 848 账号的全池扫描从 85–89 ms 降到 0.3–1 ms。测试：与旧算法参照实现逐一比对、同一列表原地修改、等级标题切换。
2. **`bea2ead9` Token 刷新通知**（main）：刷新事件携带 `sourceInstanceId` 和 `credentialId`。发布者跳过自己的事件；其他实例只合并该账号的刷新字段（`reload_refreshed_credential_from_postgres`），不再整池重载。其余 `credentials_changed` 事件不变。
3. **`4bb325f1` 会话级签名记忆**（main，新增 `src/kiro/provider/rejected_reasoning.rs`）：上游返回 `THINKING_SIGNATURE_INVALID` 时，记住该会话请求历史里的所有签名；同一会话后续请求在首次发送前就去掉这些推理块，新产生的签名保留。最多 4096 个会话，6 小时不活跃就遗忘。
4. **`fc2bc87d` 强制映射窗口适配**（forced 独有，`src/anthropic/forced_backoff.rs`、`handlers.rs`、`token.rs`）：
   - 旧做法：`fit_to_budget` 超预算时逐轮丢弃最早的对话，每丢一轮就对整个请求重新估算一次 token，开销是二次方的。
   - 新做法：本地估算对消息是逐条相加的，所以先算出每条消息的 token，丢弃时直接扣减；裁剪决定与旧实现完全相同（测试拿旧算法对 5 组规模 × 2 种对话形态比较报告和裁剪结果）。
   - `fit_to_budget` 改为通过 `run_cpu_heavy_in_place` 执行，大请求不再占住异步 worker。
   - 新增 `token::count_message_tokens_local`。
5. **`23c5ce87` 身份防护的签名处理**（forced 独有，`src/anthropic/identity_guard.rs`）：
   - 问题：forced 模式下身份防护会改写可见思考里的自我描述，还会裁掉思考首段的空白，但 `signature_delta` 原样转发。客户端下一轮回传"改写后的文本 + 原签名"，Bedrock 校验不通过。
   - 修复：思考块不再裁首段空白。流式路径累计每个思考块"收到的原文"和"转发出去的文本"，两者不同时，把签名换成对转发文本计算的合成签名（`synthetic_thinking_signature`）。非流式路径的 `scrub_content_as` 改写了思考文本时同样替换签名。合成签名在回放时由 `converter/history.rs` 识别，按无签名思考处理，不会作为签名推理发给上游。
   - 新增 3 个测试：**尚未运行**。

### 5.3 已有的单测结果

在 forced-main 快照（`67e70bd3` 加上 `fc2bc87d` 的改动）上以调试模式运行，过滤 `model_support token_manager redis_runtime forced_backoff credentials capacity selection`，跳过 `token_manager::refresh`：**580 通过、0 失败、1 忽略**。跳过的原因见共有文档第五节：两个突发刷新压力测试把同机负载顶高，被看门狗中止，它们与本次改动无关。

---

## 六、"账号连续报错"（THINKING_SIGNATURE_INVALID）详述

- **用户看到的现象**：尝试记录形如 `#358 thinking_signature_retry_same_credential … status 400 … reason=THINKING_SIGNATURE_INVALID … messages.35.content.0: Invalid signature in thinking block`，紧接着 `#358 response_headers_received_after_thinking_signature_retry`，也就是同一账号去掉历史推理后重试成功。
- **规模**（截至约 15:00 的 24 小时）：
  - 59140：173 次，占 235757 个请求的约 0.07%；59138（main 部署）：89 次，占 169346 个请求的约 0.05%。两边量级相同，**不是 forced 独有**。
  - 59140 近 6 小时：93 次最终成功（涉及 59 个账号），15 次最终失败（集中在 10:33–10:44，涉及 3 个账号）。
  - 这 173 次分布在 106 个会话里，最多的一个会话 15 次，其次 12 次、12 次。
  - 被浪费的第一次发送耗时中位 514 ms、P90 13.6 秒。
- **对账号的影响**：签名重试分支不调用任何账号失败上报，账号健康度、失败计数、冷却都不受影响。"连续报错"只是同一会话被粘在同一账号上的表象。
- **无效签名的来源**（按出错会话在本实例上的前序轮次统计）：
  - 不少出错请求在本实例上**没有任何前序轮次**。签名来自其他渠道：sub2api 在多个上游之间切换，同一 Claude Code 会话的前几轮可能由其他渠道处理，那些签名在 Kiro/Bedrock 这边自然无效。这一类无法提前判断，只能失败一次后由重试兜住，现在再加上会话记忆，避免同一会话反复失败。
  - 另一部分会话的前序几十轮都在本实例、用同一上游模型处理（例如某会话有 32 个前序轮次）。这类会话更可能是本服务自己转发给客户端的思考块被改过，但不能排除中间个别轮次由其他渠道处理（那些轮次不会出现在本实例的记录里）。代码层面已确认：forced 独有的身份防护会改写可见思考却保留原签名（5.2 第 5 项）。
  - **尚未验证的假设**：`thinking.display=omitted`（5.x 展示模型的默认值）会把思考文本清空、保留真实签名转发给客户端，回放时是"空文本 + 真实签名"。如果 Bedrock 不接受这种回放，这也会是一个来源。但出错比例只有 0.07%，而这条路径几乎覆盖所有多轮会话，所以大概率可以接受。第十节用例 E 专门验证这一点。
- **修复**：main 上的 `4bb325f1`（会话记忆）加上 forced 独有的 `23c5ce87`（身份防护）。

---

## 七、forced 独有、未修复、需要你决策：输入过长被改成 529

- **现象**："强制模型收敛：上游判定输入过长且服务端退避已用尽，按临时过载返回"，40 分钟内 61 次（约每分钟 1.5 次）。`usage_records` 里记为 error，内部 `public_status=400`、`reason=CONTENT_LENGTH_EXCEEDS_THRESHOLD`；但按日志，客户端实际收到的是 529 `overloaded_error`。
- **样本**：`original_bytes=754070`，`original_weight=1639831`，`max_weight=150000`，`original_history_entries=4`，`trimmed_history_entries=0`，`still_oversized=true`。历史几乎没有可裁的，超量全在**当前轮**（当前轮的工具结果、用户内容、文档、图片都受保护，不允许截断）。
- **机制**：
  1. 强制映射按 token 窗口适配了请求，但 Kiro 的拒绝依据是负载字节或权重阈值；
  2. `payloadGuardMode=on_too_long`，所以先把超长请求发出去，被拒后裁剪历史再重试一次，仍然超限；
  3. 出于"隐藏真实模型"的设计（1cd03e77 的提交说明："a full window is never reported as prompt-too-long or model_context_window_exceeded"），最终改成临时过载返回；
  4. 客户端把它当成临时错误，带着同样的输入反复重试，每次都必然失败，还要发两次上游调用。
- **可选方案**（需要你选）：
  - **A（推荐）**：改为返回官方语义的 HTTP 413 `request_too_large`。这是官方对请求过大的标准错误，不暴露模型窗口，客户端不会自动重试同样的输入。同时把 `payloadGuardMode` 改成 `preemptive`，在发送前就裁剪，省掉第一次必然失败的调用。
  - B：开启 `payloadShaping.truncateCurrentToolResults` 等当前轮截断。会丢失内容，影响结果正确性。
  - C：维持现状。

---

## 八、对另一份分析文档的核对（`prod-59140-huaqiangbei-429-ttft-root-cause-20261117.md`）

| 那份文档的说法 | 核对结果 |
|---|---|
| 大量 429 来自共享主 Key 的入口 RPM 300，取消限额后最终 429 停止 | 本会话没有复核 sub2api 一侧；与本服务审计时间线一致 |
| 首字升高主要发生在调度阶段，5 秒等待配置管不到完整的选号过程 | **成立**，本文确认了具体的串行点与成因 |
| "尚未证明某把锁或 memcpy 是唯一根因" | 现已证明：`entries` 锁内的模型判断占 CPU 54%，memcpy 和分配都出自这里 |
| `runtime_config()` 深拷贝是潜在放大点 | **不成立**：perf 中只占 0.4% |
| 建议的配置调整（`payloadGuardMode=preemptive`、首内容超时 30 秒、hold 45 秒） | 解决不了调度串行；`preemptive` 只对第七节有用 |
| 529 来自输入过长被改成过载 | **成立**（第七节） |
| 建议补齐调度分段指标、统一的调度期限 | 仍然有价值，列为后续工作 |

---

## 九、配置建议（全部未执行，需要你确认）

### 9.1 代码上线前可以立刻缓解的手段

1. **关闭外部池** `externalPools.externalPoolsEnabled=false`（通过管理端运行配置修改）。
   - 依据：近 3 小时外部池使用次数为 0，但它的前置检查占 CPU 36.6%。
   - 预期：串行开销减少约三分之二，调度吞吐提升到约 2–3 倍。
   - 代价：本地池完全不可用时没有外部兜底。
2. **把流量分到更空闲的实例**：15:48 时 59143 的 CPU 是 0.4%。注意：每个实例都会扫描各自的账号池；多个实例如果共用同一批账号，单实例的扫描开销不会下降。
3. **临时减少启用账号**：扫描开销与"启用且未被额度封禁"的账号数成正比。比如暂时禁用一半新导入的账号，吞吐上限大约翻倍。当前在途约 150 个，容量远远过剩。
4. **不要做的事**：
   - **不要清空或缩短 `free` 等级的模型列表**。清空后可能被"自动拉取 Free 模型列表"重新填充（`set_subscription_model_list` 的 `only_if_missing`）。而且调度筛选到底用展示模型名（例如 claude-opus-5）还是映射后的目标模型名，在流式路径上没有完全核实（`handle_stream_request` 收到的是 `&payload.model`），缩短列表可能导致所有账号被判为"不支持该模型"，造成全量失败。
   - 提高或降低单号并发、RPM、全局上限都无效。在途远低于这些上限。
   - 恢复入口限流只是把超时换成 429，不能提高吞吐。只适合在修复上线后，作为过载保护重新设一个较宽的上限。

### 9.2 修复上线后

- 可以继续保持外部池开启，单次扫描已经很便宜。
- 建议给入口设一个宽松的保护上限（按上线后实测的吞吐和在途决定，本文不凭空给数字）。
- 第七节方案 A 若被采纳，同时把 `payloadGuardMode` 改为 `preemptive`。

---

## 十、未完成的工作与验证方案

### 10.1 编译资源规则（本次曾挤爆同机内存，必须遵守）

本次事故：kiro-rs 是一个很大的单 crate，rustc 峰值内存 4–5.6G。release 构建如果不覆盖 `Cargo.toml` 里的 `lto = true`，单个 rustc 会占用 5G 以上、持续半小时以上。我给编译容器设了 4G 内存上限，却允许溢出到 10G 交换分区，等于没有硬上限，挤占了同机生产服务（59138、59140、59142、59143、kirotest、trends 等）的内存和 swap。所有编译容器已在 15:3x 全部删除。

今后的规则：

- 开始前，主机可用内存 ≥ 8G、1 分钟负载 < 6；
- 容器硬上限 `--memory 5g --memory-swap 5g`，CPU 不超过 2 核，`nice -n 19`；一次只跑一个编译，结束立即 `docker rm`；
- 调试测试设 `CARGO_PROFILE_DEV_DEBUG=0`、`CARGO_PROFILE_TEST_DEBUG=0`；release 构建设 `CARGO_PROFILE_RELEASE_LTO=false`、`CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16`；
- 测试加 `--test-threads=1`，并跳过 `token_manager::refresh` 里的突发压力测试；
- 或者改用 GitHub CI 编译，本机零占用，但需要推送分支。

### 10.2 待做的单测与 CI 门禁

```bash
# forced-main 快照上（含全部改动）
cargo test --locked --bin kiro-rs -- --test-threads=1 rejected_reasoning thinking_signature identity_guard signature converter::history forced_backoff
# main 上
cargo test --locked --bin kiro-rs -- --test-threads=1 rejected_reasoning thinking_signature model_support redis_runtime
# 两个分支都要过的 CI 门禁（见 .github/workflows/build.yaml）
cargo fmt --check
node scripts/ci/check-clippy-baseline.mjs
cargo test --locked --all-targets -- --test-threads=1
```

预期：
- `rejected_thinking_signatures_are_dropped_before_later_turns_are_sent`：第一轮发送 2 次，之后每轮发送 1 次；
- `rewritten_visible_thinking_gets_a_signature_that_replays_as_unsigned`：改写后的思考带合成签名，且签名与文本匹配；
- `untouched_visible_thinking_keeps_its_text_and_upstream_signature`：未改写的思考，文本（包括首段空白）和签名都原样保留。

### 10.3 本地端到端压测（验证调度修复）

完全隔离：独立 docker 网络 `kirobench-net`，不映射主机端口，只使用本地伪造数据，不连接任何现有服务的存储。脚本在 `scripts/` 下，**还没有实际运行过**，首次使用可能需要调整。

1. 准备：
   - 被测二进制：旧版用线上二进制副本 `tmp/diag59140/perf/kiro-rs-prod-0.0.257`（sha256 `d15250186d8799a244fd5338dffdb672555028f981af64679d511fa1aae4af6a`）；新版需按 10.1 的规则构建 forced-main 当前 HEAD 的 release 二进制；
   - 压测和假上游工具 `tmp/kirobench/bin/kiro_loadtest`，已构建；
   - `python3 scripts/sanitize_config.py <目录>/config.json` 生成脱敏配置：线上全部业务配置保留（含约 1MB 的 cachePolicy），密钥和存储地址替换，上游指向假 Kiro，关闭后台刷新；
   - `python3 scripts/make_seed_credentials.py` 生成 940 个伪造账号（KIRO FREE、无自带模型目录、85 个禁用）。
2. 执行：`scripts/bench.sh infra` 启动 PG、Redis、假上游（每个流先等 800 ms，再分 20 段、每段间隔 100 ms 输出，约 3 秒一个流）→ `bench.sh app <二进制>` → `bench.sh load <标签> <并发> <秒数>`。旧版、新版各跑一组：并发 50、100、200，每组 120 秒。
3. **为了贴近线上**，还要在 `external_upstream_pools` 表里插入一个指向不可达地址的假外部池，让外部池前置检查真正执行（脚本还没包含这一步）。
4. 观测：本地 `usage_records` 的调度耗时 P50/P95、首字 P50/P95、完成量；采样期间 `/readyz` 的 `runtimeLagMs`；容器 CPU。
5. 验收：
   - 旧版在并发 100 以上时，调度耗时随时间持续增长，吞吐被限制在约每秒 6 个，`runtimeLagMs` 达到百毫秒到秒级；
   - 新版吞吐只受假上游限制，调度 P50 < 50 ms、P95 < 500 ms，`runtimeLagMs` < 50 ms。

### 10.4 真实上游签名验证（导入少量 59140 账号）

1. **安全措施**：
   - 只从 59140 只读挑 3 个启用、失败计数为 0、`expiresAt` 至少还有 40 分钟的账号，复制 `credentials.data`，**删掉 `refreshToken`**；
   - 本地配置关闭后台刷新（`sanitize_config.py --real-upstream`），保证本地实例无法刷新 Token，不会触发 refresh token 轮换、导致线上那份凭据失效；
   - 本地实例只绑定 `127.0.0.1:29191`，使用独立数据库 `kirobench_real` 和 Redis db 1；
   - 总请求量不超过 30 次；测试结束后删除本地数据库里的账号副本。
2. 执行 `python3 scripts/sigtest.py`，旧版、新版各一轮。用例与预期：
   - A：原样回放带签名的思考 + 工具调用。预期新旧都一次成功（基线）。
   - B：改写思考文本、保留原签名（模拟身份防护改写）。预期新旧都先返回 `THINKING_SIGNATURE_INVALID`，再重试成功（证明"改了文本就会签名无效"）。
   - C：同一会话下一轮仍然带着 B 的历史。**旧版**再失败一次后重试成功；**新版**直接一次成功（会话记忆生效）。
   - D：改写思考文本、换成合成签名（新版身份防护的输出）。预期新旧都一次成功。
   - E：`display=omitted` 时回放"空文本 + 真实签名"。若被判无效，说明这也是一个来源，需要另外修。
3. 判断依据：本地 `usage_records.data.credentialAttempts` 的 `action` 序列（脚本会打印出来）。

### 10.5 上线方案（需要你批准）

1. 推送 main 和 forced-main，按发布流程升版本号，由 CI 构建镜像。
2. 先在一个实例上更新（由你决定是哪一个），保留旧镜像 `0.0.257` 用于回滚；容器配置了 45 秒排空、60 秒优雅退出，重启会中断正在进行的长流，建议选低峰时段。
3. 同等流量下观察至少 30 分钟。验收指标：
   - `/readyz` 的 `runtimeLagMs` < 100 ms；
   - 调度 P50 < 100 ms、P95 < 1 秒；
   - 流式首字 P50 回到 2–3 秒；
   - 客户端断开接近 0；
   - perf 里 `model_is_supported_by_list` < 2%；
   - 日志不再出现 `reason=token_refreshed` 的整池重载；
   - 同一会话的 `THINKING_SIGNATURE_INVALID` 不超过 1 次。
4. 回滚：把镜像改回 `ghcr.io/2ue/kiro-rs-forced:0.0.257` 并重启。

### 10.6 后续代码工作（未开始）

- 给调度等待加整体期限（共有，见共有文档第七节）；
- 外部池容量宽限的 50 ms 轮询改为等待容量变化信号（共有）；
- 会话签名记忆挪到 Redis，供多实例共享（共有）；
- 第七节的 413 方案（forced 独有，待决策）。

---

## 十一、本地产物（未提交，都在本机）

| 位置 | 内容 |
|---|---|
| `tmp/diag59140/q.sh`、`api.py` | 59140 只读查询工具（PG 只读事务、管理端只读接口） |
| `tmp/diag59140/perf/` | 原始 `perf-dwarf.data`（113MB）、`perf-offcpu.data`（193MB）、展开后的 `stacks.txt` 和 `offcpu-stacks.txt`、符号表 `syms.txt`、解析脚本 `parse2.py` 和 `sym.py`、线上二进制副本 `kiro-rs-prod-0.0.257`（加载基址 `0x7f5267d1e000`） |
| `tmp/build705/` | `705622dd` 源码导出、前端产物、未 strip 的重建二进制 `kiro-rs-unstripped` |
| `tmp/build-forced-fix/` | 跑单测用的 forced-main 快照 |
| `tmp/kirobench/` | 脱敏配置 `config.json`（1.2MB，不含密钥）、`seed-credentials.sql`、`bin/kiro_loadtest`、脚本原件 |
| `tmp/bench-model-support/` | 模型判断离线基准工程 |
| docker 卷 | `kiro705-target`（release、关闭 LTO 的依赖缓存）、`kirobench-target-debug`（调试依赖缓存）、`kiro-cargo-registry` |

## 十二、本目录文件

- `evidence/latency-per-minute.txt`：09:30–15:47 每分钟的请求数、成功、断开、调度和首字分位数。
- `evidence/audit-config-timeline.txt`：11-16 以来的配置类审计记录（已隐去 Key 标识）。
- `evidence/perf-cpu-top.txt`：符号化后的 CPU 采样与线程切出采样排行。
- `evidence/bench-model-support/`：模型判断新旧实现的离线基准程序与结果。
- `scripts/`：本地隔离压测和真实上游签名验证的脚本（尚未执行，见第十节）。
