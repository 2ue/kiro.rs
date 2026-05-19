# High-Cache 连续性与运行时配置提案

本文档基于 2026-05 重构后的代码结构，描述 high-cache 模拟在连续请求中出现 cache read/write 断层的原因、最终实施方案、边界约束、双 UI 影响和验证计划。本文档是当前实现依据；旧文档中的缓存连续性描述若与本文冲突，以本文为准。

## 目标

1. 不改变上游真实 metadata cache 的优先级。
2. 不改变 `local-prompt-cache` 的严格语义。
3. 只在 `promptCacheSimulationMode = "high-cache"` 时修复连续请求中间 cache 断层。
4. 第一次小请求不能凭空出现大缓存。
5. 连续同 scope 请求已有缓存后，小请求可以继承已有 cache read。
6. `promptCacheMaxSimulatedInputTokens` 仍通过 scale 和 soft-cap jitter 间接生效，不能每次固定贴顶。
7. `/console/settings` 保存 prompt-cache 配置后应在运行中的 Anthropic 请求路径立即生效。
8. `/admin` 旧 UI 和 `/console` 新 UI 都必须继续可用。

## 最新代码事实

重构后存在两个 UI：

1. `/admin`：旧版 `admin-ui/dist`。
2. `/console`：新版 `frontend/dist`，包含 `dashboard`、`credentials`、`usage`、`pricing`、`settings` 页面。

重构后新增了 `app_config`、`storage`、`pricing` 和持久化 usage 记录，但 Anthropic 请求路径中的 prompt-cache 参数仍由启动时的静态 `config` 复制到 `AppState`。因此，当前 `/console/settings` 写入 `app_config` 后，prompt-cache 相关参数不会稳定影响正在运行的 `/v1/messages` 和 `/cc/v1/messages`。

## 问题现象

在连续请求中，前面请求已经创建较大的 simulated cache 后，中间某些输入较小的请求仍可能出现 cache read/write 都为 0，或 cache read 只有几十 k。

这不是主要由 TTL 造成。默认 cache TTL 为 5 分钟，显式 1h TTL 会走 1 小时路径。断层主要来自当前请求 token 数过小导致的提前短路。

## 根因

当前 `PromptCacheTracker::compute(...)` 的顺序是：

1. 根据当前请求的 `profile.total_input_tokens` 和 ratio 计算 `target_tokens`。
2. 如果 `target_tokens <= 0`，直接返回空 usage。
3. 之后才会查找同 scope 已存在的 cache entry。

所以当某一轮请求本身很小，即使同一个 `credential_id + conversation_id + model` 已有未过期缓存，也不会进入读取逻辑。

此外，`CacheSimulation::to_usage(...)` 当前只以当前请求的 `total_input_tokens` 为基础做放大。如果当前请求很小，最终 cache read 会被当前小 total 限制住，导致看起来只有几 k 或几十 k。

## 最终方案

### 1. 增加 high-cache 专用计算

保留现有 `compute(...)` 给 `local-prompt-cache` 使用，不改变其严格行为。

新增 high-cache 专用返回结构：

```rust
pub struct PromptCacheComputation {
    pub usage: PromptCacheUsage,
    pub simulated_total_input_floor_tokens: Option<i32>,
}
```

新增 `PromptCacheTracker::compute_high_cache(...)`：

1. 正常大请求仍复用现有 target-token 逻辑。
2. 当前请求 `target_tokens > 0` 且能命中当前 profile fingerprint 时，与现有 `compute(...)` 行为一致。
3. 当前请求 `target_tokens <= 0` 时，不直接返回。
4. 当前请求 `target_tokens > 0` 但没有命中当前 profile 的 fingerprint，且同 scope 已有更大的未过期 entry 时，也允许 continuity fallback。
5. 仅当同 scope 存在未过期 cache entry 时，允许 continuity fallback。
6. fallback 只产生 cache read，不产生 cache creation。
7. fallback 会刷新被读取 entry 的 TTL。
8. fallback 返回 `simulated_total_input_floor_tokens`，用于后续 usage 组装避免被当前小请求压小。
9. 如果无 scope、无 profile、无历史 entry 或 entry 过期，仍返回空 usage。

