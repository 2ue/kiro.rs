# 当前代码重新验证矩阵

Date: 2026-07-23

Role: 修复前复现、修复后回归和发布门禁的可执行合同

Status: `execute-ready / final-release-gate-pass / explicit-limits-recorded`

2026-07-23 最终统一候选已完成 release gate：仓库外 `kiro-rs` SHA `925525419cd48b460217df2568891a40287da0c44d2bf921a38b103c047775ee`，`kiro_loadtest` SHA `90babda7388aa93854cbbdb81c132cc436c07f46b0ea22973531b0a7ffb3aff1`。`final-c0-release-20260723-r4` 通过 `cargo +1.92.0 fmt --all -- --check`、`cargo +1.92.0 test --all-targets`（main `1750 passed / 0 failed / 6 ignored`，`kiro_loadtest 31/31`）和 `cargo +1.92.0 build --release --bins`，scoped target `removed=true reservation_released=true`；同轮 feature docs 47/47 与 115 links、`node --test feature/tests/*.test.mjs`（`283 tests / 261 pass / 22 explicit skips / 0 fail`）、`git diff --check` 和 final inventory `targets=0 reservations=0 target_processes=0 blockers=0` 通过。该批证据见 [final release gate evidence](../evidence/final-release-gate-20260723.md)。Docker 动态验证按用户要求豁免，不计 pass；真实生产 `9022` 未被停止、重启、迁移或压测。

2026-07-21/22 C0d 当前候选已补一轮非 Docker、非生产的 release-bound 静态/工具链证据：仓库外 `kiro-rs` SHA `fefd6204c1851c9795ae16fb006115997f7884570988622a77200c3e438cd7ec`，`kiro_loadtest` SHA `f92e91b4f9c2d669e29e6bbb9e4d4b58f38d2f8bfac3f4bd51260c0d2edd6782`。C0d 通过 `cargo fmt --check`、`cargo test --all-targets`（main `1742 passed / 0 failed / 6 ignored`，`kiro_loadtest 31/31`）、`cargo build --release --bins`、feature docs 47/47 与 108 links、静态 UI/cost/MCP/request-key/prompt 合同、`node --test feature/tests/*.test.mjs`（`280 tests / 258 pass / 22 explicit skips / 0 fail`）、Claude CLI raw thinking capture `6 effort × 5 rounds`、两套 UI build、C0d `kiro_loadtest` fake-upstream 小矩阵与 recovery rerun，并在只删除无引用 root debug/flycheck 产物后取得 final inventory `targets=0 reservations=0 target_processes=0 blockers=0`。该批证据见 [C0d evidence](../evidence/final-candidate-c0d-static-cli-load-ui-20260721.md)，只推进 C0/static/CLI-ingress/UI-build/loadtest-tooling；所有需要 caller-owned PG/Redis、真实 Kiro upstream、native MCP/search/image/agent、browser、upgrade 和 final release 的 case 仍按下表保持 open。

## 通用记录字段

每个运行必须记录：case ID、Git revision、dirty diff、二进制 SHA-256、端口/PID、Claude CLI 版本、配置摘要、请求数、成功/失败、下游 request ID、上游 hit 数、attempt 分类、TTFB/总延迟、RSS/FD 起峰终值、清理结果。不得记录任何真实 key、token、cookie 或 credential JSON。

## A. 转换与正文语义单点

