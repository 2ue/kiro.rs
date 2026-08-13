# Foxfishc Fork Learning Analysis

本文档记录当前仓库与 `~/Desktop/procode/kiro.rs-foxfishc` 的对比分析，目标是判断哪些设计值得学习、哪些不适合直接迁移，以及迁移时需要守住哪些边界。

分析时间：2026-05-23

分析对象：

1. 当前仓库：`/Users/yuanfeijie/Desktop/procode/kiro.rs`
   - branch：`main`
   - version：`v0.0.15`
   - commit：`3a7c99d`
2. 对比仓库：`/Users/yuanfeijie/Desktop/procode/kiro.rs-foxfishc`
   - branch：`master`
   - version：`v1.1.34`
   - commit：`cf86b1f`

## 结论

`kiro.rs-foxfishc` 值得学习，但不适合整仓 merge 到当前仓库。

foxfishc 的优势主要在账号池运营：

1. 凭据级限速。
2. 冷却与 Retry-After。
3. 新凭据雷暴防护。
4. 余额感知调度。
5. Overage 状态同步。
6. Admin 全局配置热更新。
7. 请求压缩和超大请求防护。
8. 更细的错误分类和日志脱敏。

当前仓库的优势主要在 Claude Code / Anthropic 兼容代理：

1. `/cc` high-cache writer 随机上报。
2. usage record。
3. 会话 sticky binding。
4. sticky-aware soft-failure fallback。
5. 模型过滤。
6. 凭据模型调用测试。
7. endpoint、compat profile、high-cache 配置。

因此推荐按功能点拆分迁移，不要用 foxfishc 覆盖当前实现。尤其不能影响当前 `/cc` writer 的边界：`/cc` writer 只影响对下游返回和 usage record 的 cache write 上报，不影响上游请求、不影响本地 reader 计算、不影响 prompt-cache tracker 更新。

## P0：最值得学习

### 1. Prompt-cache TTL 不应命中续命

foxfishc 的 `cache_tracker` 更贴近 Anthropic prompt cache 的 TTL 语义：TTL 从首次写入开始计算，命中不刷新 `expires_at`，再次 update 已存在 prefix 也不刷新 TTL。

参考实现：

1. `kiro.rs-foxfishc/src/anthropic/cache_tracker.rs:190`
2. `kiro.rs-foxfishc/src/anthropic/cache_tracker.rs:231`

当前仓库差异：

1. 当前 `PromptCacheTracker::compute` 在命中本地缓存时会刷新 `entry.expires_at`。
2. 当前 `PromptCacheTracker::update` 会 `entries.insert(...)` 覆盖已存在 entry，相当于重写时刷新 TTL。

当前实现位置：

1. `src/anthropic/prompt_cache.rs:224`
2. `src/anthropic/prompt_cache.rs:293`

价值：

1. 避免活跃会话在本地 cache 表中被无限续命。
2. 避免 `cache_read_input_tokens` 长期偏高。
3. 本地模拟更接近上游真实 prompt cache 行为。

风险：

1. 这会影响 reader 本地计算，属于行为变化。
2. 不能混进 `/cc` writer 3k 随机上报改动。
3. 如果下游已经依赖当前“活跃会话高 cache read”效果，切换后短期统计会下降。

建议：

1. 单独做一个 reader 口径修正版本。
2. 保留当前 `/cc` writer policy 不变。
3. 增加回归测试：命中后 TTL 不刷新、update 已存在 prefix 不刷新、过期后重新 creation。

### 2. 全凭据临时冷却时快速返回 429 + Retry-After

foxfishc 在所有启用凭据都只是临时冷却或限速，且最短等待超过阈值时，直接返回错误，由 handler 映射成 `429 Too Many Requests` 和 `Retry-After`。

参考实现：

1. `kiro.rs-foxfishc/src/kiro/token_manager.rs:1263`
2. `kiro.rs-foxfishc/src/kiro/token_manager.rs:1280`
3. `kiro.rs-foxfishc/src/anthropic/handlers.rs:493`

价值：

1. 请求不会在 HTTP handler 内无意义挂起。
2. 客户端能看到准确的 429 和等待时间。
3. 避免把“临时不可用”误报为“所有凭据禁用”。

当前仓库状态：

当前仓库已经修过临时排除导致的误报问题。`acquire_context_for_session` 在所有可用凭据都被本次请求临时排除时，会返回“本次请求临时排除了所有可用凭据”，而不是“所有凭据均已禁用”。

当前实现位置：