### 2. 增加 simulated total input floor

在 `CacheSimulation` 中增加：

```rust
pub simulated_total_input_floor_tokens: Option<i32>
```

`to_usage(...)` 先取：

```rust
base_total = max(current_total_input_tokens, floor.unwrap_or(0))
```

然后继续走 `CacheAmplification::apply(...)`。这样不会直接使用 `promptCacheMaxSimulatedInputTokens`，也不会每次固定输出 300k；触顶仍由 deterministic soft-cap jitter 产生 k 级别波动。

### 3. handlers 按模式分支

`prepare_credential_usage_context(...)` 中改为：

1. `Disabled`：不模拟。
2. `LocalPromptCache`：继续调用 `compute(...)`。
3. `HighCache`：调用 `compute_high_cache(...)`，并把 floor 传给 `CacheSimulation`。

成功请求后才调用 `prompt_cache.update(...)`。失败请求仍只记录失败 usage，不创建新 entry。

### 4. 增加 prompt-cache 运行时配置

新增共享运行时配置对象：

```rust
PromptCacheRuntimeConfig
PromptCacheRuntimeConfigSnapshot
```

包含：

1. `prompt_cache_simulation_mode`
2. `prompt_cache_target_read_ratio`
3. `prompt_cache_token_scale`
4. `prompt_cache_max_simulated_input_tokens`
5. `prompt_cache_cap_jitter_min_tokens`
6. `prompt_cache_cap_jitter_max_tokens`
7. `prompt_cache_scale_min_input_tokens`
8. `high_cache_threshold`

启动时：

1. 先从 `config` 构建默认 snapshot。
2. 再用 `app_config` 覆盖已存在的运行时值。
3. 把同一个 `Arc<PromptCacheRuntimeConfig>` 注入 Anthropic `AppState` 和 Admin 运行时。

`PUT /api/admin/config` 成功写入后：

1. 对 prompt-cache keys 调用 runtime config reload。
2. `high_cache_threshold` 同步影响 usage summary。
3. 继续保持 quota 和 load balancing 的现有热更新逻辑。

### 5. 双 UI 处理

后端缓存连续性修复本身不需要新增 UI API。

`/console`：

1. `settings` 页面保留现有字段。
2. 文案改为：缓存、配额、调度等运行时项保存后热生效；静态启动项仍需重启。
3. `usage` 页面后续可增加 100k/200k cache read/write 桶统计，但本次不是连续性修复的前置条件。

`/admin`：

1. 旧 UI 保持现有 usage 面板可用。
2. 不新增旧 UI 设置页，避免重复维护。
3. 如果后端 API 增加字段，不破坏旧 TS 类型；如果改变字段名或枚举，必须同步旧 UI 类型。

## 风险边界

1. 真实 metadata 非零 cache 仍是最高优先级。
2. high-cache 只填补 metadata cache 全 0 或 metadata 缺失的场景。
3. 第一次小请求不会生成 cache，因为 fallback 需要同 scope 已有未过期 entry。
4. scope 不放宽，仍必须是 `credential_id + conversation_id + model`。
5. 失败请求不写 cache entry。
6. `local-prompt-cache` 不使用 continuity fallback。
7. floor 不是 cap；最终 total 仍受 scale、门槛和 soft-cap jitter 控制。
8. app_config 中非 prompt-cache 的配置并非全部在本次热更新范围内，不能借本次修改扩大行为面。

## 测试计划

### 单元测试