| Case | 场景 | 轮次 | 验收 |
| --- | --- | ---: | --- |
| A01 | clean short/long Unicode text、代码块、引用、缩进中的 marker | 每 fixture 20 次 + 1000 个 chunk partition | byte/value identical；0 suppression；无额外 DOM serialize |
| A02 | `user Continue`、legacy、roleless、无 hash 工具名与未知历史映射名 | 每形态 20 次 | 只删除完整可信 transcript；不靠单个 hash/token 判定 |
| A03 | orphan/mismatch/duplicate tool_result | 每类 10 次 | 默认拒绝或中性占位；原始输出绝不进入普通 user/assistant text |
| A04 | 20/100 tool cycles，active current result，触发 trim | 每规模 5 次 | tool_use/tool_result 一一配对；逻辑 turn 原子保留/删除；无 scaffold |
| A05 | thinking/text/tool 跨 block；native/XML/signed/redacted | 每组合 5 次 | 策略一致；无内容改写导致签名失配；无泄漏 |
| A06 | tool_choice auto/none/any/named，prompt master ON/OFF | 每组合 5 次 | 结构化选择恒为 N/0/N/1；不受 operator prompt master 影响 |
| A07 | count_tokens 与 messages | 每组合 10 次 | 采用同一已声明口径；差异可解释且 UI 不宣称虚假一致 |
| A08 | 已确认 transcript 后仍有合法尾文；污染占满全部 text/thinking；signed/redacted 原子抑制 | local/external、stream/non-stream 每形态 5 次 | 首输出前受共享预算约束重试或规范失败；首输出后规范 stream error；不得空白 success、静默截断或伪造 terminal |
| A09 | `output_config.effort` absent/low/medium/high/max 与 `thinking` disabled/enabled/adaptive/budget；prompt/body 开关及 alias 正交 | 每组合 5 次 | CLI 原始 body、converter 事实和最终 Kiro wire body 可对账；无无证据 max->high clamp 或 adaptive drop；不支持组合明确失败而非静默退化 |

A05/A08/C06 分批证据：request history、local stream/non-stream、external response/SSE 已覆盖 raw/unhashed/deterministic-hash 历史工具名、unsigned/XML/signed/signature-field/redacted、跨 block、CRLF/multi-data、逐字符与单字节 transport、EOF/tool boundary、clean identity 和 1 MiB 原子上限；污染 response fail closed，不重组 signature，不发 success terminal。2026-07-21 又新增纯 Node 源码合同 `10/10`，锁定 sanitizer 不信任任意 `Hashxxxxxxxx`、raw marker-free body 不 DOM parse/serialize、assistant 清理不改 user/tool data、signed/redacted 原子策略、strict request bypass 防护、stream/non-stream/external fail-closed terminal，以及用户点名的 truncated `user Continue\n\nBash` 等非 Hash 形态；与 Redis fault-domain 合批 `56 tests / 47 pass / 9 explicit live-signal skips / 0 fail`。同日新增生产 marker inventory 合同 `4/4`，锁定 `user Continue`/`Tool results provided`/`Tool results:`/function-results/function-calls/placeholder/textification 生产源码边界，确认不再只依赖 Hash 指纹。详见 [Thinking 与签名内容专题](../issues/thinking-and-signed-content-safety.md)、[协议污染 fail-closed 证据](../evidence/protocol-contamination-fail-closed-20260716.md)、[protocol contamination source contract](../evidence/protocol-contamination-source-contract-20260721.md) 和 [protocol marker inventory source contract](../evidence/protocol-marker-inventory-source-contract-20260721.md)。真实 CLI、20-tool/120k history、HTTP 首输出前 retry fault 和最终候选性能仍 pending，因此三项尚未关闭。

## B. Body 与多模态单点

| Case | 场景 | 轮次 | 验收 |
| --- | --- | ---: | --- |
| B01 | raw clean/polluted、normalized、strict、direct/fallback/retry | 每路径 5 次 | clean raw byte-identical；policy request-scoped；未知字段按合同保留 |
| B02 | PNG/JPEG/WebP/GIF、data URL、URL、坏图、伪 MIME、空图 | 每类 5 次 | 支持项 round trip；坏输入本地明确 4xx、0 upstream；按 decoded bytes 限制 |
| B03 | documents/web fetch/大 user text/大 tool_result | 每类 5 次 | 仅配置允许的字段被裁剪；标记不重复；Unicode 不截坏 |
| B04 | 60 深 schema、8 大工具、near/over max bytes | 每档 5 次 | `maxBytes` 若称硬上限则最终不越界；否则返回明确 oversize 错误/观测 |
| B05 | clean body 1 KB/100 KB/1 MB/5 MB | 每档 100 次 | sanitizer/payload p95 开销相对基线不超过约定预算；无近二次增长 |
| B06 | 五 Messages 路由 `50 MiB + 1`；handler/external JSON 与纯文本 413 | 每路由/来源 3 次 | 真正超限为规范 Anthropic 413、0 upstream；非 body-limit 413 保持原分类/正文，不被错误归因；request ID 一致 |
| B07 | remote image/document source count、累计下载/base64、redirect/HTTP attempt、slow deadline、DNS rebinding/proxy、全局工作槽 | 每形态 5 次 + burst 3 轮 | 超限在对应边界 fail closed；预扫描超限 0 HTTP；真实 transport 不连接 blocked address；取消释放资源并 5/5 恢复；inline/file/source 正常语义不变 |

