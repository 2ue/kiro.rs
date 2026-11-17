# 调度热路径模型判断、Token 刷新重载风暴、思考签名重复失败：问题与修复（main）

本文记录 2026-11-17 在 main 上做的三项修复：问题现象、成因、改法、验证情况和还没做完的工作。所有时间都是北京时间（UTC+8）。

触发这次排查的是 forced-main 部署的 59140 实例（运行版本 0.0.257，提交 `705622dd`）。生产现场的完整时间线、证据，以及 forced-main 独有的问题，见 forced-main 上的 `docs/analysis/prod-59140-ttft-scheduler-collapse-20261117/README.md`。本文只讲 main 和 forced-main 共有的代码问题。调度相关代码在 `705622dd` 和 origin/main（`66c85a7b`，0.0.253）之间只差 21 行，都是 forced 专用的管理端函数。因此这里的问题在 main 上同样存在，也必须在 main 上修，再合并进 forced-main。

## 一、提交清单

| 提交 | 分支 | 内容 | 单测状态 |
|---|---|---|---|
| `ae0ce8ab` perf(scheduler): cache model-support expansions used by every pool scan | main | 问题一：模型判断缓存 | 已通过（见第五节） |
| `bea2ead9` fix(runtime-events): stop full credential reloads on every token refresh | main | 问题二：Token 刷新通知 | 已通过 |
| `4bb325f1` fix(provider): stop re-sending thinking signatures a conversation already had rejected | main | 问题三：会话级签名记忆 | **未运行**（编译被中止，见第五节） |

三个提交都基于 origin/main `66c85a7b`。本地 `main` 已快进到 `4bb325f1`，尚未推送。forced-main 通过合并提交 `67e70bd3`（含前两个）和 `889009bf`（含第三个）引入。

## 二、问题一：调度每次扫描都在为每个账号重新展开模型列表

### 现象

- 59140 在账号从约 476 个增加到 940 个（启用约 850 个）、并解除 Key 入口限流后，选号阶段的耗时（`latencyTrace.credentialDispatchElapsedMs`）中位数从约 36 ms 涨到 50–107 秒。首字中位涨到 60–90 秒，大量请求在约 120 秒时被客户端断开。
- 调度容量其实很充裕：在途约 142 个，排队 0，可调度账号 853 个。慢的原因不是缺号。
- 进程 CPU 只有约 1.07 核（8 核机器），但任意时刻都至少有一个线程在运行（15 秒采样中"0 个线程在跑"的比例为 0%）。futex 系统调用只有约 257 次/秒。主运行时没有任何一个 worker 停在 `epoll_wait`，`/readyz` 报出的运行时调度延迟（`runtimeLagMs`）在 0.3–2.4 秒之间。

### 原因

所有"按模型筛选账号"的判断，最终都会调用 `src/model/model_support.rs` 的 `model_is_supported_by_list`。调用链如下：

- `KiroCredentials::supports_model_with_tier_lists`
- `credential_is_usable_for_model`
- `credential_is_dispatchable`
- `credential_catalog_lists_model`

旧实现每调用一次都要做这些事：

1. 把账号的模型列表逐个 `clone`，再 `normalize_supported_models`。这个函数去重是 O(M²) 的字符串比较。账号没有自己的列表时，用的是订阅等级列表，59140 的 `free` 等级手工配了 33 个模型。
2. 对每个模型调用 `support_match_variants`。这一步会生成点号/横线、带日期、`-thinking` 等多个变体，用 `format!` 产生大量短字符串，再排序、去重。
3. 对候选模型做同样的变体展开，然后两两比较。

在 59140 上，848 个启用账号全部是 KIRO FREE，都没有自己的模型目录，全部走同一张 33 模型列表。一次全池扫描就要为同一张列表重复展开 848 次，单次扫描约 85–89 ms。

而一个请求要做好几次全池扫描，并且这些扫描都持有调度器的全局同步锁 `entries`（`parking_lot::Mutex`），选号那次还处在全局门锁 `selection_reservation_gate` 之内：

| 扫描位置 | 作用 |
|---|---|
| `MultiTokenManager::compute_local_pool_route_state` | 入口检查（`local_pool_route_state_cached_for_route`）和外部池前置检查（`local_pool_route_state_fresh_for_route`，`handlers.rs` 多处调用；容量宽限循环中每 50 ms 重算一次） |
| `select_next_credential_excluding` → `credential_is_dispatchable` / `select_candidate` → `prefer_catalog_listed` | 选号 |
| `get_bound_credential`、`should_migrate_sticky_to_better_tier`、`credential_accepts_session_binding` | 会话粘性相关 |

