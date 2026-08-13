# 当前代码事实矩阵

Date: 2026-07-16

Role: 区分当前源码事实、当前构建动态复现、历史证据和修复后待重跑门禁

Status: 复核进行中；本表不是发布通过声明

## 证据等级

| 等级 | 含义 |
| --- | --- |
| `D-current` | 对当前未提交工作树构建身份有可重复动态证据 |
| `S-current` | 当前源码路径可直接证明，但缺当前构建动态故障注入 |
| `H-only` | 只有旧构建、旧报告或聊天记录，必须重跑 |
| `N` | 尚未测试 |

“测试存在”不等于“测试通过”；“历史构建通过”不等于“修复后构建通过”。最终发布矩阵必须为每项同时记录 Git revision、dirty diff 摘要、二进制 SHA-256、命令、隔离端口、轮次和报告路径。

## 当前结论

| ID | 问题类 | 当前结论 | 证据 | 发布前缺口 |
| --- | --- | --- | --- | --- |
| PRO-001 | assistant transcript / tool history 泄漏 | `focused-implementation-pass / cli-and-long-history-pending`；hash 只是指纹，结构化历史破坏、命令型占位和 response 假成功才是根因类 | 修复前动态证据：malformed orphan/mismatch/duplicate 各 5/5，trim 拆 active pair 5/5。当前聚焦证据：原文 textify 已删除、turn 原子裁剪、精确 raw/mapped/history tool matcher、占位改为 `.`；local/external response contamination fail closed。见专题与 protocol evidence | 当前冻结候选重跑；真实 CLI、20/100 tool cycle、120k history/resume、payload trim、HTTP 首输出前 retry fault 和性能矩阵 |
| PRO-002 | thinking、signed/redacted thinking 泄漏 | `focused-state-machine-pass / cli-and-e2e-pending`；修复前 text-only sanitizer 的绕过已纳入 current policy | 修复前：local/跨 block/外部与真实 CLI 均复现。当前聚焦证据：native/XML/unsigned/signed/signature-field/redacted、request/response、local/external、stream/non-stream、逐字符/multi-data/EOF 和 1 MiB 原子边界通过；污染 response fail closed，clean signed/redacted value-identical | 当前冻结候选重跑；真实 CLI C2-C4、20-tool/120k history、HTTP fault、MCP/agent 混合和 release p95/L5 |
| PRO-003 | payload guard 语义、soft target、绝对上限、图片 | `reproduced-defect-with-contract-correction`；完成周期 flatten 已删除，但 repair/trim、图片边界和资源上限动态门禁未关闭。`payloadGuardMaxBytes` 按部署/UI 合同是 soft shaping target，不应直接改成 hard reject | `D-current`：tool schema 可令 100 KB target 后仍约 607 KB，符合 `still_oversized` 透传合同但暴露旧注释矛盾；图片 placeholder 重复 5/5。`S-current`：5 MiB 按 base64 字符数而非 decoded bytes；最多 64 次重序列化；router 有固定 50 MiB `DefaultBodyLimit` | 语义不变单点矩阵、soft target 合同、50 MiB 413/0-upstream/burst memory 验证、decoded image bytes、性能基准 |
| PRO-004 | external raw/normalized/SSE/strict | `reproduced-defect`；策略与路由不一致 | `D-current`：thinking、多 data line、start text、跨 block 泄漏；strict 同时被修改又有绕过；normalized prompt 开关无效 | request-scoped policy；完整 SSE parser；strict 合同；usage suppression 计数 |
| PRO-005 | stream 终止、HTTP 200 exception、malformed EventStream | `reproduced-defect`；首输出前 retry 有实现但缺确定性门禁 | `D-current`：200 exception 空成功 6/6；malformed stream/non-stream 误报 200；CLI 3/3 误报 success。`S-current`：downstream commit guard | 5 轮 fault injection；缺终止事件必须失败；已提交后绝不 retry |
| PRO-006 | 单请求重试预算与内部 RPM 放大 | `process-local-budget/singleflight/cancel-focused-pass / cluster-provider-load-pending`；历史 attempt 可随账号池线性放大，当前 dirty tree 已封住单进程 OAuth 跨账号和同账号失败波 | 历史 `D-current`：500/429 最高 30x、partial disconnect 9x。当前：23/23 auxiliary；128-account shared 与 20-account independent 各 12 类 x c1/c8/c32 x 5 轮；32-waiter 每轮 1 hit；60/8 每轮 8/120；详见 2026-07-18 evidence | live Redis/PG、两实例 aggregate、真实 handler/CLI、persistent usage attribution、客户端重试和冻结 L3-L5 |
| PRO-007 | prompt master、tool_choice、scope、count_tokens | `reproduced-defect`；operator prompt 开关控制了协议语义 | `D-current`：master OFF 时 none/named/any 均退化 N；count_tokens 与 messages 不一致。`S-current`：converter 直接依赖 master | 拆 operator/policy；两 UI 不互相覆盖；逐开关正交测试 |
| PRO-008 | tool/schema/image/search/MCP/agent 基础兼容 | 部分通过，不得外推 | `D-current`：tool name/schema round trip、direct image、MCP search、CLI Bash/Read 通过。`N`：native WebSearch、agent、真实 CLI image | 每能力独立多轮；不能用 MCP search 代替 native WebSearch |
| SEC-001/RES-001 | remote multimodal SSRF、辅助 HTTP 与资源上限 | `focused-handler-pass / cli-load-pending`；旧双 DNS/透明 proxy 与无累计预算已替换 | `D-current`：19/19 模块、handlers 90/90；50/50 remote handler 400/0 inference、30/30 inline；source/byte/base64/attempt/deadline/global admission、真实 reqwest blocked-address、cancel/recovery；四档 clean text 各 100 轮 identity | PDF/text/CLI image、20/32 临界、burst RSS/FD、L5、统一候选重跑 |
| PRO-009/SEC-003 | WebSearch/MCP 特殊路径协议、错误、usage、资源与隐私 | `static-reproduced-defect`；当前代码必然存在，与 CLI 是否暴露 native WebSearch 无关 | `S-current`：`Err => None => success SSE`、stream flag 未使用、`messages.first()`、name-only route、raw info/debug logs、0 UsageRecorder、完整 body 聚合 | 固化 fake MCP 红测；实现统一 context/ledger/renderer/usage/limited body；真实 CLI 能力可用时验证 |
| SCH-001 | Redis scheduler degraded 与 external fallback | `reproduced-defect`；升级后新分类默认不 fallback | 生产只读证据：`local_error_no_fallback`、低 global in-flight、3 ms 本地失败。`S-current`：新 flag 默认 false 且无旧配置迁移 | 隔离 Redis 延迟/断连 3 轮；fallback 矩阵；消除全局 breaker 共因 |
| SCH-002 | strict local-first、账号偏斜、lease race、两实例 | `static-evidence`；fallback 前不重查 local route state，普通 wait 可粘住竞争失败账号 | 当前源码与有限 balanced 单实例测试 | 60 账号四策略、同/异 session、两实例 Redis、峰值 in-flight 与恢复 |
| SCH-003 | 高并发低 RPM、排队与运行态假禁用 | `focused-implementation-pass / storage-and-frozen-load-pending`；PgSQL mutation backlog 与 dispatch quarantine 已拆分，自动健康 Patch 又进一步从调度状态 Patch 分离 | `runtime-quarantine-focused-r3` 五个 exact filter 通过；`runtime-quarantine-patch-semantics-r4` 字段矩阵、40 账号四类 backlog、40x15/global-500 三条各内部 5 轮通过；两个 scope 均完整回收 | 真实 PgSQL pool timeout、100 秒假慢流、Redis+usage 联合抖动、external fallback、跨实例、RSS/FD 和冻结 release 性能 |
| SCH-004 | local preflight/acquire 容量竞态与 external fallback 延迟 | `focused-policy-and-manager-pass / storage-and-load-pending`；删除 250 ms runtime hint 对 fail-fast 的错误所有权，static eligible 且 capacity fallback 开启时抢槽失败直接回到 external selection，不进入本地默认 120 秒队列 | `queue-refresh-integration-r3` policy、global full、CapacityFull、alternate reselect 均 `running 1 / passed 1` 且内部 5 轮，queue 0；all-targets check 通过，`size_kib=2016724` 且完整回收；三个 storage filter 缺 PG 后 skip，不计动态 pass | external available/full/cooling/coordinator、40x15 慢流 burst、Redis+usage chaos、两实例和 frozen L3-L5 |
| SCH-005 | finite queue lease 内部 Redis RPM 放大 | `focused-implementation-pass / real-redis-and-frozen-load-pending`；local/external finite wait 一次 TTL 覆盖冻结 request deadline，0 periodic renewal；unlimited wait 保留 renewal | `queue-lease-refresh-provider-r1` 七个 exact filter 实际 7/7；500 guard、TTL/override、动态 config、40x15/global-500 各内部 5 轮，all-target check 无 warning，scope 完整回收；真实 Redis 22 秒程序 compile-only | 隔离 Redis 三轮、两实例 latency/disconnect/recovery、500 waiter 联合 usage pressure、冻结 L3-L5 |
| OPS-001 | usage 清空与 Redis/PG 干扰 | `cleanup-suite-three-outer-runs-pass / final-gates-open`；同步数据面已改为持久后台任务，soft cleanup 同事务扣减 detail 与权威 rollup，hard cleanup 不双扣，Redis 使用 bounded invalidation；same-ID soft-tombstone 合同和两 UI 源码说明已同步；writer/cleanup transaction advisory lock 已加入 | 隔离 cleanup 36/36 x3 外层（108/108、0 ignored）；summary/dashboard fallback、high-cardinality/legacy cost/duration/watermark/Redis burst；post-contract round-trip 1/1 | 完整 PostgreSQL/全树、advisory-lock writer 性能；两 UI browser；Redis 慢断/恢复、cleanup+scheduler、双实例和生产规模验证 |
| OPS-003 | Redis usage writer 原子性、基数与 scheduler 干扰 | `focused-implementation-pass / real-redis-pending`；snapshot/detail/index、aggregate 与 seen 已合并为一个 Lua EVAL，命令级错误 invalidation，cache-read bucket 上限 4096，64 条 batch 改为单共享 deadline 串行准入 | `usage-summary-atomic-c0-r3` 两个精确测试均 `running 1 / passed 1`，各内部 5 轮；all-targets 编译真实 Redis WRONGTYPE/基数测试；scope `2016696 KiB` 且完整清理 | 本机无非 Docker Redis/Lua，R1/R2 和 usage+scheduler latency/disconnect/recovery 尚未动态执行；冻结 release、多实例和生产规模性能 |
| UI-001 | 两 UI 费用精度与配置权威 | `static-evidence`；列表/详情 8 位，汇总仍有 2/6 位；prompt/body 配置会互相覆盖 | 两套 UI 源码，无格式化或浏览器回归 | formatter 单测、两 UI build、浏览器截图与交互矩阵 |
| MIG-001 | v0.0.101/102/103 升级 | `not-tested`；startup 禁止历史 usage 扫描已有静态保护 | `S-current`：migration 源码与 forbidden SQL 测试；旧日志来源不完整 | 三个 tag 各自数据集，每版 3 轮，二次启动幂等与回滚 |
| CRD-001 | AWS Kiro API Key + region 生命周期 | `core-and-malformed-lifecycle-pass-on-provisional-build / non-Docker-runner-contract-pass / final-and-browser-pending` | 十个隔离完整运行共 90/90 正向 case；schema-v2/v3 pipe、显式 region 与 plain malformed 共 153/153 在 0 upstream/0 active PG 下拒绝。2026-07-21 runner 改为 caller-owned PG/Redis 后合同 6/6 通过，合批 runtime runner contract 36/36 通过；覆盖 normalize/restart/scheduler/region headers、duplicate、delete/export/audit/cleanup；详见 F06 evidence 与 non-Docker runner evidence | 最终冻结 SHA 重跑；两 UI browser；多实例同时重复导入 auxiliary admission；旧 PG 非法行；L5/高并发 import |
| AUD-001 | 生产 evidence skill quick validation | 打包与脱敏有历史证据，quick validation 未关闭 | `H-only`：曾因 PyYAML 缺失未跑通 | 使用无 PyYAML 校验或明确依赖，当前代码重新 quick validate |