B07 分批证据：模块级 19/19 通过，新增 source count、预算边界、真实 reqwest blocked-address 0 connect、DNS 变更、单 PNG、chunked over-limit、redirect 私网/attempt、slow cancel/recovery 和全局工作槽均各 5 轮通过；1 KiB 至 5 MiB clean text 各 100 轮 value identical/0 remote admission。五个 Messages + 五个 count_tokens 路径各 5 轮 remote over-count 为 50/50 规范 400/0 inference，inline controls 30/30 正常；handlers 90/90。详见 [远程多模态证据](../evidence/remote-multimodal-resource-and-ssrf-20260716.md)。B07 仍未关闭，因为 PDF/text/CLI image、20/32 临界组合、burst RSS/FD、L5 与统一候选 SHA 尚待执行。

## C. 流与错误完整性

| Case | 场景 | 轮次 | 验收 |
| --- | --- | ---: | --- |
| C01 | normal/thinking/tool stream 与 non-stream | 每类 5 次 | 事件顺序、stop、usage 正确；无空成功 |
| C02 | 首输出前 idle/read error/status error/JSON exception | 每类 5 次 | 可重试但总 attempt 不超预算；最终成功无重复事件 |
| C03 | text/thinking/tool 任一提交后断连 | 每提交点 5 次 | 0 服务端重试；规范 error；不伪造 message_stop |
| C04 | malformed EventStream：CRC/frame/truncated/unknown/missing terminal | 每类 stream/non-stream 各 5 次 | 不返回 success；usage 为 error；公开错误脱敏且含 error ID |
| C05 | SSE CRLF、多 `data:`、逐字符、start content、EOF/error | 每类 5 次 | parser 符合 SSE；pending block 有完整收尾或明确 error |
| C06 | transcript suppression 发生在首输出前、可见 text 后、thinking 后、tool_use 前后 | 每提交点 5 次 | 未提交时可重试且总 hit 不超预算；已提交时 0 重试并明确 error；usage 状态与下游 terminal 一致 |
| C07 | pure WebSearch stream/non-stream、last-user query、普通同名 tool；MCP 400/429/500/timeout/disconnect/malformed/isError/over-limit | 每形态 5 次 | 不搜索旧 turn、不劫持 client tool、不伪造无结果成功；response 形态正确；usage/attempt/channel 完整；raw query/result 0 日志泄漏 |

C07 focused 证据：parser `18/18`、handler `8/8`，其中 13 类错误 x 5 + recovery x 5 为 `70/70`；canonical/custom/mixed、20/100 tool cycle、zero/non-text、stream/non-stream、stream drop、MCP header/body cancel `10/10`、privacy 与 usage channel 均重复通过。provider MCP `7/7` 覆盖 1/20/60 credentials 各 5 轮和 shared hard budget。详见 [WebSearch/MCP 聚焦证据](../evidence/websearch-mcp-protocol-usage-privacy-20260716.md)。C07 仍未最终关闭，因为 native Claude CLI D04/D06、profile/refresh/catalog production attribution、冻结候选 HTTP/load gate 尚未完成。

## D. 真实 Claude Code CLI

使用隔离 `HOME`/`CLAUDE_CONFIG_DIR` 和临时端口，不触碰 `9022`。