这带来三个后果：

1. **吞吐被串行开销锁死**。按 perf 估算，单个请求光模型判断就要在锁内花约 95 ms，调度吞吐上限约每秒 6 个。线上 `/readyz` 实测 `dispatchRatePerSec` 为 4–8，正好对应每分钟 300–400 个完成请求的天花板。
2. **账号数翻倍，吞吐减半**。开销与"启用账号数 × 列表长度"成正比；已禁用或额度已封的账号会在 `credential_is_usable_for_model` 里先被短路，不产生开销。
3. **运行时被拖垮**。需要 `entries` 的其他任务（流的释放与心跳、统计、Redis 同步回写等）在 futex 上阻塞，worker 线程被占住，没人轮询 I/O 驱动。所有 Redis、PG、HTTP 操作的完成通知都晚 1–2 秒才被处理，于是出现 Redis 协调超时、熔断打开、Token 刷新协调失败、`SELECT now()` 被记成慢语句等连锁现象。

解除 Key 入口限流（原来是 RPM 300、并发 32）之后，到达率超过了这个天花板，排队无界增长，调度耗时从秒级涨到百秒级。

### 证据

- 用与线上完全相同的提交和工具链离线重建了未 strip 的二进制，`.text` 段与线上逐字节一致。用它符号化线上 15 秒的 perf 采样后：`model_is_supported_by_list` 含子调用占全部 CPU 采样的 **53.6%**，其中 `format_inner` 占 32.7%，`memcpy` 自身占 20.8%。它的两个主要调用方：`compute_local_pool_route_state` 占 36.6%，`select_next_credential_excluding` 占 18.1%。作为对照，整份配置的深拷贝 `Config::clone` 只占 0.4%。详见 forced-main 文档的证据目录。
- 离线基准（独立小程序，代码直接取自 origin/main 的旧实现和本次的新实现，使用线上同样的 33 模型列表、848 个账号）：一次全池扫描旧实现 83–89 ms，新实现 0.25–1.1 ms，快 77–360 倍；单个账号从约 100 µs 降到 0.3–1.3 µs。新旧实现对 `claude-haiku-4.5`、`claude-opus-5`、`claude-sonnet-4-6[1m]`、`gpt-4o` 的判断结果完全一致。

### 修复（`ae0ce8ab`，仅改 `src/model/model_support.rs`）

- `model_is_supported_by_list` 改成先查线程内缓存 `MODEL_SUPPORT_CACHE`：
  - 列表展开结果（变体 `HashSet<String>`）按列表内容缓存。键是整张列表的哈希；命中后还要逐项比较缓存的原始列表与传入列表，完全相同才复用，所以列表内容一旦变化（包括同一块内存被原地修改）都会重新计算，不会用到过期结果。另外保留"上一次的列表"快速路径：相邻账号通常共用同一张等级列表，连哈希都不用算。
  - 候选模型的变体按去空白后的原文缓存。
  - 两类缓存各最多 256 项，超过就清空重来。
- 判断语义与旧实现逐条等价：
  - 空列表视为不限制；
  - 候选全为空白时视为不限制；
  - 列表全为空白时视为不支持；
  - 列表变体集合与候选变体集合有交集即为支持。
- `subscription_tier_key` 记住上一次的标题和结果，相同标题不再每次 `to_lowercase` 并收集新字符串。原逻辑移到 `subscription_tier_key_uncached`，没有改动。
- 新增测试：
  - `cached_support_match_agrees_with_expanding_on_every_call`：用一份按旧算法写的参照实现，逐一比较 5 组列表 × 9 组候选、两轮（第二轮全部命中缓存）的结果；
  - `cached_support_match_follows_list_changes`：同一 Vec 原地改内容、在两张列表之间来回切换；
  - `subscription_tier_key_cache_follows_title_changes`。

## 三、问题二：Token 刷新后，本实例会对自己的通知做全池重载

### 现象

59140 日志里每 10 分钟约有 376 次 Social Token 刷新。几乎每次刷新都伴随一条"已根据 Redis 通知同步凭据快照 reason=…token_refreshed"，以及"从 PgSQL 一致性重新加载凭据和运行态"（单次 0.1–5 秒，经常超过 5 秒上限）和"已从 PgSQL 加载 940 条凭据统计"。

### 原因