1. high-cache 首次大请求产生 creation。
2. high-cache 第二次同 scope 请求产生 read。
3. high-cache 已有缓存后，低于最小 cacheable token 的小请求通过 continuity fallback 产生 read。
4. high-cache 已有缓存后，高于最小 cacheable token 但明显小于已有 entry 且未命中 fingerprint 的请求也通过 continuity fallback 产生 read。
5. high-cache 首次小请求仍无 cache。
6. local-prompt-cache 小请求仍不使用 continuity fallback。
7. 不同 credential、conversation、model 不串 cache。
8. metadata 非零 cache 优先，不被 simulation 覆盖。
9. metadata cache 全 0 时 high-cache 可填补。
10. floor 可以避免小请求 cache read 被当前 small total 压小。
11. soft-cap jitter 不固定贴到 `promptCacheMaxSimulatedInputTokens`。
12. runtime config 从 app_config 覆盖启动默认值。
13. runtime config reload 后新请求读取新参数。

### 本地接口测试

1. `/v1/messages` 非流式多轮。
2. `/v1/messages` 流式多轮。
3. `/cc/v1/messages` Claude Code 兼容流式。
4. 小请求首轮无缓存。
5. 大请求建立缓存后连续小请求有 cache read。
6. 超过 100k 和 200k 的 cache read/write 统计合理。

### CLI 真实测试

只通过 `ccman` 切换配置，不改 shell 环境变量。

1. 用 `ccman cc add/use` 配置 Claude Code 到本地服务。
2. 用 `ccman oc add/use` 配置 OpenCode 到本地服务。
3. 在 `~/Desktop/procode/ccman` 目录执行多轮真实对话。
4. 覆盖普通问答、读文件、写无用测试文件、工具调用、长上下文、多轮延续。
5. 测试完成后按需要用 `ccman cc use` 和 `ccman oc use` 切回原 provider。

## 实施顺序

1. 新增 runtime config 模块。
2. 注入 Anthropic router、AdminState、AdminService。
3. 修改 Admin config update 的 prompt-cache 热更新。
4. 新增 `compute_high_cache(...)` 与 computation/floor。
5. 修改 `CacheSimulation` 支持 floor。
6. 修改 handlers 的 high-cache/local-prompt-cache 分支。
7. 调整 `/console/settings` 文案。
8. 补单元测试。
9. 运行 cargo test。
10. 启动本地服务。
11. 用 HTTP、Claude Code CLI、OpenCode CLI 做真实多轮测试。

## 2026-05-19 本地验证记录

### 代码与前端构建

1. `cargo fmt` 通过。
2. `cargo test` 通过：289 passed，0 failed。
3. `frontend` 执行 `pnpm build` 通过，生成新版 `/console` 静态资源。
4. 本地服务使用 `config.json` 启动在 `127.0.0.1:9022`。
5. `/admin` 与 `/console` 均返回 HTTP 200。

### HTTP 验证

使用 `x-api-key` 直接请求本地服务，不依赖 CLI 环境变量。

1. 小请求首轮：`/v1/messages` 返回成功，cache read/write 均为 0，符合“不能凭空给小测试请求制造缓存”的约束。
2. 大请求建缓存：`/v1/messages` 同一 session 首个大请求返回 `cache_creation_input_tokens ~= 159k`。
3. 连续小请求：同一 session 后续小请求返回 `cache_read_input_tokens ~= 159k`，没有因为本次输入小而归零。
4. 流式请求：`/v1/messages` stream 同一 session 返回 `cache_read_input_tokens ~= 159k`。
5. Claude Code 兼容流式：`/cc/v1/messages` stream 同一 session 返回 `cache_read_input_tokens ~= 159k`。
6. 工具历史请求：包含 `tool_use/tool_result` 的请求返回成功，并延续同一 session 的 cache read。
7. `count_tokens` 文本请求返回成功。
8. `document` text/plain 请求返回成功。
9. `image/bmp` 被本地校验拒绝为 400，符合 unsupported media type 的预期。
10. `image/png` base64 请求通过本地转换后被 Kiro upstream 返回 400 `Improperly formed request`；因此当前只能证明本地 schema/转换接受 png，不能证明真实上游图片识别链路可用。