| Case | 场景 | 轮次 | 验收 |
| --- | --- | ---: | --- |
| D01 | normal、alias、thinking、Bash、Read | 各 5 次 | usage 非零、thinking 真实、tool 配对、无 internal marker |
| D02 | 20/100 tool cycles、session/resume、长 history | 各 5 个 session | 长对话连贯；resume/cache 正确；无 hash/scaffold/0 usage |
| D03 | MCP search | 5 次 | MCP 连接、调用、结果和后续回答正确 |
| D04 | native WebSearch、agents/subagents、CLI image | 能力可用时各 5 次 | 必须是实际能力证据；不可用则记录环境限制，不能用替代项冒充 |
| D05 | model-unavailable 400、invalid-model 400、invalid-tool 400、空图/body-invalid 400、普通 malformed、429/500/partial | 各 5 次 | CLI 不假成功；仅精确可恢复类换号；确定性 body/image 错误 0 或 1 upstream hit；总 hits 受硬预算；错误后 5/5 normal 恢复 |
| D06 | native WebSearch 与 MCP 错误/恢复 | 能力可用时各 5 次 | native 与 MCP 证据分开；无假成功/0 usage；query、attempt、terminal 与错误恢复正确 |
| D07 | thinking 主动/被动触发、CLI effort 各档、adaptive、thinking alias、tool 前决策与长会话 | 每形态 5 个独立 session | 捕获真实 CLI request 和 Kiro wire body；thinking block/delta/usage 与映射一致；错误后 normal 5/5 恢复 |

D01/D07 分批证据：2026-07-19 使用 frozen binary `e16df13a0...` 和真实 Claude Code CLI 2.1.197 关闭两个 fake-upstream runtime gate。`bare-invoke-claude-cli.mjs` 为 20 cases pass（15 literal XML negative、5 structured Bash tool loop、25 inference、tool_use/tool_result 各 5、unknown 0、cleanup 全 true，report SHA `67c9d7c9...`）；`thinking-effort-kiro-wire.mjs` 为 CLI/IDE × 6 effort × 5 共 60 cases pass（inference 60、model discovery/schema 2、unknown/invalid/violations 0、cleanup 全 true，report SHA `439e1e69...`）。2026-07-20 新增 `claude-cli-long-session-continue.mjs`：同一 frozen binary 下 5x20 与 5x100 均通过，分别 110/510 CLI turns、210/1010 inference、100/500 tool pairs，0 internal marker，report SHA `f8a5faa3...`、`cd79f11b...`；wire 映射名与公开 Bash/Read 往返、history/tool_result pairing、session ID/resume 和 non-zero usage 均逐 turn 核对。D02 的 fake-upstream 长会话子项已关闭；D01/D07 仍未最终关闭，因为 MCP/search/image/agents、真实 thinking delta/usage、真实 Kiro upstream、故障恢复 5/5、UI/upgrade 等尚未执行。

D05 分批证据：400 provider HTTP harness 已覆盖 10 类 x 1/20/60 账号 x 5 轮，共 150 请求、240 exact inference hits；确定性错误均 1 hit，可恢复类 <=4。详见 [Provider 400 分类证据](../evidence/provider-400-retry-classification-20260716.md)。D05 仍未完成，因为 429/500/partial、真实 Claude CLI 和错误后 5/5 恢复尚未执行。

## E. 调度、Redis 与流量

| Case | 场景 | 轮次 | 验收 |
| --- | --- | ---: | --- |
| E01 | 60 账号，priority/balanced/health/weighted，异 session | 每策略 3 轮 | 记录峰值 in-flight/选择数；无单账号无理由热点 |
| E02 | 同 session sticky、lease 竞争、其他账号有空槽 | 每策略 3 轮 | sticky 不突破容量；竞争失败会重选可用账号 |
| E03 | 两实例共享 Redis，reservation/renew/release/restart | 每故障点 3 轮 | 无超卖、无重复 lease、TTL 后恢复、实例重启无残留 |
| E04 | Redis 50/75/100/150/300/500 ms、断连、恢复 | 每档 3 轮 | 无进程级反馈回路；分类准确；fallback 按策略；恢复后 100% normal |
| E05 | local ready/cooling/full/degraded 与 external ready/full/error | 矩阵每格 5 次 | strict local-first；只在对应策略允许时 fallback；无循环 |
| E06 | 单 key/多 key burst、客户端重试、auxiliary calls | 每档 3 轮 | admission 按 key 生效；attempt/refresh/catalog 分开计数；无 N-account 尖峰 |
| E07 | finite/unlimited local 与 external queue lease、长 override、runtime wait config 变更 | policy 每项 5 轮；真实 Redis/500 waiter 各 3 轮 | finite 初始 TTL 覆盖请求且 0 periodic renewal；unlimited 可续租；Redis deadline 不移动；取消/release 后 queue=0；配置变更不改变已 admission deadline |
| E08 | external static eligibility、权威 pool list、Redis runtime、lease、prepare 后 revision fence | c32/c128 各 5 轮；runner 3 outer | cold generation PG list=1；同 revision in-flight fence=1、完成后不缓存；坏行按池隔离；更新后 HTTP hit/attempt=0；健康池不被连坐 |
| E09 | business Redis 与 observability Redis 故障域隔离 | runner 3 outer；每 invocation 内部 3 轮 | DB/prefix 伪隔离拒绝；`run_id` 相同拒绝启用；observability 慢/断不影响 business scheduler；business fault fail closed 且不伪装 `AllDisabled`；observability 不回落 business Redis |