2026-10-30 的 `bfc2c871` 修过一次"凭据重载风暴"：运行时事件开始携带 `sourceInstanceId`，实例会忽略自己发出的事件。但 `manager.rs` 里 Token 刷新成功后发布 `credentials_changed` 的两处（共享刷新路径和自动恢复路径）是手写的 JSON，没有经过统一的 `runtime_event_payload`，因此**不带 `sourceInstanceId`**。结果，刚刚刷新完、本地已经合并了新 Token 的实例，仍会对自己的通知执行 `reload_credentials_from_postgres`，也就是全池重载：

- 从 PgSQL 读出全部凭据和运行态；
- 在 `entries` 锁内逐个克隆、规范化、比较 940 个账号；
- 有变化时还要重载统计、重新选优先级（日志里的"优先级变更后切换凭据"）、作废模型能力分组。

多实例部署时，其他实例也会因为每一次刷新做全池重载。

需要说明它的权重：这个问题在 perf 里只占约 0.3% 的 CPU。它的主要危害是周期性持有 `entries` 锁和给 PG 带来压力，是**次要放大因素**，不是首字慢的主因。

### 修复（`bea2ead9`）

- `redis_runtime.rs` 新增 `token_refreshed_event_payload(source_instance_id, credential_id)`，事件同时带 `sourceInstanceId` 和 `credentialId`；另新增解析函数 `token_refreshed_event_credential_id`。两处发布点都改用它。
- 发布者自己收到时，被 `runtime_event_is_from_this_instance` 跳过。
- 其他实例收到后，在 `main.rs` 里改走 `spawn_refreshed_credential_reload`：在阻塞线程上调用新增的 `MultiTokenManager::reload_refreshed_credential_from_postgres(id)`，只读这一个账号；PgSQL 里的 `storage_revision` 比本地新时，才用 `merge_refresh_fields` 合并刷新字段。这和刷新方本地做的合并完全一样。其余 `credentials_changed` 事件仍走原来的全量重载合并器。
- 新旧版本混跑时：旧实例收到新事件，会像以前一样全量重载；新实例收到旧事件（没有 `credentialId`），也走全量重载。行为都是安全的。
- 新增测试 `token_refresh_events_name_the_instance_and_the_credential`。

## 四、问题三：会话里一旦出现被拒的思考签名，之后每一轮都先失败一次

### 现象

- 用户看到"很多账号连续报错"。尝试记录形如：`#358 thinking_signature_retry_same_credential … THINKING_SIGNATURE_INVALID … Bedrock error message: … messages.35.content.0: Invalid signature in thinking block`，接着是 `#358 response_headers_received_after_thinking_signature_retry`，也就是同一账号去掉签名后重试成功。
- 24 小时数据：59140 有 173 次（共 235757 个请求，约 0.07%）；main 部署的 59138 有 89 次（共 169346 个，约 0.05%），**两边都有**。59140 上这 173 次分布在 106 个会话里，最多的一个会话 15 次，其次 12 次、12 次。被浪费的第一次发送耗时中位 514 ms、P90 13.6 秒。
- 这类 400 不会调用任何账号失败上报（已核对 `provider.rs` 签名重试分支），**不影响账号健康度，也不会导致禁用或冷却**。"连续报错"只是尝试记录里看起来如此：会话粘性把同一会话绑在同一个账号上，所以表现为该账号连续出错。

### 原因

- 上游拒绝后，`provider.rs::call_api_with_retry` 会在同一账号上用 `build_thinking_signature_retry_body` 重试一次，该函数调用 `ConversationState::clear_history_reasoning_content_for_compatibility_retry`，去掉全部历史 `reasoningContent`。
- 但客户端下一轮仍会带着同样的历史过来，而服务端没有任何记忆，所以该会话之后每一轮都要先发一次必然失败的请求。
- 无效签名的来源（数据显示两类都有）：
  - 一部分出错请求在本实例上**没有任何前序轮次**。签名很可能来自其他渠道，比如 sub2api 在多个上游之间切换，同一会话前几轮由别的渠道处理；
  - 另一部分会话的前序几十轮都在本实例、用同一个上游模型处理。其中 forced-main 有一个确定成因：身份防护改写了可见思考文本，但保留了原签名（forced-main 独有，在 forced-main 文档和提交 `23c5ce87` 中修复）。

### 修复（`4bb325f1`）

- 新增 `src/kiro/provider/rejected_reasoning.rs`，内含 `RejectedReasoningSignatures`：
  - 按 Kiro 请求里的 `conversationState.conversationId` 记录被拒请求历史里所有 `reasoningContent.reasoningText.signature` 的哈希；
  - 最多保存 4096 个会话，6 小时不活跃就遗忘，超出上限时淘汰最久未访问的会话。
