# Runtime Correctness And Release Final Report

Role: 本轮问题、复现、修复、验证、残余风险与发布结果的最终汇总入口

Status: `released / v0.0.114`

Last updated: 2026-07-23

## 1. 使用口径

本报告最终必须回答六个问题：实际发生了什么；为什么发生；如何稳定复现；选择了什么修复；修复后哪些正常、异常、长对话和负载场景已被当前构建证明；哪些风险仍未覆盖。任何结论都必须回链到专题文档、可重复命令或脚本、报告和构建身份。

以下内容不能单独构成“已修复”：源码看起来合理、单个关键词不再出现、单轮 happy path、单元测试总数全绿、旧版本报告、没有运行到的 skipped 集成测试、没有真实执行 Claude Code CLI 的 curl 结果。

## 1.1 2026-07-23 当前候选判定

当前候选已完成用户本轮要求中最关键的复核集合：真实 Claude Code CLI 长会话、bare invoke、thinking/output_config wire、body/reasoning byte semantics、runtime quarantine、scheduler Redis chaos、business/observability Redis fault-domain、SchedulerRedisDegraded external takeover 正/负向动态验证、协议 marker/source 合同和文档门禁。核心功能复测见 [2026-07-22 当前工作树回归复测证据](evidence/final-regression-rerun-20260722.md)。

2026-07-23 又完成最终统一候选 release gate。冻结候选身份：

```text
925525419cd48b460217df2568891a40287da0c44d2bf921a38b103c047775ee  kiro-rs
90babda7388aa93854cbbdb81c132cc436c07f46b0ea22973531b0a7ffb3aff1  kiro_loadtest
```

最终门禁结果见 [2026-07-23 最终发布门禁复测证据](evidence/final-release-gate-20260723.md)：

- `cargo +1.92.0 fmt --all -- --check`: pass。
- `cargo +1.92.0 test --all-targets`: pass，main `1750 passed / 0 failed / 6 ignored`，`kiro_loadtest 31/31`。
- `cargo +1.92.0 build --release --bins`: pass。
- scoped target cleanup: `removed=true reservation_released=true`。
- `node feature/tests/check-feature-docs.mjs`: 47 issue docs / 115 links pass。
- `node --test feature/tests/*.test.mjs`: `283 tests / 261 pass / 22 explicit skips / 0 fail`。
- `git diff --check`: pass。
- `node feature/tests/inventory-build-artifacts.mjs --gate`: pass，最终 `targets=0 reservations=0 target_processes=0 blockers=0`。

发布产物清单门禁曾被 rust-analyzer/flycheck 重新生成的仓库根 `target/` 阻断。该 `target/` 只包含可再生 `debug/flycheck0/.rustc_info` 输出；本轮未停止或重启既有 `127.0.0.1:9022` 服务，只删除 disposable repo `target/` 文件树后重新执行 inventory，并取得 release-gate pass。

当前判定：可以进入 Git 发布流程。Docker 动态验证按用户明确要求未执行，不能标记为 Docker pass；既有生产 `9022` 未被修改或压测；需要外部真实 Kiro upstream 凭据的新高压用例未在本最终门禁中新增执行，其边界在证据文件中明示。

## 2. 最终交付结构

| 内容 | 权威入口 | 最终必须包含 |
| --- | --- | --- |
| 总结与发布判定 | 本报告 | 总体影响、问题清单、修复批次、验证总表、性能结论、残余风险、版本/tag/回滚点 |
| 单类问题 | [issues/README.md](issues/README.md) | 现象、所有已知指纹及无指纹变体、影响、源码链、最小/多轮/长会话/异常复现、根因、方案比较、选定方案、风险、验收、修复后结果 |
| 当前事实 | [audits/current-fact-matrix-20260716.md](audits/current-fact-matrix-20260716.md) | 修复前证据、当前静态事实、当前动态证据、历史证据必须分栏 |
| 实施计划 | [plans/remediation-and-release-plan.md](plans/remediation-and-release-plan.md) | 顺序、依赖、退出条件、回滚、禁止提前发布的门禁 |
| 可执行验证 | [tests/reverification-matrix.md](tests/reverification-matrix.md) | case、fixture、轮次、命令/脚本、pass/fail、报告、构建身份、清理结果 |
| 证据索引 | [evidence/README.md](evidence/README.md) | 报告路径、哈希、revision、dirty diff、端口/PID、脱敏规则和限制 |
| 发布记录 | [releases/README.md](releases/README.md) | 远端基线、工作提交、版本提交、tag、分支/tag 推送、回滚与发布后观察 |

## 3. 问题总表