E06 分批证据：本地 admission 单/多 key、RPM refill/burst、并发、queue timeout/cancel/config lowering、body EOF/error/drop、disabled、状态 churn、规范 429 与实际 Router 顺序已各 5 轮通过；历史双实例 provisional 证据已证明当前语义是 per-instance，不是 cluster-global。2026-07-21 `request-api-key-admission-multi-instance.mjs` 已改为 caller-owned PG/Redis + 本地 `redis-chaos-proxy.mjs`，不再 Docker/Toxiproxy/建库/flush；contract 5/5 通过，且与 F06/runtime/external/E01/E02/E05 runner 合批 41/41 通过。详见 [Request API Key Admission](../issues/request-api-key-admission.md) 与 [non-Docker runner contract](../evidence/request-api-key-admission-nondocker-runner-contract-20260721.md)。E06 仍未完成，因为 auxiliary 分账、usage channel attribution、冻结动态服务 burst、多实例 aggregate 和 release-mode latency 证据尚缺。

E07 分批证据：500 finite guard、unlimited 对照、local/external TTL、300 秒 override、亚秒舍入和 runtime config deadline 各内部 5 轮通过；40x15/global-500 对照再次通过。2026-07-19 使用当前仓库专属隔离 PostgreSQL/Redis 执行 runtime quarantine storage suite，`finite_redis_dispatch_queue_lease_deadline_does_not_move_after_renew_interval`、`redis_dispatch_queue_waiter_fails_closed_after_coordination_degrades`、`redis_dispatch_queue_cancelled_waiter_releases_local_and_remote_lease` 在 3 个 outer rounds 下通过；2026-07-20 非 Docker Redis chaos 又完成 7 tests × 3 outer，覆盖 latency/disconnect/recovery/cancel/release。冻结负载、两实例和 usage+scheduler 联合压力仍未关闭。详见 [Queue lease RPM amplification](../issues/dispatch-queue-lease-renewal-rpm-amplification.md)、[Scheduler Redis chaos](../evidence/scheduler-redis-chaos-nondocker-20260720.md) 与 [2026-07-19 storage evidence](../evidence/storage-integration-and-artifact-gate-20260719.md)。

E08 分批证据：per-request authoritative PG list、32-query admission、prepare 前 fence 和 malformed 默认已由源码复核确认；250ms generation-bound list singleflight、100ms failure cache、manager-owned cancel-safe refresh、list/fence c128 timeout suppression/recovery、in-flight-only revision fence、prepare 后 linearization 与 strict row isolation 已实现。11 个无存储 body/parser exact filters 实际通过，all-target check 三次无 warning；2026-07-18 当前仓库隔离 PostgreSQL/Redis 的 17 个 external storage filters × 3 outer 通过，2026-07-19 又在合批 storage suite 中复跑同一 17 filters × 3 outer 并通过。冻结性能、两实例、RSS/FD 与负载/chaos 仍未关闭。详见 [外部池权威 dispatch](../issues/external-pool-authoritative-selection-and-dispatch-fence.md) 与 [2026-07-19 storage evidence](../evidence/storage-integration-and-artifact-gate-20260719.md)。

