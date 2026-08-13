# 修复、验证与发布总体计划

Date: 2026-07-18

Role: 本轮实现顺序、依赖、回滚和发布门禁权威

Status: `execute-ready`；P0 事实、修复边界和验收合同已明确，正在先补失败测试再分批实现

## 原则

1. 先修结构化数据的所有权和状态机，再保留 sanitizer 作为历史兼容保护；不能用字符串删除掩盖 converter/payload 的结构破坏。
2. operator prompt 与协议转换分离；任何 UI 文案开关不得改变 `tool_choice` 等结构化协议语义。
3. 每个下游请求只有一个共享 attempt budget；credential、stream、payload、external 和 rescue 不得各自重置预算。
4. Redis degraded 是协调系统故障，不等于账号容量不足；fallback、fail-closed、恢复和观测必须显式。
5. 正常 body 语义与性能是第一等门禁。clean raw 必须保持 byte identity；normalized 路径必须有字段保留合同。
6. 验证构建产物必须有 owner 和生命周期；无论单支线、多支线、串行或并行，报告落盘后都必须删除本批 build target。并发只影响峰值，不是本次磁盘耗尽的根因；根因是跨批未清理。最终只保留冻结候选二进制和小型脱敏证据。
7. 任何运行时 runner 都必须接收显式冻结 binary，不能从默认 target 猜测或触发重建；发布前只读 inventory 不完整、发现未知 target 或发现 target 引用进程时一律 NO-GO。

## Phase 0：事实与失败测试

- 将 `current-fact-matrix` 每个 `S-current/H-only/N` 项转成可执行 fixture 或明确环境门禁。
- 为 PRO-001 至 PRO-007 添加修复前失败测试，覆盖 thinking、false positive、orphan 原文、active pair trim、200 exception、malformed terminal 和 tool_choice master OFF。
- 固化 deep-audit 脱敏 transcript fixture，不允许用环境变量缺失而静默跳过核心回归。
- 建立 `feature/evidence/<run-id>/` 清单，原始大文件继续放忽略目录，只提交摘要和哈希。
- 所有 Rust 验证批次执行磁盘 preflight，并通过 scoped target wrapper 保证成功、失败、中断后自动清理；每批以 `removed=true`、目标路径不存在和全局残留扫描为零作为独立退出条件。
- 并发构建通过 Git common dir reservation 原子准入，默认每批 12 GiB、保留 20 GiB floor；每批结束立即释放，不以“所有支线结束”为清理时点。
- 将仍回退 `target/debug/kiro-rs` 或固定写根 `target/<reports>` 的 runtime runner 改为冻结绝对 binary 必填、报告目录显式注入、runner 退出后清理。完成前相关 F/E gate 保持 open。

Exit：P0 每个缺陷至少一个稳定失败测试，且 clean-body/performance baseline 已记录。

## Phase 1：协议与 payload 根因修复

- 以逻辑 turn 为单位规范化和裁剪 tool history；默认不把 malformed tool output 原文转普通 text。
- 定义 thinking 策略：无签名可由同一增量状态机安全过滤；signed/redacted 内容不可局部改写，命中污染时整块抑制或返回规范错误。
- 在完整 CLI 门禁前完成请求侧 thinking 能力审计：对账 Claude CLI 原始 `output_config.effort`/`thinking`、所有相关开关与 alias、converter/endpoint 映射和最终 Kiro wire body；`max` clamp 或 adaptive drop 只有在 fake capture、公开协议与受控真实调用证据齐全后才能定性和修复。
- 将 sanitizer policy 放入 request scope，覆盖 local/external、raw/normalized、stream/non-stream、direct/fallback/retry。
- 用真实 SSE event parser 处理 CRLF、多 data line、start content、EOF/error；缺 terminal 不能合成成功。
- 收敛 WebSearch/MCP 平行路径：canonical capability 识别、last-user query、shared attempt/usage context、stream/non-stream renderer、typed failure、limited body 和无原文 tracing。
- 明确 payload hard-limit 行为，修复图片 decoded-byte 和重复 placeholder，限制 serialize 迭代。
- 远程 image/document 在任何 HTTP 前执行 source-count admission；所有源共享累计下载/base64/attempt/deadline，transport resolver 绑定已校验地址且禁用透明 proxy，进程级工作槽限制突发辅助 I/O。

Exit：A/B/C 全部通过；clean body 性能预算通过；PRO-001 至 PRO-005 无阻断项。

## Phase 2：重试、准入与调度