1. `src/kiro/token_manager.rs:1045`
2. `src/kiro/provider.rs:811`

建议：

1. 学习 foxfishc 的错误映射和 `Retry-After` 输出。
2. 不要直接照搬全部 cooldown 策略。
3. 429 默认仍应优先作为本轮 retry exclude，不应长期冻结唯一可用凭据。

### 3. 凭据级 RateLimiter

foxfishc 增加了独立 `RateLimiter`，支持每个凭据的最小请求间隔、每日上限、退避和原子 `try_acquire`。

价值：

1. 避免并发请求同时打到同一个凭据。
2. 减少上游 429。
3. 多账号池会更平滑，不容易让某个账号短时突刺。

风险：

1. 如果默认限速过强，会让只有一个启用账号的场景明显变慢。
2. 如果 `credentialRpm = 0` 没有严格表示禁用本地限速，会造成配置语义反直觉。
3. 接入不当会破坏当前 session sticky。

建议：

1. 在当前 `MultiTokenManager` 内新增轻量 limiter。
2. 只先做 `credentialRpm`、per-credential interval、atomic reservation。
3. `credentialRpm = 0` 必须表示完全禁用本地 limiter。
4. 绑定凭据被限速时，只做本次请求临时 fallback，不永久改绑。

### 4. 新凭据雷暴防护

foxfishc 的 README 明确说明：新凭据加入时如果 `recent_usage = 0`，balanced 逻辑会认为它最少使用，导致新账号短时间承接大量流量。

参考说明：

1. `kiro.rs-foxfishc/README.md:14`

价值：

1. 新账号不会刚加入就被瞬间打爆。
2. 降低新凭据触发 429 或风控的概率。
3. 对多账号池长期运行很有帮助。

建议：

1. 新增账号时，为其设置接近现有账号 usage 中位数的 baseline。
2. 不要替换当前 sticky binding。
3. 只增强 balanced 模式，不改变 priority 模式语义。

### 5. 凭据文件原子写

foxfishc 在回写 credentials 时使用临时文件加 rename，并处理 runtime-only 凭据。

价值：

1. 避免进程中断时写坏凭据文件。
2. 避免自动状态被错误永久化。
3. 对稳定性提升明显，且不影响请求路径。

当前仓库风险：

如果当前仓库直接使用 `std::fs::write` 回写 credentials，一旦写入过程中崩溃，可能导致凭据文件部分写入或损坏。

建议：

1. 优先迁移 atomic persist。
2. 保留当前 `DisabledReason` 语义。
3. 重新设计哪些 disabled 状态应该落盘：手动禁用可以落盘，自动失败禁用应谨慎落盘。

## P1：有价值，但要谨慎

### 1. 请求压缩和截断管道

foxfishc 在 Anthropic 请求转换为 Kiro 请求后、发送上游前执行压缩：

1. 空白压缩。
2. thinking 丢弃或截断。
3. tool_result 截断。
4. tool_use input 截断。
5. 历史截断。
6. 修复 tool_use / tool_result 配对。
7. 修复空 content。

参考实现：

1. `kiro.rs-foxfishc/src/anthropic/compressor.rs:41`
2. `kiro.rs-foxfishc/src/anthropic/compressor.rs:82`

价值：

1. 减少超大请求体导致的上游 400。
2. 减少 `Improperly formed request`。
3. 对长会话、工具调用、多轮历史场景有保护作用。

风险：

1. 会改变真实发给上游的 prompt 内容。
2. thinking 和工具结果截断可能影响模型回答质量。
3. 历史截断如果修复不完整，可能破坏工具调用配对。

建议：

1. 必须配置化。
2. 默认只启用低风险 whitespace compression。
3. history / thinking / tool truncation 默认关闭，或只在请求体超过阈值时触发。
4. 必须补测试：tool_use/tool_result 配对、空 content、tool-only 响应、超大 history、stream/non-stream 一致性。

### 2. 更细的错误分类和日志脱敏

foxfishc 在 handler 层对错误做了更细映射：

1. no credentials -> 503。
2. all cooling down -> 429 + Retry-After。
3. quota exhausted -> 429。
4. transient upstream -> 429 或 502。
5. oversized / improper request -> 400。

参考实现：

1. `kiro.rs-foxfishc/src/anthropic/handlers.rs:481`
2. `kiro.rs-foxfishc/src/anthropic/handlers.rs:493`
3. `kiro.rs-foxfishc/src/anthropic/handlers.rs:527`

价值：