E09 当前状态：源码已经拆出 optional `observabilityRedis`、配置 authority guard、启动 `run_id` identity guard、UsageRecorder/Admin observability-only 注入、readiness business-only 合同，以及 `RedisStore` usage materialization 入口的 production role guard。2026-07-21 当前项目两个 loopback Redis authority 下，纯 Node contract 默认 28/28 早拒绝 pass（9 live signal skipped）、live signal 37/37 pass，基础 Redis fault-domain runner 3 outer pass；产品 runner `redis-fault-domain-product-r2` 为 3 outer × 1 exact × 内部 3 轮，3/3 exact invocations passed，并确认 `protected9022ProbeSkipped=true`、`flushDbUsed=false`、`dockerUsed=false`、scoped cleanup `size_kib=1714572 removed=true reservation_released=true`。Rust 1.92.0 scoped C0 子集 `redis-fault-domain-c0-r2` 也通过 fmt、diff、4 个 observability config tests 与 `cargo check --all-targets`，scope `size_kib=2051072 removed=true reservation_released=true`。后续新增生产源码合同、RedisStore role guard 合同和主/观测 Redis 路径隔离合同后，`run-redis-fault-domain-product-validation.contract.test.mjs` 为 46 tests：37 pass、9 live signal skips、0 fail；scheduler/fault-domain 合批为 74 tests：53 pass、21 live-fixture skips、0 fail；scoped `cargo +1.92.0 check --bin kiro-rs` 通过，scope `size_kib=446876 removed=true reservation_released=true`。新增合同锁定 `main.rs` usage/Admin 注入、scheduler/external/runtime-event/health business Redis 专用路径、observability Redis 启动失败不回落 business Redis、`UsageRecorder` observability role 断言、主请求路径仅 enqueue 观测 writer 且压力丢弃 summary、`AdminService` cache/cleanup wiring、`RedisStoreRole`、Redis usage materialization entrypoint guard 和 config env/authority guard。一次 Rust 1.86 默认工具链 C0 红项为无效门禁，已单独记录。详见 [业务/观测 Redis 故障域专题](../issues/business-observability-redis-fault-domain.md) 与 [证据](../evidence/business-observability-redis-fault-domain-20260721.md)。E09 focused/product/source-contract gate 已通过；两实例真实服务、external takeover、生产高基数和 final release gates 仍 open。

Runner 环境隔离补证：2026-07-21 新增 `validation-child-env.mjs` 并把真实 runtime/Claude/scheduler/fault-domain runner 子进程改为白名单环境。`node --test feature/tests/runtime-validation-paths.test.mjs` 当前 11/11 通过，新增断言确认 child env 不继承 `DATABASE_URL`、`REDIS_URL`、Anthropic/OpenAI key、`KIRO_API_KEY`、`KIRO_RS_TEST_REDIS_URL` 或任意未显式传入的 `KIRO_*`；所有非测试 `feature/tests/*.mjs` validation runner 均无 `...process.env`。详见 [runner child environment evidence](../evidence/runner-child-environment-isolation-20260721.md)。该项只关闭验证污染合同，不关闭任何动态产品门禁。

E05 / SchedulerRedisDegraded external takeover 当前状态：2026-07-21 已完成源码路径复核和 focused handler/fallback tests。`external-takeover-focused-20260721-r2` 四个 exact tests 全部通过，覆盖 fallback toggle、preflight reason、fresh-state local dispatchable guard 和 parsed entrypoint eligibility；新增 `external-takeover-scheduler-degraded-nondocker.mjs` runner，contract `8/8` 通过，确认不使用 Docker/Cargo、不触碰 `9022`、拒绝非隔离 PG/Redis 和共享 prefix。动态 service gate 仍 pending：需要 caller-owned empty PostgreSQL database URL、loopback nonzero Redis DB/prefix 和仓库外冻结 binary，然后分别执行 fallback enabled/disabled 两组并验证 degraded→external、disabled→脱敏失败、recovery→local。详见 [external takeover evidence](../evidence/external-takeover-scheduler-degraded-20260721.md)。因此 E05 不能关闭。

E01/E02 runner 当前状态：2026-07-21 `scheduler-fairness-sticky-race.mjs` 已去掉 Docker-managed PG/Redis 入口，改为 caller-owned PostgreSQL URL template、预创建 `kiro_e0102_*` database 列表、loopback Redis DB1..15 和 caller-owned Redis prefix。runner 不 `FLUSHDB`，不创建 database，每 case 使用独立 `redis.keyPrefix` 并清理 owned prefix。contract `7/7` 通过，`runtime-validation-paths` `9/9` 通过，并复跑 external takeover contract `8/8`。动态分布公平仍 pending：需要同一冻结 binary、12 个默认空 DB（4 modes × 3 rounds）或 mode subset 对应数量、独占 Redis DB/prefix 后执行。详见 [E01/E02 runner evidence](../evidence/scheduler-fairness-nondocker-runner-contract-20260721.md)。因此 E01/E02 不能关闭。

