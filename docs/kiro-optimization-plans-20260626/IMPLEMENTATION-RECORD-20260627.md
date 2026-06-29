# Kiro 优化计划实现记录（2026-06-27）

本文记录 `docs/kiro-optimization-plans-20260626/` 中方案与当前代码事实的对照结果，以及本次已实施的改造。它不是测试报告；Claude Code CLI、压测、异常恢复、内存和调度压力测试结果另见 `docs/testing/claude-code-cli-full-regression-20260628.md`。

## 总体原则

- 默认行为保持兼容：新能力默认关闭或只在显式配置后生效。
- 不改变 `/cc`、`/ha`、`/na`、`/dfcache/*` 已有路由语义。
- 不改变下游 usage 的核心设计：继续输出官方兼容 usage 字段，并按当前系统配置整流本地 prompt cache 估算结果。
- 不默认启用 full response cache，不缓存完整响应文本。
- 只记录内部 diagnostics，不把 cachePoint、外部池、凭据、调度内部细节暴露给下游。

## 计划对照

| 计划 | 代码事实状态 | 本次处理 |
| --- | --- | --- |
| 01 token manager module split | `src/kiro/token_manager/` 已经是目录模块，含 `manager.rs`、`strategy.rs`、`capacity.rs`、`rpm.rs`、`queue.rs` 等拆分。 | 不重复实施。后续如继续拆 `manager.rs` 内复杂函数，需单独行为等价测试。 |
| 02 selection failure reasons | 已有结构化 selection failure、usage/metadata 记录、error id，且本批已补 `selection_failure_sample_limit` 和 `selection_failure_record_enabled` 配置链路。 | 保留并验证。 |
| 03 scheduler strategies health score | 已有 `health_balanced` 权重体系，本批新增 `weighted_least_inflight` opt-in 策略和管理端选项。默认策略不变。 | 已实施。 |
| 04 loadtest and chaos harness | `src/bin/kiro_loadtest.rs` 已存在 fake server、真实上游双开关、stream/异常/恢复场景和资源采样。 | 不重复造工具；后续测试直接使用。 |
| 05 tool-use malformed regression | `src/anthropic/payload_guard.rs` 已有工具配对修复、重复/孤立 tool_use/tool_result 诊断；`src/anthropic/converter.rs` 已有 schema 归一化。 | 保留并增加 cachePoint 序列化回归。 |
| 06 stream idle and upstream exception | 已有 stream idle timeout、200 JSON exception sniff、错误归一化和 usage 记录。 | 后续真实 CLI/压测验证，不在本批重写。 |
| 07 profileArn and region self-heal | `src/kiro/provider.rs` 已有 `fetch_enterprise_profile_arn_for_context`、`ensure_profile_arn_for_context`、ListAvailableProfiles 和写回逻辑。 | 不重复实施。 |
| 08 cachePoint and cache normalization | prompt cache 边界、volatile id normalization 已有部分实现；真实 Kiro cachePoint 计划此前未完成。 | 本批补齐 cachePoint 默认关闭试验能力、最终序列化注入、上游拒绝后单次无 cachePoint fallback。 |
| 09 endpoint failover policy | 当前没有经过验证的等价 endpoint candidate；盲目实现默认关闭配置会制造“看起来可用但不可验证”的能力。 | 暂不实施。只有拿到真实等价候选 endpoint 并验证协议一致后才值得做。 |
| 10 observability trace and error normalization | request id、error id、usage diagnostics、payload report、provider error metadata 已存在。 | cachePoint retry 会写入 payload diagnostics；不新增同步写库。 |
| 11 admin account UX and cache bounds | UI 已有部分重构，配置页已有 prompt cache 边界字段；本批只补 cachePoint 开关。 | 大规模 UI 继续作为独立阶段，不混入后端热路径改造。 |
| 12 sequence and release gates | 阶段原则仍有效：默认关闭、先测后发、真实上游需要双开关。 | 本批遵守，未提交未发版。 |

## 本次实现内容

### 1. Prompt cache 边界与调度诊断

代码事实：

- `src/model/config.rs` 增加或保留：
  - `selection_failure_sample_limit`
  - `selection_failure_record_enabled`
  - `prompt_cache_max_entries_per_account`
  - `prompt_cache_max_entries_global`
  - `prompt_cache_entry_ttl_secs`
  - `prompt_cache_estimated_bytes_limit`
- `src/anthropic/prompt_cache.rs` 使用 `PromptCacheBounds`，按 per-account、global、TTL 和估算字节限制做淘汰。
- `src/admin/types.rs`、`src/admin/service.rs`、UI runtime config 已接入上述字段。

风险控制：

- 默认值保守，旧配置缺字段时继续使用默认值。
- prompt cache 仍只记录指纹和 usage 估算，不缓存完整响应文本。

### 2. `weighted_least_inflight` 调度策略

代码事实：