1. 下游看到的 HTTP status 更准确。
2. 日志更容易定位问题。
3. 默认不输出请求体，降低敏感信息泄露风险。

当前仓库状态：

当前仓库已经补了凭据 label 日志和“后续 acquire 失败不覆盖之前真实上游错误”的逻辑。

当前实现位置：

1. `src/kiro/provider.rs:811`
2. `src/kiro/provider.rs:823`

建议：

1. 学习错误分类，不要打开完整请求体日志作为默认。
2. 错误日志里保留 credential id / label / endpoint / model / status。
3. sensitive logs 必须 feature-gate。

### 3. 余额缓存和余额感知调度

foxfishc 启动后会初始化余额，并在调度时结合 recent usage 和 balance。

价值：

1. 多账号池分配更均衡。
2. 低余额账号可以在手动或按需查询后被降权或禁用。
3. Admin 展示更接近真实状态。

风险：

1. 自动周期刷新本身会增加上游请求。
2. 启动时并发刷新过多也可能触发风控。
3. 不能混入“凭据测试”功能，因为当前测试目标只是验证模型调用。

建议：

1. 单独作为账号池调度增强。
2. 限并发、限频。
3. 不改变 priority 模式。
4. balanced 模式可从 `success_count` 升级为 `recent_usage + balance + round-robin`。

### 4. Admin 全局配置热更新

foxfishc Admin 支持全局配置热更新，例如 `credentialRpm`、prompt cache TTL、compression、global proxy、available models 等。

本轮迁移只学习其中低风险的账号池运行时配置：`credentialRpm`、临时冷却、低概率真实请求预热、compression。后台余额周期刷新会增加额外上游请求，按当前策略不迁移；prompt-cache TTL 属于 P0-1 reader 语义，按要求保持当前仓库实现，不作为运行时配置迁移。

价值：

1. 运维时不需要重启服务。
2. 可以在 Admin 中观察和调整账号池策略。
3. 对 limiter、compression 这类运行期策略很有帮助；cache TTL 这次不迁移，避免影响 reader 计算。

风险：

1. 当前仓库 Admin 已有 usage records 和 credential test，不能直接替换。
2. 全局配置热更新如果和运行时共享状态设计不好，容易出现配置文件和内存状态不一致。

建议：

1. 增量迁移 UI 交互，不替换当前 Admin。
2. 每个热更新字段都要有后端运行时状态、配置落盘、前端回显、测试覆盖。

## P2：暂不建议优先迁移

### 1. Overage / Web Portal 操作

foxfishc 支持从 Web Portal 读取 overage 状态、启停 overage，并通过 SSE 返回进度。

价值：

1. 对账号池运营很强。
2. Admin 能看到更完整的额度状态。

风险：

1. 依赖非公开 Web Portal 协议。
2. 依赖 CBOR shape、CSRF、Cookie 和页面结构。
3. 上游一变就可能失效。
4. 不应该成为核心 `/v1/messages` 或 `/cc/v1/messages` 请求路径依赖。

建议：

1. 暂不迁移到核心路径。
2. 如果后续做，只作为可选 Admin feature。
3. 失败时不能影响代理转发。

### 2. Fingerprint

foxfishc 有 fingerprint 模块，但从当前观察看，它更像预留或辅助结构，不是核心请求链路的硬依赖。

建议：

1. 暂不迁移。
2. 除非后续明确要把每凭据 UA、`x-amz-user-agent`、machine profile 接入 endpoint 请求，否则收益不明显。

### 3. Release / Docker workflow

foxfishc 的 release 和 Docker workflow 更完整，但当前仓库已经使用 `v0.0.x` 版本线和当前发布流程。

建议：

1. 可以参考 workflow 细节。
2. 不建议切换版本体系。
3. 不建议为了发布流程迁移整个项目结构。

## 和当前 `/cc` Writer 的边界

当前 `/cc` high-cache writer 逻辑来自路径级 usage 上报策略：

1. 只对 `/cc/v1/messages` 生效。
2. 只影响返回给下游的 usage 字段。
3. 只影响 usage record 中记录的 writer 上报值。
4. 不影响上游请求。
5. 不影响本地 prompt-cache reader 计算。
6. 不影响 prompt-cache tracker 更新。

当前配置字段：

1. `src/model/config.rs`
2. `reportedUsage.pathOverrides["/cc"].cacheCreation`

当前 writer policy：

1. `src/anthropic/cache.rs:34`
2. `ReportedCacheCreationPolicy`

当前 sampling 特性：