E05 runner 当前状态：2026-07-21 `strict-local-first-routing.mjs` 已改为非 Docker 全矩阵入口，保留 10 类 E05 模式，但要求仓库外冻结 binary、owned artifact root、`modes × rounds` 个预创建 `kiro_e05_*` PostgreSQL database、loopback Redis DB1..15 和 caller-owned Redis prefix。脚本不启动 Docker、不创建 database、不 `FLUSHDB`、不调用 Cargo、不探测 `9022`；Redis fault 使用 `redis-chaos-proxy.mjs`，结束只清理 owned prefix。contract `6/6` 通过，同批 `runtime-validation-paths` `9/9`、external takeover contract `8/8`、E01/E02 contract `7/7` 通过。动态 service run 仍 pending；详见 [E05 non-Docker runner evidence](../evidence/strict-local-first-nondocker-runner-contract-20260721.md)。因此 E05 不能关闭。

Load/chaos runner 当前状态：2026-07-21 `frozen-load-chaos-runner.mjs` 已改为非 Docker caller-owned PG/Redis 入口，要求仓库外冻结 `kiro-rs` 与 `kiro_loadtest`、owned artifact root、tier 对应数量的预创建 `kiro_load_chaos_*` database、loopback Redis DB1..15 和 caller-owned prefix。脚本不启动 Docker、不创建/drop database、不 `FLUSHDB`、不调用 Cargo、不探测 `9022`；fake/proxy/loadtest 子进程使用最小环境，Redis 只 SCAN/DEL owned prefix。contract `6/6` 通过，同批 runtime path 合同 `15/15` 通过。详见 [load/chaos runner evidence](../evidence/frozen-load-chaos-nondocker-runner-contract-20260721.md)。历史 r8 L3/L4/L5 fake-upstream 行为 pass 仍保留，但最终候选必须用改造后的 runner 重跑，不能直接复用历史动态结果。

Redis storage runner cleanup 当前状态：2026-07-21 `run-token-refresh-cluster-validation.mjs` 与 `run-multi-instance-redis-coordination-validation.mjs` 已去掉 runner 级 `FLUSHDB` 兜底；启动前仍要求 caller-confirmed empty DB1..15，测试后若有残留只报告 `residualKeyCount` 并让门禁失败。纯 Node 合同 `19 pass / 1 skip / 0 fail`，skip 为显式 live nonempty Redis opt-in；source 扫描无 `FLUSHDB/FLUSHALL`。详见 [no-FLUSH runner evidence](../evidence/redis-storage-runner-no-flush-contract-20260721.md)。token-refresh cluster 与 multi-instance Redis dynamic 仍需重跑。

## F. 资源、清理、升级与 UI

| Case | 场景 | 轮次 | 验收 |
| --- | --- | ---: | --- |
| F01 | client drop、idle、proxy restart、429/500 burst recovery | 每类 5 次 | 新请求恢复；无孤儿任务/socket；错误公开文案脱敏 |
| F02 | fake upstream soak | 3 x 15 分钟 | RSS 空闲后回到 `max(start+32 MiB, start*1.2)` 内；FD 回到 start+5 内；p95 无持续漂移 |
| F03 | usage soft/hard batch cleanup、cancel/pause/failure、晚到重放、in-flight commit、rollup 一致性、调度并发压力 | 每类 3 轮 | 无同步全表/大 DEL；soft cleanup 对 detail/summary/Dashboard/cost/credential/cache/duration 贡献只扣一次；hard cleanup 不双扣；已开始的旧 writer 不在 watermark 后晚提交；新记录保留；soft tombstone 存在期间同 ID 不复活；调度无 degraded；任务可审计恢复 |
| F04 | v0.0.101/102/103 数据集升级与二次启动 | 每版本 3 轮 | readiness 时限、数据保留、配置迁移、幂等、失败回滚均通过 |
| F05 | `ui`/`admin-ui` formatter、页面、tooltip、CSV、配置保存 | 值矩阵 x 3 viewport | 费用精度一致；负值/NaN/null 安全；prompt/body 字段不互相覆盖 |
| F06 | AWS API key + region 生命周期 | 每入口 3 轮 | import/PG/reload/select/header/delete/duplicate/export masking 完整；runner non-Docker contract 6/6 + shared runtime contract batch 36/36 |