- `src/kiro/token_manager/strategy.rs` 新增 `select_weighted_least_inflight`。
- `src/kiro/token_manager/manager.rs` 在 `load_balancing_mode == "weighted_least_inflight"` 时启用。
- Admin API 和 UI 允许选择该模式。

行为边界：

- 默认 `load_balancing_mode` 不变。
- 新策略只作为 opt-in，高并发下用于降低已经繁忙账号继续被选中的概率。

### 3. 真实 Kiro cachePoint 试验能力

代码事实：

- `src/model/config.rs` 新增：
  - `kiro_cache_point_enabled`：默认 `false`。
  - `kiro_cache_point_tools_only`：默认 `true`。
  - `kiro_cache_point_record_plan`：默认 `true`。
- `src/anthropic/converter.rs`：
  - `ConverterOptions` 接收 cachePoint 配置。
  - 只对实际发送给 Kiro 的工具生成插入计划。
  - 被 `tool_choice` 过滤掉的工具不会生成插入计划。
  - 去重跳过的工具不会生成插入计划。
  - 占位工具不会生成插入计划。
- `src/kiro/model/requests/kiro.rs`：
  - `tool_cache_point_insert_after` 是运行期字段，`serde(skip)`，不会直接序列化。
  - `cache_point_plan_recording_enabled` 控制是否记录 plan。
- `src/anthropic/payload_guard.rs`：
  - 普通请求无计划时仍走原来的 `serde_json::to_string` 热路径。
  - 只有存在插入计划时才转成 JSON value，并向最终 body 的 `tools` 数组插入 `{"cachePoint":{"type":"default"}}`。
  - payload report 记录 planned/inserted 数量。
- `src/anthropic/handlers.rs`：
  - 如果带 cachePoint 的请求被上游以 body invalid、tool format 或 400 bad request 拒绝，自动清空本次请求的 cachePoint 计划重试一次。
  - retry 原因写入内部 payload diagnostics，不返回给下游。

行为边界：

- 默认关闭，不改变线上请求体。
- 开启后只处理 tool-level `cache_control`，不改写 system message、history 或用户内容。
- cachePoint fallback 只多一次同账号/同 provider 调用，不开启额外队列，不改变外部池对下游的统一错误口径。

### 4. 管理端配置接入

代码事实：

- `RuntimeConfigResponse` / `UpdateRuntimeConfigRequest` 增加 cachePoint 配置字段。
- UI `RuntimeConfig` 类型和默认值增加 cachePoint 字段。
- 配置页“兼容与统计”区域增加：
  - 发送真实 cachePoint。
  - 只处理工具缓存标记。
  - 记录 cachePoint 计划。

文案边界：

- 页面只说明配置作用，不展示内部调度逻辑。
- 不新增原生 select/confirm 等组件。

### 5. Redis 并发 lease 释放竞态修复

代码事实：

- `src/kiro/token_manager/concurrency.rs` 增加本地 released lease tombstone：按 `(credential_id, lease_id)` 记录短 TTL 已释放 lease。
- `InFlightLeaseGuard::release()` 在本地释放前记录 tombstone，再异步释放 Redis lease。
- `src/kiro/token_manager/manager.rs` 在 Redis 调度状态 apply 前过滤近期已释放 lease，覆盖全量同步、按 id 同步和强制 apply。
- tombstone 有 TTL、定期裁剪和硬上限，避免高并发下无界增长。

修复原因：

- Redis release 是异步 best-effort。高并发下后台 Redis 状态同步可能先读到旧 lease，再把它重新导入本地 in-flight。
- 被重新导入的 lease 没有本地 guard，后续只能等 `credentialInFlightLeaseMaxSecs` 清理，期间会错误占用账号并发槽。

验证结果：

- `scheduler_state_apply` 相关测试通过，其中包括“近期释放的 Redis lease 不应重新导入本地”的回归测试。
- 隔离 c16/c32/c64 正常压测均 `100% success`。
- c128 在 `credentialDispatchMaxWaitSecs=3` 下出现队列等待上限导致的 429；把等待时间调到 `10` 后 c128/r256 `256/256 success`。
- 强杀代理后立即恢复 c16/c64 均 `100% success`；旧 lease 只出现预期的超时自愈清理，没有持续泄漏。

### 6. 运行配置冷却上限保存兼容

代码事实：

- 运行时冷却计算中，`credentialMaxCooldownSecs` 是最终上限；基础冷却或连续退避超过上限时会被 clamp。
- 管理接口此前错误要求所有基础冷却都不能超过最大冷却，导致旧配置原样保存失败。
- 本批调整 `src/admin/service.rs` 校验：冷却秒数必须大于 `0`，但允许基础冷却大于最大冷却上限。
- UI 同步移除错误前端拦截，并把说明改为“连续出错时最多暂停多久”。

验证结果：