### 运行时配置热更新验证

通过 `PUT /api/admin/config` 临时修改：

```json
{
  "prompt_cache_target_read_ratio": 0.5,
  "prompt_cache_token_scale": 1.0,
  "prompt_cache_scale_min_input_tokens": 0
}
```

随后不重启服务直接请求 `/v1/messages`，cache creation 按低比例降到约 5.1k。测试后已恢复：

```json
{
  "prompt_cache_target_read_ratio": 0.98,
  "prompt_cache_token_scale": 1.6,
  "prompt_cache_max_simulated_input_tokens": 300000,
  "prompt_cache_cap_jitter_min_tokens": 12000,
  "prompt_cache_cap_jitter_max_tokens": 24000,
  "prompt_cache_scale_min_input_tokens": 20000
}
```

### Claude Code CLI 验证

严格通过 `ccman` 临时切换 Claude Code 服务商到 `http://127.0.0.1:9022`，未修改 shell 环境变量。测试完成后已通过 `ccman cc use okmcode-kiro` 恢复原服务商，并删除临时 provider。

在 `~/Desktop/procode/ccman` 中完成：

1. 普通 `claude -p`：成功，返回预期文本。
2. `--resume` 同一 session：成功，后续请求读到已有 cache。
3. 工具读写：允许 `Read/Write/Bash`，读取 `package.json` 并写入 `.tmp/kiro-rs-cli-smoke/claude-code-tool.txt`，成功。
4. 子 agent：允许 `Task/Read`，使用子 agent 读取 `README.md`，成功。

Admin usage 中，相关 Claude Code CLI 请求均进入本地服务，`clientUserAgent` 为 `claude-cli/...`，`usageSource = local_prompt_cache`。本轮相关记录中，单条 cache creation 超过 100k 的记录有 3 条，单条 cache read 超过 100k 的记录有 1 条；Claude CLI 工具场景自身汇总 usage 中 cache read 达到约 256k。

### OpenCode CLI 验证结论

严格通过 `ccman` 临时切换 OpenCode 服务商到 `http://127.0.0.1:9022`，未修改 shell 环境变量。测试后已通过 `ccman oc use GMN` 恢复原服务商，并删除临时 provider。

当前 OpenCode CLI 未能完成真实请求，原因不是 high-cache continuity 逻辑，而是 OpenCode 配置/协议不匹配：

1. 当前安装的 `ccman` 版本为 3.3.19，不支持源码中已有的 `ccman oc add --name --base-url --api-key` 非交互参数，只能走交互输入。
2. `ccman oc add/use` 写入的 OpenCode 配置使用 `provider.openai` 和 `model = "openai/gpt-5.4"`。
3. 当前 OpenCode 1.15.4 实际加载 provider 时只识别带 `name/npm` 元数据的 provider；日志显示只加载了旧的 `codex` 和 `okmcode`，没有加载 `openai`。
4. 执行 `opencode run --model openai/gpt-5.4 ...` 在本地 OpenCode 侧报 `ProviderModelNotFoundError`，没有请求到 `kiro-rs`。
5. 即使 OpenCode 识别 `openai` provider，本项目当前暴露的是 Anthropic-compatible `/v1/messages`，不是 OpenAI `/chat/completions`，因此 OpenCode 要稳定接入应使用 Anthropic provider 形态，例如历史备份里存在过的 `kiro-local` provider：`npm = "@ai-sdk/anthropic"`、`baseURL = "http://127.0.0.1:9022/v1"`、`model = "kiro-local/sonnet"`。

基于“不手改配置、只用 ccman 切换”的约束，本次没有手动恢复或写入 `kiro-local` OpenCode provider。后续若需要 OpenCode 真实回归测试，应先修复 `ccman` 的 OpenCode writer，使其能够生成当前 OpenCode 版本可识别的 Anthropic provider 配置，再执行 OpenCode CLI 多轮测试。