## 已验证但不能代表最终通过的当前基线

- 早期基线 `cargo test transcript_sanitizer -- --nocapture` 为 17/17；该数字对应修复前合同，已经被后续 thinking/history/fail-closed 测试扩展，不能继续称为当前最终门禁。最近聚焦命令、数量和 test-binary identity 见 thinking 专题与 protocol evidence，冻结候选仍需重跑。
- 早期全树基线曾为主 target 1199/1199、`kiro_loadtest` 26/26；并行修复后测试总数和结果已变化，最终只能使用冻结候选的重新执行结果。
- 当前 `git diff --check`、`cargo fmt --all -- --check`、`cargo check` 和 `cargo check --tests` 通过；尚未重跑冻结候选的所有静态/全量发布门禁。
- `target/release/kiro-rs` SHA-256 为 `4623cdf4e3f7bc0e2fe3defa4e0862237e1f64bccbb91671c134e0d6b51556d8`，与 2026-07-15 deep-audit 记录一致；该报告可作为当前候选的修复前动态基线。
- evidence skill 已增加无 PyYAML 的 quick validation 并完成本地三轮脱敏验证；最终冻结候选的发布总门禁仍需重跑。
- 修复前 17 个 sanitizer 测试曾要求保留 thinking，并允许任意 `*Hash<8hex>` 兜底；该历史合同已被精确 request tool mapping、thinking/signature/redacted 和 false-positive 反例替换。是否最终修复仍由真实 CLI/长历史/故障注入门禁决定，不能只看单元测试总数。
- 修复前全量测试曾主动断言 duplicate/orphan tool result 被 textify；当前实现和聚焦测试已经改为不复制原文。冻结候选仍需通过 converter/payload/CLI 组合回归，防止其他路径重新引入同类行为。
- 历史 deep-audit 的正常 CLI、Bash、Read、MCP、20 tool cycles、120k prompt、resume 和 bounded load 是回归基线，不是后续修复后的验收结果。

## 当前直接阻断项

1. PRO-001 至 PRO-007 任一未关闭都禁止发布。
2. SCH-001/SCH-002 未完成 Redis、local-first 和两实例复验禁止发布。
3. 任何错误场景的 upstream attempts 超过选定硬预算，禁止发布。
4. 任何 internal transcript、credential/pool/scheduler 私有词进入下游 text/thinking/error，禁止发布。
5. MIG-001、两 UI、Docker、C0-C4、L0-L5 中适用门禁缺证据，禁止发布。
6. Cleanup 过滤组虽 36/36 x3 外层通过，但完整套件、writer advisory-lock 性能、Redis chaos 和 UI browser 合同未验证，禁止发布。
7. OPS-003 的真实 Redis WRONGTYPE、基数上限、disconnect/recovery 与 scheduler 联合压力未完成，禁止发布。