- 单测覆盖基础冷却大于上限的合法场景，以及 `0` 值非法场景。
- 隔离管理接口实测旧配置组合可 PUT 保存。
- 保存修复后 c16/c64 正常流式回归均 `100% success`。

### 7. Claude Code CLI thinking 输出边界

代码事实：

- `src/anthropic/handlers.rs` 会识别 `*-thinking` 模型名，并在调用方没有显式 `thinking` 字段时注入默认 thinking 配置。
- 当前 Kiro 上游不接受 `claude-sonnet-4-6-thinking` 作为真实 `modelId`，实测返回 `INVALID_MODEL_ID`。
- 因此 `*-thinking` 请求仍映射到 Kiro 基础模型，但 `src/anthropic/converter.rs` 会在兼容模式下把 thinking 控制写入上游历史提示。
- 本批收窄了可见 thinking 输出策略：
  - 普通 `thinking.type=adaptive` 只作为 Claude Code 兼容控制，不强制输出可见 `<thinking>`。
  - 显式 `*-thinking` 模型名会附加 `thinking_output_policy`，要求工具调用前也先输出并闭合 `<thinking>` 块。
  - 显式 `thinking.type=enabled` 同样附加 `thinking_output_policy`。
  - 显式 `thinking.type=disabled` 不注入 thinking 控制。

修复原因：

- Claude Code CLI 2.1.156 对普通 `sonnet` 请求也会发送 `thinking: {type: adaptive}` 和默认 `output_config.effort=high`。
- 如果对所有 `adaptive` 都强制可见 thinking，会让普通 `sonnet` 请求额外输出 `thinking_delta` 和 thinking tokens，扩大行为和成本影响。
- 用户显式选择 `sonnet-thinking` 时，期望可以看到思考输出；旧链路在工具调用轮可能直接进入 `tool_use`，没有稳定产出 `thinking_delta`。

验证结果：

- 一次性捕获端口验证 CLI 请求体：`think`、`think hard`、`ultrathink` 不改变模型名、不改变 effort；`--effort` 才改变 `output_config.effort`；`--model sonnet-thinking` 才改变模型名。
- 9022 direct API 验证：普通 `sonnet`、`sonnet + thinking.adaptive`、`sonnet-thinking + thinking.disabled` 均无 `thinking_delta`。
- 9022 direct API 验证：`sonnet-thinking`、`sonnet + thinking.enabled` 均有 `thinking_delta` 和 `thinking_tokens`。
- 真实 Claude Code CLI 验证：普通 `sonnet`、prompt 中包含 `think`/`ultrathink`、`--effort low/max` 均无 `thinking_delta`；`--model sonnet-thinking` 有 `thinking_delta` 和 `thinking_tokens`。
- 真实 Claude Code CLI 验证：`sonnet-thinking + Bash tool-use` 已能在工具调用前输出 thinking 块，再进入 `tool_use`。

## 暂不实施项

### Endpoint failover

不实施原因：

- 方案明确要求“只有在测试证明上游 endpoint 存在可替代地址且协议一致时才能启用”。
- 当前代码和本地配置没有事实上的等价 candidate endpoint。
- 仅增加一组默认关闭配置不会提升功能价值，反而会让管理端出现无法验证的开关。

后续触发条件：

- 拿到至少一个可验证的 Kiro 等价 endpoint。
- fake server 和真实上游均验证 headers、profileArn、model、stream/non-stream body 格式一致。
- 确认不会对已开始输出的 stream 做 failover。

## 已完成验证

本轮已完成以下验证，详细命令、报告路径和结论见 `docs/testing/claude-code-cli-full-regression-20260628.md`。

- `cargo fmt`
- `cargo test --locked --no-default-features`
- `cargo test --locked --no-default-features --bin kiro_loadtest`
- `cargo build --locked --no-default-features --bin kiro-rs --bin kiro_loadtest`
- `pnpm --dir ui check`
- `pnpm --dir ui build`
- `kiro_loadtest` 隔离代理压测：普通突发并发、thinking、显式 thinking、RPM、dfcache、异常矩阵、异常恢复、client drop、tool-use stream。
- Claude Code CLI 当前代码验证：`sonnet-thinking`、完整 thinking 模型名、`--effort high`、Bash tool-use、MCP tool-use 成功与错误回传。
- cachePoint 当前代码验证：默认运行时开启状态下插入 `cachePoint`；上游拒绝后自动去掉 cachePoint 并重试一次。

测试边界：

- 本轮没有使用真实 Kiro 上游账号做大并发压测，避免影响真实账号和现网状态。
- 本轮真实 Claude Code CLI 验证使用真实 CLI 客户端，但上游是隔离 fake Kiro server；它能验证协议、stream、thinking、tool-use、MCP、usage 字段，不等价于真实模型智商、图片识别或大文档理解质量验证。
- 旧的真实 Claude Code CLI agent/MCP 产物只能作为历史基线，未作为本轮最终验收依据。