本表是最终报告的问题覆盖清单。`partial` 或 `pending` 表示该专题仍有扩展验证、真实环境或长期运维证据未覆盖；是否阻止本次发版以 [1.1 当前候选判定](#11-2026-07-23-当前候选判定) 和已明示的用户豁免/限制为准，不能把历史 `NO-GO` 字样单独当作当前发布结论。

| ID | 问题类 | 用户可见现象 | 复现入口 | 当前状态 | 专题 |
| --- | --- | --- | --- | --- | --- |
| PRO-001 | transcript/tool history 泄漏及抑制后的空成功/截断 | `user Continue`、旧 `Tool results provided.`、工具名/hash、tool output 或无固定指纹的内部 scaffold 出现在回答；保护命中后也可能只返回空格或截断合法尾文 | A01-A04、A08、C06、D01-D02 | focused-fix-pass + source-contract 10/10 + marker-inventory source-contract 4/4 + real CLI 5x20/5x100 fake-upstream pass / native upstream and fault recovery pending | [协议与工具历史](issues/protocol-transcript-and-tool-history-leak.md) |
| PRO-002 | thinking 与 signed/redacted 内容 | thinking 标签/内部片段跨 text/tool block 泄漏；局部改写可能令签名失效 | A05、C01、D01 | partial | [thinking 安全](issues/thinking-and-signed-content-safety.md) |
| PRO-010 | thinking effort/adaptive 请求侧映射 | `max` 可能被静默截为 `high`，adaptive/预算/alias/开关可能未进入最终 Kiro wire body；2026-07-19 又复现普通 CLI `sonnet` 因无 native schema 直接 400 | A09、C01、D01、D07、F05 | CLI ingress no-probe pass + development final-wire pass + frozen fake-upstream wire pass + generic Bash/Read long-session pass / active-passive thinking long-session and real upstream pending / NO-GO | [thinking effort 与上游映射](issues/thinking-effort-adaptive-upstream-mapping.md) |
| PRO-003 | payload/body/图片上限、413 归因与性能 | body 被误删/误改、hard limit 失真、重复占位、图片大小口径错误、纯文本 413 被误称 body limit、CPU/内存放大 | B01-B06 | pending | [payload guard](issues/payload-guard-semantics-limits-and-performance.md) |
| PRO-004 | external raw/normalized/SSE/strict | 本地与外部路径清理策略不一致；SSE 分块可绕过或误删 | B01、C05 | partial | [external profile](issues/external-pool-profiles-and-sse-safety.md) |
| PRO-005 | stream terminal/HTTP 200 exception | 断流、坏帧、JSON exception 被报告为 200/success/空回答 | C01-C05、D05 | focused + frozen L1 fake-upstream pass / CLI and chaos pending | [stream 完整性](issues/stream-terminal-errors-and-precommit-retry.md) |
| PRO-006 | 重试与内部 RPM 放大 | 下游低 RPM，但一次请求在错误期间产生多账号、多层重复上游 attempt；token refresh 还可因短 TTL、失败 waiter、invalid-bearer force 或跨实例波次独立放大 | D05、E06、F01 | budget/400/process-local OAuth budget/singleflight/cancel/final-attempt focused-pass + request-admission non-Docker runner contract pass + isolated Redis refresh pass / PG cluster-provider-load-dynamic pending / NO-GO | [attempt/admission](issues/retry-budget-admission-and-rpm-amplification.md)、[request admission](issues/request-api-key-admission.md)、[token refresh wave](issues/token-refresh-failure-wave-and-cluster-rpm.md) |
| PRO-007 | prompt master 未覆盖全部新增提示 | 关闭“提示词引导”后仍注入 tool_choice/thinking/chunked 兼容提示或自动 thinking；同时不能破坏客户端显式结构化字段 | A06-A07、F05 | focused-fix-pass / browser-cli-count-tokens-pending | [prompt/tool_choice](issues/prompt-policy-tool-choice-and-count-tokens.md) |
| PRO-008 | tool/schema/image/search/MCP/agent 能力 | 单类成功被误外推到其他能力；长会话/resume 后出现结构或 usage 异常 | B02、D01-D04 | long-session Bash/Read resume fake-upstream pass / search/MCP/image/agent/native upstream pending | [协议能力矩阵](issues/protocol-capability-regression-matrix.md) |
| SEC-001/RES-001 | remote multimodal SSRF 与辅助 I/O/资源放大 | 小 body 触发无界 URL/redirect、累计下载/等待；DNS 检查地址与真实连接地址不一致 | B02、B07、E06、F01-F02 | focused-handler-pass / CLI-load pending | [远程多模态边界](issues/remote-multimodal-resource-and-ssrf-bounds.md) |
| PRO-009/SEC-003 | WebSearch/MCP 特殊路径 | MCP 错误伪装成无结果成功；non-stream 返回 SSE；长历史搜索旧 query；usage/attempt 缺失；raw query/body 写日志 | C07、D04、D06、E06、F01-F02 | focused implementation/verification pass / native CLI-frozen-load pending | [WebSearch/MCP 边界](issues/websearch-mcp-protocol-usage-and-privacy.md) |
| SCH-001 | Redis scheduler degraded/fallback | 账号和外部池有容量仍快速 429；`local_error_no_fallback`；突发成片失败 | E03-E05 | focused + isolated storage + single-instance Redis chaos 21/21 pass + single-instance usage fault r6 24/24 pass / E03 two-process scheduler pass / external takeover enabled 3 clean rounds pass + disabled 1 clean round pass / two-instance fault pending / NO-GO | [Redis degraded](issues/redis-scheduler-degraded-and-fallback.md) |
| SCH-002 | local-first/分布/lease/多实例 | 本地尚有容量却进入外部池，账号热点，竞争失败后错误排队 | E01-E05 | coordinator and real-process E03 pass / external takeover runner contract pass / E01-E02 runner contract pass / E05 non-Docker runner contract pass / dynamic takeover, E05 and distribution fairness gates open | [调度与多实例](issues/strict-local-first-distribution-and-multi-instance.md) |
| SCH-003 | 高并发低 RPM 与运行态假禁用 | 健康账号因 runtime mutation backlog 被错误隔离，或长占用让高并发/低 RPM 看似矛盾 | E01-E06 | focused + isolated PG/Redis + frozen L3-L5 + normal usage/scheduler burst + single-instance Redis chaos/joint-fault pass / E03 two-process scheduler pass / external takeover focused+runner contract pass but dynamic service pending / cross-instance fault pending / NO-GO | [运行态 quarantine](issues/high-concurrency-low-rpm-runtime-quarantine.md) |
| SCH-004 | preflight/acquire 容量竞态 | 预检时 Ready、抢槽时 full，external 已配置却先进入本地默认 120 秒队列，首字慢且 fallback 延后 | E01-E06 | focused policy pass / dynamic pending | [预检竞态](issues/local-capacity-preflight-race-and-external-fallback-latency.md) |
| SCH-005 | queue lease 内部 Redis RPM 放大 | finite local/external waiter 的初始 TTL 已覆盖等待期却仍每 20 秒续租；Redis 越慢、waiter 越多，额外 scheduler 写越多 | E07 | focused + isolated Redis deadline/cancel + normal joint burst + single-instance chaos/joint-fault + E03 two-process scheduler pass / external takeover, two-instance and final-candidate rebind pending / NO-GO | [Queue lease renewal](issues/dispatch-queue-lease-renewal-rpm-amplification.md) |
| SCH-006 | external 权威选池 PG 扇出与发送一致性 | 每个 external 请求完整 list PG，c128 超出 32 admission；revision fence 早于 body/header prepare；坏行可被默认成 broad eligible | E08 | source fixed / non-storage focused pass / isolated storage dynamic pass / frozen load pending | [External authoritative dispatch](issues/external-pool-authoritative-selection-and-dispatch-fence.md) |
| OPS-001 | usage 清理与 Redis 隔离 | 高基数清理阻塞 Redis 单线程，引发 scheduler 超时和 429；清理后明细与累计统计不一致；删除合同或 tombstone 语义漂移 | E04、F03 | focused storage pass / updated contract rerun + browser + chaos pending | [usage 清理](issues/usage-cleanup-safety-and-redis-isolation.md) |
| OPS-003 | Redis usage writer 原子性、基数与 scheduler 隔离 | snapshot、aggregate、seen 多 RTT 或高基数 cache-read 可能在 Redis 单线程上拖慢 scheduler；错误路径可能暴露半成品 | E04、F03 | focused + isolated Redis + normal joint-pressure + single-instance joint-fault pass / cross-instance and production-cardinality pending / NO-GO | [Redis usage writer](issues/redis-usage-writer-atomicity-cardinality-and-scheduler-isolation.md) |
| OPS-004 | 业务 Redis 与观测 Redis 故障域共因 | usage/Admin/cache/cleanup 与 scheduler 共用同一 Redis authority 时，观测慢查询或断连可拖垮业务调度热路径；不同 DB/prefix 不能隔离 Redis 单线程 | E09 | product-focused pass + production RedisStore role guard compile/contract pass / two-instance, external takeover dynamic, production-cardinality and final release gates pending / NO-GO | [业务/观测 Redis 故障域](issues/business-observability-redis-fault-domain.md) |
| UI-001 | 两 UI 精度与配置权威 | 明细费用精度或配置 round-trip 不一致，UI 保存覆盖后端独立字段 | F05 | partial | [两套 UI](issues/two-ui-cost-precision-and-config-authority.md) |
| MIG-001 | 101/102/103 升级 | 旧数据升级耗时、迁移副作用、二次启动或失败恢复没有真实证据 | F04 | pending | [升级 smoke](issues/upgrade-v101-v102-v103-smoke.md) |
| CRD-001 | AWS API key + region 生命周期 | 导入后 region/header/reload/select/delete/export 或辅助 RPM 可能不一致 | F06 | core verified on provisional build + non-Docker runner contract verified / final build + browser + multi-instance pending | [AWS 凭据](issues/aws-kiro-api-key-region-lifecycle.md) |
| AUD-001 | 生产证据校验与脱敏 | evidence 校验依赖缺失或 archive 泄漏敏感字段/不可复现 | AUD gate | partial | [evidence skill](issues/evidence-skill-validation-and-redaction.md) |
| RES-002 | 运行时栈溢出/handler future | 默认测试线程栈在真实 Router fault matrix 进程级 abort；生产范围尚未证实 | C01-C06、L03-L05 | current default unit tree pass / frozen release HTTP-load pending | [运行时栈](issues/runtime-stack-overflow-and-handler-future-size.md) |
| SEC-004 | 上游错误诊断隐私 | 普通 provider 错误 body 进入日志、attempt、scheduler reason 或 usage | C02-C05、D05、AUD gate | focused provider/handler pass / persistent-storage-frozen-load pending | [错误诊断隐私](issues/upstream-error-diagnostic-privacy-and-bounds.md) |
| OPS-002 | 构建产物生命周期与磁盘安全 | 串行/并发验证完成后 build target 未删除，约 50 GiB 跨批累积并把数据卷压到约 446 MiB；未知 target、活进程或 runner 误探测受保护端口令验证不安全 | C0、L0、release cleanup gate | wrapper lifecycle + runner path/no-9022-probe + scoped Rust C0/release cleanup pass；2026-07-23 删除 disposable root target 后 final inventory `targets=0 reservations=0 target_processes=0 blockers=0` | [构建产物生命周期](issues/validation-build-artifact-lifecycle-and-disk-safety.md) |

## 4. 复现与验证层级

每类问题至少执行适用的四层验证，不能用下一层替代上一层，也不能用 mock 冒充真实 CLI：

1. 结构单点：直接测试 converter、sanitizer、payload、stream state machine 和 config round-trip，覆盖反例、分块排列、Unicode、误报和边界值。
2. 隔离端到端：当前 release binary + fake Kiro/external upstream + 隔离 PgSQL/Redis；记录代理请求、upstream hits、attempt ledger、usage、RSS/FD 和恢复。
3. 真实 Claude Code CLI：隔离 `HOME`/`CLAUDE_CONFIG_DIR` 和临时端口，覆盖 text、thinking、Bash、Read、tool loop、MCP、search、agent、图片、长 session/resume 和错误恢复。
4. 负载与混沌：Redis 延迟/断连/restart、多实例、429/500/partial/malformed、client drop、burst/recovery 和三轮 soak。

每个 case 的具体轮次和通过条件由 [重新验证矩阵](tests/reverification-matrix.md) 管理。未执行、环境跳过、报告缺 revision 或清理不完整均记为未通过。

## 5. 修复计划与批次

实施顺序由 [总体计划](plans/remediation-and-release-plan.md) 管理，摘要如下：

1. 先恢复 transcript/tool/thinking/stream 的结构所有权，sanitizer 仅保留兼容保护。
2. 再关闭 payload hard limit、图片真实字节、raw byte identity 和性能问题。
3. 引入 request-scoped attempt budget、per-key admission、严格错误分类和辅助调用分账。
4. 修复 Redis degraded/fallback、strict local-first、lease 重选、账号分布和 usage 清理隔离。
5. 完成两 UI、旧版本升级、AWS 凭据和 evidence 工具验证。
6. 执行 C0-C4、L0-L5、两套 UI、清理和生产只读 recurrence gate；按用户明确要求不运行本地 Docker 动态验证，但 Docker/依赖场景的开发验证程序必须编译并在报告中记为显式豁免而非 pass。

## 6. 当前已实现但尚未完成最终验收

以下只表示当前工作树已有实现或聚焦测试，最终状态仍取决于完整矩阵：

- malformed tool result 不再复制原文到普通 text；history trim 改为逻辑 turn 原子处理；内部工具名只接受真实映射。
- 命令型空消息占位由 `Continue`/`Tool results provided.` 收敛为无语义 `.`。
- prompt operator master 已成为所有代理新增提示的总开关；结构化 tool filtering 与客户端显式 thinking/output_config 映射仍保留在协议转换层，后端和两 UI 已同步文案与测试。
- request-scoped shared inference budget 已覆盖 local credential、payload/cache、stream precommit、external direct/fallback/failover 和 local rescue；默认硬上限 4，仍缺 429/500/partial/CLI/load 全矩阵。
- token refresh 已建立独立专题和 focused evidence：短 TTL 16 caller/30 sends、timeout 32 caller 放大及旧 invalid-bearer force-refresh fan-out 已登记；process-local 60/8、limit/config、metadata revision fence、API/MCP final-attempt zero-refresh 各内部 5 轮通过。新增的取消红绿、23/23 auxiliary、128-account shared、20-account independent、32-waiter singleflight 证明 process-local budget/permit/失败波已收敛；2026-07-19/20 两次真实隔离 Redis 的 leader、health claim、identity fence、stale leader、cancel-before-send、bucket TTL/version 五个五轮程序三轮通过，后一轮还复跑 API/MCP final-attempt。2026-07-22 又复现并修复 cross-instance fast-500 二次 leader：Redis waiter poll 为 500ms，而 jitter 后 failure replay 窗口可为 400-500ms；修复为 replay 下限 750ms 后，两实例 PostgreSQL/Redis cluster 7 exact × 3 outer × 5 internal（105 内部轮次）通过。完整 invalid-bearer/provider/load、token-refresh-specific Redis slow/error/restart、usage attribution、真实 upstream/native CLI 和 frozen-candidate 仍未关闭。
- generic `REQUEST_BODY_INVALID`、图片、invalid model 和 malformed 400 不再误入 tool retry；10 类 x 1/20/60 账号 x 5 轮真实本地 HTTP provider 矩阵通过。
- response sanitizer suppression 已从空格/部分 success 改为 fail-closed；local/external stream/non-stream 聚焦状态机通过，仍缺真实 CLI、local non-stream response retry 和 external SSE pre-byte 跨池 retry。
- stream/non-stream 已增加 EventStream 完整性、HTTP 200 JSON exception、空体、CRC、半帧和 terminal 检查；2026-07-19 frozen L1 fake-upstream 已覆盖 normal stream/non-stream、thinking/tool stream、JSON exception 200、429/500、invalid tool format、malformed SSE 和 client drop 12/12，r8 L3/L4 又覆盖 burst/recovery、restart/failure/client-drop/mixed-chaos。真实 Claude CLI fault gate、native upstream 和 thinking/search/image/MCP/agent 长会话仍未关闭。
- Redis scheduler degraded fallback 默认与旧配置迁移已调整，external wait 已有有限截止；2026-07-20 单实例非 Docker Redis chaos 7 tests × 3 outer，21/21 通过，覆盖 50/500ms latency、breaker threshold/backoff、disconnect/reconnect、300 lease release、cancel 和 commit-unknown，0 `AllDisabled`。2026-07-21 joint-fault `r5` 真实红于 deterministic WRONGTYPE recovery；修复 hot-path Redis failure `commit_unknown` 分类后，`r6` 8 tests × 3 outer，24/24 通过，关闭单实例 usage-writer 与 Redis fault 同时注入。同日 external takeover focused handler tests 4/4 与非 Docker runner contract 8/8 通过，但动态 service runner 仍缺调用者确认的独占空 PostgreSQL database URL，不能算产品门禁通过。两实例 fault/fallback、external takeover dynamic 和最终冻结候选仍 pending。
- 高并发低 RPM 不是矛盾：300 RPM 在约 100 秒平均占用下可维持约 500 in-flight，global=500 会让第 501 个请求排队。PgSQL runtime backlog 已与 quarantine 拆分，并进一步把自动健康 Patch 与 disabled/reason/generation 调度状态 Patch 分开。40x15/global-500 与非终态 backlog 多轮通过；2026-07-19 真实隔离 PostgreSQL pool pressure、pending replay、generation fence 与真实 Redis queue deadline/degraded/cancel 三轮动态通过。r8 frozen L3/L4/L5 已通过；2026-07-20 正常 usage/scheduler 联合 burst 又完成 9 个测量轮，scheduler loaded/recovery p99 最大 `55.785/69.204 ms`，随后单实例 Redis chaos 21/21；2026-07-21 单实例 usage-writer+Redis fault r6 24/24，external takeover focused/runner contract 也已通过。external 接管动态服务、两实例 fault/fallback 和真实上游仍未关闭，因此 SCH-003 不能关闭。
- 默认 finite queue lease 本已覆盖等待期却仍按 waiter 每 20 秒续租。local/external 已改为有限等待一次 TTL、无限等待才 renewal，并冻结 request deadline；500 guard、动态 config 与 TTL policy 均多轮通过。2026-07-19 真实隔离 Redis 的 22 秒 deadline 不移动、degraded waiter fail-closed 和 cancel release 程序三轮通过；2026-07-20 正常 usage/scheduler 联合 burst、单实例 Redis chaos 以及 r8 frozen L3-L5 也已通过；2026-07-21 单实例 joint fault r6 24/24，external takeover focused/runner contract 通过。external takeover 动态服务、两实例和最终候选重绑仍未执行，因此 SCH-005 仍保持 open。
- current candidate 另有独立的 preflight/acquire 竞态：static external eligibility 已确认但旧 250 ms runtime hint absent 时会回落到本地默认 120 秒等待。hint authority 已删除，五轮 policy 合同通过且正常路径不新增 Redis RTT；manager、storage、burst 和 frozen L3-L5 仍 pending，因此 SCH-004 不能关闭，也不能把它写成既有生产事件的唯一根因。
- external dispatch 原先每请求执行完整 PostgreSQL pool list，c128 可在 32-query hard cap 后直接 admission failure；发送 fence 又早于 body/header/model prepare。当前改为 manager-owned、generation-bound 250ms list singleflight和100ms failure cache，caller cancellation 不取消 authority refresh；同 revision 使用 in-flight-only fence，并将 revision linearization 移到 prepare 后、attempt/send 前；15 类坏持久化配置按池 strict fail closed且日志聚合。all-target check 与 11 个无存储 body/parser filters 通过；2026-07-18 当前仓库隔离 PostgreSQL/Redis 三轮 storage dynamic 为 51/51 passed，2026-07-19 同一 17 filters 再次三轮通过并与 runtime quarantine storage 合批。两个中间红项是测试合同将 coordinator cold bootstrap 5 RTT 误当作 steady-state 1 RTT，修正后 warm selection/acquire 仍是 1/2 RTT。SCH-006 仍需 frozen load、两实例、RSS/FD 和 release gate 才能最终关闭。
- usage soft cleanup 已改为在同一 PostgreSQL 事务中删除明细并扣减对应 summary、Dashboard、费用、credential、cache-read 和 duration rollup；hard cleanup 对正常 tombstone 不重复扣减。soft tombstone 存在期间同 ID 不复活，cutoff 后的新 ID 可写；hard cleanup 后只由 watermark 拒绝旧 `created_at` replay，不承诺永久 ID 防重。当前 PostgreSQL 测试源码和两套 UI 文案已同步该合同，并新增 writer shared / cleanup exclusive transaction advisory lock 关闭 in-flight commit 竞态。隔离 cleanup 组 36/36 x3 外层（108/108、0 ignored）、updated round-trip 1/1、1000 external billing cleanup 1/1、replay/new-ID 内部三轮、in-flight guard 内部三轮、正常 Redis writer/scheduler burst 和普通单实例 scheduler Redis chaos 均通过；完整套件、writer 性能、browser、cleanup 与 Redis fault 同时注入仍未关闭，因此 OPS-001 不能关闭。
- Redis usage writer 已从旧 3 RTT/late gate/early seen 改为 snapshot、aggregate、seen 同一 Lua 1 RTT；exact cache-read bucket 默认上限 4096，batch 使用单共享 deadline。两个无 Redis 合同测试各内部 5 轮通过；2026-07-18 与 2026-07-19 当前仓库隔离 Redis 均完成三轮动态验证，cache-read cap 与 partial-error 两个五轮程序通过。2026-07-20 正常 usage/scheduler 联合 burst 9 个测量轮又通过，writer throughput `449.03..617.72 records/s`、writer p99 最大 `31.482 ms`、scheduler loaded/recovery p99 最大 `55.785/69.204 ms`、RSS/FD 门禁通过；普通单实例 scheduler disconnect/recovery 另有 21/21 chaos pass。2026-07-21 单实例 usage-writer 与 Redis fault 同时注入 r6 为 24/24。跨实例和生产高基数仍 pending，OPS-003 不能关闭。
- 业务 Redis 与观测 Redis 故障域已经从产品路径拆分：`observabilityRedis` optional 配置拒绝同 authority，即使 DB/prefix 不同；启动时比对 `run_id`，不可证明独立则 observability Redis 降级为 PostgreSQL/进程内观测，不能回落 business Redis；UsageRecorder/Admin cache/cleanup 只接收 observability Redis。2026-07-21 产品 runner 使用当前项目两个 loopback Redis authority 完成 3 outer × 1 exact × 内部 3 轮，observability latency/disconnect 不影响 business scheduler，business fault fail closed 且不伪装 `AllDisabled`，scoped cleanup `removed=true / reservation_released=true`；scheduler Redis failure classification 修复后 `redis-fault-domain-product-20260721-r4` 又完成同矩阵 3/3；Rust 1.92.0 C0 子集也通过。后续又给 `RedisStore` usage materialization 专用入口加 production role guard，并追加主/观测 Redis 路径隔离合同，确认 scheduler/external/runtime-event/health 只用 business Redis，observability Redis 启动失败不回落 business Redis，UsageRecorder 主请求路径只入队观测 writer、压力下丢弃 summary 而不阻塞请求；合同 `46 tests / 37 pass / 9 skip`、scheduler/fault-domain 合批 `74 tests / 53 pass / 21 skip` 和 scoped `cargo +1.92.0 check --bin kiro-rs` 均通过。该项只关闭 E09 focused/product/source-contract gate；两实例真实服务、external takeover dynamic、生产高基数和 final release gates 仍 pending，不能作为发版通过依据。
- evidence 打包脱敏和无第三方依赖 quick validation 已实现；最终清洁 fixture gate 尚未归档。
- remote image/document 已增加预扫描 source count、累计 download/base64/HTTP attempt、45 秒 workflow deadline、4 工作流全局准入、连接时 DNS 地址过滤与 `no_proxy`；19 个模块测试、handlers 90/90、50 次 remote handler 预拒绝、30 次 inline 对照及四档 clean-text 100 轮 identity 通过，CLI/load 仍待关闭。
- 构建生命周期 wrapper 已实现逐批 target/reservation 清理、原子磁盘准入、command PGID ownership 和 cleanup 二次信号保护；只读 inventory 已实现 known/unknown target、stale/active owner、相对 binary、cwd/txt/executable 和检查不完整 fail-closed。runtime/CLI/load/scheduler/fault-domain runner 已删除根 target binary/report fallback，并以多项各 5 轮合同证明只接受仓库外冻结 binary 与 owned artifact root；当前又加入 no-9022-listener-probe 静态门禁和 `validation-child-env.mjs` 子进程白名单环境，确认非测试 validation runner 不再继承整份 `process.env`。2026-07-23 最终 C0/r4 scoped build 已清理 `size_kib=2516216 removed=true reservation_released=true`；随后只删除 disposable root `target/debug`、`target/flycheck0` 和 `.rustc_info.json`，inventory 为 `targets=0 reservations=0 target_processes=0 blockers=0`。OPS-002 对本次发布门禁已关闭；后续仍需防止 rust-analyzer/flycheck 在验证间隙重建 repo root target。
- 当前默认 bin 完整单测树已从两次 stack abort、4 个普通失败、invalid-refresh client 初始化放大、provider matrix 资源超时和 1 秒 deadline 于 1.651 秒接受 500 的迟到响应，收敛到当前 `1715 passed / 0 failed / 6 explicit perf probes ignored`。`full-unit-current-r12` 测试 `351.96s`、wall `581.7s`，scoped target `1682460 KiB` 后 `removed=true / reservation_released=true`；无验证 target 或临时日志残留。Kiro-RS Tool external usage、invalid refresh 零 client/admission/send/health、provider fault/privacy、strict deadline 与 queue/provider fixture 都包含在该树中；storage 缺 URL 的正文早退只算 compile coverage。Rust 1.92.0 `cargo check --all-targets` 零 warning；真实 storage、all-target tests/no-default/release/frozen/CLI/load 仍 open。
- 2026-07-18 provider/MCP transient retry-target guard 新增一轮红绿闭环：full all-target 开发验证先红于 `provider_transport_and_body_fault_matrix_is_private_typed_and_bounded` 的 `provider_header_timeout` 30 秒超时；修复后瞬态失败写入冷却时只有存在可立即调度的备选凭据才继续重试，否则立即 typed fail，不再进入下一轮 scheduler acquire 等冷却。复测结果为 `cargo fmt --check + cargo check --all-targets` 通过、focused provider fault matrix `1/1` 通过、full `cargo test --all-targets` 为 `1724 passed / 0 failed / 6 ignored`，`kiro_loadtest` 为 `27/27`；Node 合同 `21/21`、runner/path/signal `61/61`、feature docs/费用/MCP/request-key/prompt 合同均通过。该批因本机磁盘不足使用 7-10 GiB development reservation，不能替代最终 12 GiB release gate。详见 [provider transient evidence](evidence/provider-transient-no-retry-target-20260718.md)。
- 上游错误正文的 focused 隐私矩阵已覆盖 13 类 status/JSON 与 6 类 transport/body 故障，stream/non-stream、1/20/60 pool、每格 5 轮，共 570 个 outcome/1530 个预算 send；private marker 在 provider error、attempt、scheduler snapshot、DEBUG log 和代表性 Router UsageRecord 中均为 0。持久 PostgreSQL/Redis 扫描、冻结 HTTP 与混合 error burst 仍阻断 SEC-004 最终关闭。

## 7. 最终验证结果

当前最终发布门禁已在 2026-07-23 完成。下表第一行是当前统一候选的 release gate；后续历史/分批行保留为专题覆盖证据，不能覆盖第一行的当前构建身份，也不能被单独外推为 Docker/生产/真实 upstream pass。

| 范围 | 当前证据 | 判定 |
| --- | --- | --- |
| 2026-07-23 final release gate | 当前统一候选 `kiro-rs` SHA `925525419cd48b460217df2568891a40287da0c44d2bf921a38b103c047775ee`，`kiro_loadtest` SHA `90babda7388aa93854cbbdb81c132cc436c07f46b0ea22973531b0a7ffb3aff1`。`final-c0-release-20260723-r4` scoped batch 通过 `cargo +1.92.0 fmt --all -- --check`、`cargo +1.92.0 test --all-targets`（main `1750 passed / 0 failed / 6 ignored`，`kiro_loadtest 31/31`）和 `cargo +1.92.0 build --release --bins`；scoped cleanup `size_kib=2516216 removed=true reservation_released=true`。同轮 `node feature/tests/check-feature-docs.mjs` 为 47 issue docs / 115 links pass，`node --test feature/tests/*.test.mjs` 为 `283 tests / 261 pass / 22 explicit skips / 0 fail`，`git diff --check` pass；删除 disposable root `target/` 后 `node feature/tests/inventory-build-artifacts.mjs --gate` 为 `targets=0 reservations=0 target_processes=0 blockers=0`。详见 [final release gate evidence](evidence/final-release-gate-20260723.md)。 | release gate pass / publish pending；Docker dynamic explicitly waived and not counted as pass |
| C0d final candidate static/build/CLI-ingress/UI-build/loadtest-fake | 当前仓库外候选 `kiro-rs` SHA `fefd6204c1851c9795ae16fb006115997f7884570988622a77200c3e438cd7ec`，`kiro_loadtest` SHA `f92e91b4f9c2d669e29e6bbb9e4d4b58f38d2f8bfac3f4bd51260c0d2edd6782`。C0d scoped batch 通过 `cargo fmt --check`、`cargo test --all-targets`（main `1742 passed / 0 failed / 6 ignored`，`kiro_loadtest 31/31`）和 release build；cleanup `size_kib=2446284 removed=true reservation_released=true`。同轮静态合同通过：feature docs 47/47 与 108 links，UI/cost/MCP/request-key/prompt 合同全绿，`node --test feature/tests/*.test.mjs` 为 `280 tests / 258 pass / 22 explicit skips / 0 fail`；Claude CLI 2.1.197 raw thinking capture `6 effort × 5 rounds` 通过，确认 CLI 发送 `thinking.type=adaptive` 且 `max` 不被 CLI clamp；两套 UI build 通过；C0d `kiro_loadtest` fake-upstream 小矩阵覆盖 stream/non-stream/thinking/tool/429/500/JSON-exception200/malformed/client-drop/mixed/recovery。只删除无引用 root `target/debug`/`flycheck0`/`.rustc_info.json`，未停止 PID 84264；最终 inventory `targets=0 reservations=0 target_processes=0 blockers=0`。详见 [C0d evidence](evidence/final-candidate-c0d-static-cli-load-ui-20260721.md)。 | C0/static/build/CLI-ingress/UI-build/loadtest-tooling pass; PG/Redis dynamic, real upstream, native capability, browser, upgrade and final release gates still pending / NO-GO |
| Usage cleanup 组 | 隔离 PostgreSQL/Redis `cargo test cleanup` 为 36/36 x3 outer runs，即 108/108、0 ignored；summary p95 `4.952-9.945 ms`，dashboard p95 `16.645-49.070 ms` | cleanup filter pass; full/performance/chaos pending |
| Protocol contamination source contract | 新增纯 Node 源码合同，不启动 Docker、不启动 `kiro.rs`、不调用 Cargo。单独运行 `10 tests / 10 pass / 0 fail`；与 business/observability Redis fault-domain 合批 `56 tests / 47 pass / 9 explicit live-signal skips / 0 fail`。合同锁定 sanitizer 不信任任意 `Hashxxxxxxxx`、raw marker-free body 在 DOM parse 前返回、assistant 清理不改 user/tool data、signed/redacted thinking 原子 fail closed、strict request 不 raw-external bypass、stream/non-stream/external 污染后不产生空白/部分 success terminal | source-contract pass; native upstream/CLI/fault/load still pending |
| Protocol marker inventory source contract | 新增纯 Node 生产源码 marker inventory 合同，不启动 Docker、不启动 `kiro.rs`、不调用 Cargo。单独运行 `4 tests / 4 pass / 0 fail`。合同锁定生产代码中 `user Continue`/`user Tool results provided` 仅在 sanitizer，`Tool results:` 仅在 sanitizer/stream observability，function-results/function-calls 标记仅在 stream adapter；生产代码无 bare `<invoke>` 字面量、无 `[previous output]`/`[trimmed output]`/`[duplicate output]`，tool-result-only placeholder 为 `"."`，invalid tool-result repair 不 textify rejected content | source-contract pass; native upstream/CLI/fault/load still pending |
| Redis usage-writer/scheduler burst | 早期三轮 loaded p95/p99 `3.027-4.406/4.319-15.936 ms`；2026-07-20 标准 runner 又做 3 outer × 3 internal，共 9 个测量轮，writer p99 最大 `31.482 ms`、scheduler loaded/recovery p99 最大 `55.785/69.204 ms`、RSS 增量最大约 `8.9 MiB`、FD 恒为 `15`；2026-07-21 joint-fault r6 为 24/24 | normal joint-pressure + single-instance simultaneous fault pass; two-instance and production-cardinality pending |
| Scheduler Redis 单实例 chaos | 2026-07-20 非 Docker runner 使用独占空 DB15 与 loopback proxy，7 tests × 3 outer，21/21；50ms 成功、500ms 约 250ms deadline fail-closed 并恢复，覆盖 breaker/backoff、disconnect/reconnect、300 lease release、cancel、commit-unknown，0 `AllDisabled`；2026-07-21 r5 红于 WRONGTYPE recovery，修复后 r6 8 tests × 3 outer 为 24/24 | single-instance Redis chaos + single-instance simultaneous usage fault pass; two-instance fault/fallback and external takeover dynamic pending |
| SchedulerRedisDegraded external takeover | 源码复核确认 fallback toggle、fresh-state guard 和 external eligibility 链；`external-takeover-focused-20260721-r2` 四个 exact handler/fallback tests 通过并清理 `1708372 KiB`；非 Docker runner contract 8/8 通过 no Docker/Cargo、no protected `9022` probe、DB0/非 loopback/unsafe prefix 预拒绝与 validate-only 合同；2026-07-22 frozen r12 `eca8ce4...` 动态 enabled 3 个 clean-DB 轮次通过，每轮 5/5 degraded external 200 + 5/5 recovery local 200；disabled 1 个 clean-DB 轮次通过，5/5 degraded 429 且 local/external hits 0，恢复 5/5 local 200；所有 runner cleanup remaining 0 | focused/runner-contract + single-instance dynamic enabled/disabled pass; two-instance fault/fallback, native upstream/full capability and final release inventory pending |
| E01/E02 scheduler fairness runner | `scheduler-fairness-sticky-race.mjs` 已改为 caller-owned PG/Redis，不再 Docker run/exec、不再 `FLUSHDB`、不再 `CREATE DATABASE`；每 case 使用独立 Redis `keyPrefix`。contract 7/7、runtime path contract 9/9、external takeover contract 8/8 和 `git diff --check` 通过 | runner safety contract pass; dynamic distribution/sticky/lease-race run pending on frozen binary and pre-created empty PG DBs |
| E05 strict-local-first runner | `strict-local-first-routing.mjs` 已改为 caller-owned PostgreSQL/Redis 的非 Docker 全矩阵入口；保留 10 类模式，要求冻结 binary、owned artifact root、`modes × rounds` 个预创建 `kiro_e05_*` database、loopback Redis DB/prefix；不启动 Docker、不创建 database、不 `FLUSHDB`、不调用 Cargo、不探测 `9022`，子进程不继承整份 `process.env`。contract 6/6 通过，同批 runtime path 9/9、external takeover 8/8、E01/E02 7/7，合计 30/30 通过；最小环境断言曾红于 `startService` 继承环境，修复后 E05 contract 6/6；`git diff --check` 通过；后续补证清理 root target 后 inventory 为 `targets=0 reservations=0 target_processes=0 blockers=0` | runner contract pass; dynamic E05 service run pending |
| F06 AWS API key + region runner | `aws-api-key-region-lifecycle.mjs` 已改为 caller-owned PostgreSQL/Redis 的非 Docker入口；要求冻结 binary、owned artifact root、`kiro_f06_*` database、loopback Redis DB1..15 和 caller-owned prefix；不启动 Docker、不创建 database、不 `FLUSHDB`、不调用 Cargo、不探测 `9022`，服务子进程使用最小环境，Redis 只删除 owned prefix。contract 6/6 通过，同批 runtime path / external takeover / E01/E02 / E05 runner contracts 36/36 通过，`git diff --check` 通过 | runner contract pass; dynamic F06 lifecycle run pending on frozen binary and caller-owned empty PG/Redis |
| Request API key admission runner | `request-api-key-admission-multi-instance.mjs` 已改为 caller-owned PostgreSQL/Redis + 本地 `redis-chaos-proxy.mjs`；要求 `kiro_request_admission_*` database 列表和 loopback Redis DB1..15/prefix；不启动 Docker、不创建 database、不 `FLUSHDB`、不调用 Cargo、不使用 `host.docker.internal`、不探测 `9022`，两个服务实例使用最小环境。contract 5/5 通过，同批 request-admission/F06/runtime/external/E01/E02/E05 contracts 41/41 通过，`git diff --check` 通过 | runner contract pass; dynamic multi-instance admission run pending on frozen binary and caller-owned empty PG/Redis |
| Frozen load/chaos runner | `frozen-load-chaos-runner.mjs` 已改为 caller-owned PostgreSQL/Redis；要求仓库外冻结 `kiro-rs` 与 `kiro_loadtest`、owned artifact root、tier 对应数量的预创建 `kiro_load_chaos_*` database、loopback Redis DB1..15 和 caller-owned prefix；不启动 Docker、不创建/drop database、不 `FLUSHDB`、不调用 Cargo、不探测 `9022`，fake/proxy/loadtest 子进程使用最小环境，Redis 只 SCAN/DEL owned prefix。contract 6/6 通过，同批 runtime path 合同 15/15 通过，`git diff --check` 通过 | runner contract pass; final L3/L4/L5 dynamic rebind pending |
| Redis storage runner cleanup | `run-token-refresh-cluster-validation.mjs` 与 `run-multi-instance-redis-coordination-validation.mjs` 已去掉 runner 级 `FLUSHDB` 兜底；启动前仍要求 caller-confirmed empty DB1..15，测试后若有残留只报告 `residualKeyCount` 并让门禁失败，不清空整库。纯 Node 合同 `19 pass / 1 skip / 0 fail`，skip 为显式 live nonempty Redis opt-in；source 扫描无 `FLUSHDB/FLUSHALL` | runner cleanup contract pass; token-refresh cluster and multi-instance dynamic reruns pending |
| Runner 子进程环境隔离 | 新增 `validation-child-env.mjs`，真实 runtime/Claude/scheduler/fault-domain runner 子进程改为白名单环境。`runtime-validation-paths.test.mjs` 当前 11/11 通过，确认 child env 不继承 `DATABASE_URL`、`REDIS_URL`、Anthropic/OpenAI key、`KIRO_API_KEY`、`KIRO_RS_TEST_REDIS_URL` 或任意未显式传入的 `KIRO_*`；所有非测试 `feature/tests/*.mjs` validation runner 均无 `...process.env` | validation contamination contract pass; no product dynamic gate closed |
| 轻量合同回归 | 主体不启动 Docker、不启动 kiro.rs 服务。feature docs 初始 47/47 与 102 links，通过 runner env 补证后复跑为 47/47 与 104 links；cost format、MCP attempt channel、request API key ID、prompt control independence/default parity 均通过；E03/token-refresh/multi-instance runner contracts 70 pass/1 skip，scheduler chaos/fault-domain contracts 初始 44 pass/21 skip、业务/观测 Redis 源码合同补证后合批 49 pass/21 skip/0 fail、RedisStore production role guard 补证后合批 50 pass/21 skip/0 fail、主/观测 Redis 路径隔离合同补证后合批 53 pass/21 skip/0 fail，thinking wire 45/45，Claude capture/bare signal 5/5，wrapper lifecycle 21/21；最新继续复核中 UI/prompt 合同 PASS，non-Docker runner/path 合批 49/49，E03/token-refresh/multi-instance/scheduler/fault-domain 合批 146 tests：124 pass、22 explicit fixture skips、0 fail，thinking/Claude signal 合批 50/50；RedisStore role guard scoped `cargo +1.92.0 check --bin kiro-rs` 通过并清理 `446876 KiB`；清理 root target 后 inventory 为 `targets=0 reservations=0 target_processes=0 blockers=0`，本轮只清理 5 个明确测试前缀小临时目录 | contract pass; skipped live fixtures and dynamic gates still open |
| 高并发低 RPM/runtime quarantine | 40 账号 backlog、Patch 字段矩阵、40x15/60-RPM/global-500 均各内部 5 轮；2026-07-19 真实隔离 PG/Redis storage `3 outer × 6 exact` 通过；r8 frozen L3/L4/L5、2026-07-20 正常联合 burst、单实例 Redis chaos 与 2026-07-21 r6 simultaneous fault 通过 | focused + isolated storage + frozen fake-upstream load + single-instance chaos/joint-fault pass; external takeover dynamic/two-instance pending |
| Queue lease Redis RPM | 500 finite guard、unlimited、local/external TTL、长 override 与动态 config deadline 各内部 5 轮；2026-07-19 真实 Redis deadline/degraded/cancel 三轮通过；2026-07-20 正常联合 burst 与单实例 Redis chaos 通过；2026-07-21 r6 simultaneous fault 通过 | focused + isolated Redis + single-instance chaos/joint-fault pass; two-instance/external takeover dynamic pending |
| Scheduler focused rerun | development reservation 6 GiB；scheduler fallback toggles、preflight toggles、runtime backlog false-disable、finite queue lease、deadline freeze、40x15/global-500 六个精确 filter 均 `running 1 / passed 1`；后续真实 PG/Redis storage、正常 joint burst、单实例 chaos、r6 simultaneous fault 和 r8 frozen load 已分别通过 | development + isolated storage + normal joint-pressure + single-instance chaos/joint-fault + frozen fake-upstream load pass; external takeover/two-instance/final rebind pending |
| Storage validation wrapper protected-port gates | token-refresh Redis、Redis usage writer、runtime quarantine、external dispatch 四个 wrapper `bash -n` 通过；URL 指向 `127.0.0.1:9022` 均在 Cargo/TCP 连接前 exit 64；2026-07-19 三组真实 storage gate 通过，复核 `target=0B` 且 inventory `blockers=0` | fail-closed wrapper + isolated storage pass; broader frozen/load pending |
| Local capacity preflight race | policy、global full、CapacityFull、alternate reselect 各内部 5 轮通过且 queue 0；三个 storage filter 缺 PG 后提前返回 | focused policy/manager pass; storage/load pending |
| Token refresh process-local/final attempt + cluster PG/Redis | 60/8 每轮 128 reservations 为 8/120；API/MCP 各 5 轮 inference 1/OAuth 0；revision/config/limit 各 5 轮；2026-07-19/20 两次 token-refresh Redis 三轮动态通过，后一轮还复跑 API/MCP final-attempt，scope `1698460 KiB removed=true reservation_released=true`；2026-07-22 修复 Redis fast-failure replay 窗口小于 waiter poll 的二次 leader 问题后，两实例 PostgreSQL/Redis cluster gate 7 exact × 3 outer × 5 internal（105 内部轮次）通过，scope `1715296 KiB removed=true reservation_released=true` | focused + isolated Redis state-machine + two-instance PG/Redis cluster pass; full invalid-bearer provider, refresh-specific Redis slow/error/restart, usage attribution/frozen load pending |
| OAuth auxiliary budget/singleflight/cancel | 首次 cancel 21/22 红；结构化取消后 auxiliary 23/23；128-account shared 与 20-account independent 各 12 类 x c1/c8/c32 x 5 轮；32-waiter 四类 x 5 轮严格 1 hit | process-local focused pass; Redis/PG/provider/usage/L3-L5 pending |
| Claude CLI thinking/effort frozen gate | 真实 Claude Code CLI 2.1.197；历史三次 `6 档 x 5 轮` 共 90/90 Messages hit 证明 adaptive 恒存在、absent 默认 high、显式 max 原样发送；2026-07-19 pre-fix frozen `70c9741b...` 复现普通 `sonnet` + old catalog 400，修复后 frozen `e16df13a...` 的 thinking wire `60/60` 通过，report SHA `439e1e69...`，`max` 不截断、不发明未声明 `thinking`；通用 Bash/Read `--continue` 5x20/5x100 另行通过 | ingress/no-probe/frozen fake-upstream wire + generic long-session pass; active/passive thinking long-session, search/image/MCP and real upstream pending |
| Bare invoke real Claude CLI | frozen `e16df13a...` 跑 `bare-invoke-claude-cli.mjs`：20 cases，15 literal XML negative、5 structured Bash tool loop，25 inference，tool_use/tool_result 各 5，unknown 0，cleanup 全 true，report SHA `67c9d7c9...` | fake-upstream C2 pass; C3/C4/load pending |
| Frozen loadtest fake-upstream | L1 product `e16df13a...`、loadtest `23c04221...` 为 12/12；r8 product `131696bd...` 又通过 L3 9/9、L4 12/12、L5 300 秒 2281/2281 与 900 秒 6821/6821，0 Redis degraded/capacity failure/429，RSS/FD 通过；raw roots 和 scoped targets 清理 | L1/L3/L4/L5 fake-upstream pass; final candidate rebind, real upstream, two-instance, UI/upgrade and final inventory pending |
| Provider transient retry target | full all-target development 先红 `1723/1/6`，修复后 `cargo check --all-targets`、focused fault matrix `1/1`、full all-target `1724/0/6` 与 `kiro_loadtest 27/27` 通过；所有 scoped targets 回收 | development pass only; default-reservation release C0/frozen load pending |
| 2026-07-19 storage/artifact gate | token-refresh Redis `15/15 exact`、Redis usage writer `6/6 exact`、external dispatch + runtime quarantine storage `69/69 exact`；三个 scoped targets 均 `removed=true / reservation_released=true`；关闭旧 `kiro_cli_repro` tmux 残留后 inventory `targets=0 reservations=0 target_processes=0 blockers=0` | isolated storage/artifact pass; final release inventory still pending |
| Release admission lightcheck | 默认 wrapper 以 12 GiB reservation + 20 GiB floor 运行 `true`；`available_kib=28588348`，返回 75；自身 16 KiB 临时 target 已清理，复核 `target=0 KiB` | release C0 blocked by whole-disk free space, not by current target residue |
| 当前默认 bin 完整单测树 | 红树保留两次 stack abort、4-failure、invalid-refresh、provider-timeout 和迟到 HTTP 500；当前 `1715 passed / 0 failed / 6 ignored`，`351.96s`，scope 清理完成；all-target check 零 warning | dirty-tree default-bin/check pass; real storage/all-target tests/no-default/release/frozen gates pending |
| Redis usage writer 新 1 RTT 合同 | 提交顺序与 batch shared-deadline 两个精确测试各内部 5 轮通过；当前仓库隔离 Redis 2026-07-18/19 两批三轮动态 cache-read cap / partial-error 通过；2026-07-20 正常 writer/scheduler 联合 burst 9 个测量轮通过；2026-07-21 r6 single-instance joint-fault 24/24 通过 | focused + isolated Redis + normal joint-pressure + single-instance fault pass; cross-instance and production-cardinality pending |
| 业务/观测 Redis 故障域 | 2026-07-21 纯 Node contract 初始 28/28 早拒绝 pass（9 live signal skipped）、live signal 37/37 pass、基础 runner 3 outer pass；产品 runner `redis-fault-domain-product-r2` 为 3 outer × 1 exact × 内部 3 轮，3/3 exact invocations passed；post-fix `redis-fault-domain-product-20260721-r4` 同矩阵通过；Rust 1.92.0 scoped C0 子集通过 fmt/diff/4 个 config tests/`cargo check --all-targets`；后续生产源码合同、RedisStore role guard 和主/观测 Redis 路径隔离合同补证后默认 Node contract 为 46 tests / 37 pass / 9 skip / 0 fail，scheduler/fault-domain 合批 74 tests / 53 pass / 21 skip / 0 fail，scoped `cargo +1.92.0 check --bin kiro-rs` 通过；锁定 usage/Admin/cleanup 只接收 observability Redis，Redis usage materialization 入口生产环境不能使用 business scheduler Redis，scheduler/external/runtime-event/health 只使用 business Redis，observability 启动失败不回落 business Redis，UsageRecorder 主请求路径不被观测 Redis IO 阻塞；产品/C0/role-guard scopes 均 `removed=true / reservation_released=true` | E09 product/source-focused pass; two-instance real processes, external takeover dynamic, production-cardinality and final release inventory pending |
| PostgreSQL cleanup contracts | updated round-trip 1/1；1000 external billing、replay/new-ID、in-flight writer/watermark guard 均被 36-test cleanup 组外层重复三次，后两项每次再内部三轮 | focused/cleanup pass; full/writer-performance pending |
| Cleanup UI 合同 | 两套 UI 源码已明确 soft cleanup 扣累计统计/费用/Dashboard、hard cleanup 只删 tombstone | source aligned / browser pending |

所有当前最终门禁结论必须带冻结构建身份、命令、轮次/计数、清理结果和限制。后续若新增真实 upstream、Docker、浏览器或生产复测，应另增 dated evidence，而不是改写本轮结果。

## 8. 残余风险与不作保证的边界

最终报告不会承诺“以后绝不会出现任何泄漏或异常”。可作出的结论必须限定为：列明的故障模型、输入空间、路由、CLI 版本、负载档位和观察窗口均通过；未知上游协议变化仍由 fail-closed、错误归一化、attempt 硬上限和观测告警承担。

## 9. 发布判定

当前判定：`RELEASED / v0.0.114`。

2026-07-23 当前统一候选已经完成最终 C0/release build、文档、Node 合同、diff 和 build artifact inventory。Docker 动态验证按用户当前要求豁免，不记为 pass。既有 `127.0.0.1:9022` 服务未被停止、重启、迁移或压测。发布流程已完成：

- work commit: `b528ead` (`fix: harden runtime protocol and scheduler gates`)；
- release bump commit: `beb9b3420b20776db489461d65392b5b1d6e5d92` (`chore(release): 0.0.114`)；
- latest remote semver tag used as base: `v0.0.113`；
- release model: `rust-crate`；
- new version: `0.0.114`；
- annotated tag: `v0.0.114`；
- tag object: `071ccb3975fb1ae2bf6cd27f9875f9dd4b9a24e8`；
- tag peeled commit: `beb9b3420b20776db489461d65392b5b1d6e5d92`；
- push order: branch first, tag second；both succeeded.

发版前最小复核已经完成：

1. `node feature/tests/inventory-build-artifacts.mjs --gate`: pass。
2. `node feature/tests/check-feature-docs.mjs`: pass。
3. `node --test feature/tests/*.test.mjs`: pass。
4. `git diff --check`: pass。
5. Rust scoped C0/release gate: pass。

Post-release 文档记录见 [feature/releases/README.md](releases/README.md)。该记录在 tag 推送成功后补写，不移动已发布 tag。