- 引入 request-scoped attempt budget 和分层 reason/action 账本；默认上限为小常数，与账号数无关。
- 公共 upstream header/body timeout 使用 monotonic deadline-first 语义；当前 10 轮顺序测试、真实 stall、provider 6 类 transport/body 全矩阵和完整单测树已通过，冻结 CPU/IO/L3-L5 仍需证明迟到响应不会穿越 deadline 且无正常路径性能回退。
- 400 model-unavailable 只对精确可重试分类换号，并受共享 budget；invalid model/body 不换号。
- 增加请求 API Key 的并发/RPM/队列准入与 usage channel attribution；模型目录、OAuth refresh 等 auxiliary RPM 分开。
- external fallback 前重新读取 local route state；Ready 时不得因先前 transient error 直接外部。
- external 静态 eligibility 已确认且 capacity fallback 开启时，preflight/acquire 竞态不得因 runtime hint absent 进入本地默认 120 秒队列；真正 available/full/cooling/coordinator 状态由 external selection/acquire 权威判断。
- external 权威 pool list 必须按 process generation 有界 singleflight，不能在 c128 首波制造 128 次 PG list 或在 32 hard cap 后误报无容量；完整 row 只用短 fresh cache，失败短负缓存且不 stale-success。请求完成 URL/header/body/model 准备后，以无 TTL、同 revision 仅合并 in-flight query 的 fence 定义 dispatch linearization point，再占用 attempt/send；坏持久化 row 按池 strict fail closed。
- 拆分 Redis breaker 作用域，避免 sticky read/delete 等非容量操作令整池 fail-closed；完成两实例 lease/recovery。
- 拆分 PgSQL runtime mutation backlog 与 dispatch quarantine；普通 success/API failure/refresh failure 和自动健康 Patch 待重放不得把健康池逐账号变成 `local_all_disabled`，显式 Disable/调度状态 Patch 继续 fail closed。用 40x15、60 RPM、global 500 的五轮 fixture 区分并发饱和、RPM、排队和真正禁用。
- finite local/external queue lease 由 request-scoped 最大等待一次性覆盖，不再按 waiter 每 20 秒续租；unlimited wait 保留 renewal，runtime wait 配置只影响新请求。用 500 guard、真实 Redis 跨 renewal 点和动态 config deadline 证明减少内部 Redis RPM 且不泄漏 queue slot。
- 将 Redis usage snapshot、aggregate、seen 合并为单提交单元，限制 exact cache-read 基数，消除 64-future waiter fanout；用真实 Redis WRONGTYPE/断连/延迟与 scheduler 联合压力证明 1 RTT 正确性、恢复和性能。

Exit：D05/E01-E06 通过；错误期 amplification 不超过硬预算；错误后恢复 100%。

## Phase 3：配置、运维、UI 与升级

- 拆分 prompt operator policy 与 protocol conversion policy；迁移旧具体 hash task prompt。
- 两 UI 使用独立字段权威，统一费用 formatter 和风险提示。
- 关闭同步 usage 全清路径，使用 bounded background cleanup 和 Redis `UNLINK`/小批次。
- 完成 AWS API key + region E2E、evidence quick validation、v101/v102/v103 升级 fixture。
- 对首次引入 usage per-ID/lifecycle lock 的版本执行 non-rolling 切换：先关闭 admission 并排空所有旧 writer，再停止旧实例、执行离线维护/升级并统一启动新实例；禁止 mixed-version writer。

Exit：F03-F06 与两 UI build/browser gate 通过。

## Phase 4：完整回归与发布

- 运行 C0-C4、L0-L5、两套 UI、敏感信息和清理门禁。按用户 2026-07-17 明确要求不执行本地 Docker 动态验证；Docker-backed 场景只交付已编译、环境缺失时 fail-closed 的开发验证程序，并在最终报告中列为显式豁免而不是 pass。
- 冻结前执行 `run-cargo-scoped.sh --reap-stale`，再执行 `node feature/tests/inventory-build-artifacts.mjs --gate`；检查 scoped/unknown/unmanaged target、reservation、target process、扫描完整性和 Docker 只读容量提示。
- inventory 必须为 pass、构建缓存没有跨批线性增长、磁盘剩余空间满足后续冻结候选和发布预算。不得用手工删除未知目录来制造绿色结果。
- C0 构建完成后只复制 release binary 到独立 candidate 目录并记录 SHA-256；C1-C4/L1-L5、升级和 UI 全部绑定该绝对路径，任何 runner 自动发现/重建都判 gate 失败。Docker-backed 开发验证程序只要求编译和 fail-closed，不启动本地 Docker。
- 真实 Claude CLI 使用隔离配置和临时 release 服务；真实上游只做低并发小样本。
- 只读复核生产同类错误 recurrence 和配置迁移需要；不在取证流程修改生产。
- 更新所有专题的修复后结果、残余风险、报告哈希和回滚点。
- fetch 远端分支/tag 后重新计算 patch 版本；工作提交与版本提交分离；先推分支，再推 annotated tag。

Exit：所有适用矩阵为 pass，无 skipped/unknown，发布记录完整。

## 回滚

- 每个 Phase 独立提交，避免把协议、调度、UI 和版本元数据混在一个提交。
- 新策略字段提供旧配置的显式迁移和可逆默认；不得通过“关闭所有增强”回滚结构化协议行为。
- 发布回滚点是上一远端 tag；数据库/Redis 迁移必须向前兼容旧二进制或提供书面不可回滚说明。
- 本版 usage writer lock 协议不支持旧/新并行回滚；如需回滚，先停止并排空全部新实例，再统一启动旧版本，不能滚动混跑。

## 当前禁止事项

- 不声明“以后不会再泄漏”；只能在矩阵全部通过后声明已覆盖的故障模型。
- 不把 `Hashxxxxxxxx` 作为唯一 matcher 或唯一验收关键词。
- 不用单次 happy path、单元测试总数或历史报告代替当前构建多轮验证。
- 不触碰现有 `9022`；不读取或暂存 `kiro_idc_users*.txt`。
- 不把 Cargo `deps/build/incremental` 当作测试证据长期保留；不允许没有 owner、清理动作和空间复核的验证构建目录。
- 不把“限制并发”写成磁盘事故根因；串行不清理同样会耗尽磁盘。也不在 inventory 中自动 prune Docker、删除 unknown target 或终止非本批 owner 进程。