1. 最大正常值约为 `target * 1.1`。
2. 如果 target 为 3000 且 normalMaxMultiplier 为 1.2，正常范围约为 0 到 3600。
3. 分布不是固定 3000，也不是递增序列。
4. 值由请求 usage 和 seed 决定，看起来更自然。
5. 默认不出现极大值。
6. 正常情况下，在本身符合缓存的请求里，不应让 read 和 write 同时都为 0。

foxfishc 没有这套 `/cc` writer 随机上报设计。因此迁移 foxfishc 的 cache tracker、compression 或 handler 时，必须保护当前 writer policy。

## 不建议直接迁移的内容

### 1. 不要直接覆盖 Config

foxfishc 的 `Config` 与当前仓库字段差异很大。当前仓库有：

1. `auth_region`
2. `load_balancing_mode`
3. `compat_profile`
4. high-cache 配置
5. usage record 配置
6. endpoint map
7. proxy warning
8. `/cc` writer target

直接覆盖会导致当前功能倒退。

### 2. 不要替换当前 Sticky Binding

当前仓库按 conversation/session 绑定凭据，更适合 Claude Code 和 prompt-cache 场景。

foxfishc 的 affinity 更偏 `user_id -> credential_id`，逻辑较简单。如果直接替换，会丢失当前的：

1. `conversationId` 维度。
2. 软失败计数。
3. 模型过滤。
4. 绑定 TTL 和容量控制。
5. `/cc` high-cache 稳定会话收益。

建议把 limiter、cooldown、balance cache 接到当前 sticky 体系，而不是反过来替换 sticky。

### 3. 不要把 Compression 默认全打开

compression 会改真实 prompt。它适合作为超限保护，不适合作为默认语义变化。

### 4. 不要把 Overage/Web Portal 绑进核心请求链路

Web Portal 依赖面不稳定，适合作为 Admin 可选能力，不适合作为核心代理的前置条件。

## 推荐迁移路线

### 阶段 1：稳定性小改

目标：

1. credentials 原子写。
2. 自动禁用状态是否落盘重新梳理。
3. 不改请求路径。
4. 不改 reader。
5. 不改 `/cc` writer。

验证：

1. `cargo test`
2. 凭据增删改落盘测试。
3. 手动禁用可持久化。
4. 自动失败禁用不会造成无法自愈。

### 阶段 2：轻量凭据级限速

目标：

1. 新增 `credentialRpm` 配置。
2. `0` 表示禁用本地限速。
3. 每凭据原子 `try_acquire`。
4. 接入当前 `acquire_context_for_session`。
5. sticky 被限速时只临时 fallback。

验证：

1. 单账号启用时不会被错误排除。
2. 多账号并发时分布更平滑。
3. `credentialRpm = 0` 不限速。
4. 429 仍不长期冻结唯一可用凭据。

### 阶段 3：新凭据雷暴防护

目标：

1. 新账号加入时使用 existing usage 中位数 baseline。
2. balanced 模式减少新账号突刺。
3. priority 模式不变。

验证：

1. 新账号不会在高并发下承接全部请求。
2. 老账号仍按原有策略参与调度。
3. sticky 会话不被破坏。

### 阶段 4：Prompt-cache TTL 语义修正

目标：

1. 命中不刷新 `expires_at`。
2. update 已存在 prefix 不刷新 TTL。
3. 保留 `/cc` writer policy。

验证：

1. 命中后 TTL 不变。
2. TTL 到期后重新 creation。
3. 同会话 reader 数字不会无限偏高。
4. `/cc` writer 仍只影响上报。

### 阶段 5：请求压缩

目标：

1. 配置化引入 compression。
2. 默认保守。
3. 先做 whitespace compression。
4. 高风险截断只在超限时启用。

验证：

1. 超大请求不会直接 400。
2. tool_use/tool_result 配对完整。
3. 空 content 不出现。
4. stream/non-stream 行为一致。

## 总结

foxfishc 的高价值部分是账号池保护和运维能力，不是 `/cc` high-cache writer。当前仓库应学习它的稳定性和调度保护思路，但必须保持当前主线能力：

1. `/cc` writer 只影响下游上报。
2. reader 和 tracker 不被 writer 改动影响。
3. 会话 sticky 不被替换。
4. Admin 现有 usage record 和 credential test 不被覆盖。
5. 429 不应导致唯一可用凭据被长期冻结。

建议按小版本逐项迁移，先做低风险稳定性，再做账号池限速和新凭据保护，最后才考虑 reader TTL 修正和请求压缩。