F06 分批证据：Admin API、JSON file、plain file 三入口已完成十次独立完整运行，每次每入口 3 轮，共 90/90 正向 lifecycle case；PG 归一化、restart/reload、scheduler 选中 key digest、标准及自定义安全 region Host/Bearer/`API_KEY`、duplicate 409 且 0 auxiliary hit、delete/export/masking/cleanup 均通过。schema-v2/v3 覆盖 pipe 与显式 `region/authRegion/apiRegion`，累计 Admin 69、JSON 69、plain 15，共 153/153 malformed：400/启动前拒绝、0 upstream、0 active PG、无输入回显。2026-07-21 runner 改为 caller-owned PG/Redis 后，`aws-api-key-region-lifecycle.contract.test.mjs` 6/6 通过，且与 runtime path / external takeover / E01/E02 / E05 的共享 runner 合同 36/36 通过。详见 [AWS API Key 与 region 生命周期证据](../evidence/aws-api-key-region-lifecycle-20260716.md) 与 [non-Docker runner contract](../evidence/aws-api-key-region-nondocker-runner-contract-20260721.md)。F06 仍未关闭，因为最终冻结候选 SHA、两 UI 浏览器交互和多实例同时重复导入的 auxiliary admission 尚待验证。

F03 分批证据：隔离 PostgreSQL/Redis 的早期 `cargo test cleanup -- --nocapture --test-threads=1` 为 `35/35`，随后加入 same-ID 合同与 writer/cleanup advisory-lock 竞态保护。编译恢复后的聚焦结果为 updated round-trip 1/1、1000 external billing cleanup 1/1、replay/new-ID 内部 3/3、in-flight commit guard 内部 3/3；77.3 秒 wall time 包含编译锁等待，不是性能数据。最终 cleanup 过滤组又连续三次外层执行，每次 36/36，合计 108/108、0 ignored；内部 PostgreSQL fallback summary p95 范围 `4.952-9.945 ms`、dashboard p95 `16.645-49.070 ms`。独立 Redis usage-writer burst 的 scheduler loaded p95 为 `3.027-4.406 ms`、p99 为 `4.319-15.936 ms`。F03 继续等待完整 PostgreSQL/全树、writer advisory-lock 性能、Redis chaos、UI browser 和生产规模验证。完整证据见 [Usage cleanup storage integration](../evidence/usage-cleanup-storage-integration-20260716.md)。

## G. 最终静态与发布门禁

`cargo fmt --check`、`git diff --check`、clippy baseline、default/no-default 全量测试、两套 UI production build/browser、release build、Docker Buildx、临时服务 C1-C4、L1-L5、敏感信息扫描、端口/PID/Redis/PG/临时目录清理必须全部通过。任何 skipped storage test、只匹配局部过滤器但遗漏既有失败用例、未运行脚本或旧构建报告均不得计为通过。

G/L1 分批证据：2026-07-19 使用仓库外 product frozen binary `e16df13a0ded4d53ac255f26ddc24056c4d385dde418a63944a2e00d122c642a` 与 patched loadtest frozen binary `23c04221deb72dde601d491452d8cc9a99211df99b2cd39a386272141f2db8e3` 执行 `l1_fake_upstream_12_case_matrix_r3`，12/12 通过。覆盖 normal stream/non-stream、thinking stream、tool use stream、slow first text、slow thinking、JSON exception 200、429、500、invalid tool format、malformed SSE 和 client drop。该运行只关闭当前 frozen L1 fake-upstream smoke；L3 burst/recovery、L4 chaos、L5 soak、真实 Kiro upstream、两实例、长 Claude CLI、UI、upgrade 和最终 C0/release inventory 仍为 open。详见 [L1 fake-upstream evidence](../evidence/frozen-loadtest-l1-fake-upstream-20260719.md)。