- `call_api_with_retry` 收到 `THINKING_SIGNATURE_INVALID` 时，在原有的同账号重试之前，先记住当前请求体里的签名。
- 同一会话之后的请求，在首次发送前就去掉签名命中记忆的那些 `reasoningContent`（只去掉这些；拒绝之后新产生的签名保留）。去掉后的请求体放进原有的 `refreshed_request_body`，之后的所有尝试都使用它。
- 与现状相比：现在每轮重试时本来就会去掉全部历史推理。新做法上游最终收到的内容不会比现状更少，而且省掉了一次必然失败的发送。
- 局限：
  - 记忆只在进程内，多实例时每个实例各自要失败一次才会记住；
  - 每当客户端从其他渠道带来新的外来签名，仍会先失败一次（重试机制兜底）。
- 新增测试：
  - 模块内 3 个单测：只去掉被拒签名、不跨会话、容量有上限；
  - `provider.rs` 的 `rejected_thinking_signatures_are_dropped_before_later_turns_are_sent`：同一会话连续三轮，流式和非流式都测，预期第一轮发送 2 次，之后每轮发送 1 次。

## 五、验证情况

### 已完成

- 问题一的离线基准与结果比对（第二节）。
- 在 forced-main 合并快照上（`67e70bd3` 加上 `fc2bc87d` 的改动）以调试模式运行单测，过滤 `model_support token_manager redis_runtime forced_backoff credentials capacity selection` 并跳过 `token_manager::refresh`：**580 通过，0 失败，1 忽略**。
  - `token_manager::refresh` 之所以跳过，是因为其中两个突发刷新压力测试会瞬间起大量线程，把这台同时跑着生产服务的主机负载顶高，被资源看门狗中止过三次。它们与本次改动无关。在被中止前的几轮里，已运行到的 refresh 测试都通过了。

### 未完成（需要编译，已按用户要求暂停）

1. `4bb325f1` 的单测：`cargo test --locked --bin kiro-rs -- rejected_reasoning thinking_signature`。
2. CI 门禁全集：`cargo fmt --check`、`node scripts/ci/check-clippy-baseline.mjs`、`cargo test --locked --all-targets -- --test-threads=1`、前端构建。
3. 端到端压测与真实上游签名验证。方案见 forced-main 文档第十节。

**编译资源规则**：这个 crate 编译峰值内存 4–5.6G，曾挤爆同机生产服务的内存。后续本机编译必须同时满足以下条件：

- 开始前主机可用内存 ≥ 8G、1 分钟负载 < 6；
- 容器硬上限 `--memory 5g --memory-swap 5g`（不允许溢出到交换分区），CPU 不超过 2 核，`nice -n 19`；
- 一次只跑一个编译任务，结束后立即删除容器；
- 调试构建关闭调试符号（`CARGO_PROFILE_DEV_DEBUG=0`、`CARGO_PROFILE_TEST_DEBUG=0`）；
- release 构建要覆盖 `Cargo.toml` 里的 `lto = true`（设 `CARGO_PROFILE_RELEASE_LTO=false`、`CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16`，与 Dockerfile 一致），否则单个 rustc 会占用 5G 以上内存并持续半小时以上。

也可以改用 GitHub CI 编译，本机零占用，但需要推送分支。

## 六、风险与回滚

- 问题一的缓存是纯函数结果的按内容缓存，命中前会校验内容，不会因配置或账号模型列表变化而返回旧结果。每个线程至多缓存 256 张列表和 256 个候选，内存可以忽略。回滚只需单独 revert `ae0ce8ab`。
- 问题二只改变 Token 刷新事件的载荷和接收处理，其他事件不受影响。回滚 revert `bea2ead9`。
- 问题三只会去掉"该会话已被上游明确拒绝过"的签名所在的推理块；最坏情况是保留了较少的历史推理，不会导致请求失败。回滚 revert `4bb325f1`。

## 七、未纳入本次修改、但建议后续处理的共有问题

1. 调度等待没有整体期限：`acquire_context_for_dispatch_with_version` 里的门锁等待、会话绑定读取等步骤，不受 `credentialDispatchMaxWaitSecs` 约束，这是调度能等上百秒的直接原因之一。问题一修好后门锁等待会回到毫秒级，但仍建议补一个整体期限，并在期限内正确释放 provisional slot、Redis lease 和 RPM 预留。
2. 外部池容量宽限循环（`handlers.rs::local_pool_preflight_reason_after_capacity_grace`）在本地容量满时每 50 ms 做一次全池扫描。问题一修好后单次扫描很便宜，但等待的请求一多仍会放大开销，建议改为等待容量变化信号。
3. 问题三的记忆可以挪到 Redis，供多实例共享。
