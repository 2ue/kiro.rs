# 内置路由策略必须完全由配置决定

Status: `analysis-recorded / backend-and-ui-implemented / focused-verified / full-rust-and-ui-gates-passed / cli-fake-upstream-verified / release-pending / live-reload-pending`

Severity: P0 correctness / configuration authority

Last reviewed: 2026-08-02 Asia/Shanghai

本轮实现和验证已完成：后端运行时策略解析、新版 `ui`、旧版 `admin-ui` 均已同步；冻结候选二进制已通过真实 Claude Code CLI 假上游套件。没有重启现网服务，也没有把浏览器交互或生产复发观察误记为已通过。

## 范围与结论

修复前实现有多处把 `/cc`、`/v1`、`/ha`、`/na` 当成策略开关，而不仅仅是内置路由入口。用户明确的目标是：

1. 所有运行逻辑都应该基于配置判断，而不是基于固定路径名判断。
2. 所有路由的特性都由配置决定；例如 `/cc` 完全可能配置成 `/ha` 或 `/na` 的策略。
3. 内置路由仅代表路由地址内置，不代表策略不可变；缓存、usage、提示词、外部池、模型处理等策略都应可调整。

这个目标合理。它把“入口存在”和“入口行为”分成两层，能减少后续新增内置路由、兼容路由或自定义路由时出现的隐式特判，也能让页面配置与真实运行一致。

## 用户可见风险（修复前）

- 页面把某路径配置为无缓存或高缓存后，请求链路可能仍受代码里的内置路径名单影响。
- `/cc` 可能被提示词引导特殊处理，不能自然配置成普通 `/v1`、`/ha` 或 `/na` 策略。
- `/na` 可能被迁移逻辑强制恢复为无缓存，导致页面调整后重启或 reload 不符合预期。
- `count_tokens` 对 `/cc` 的处理和 messages 请求不完全同构，容易出现估算与真实请求不一致。
- 前端显示的默认值、别名归一化和“仅 /cc 路径”文案会继续暗示内置路由策略不可变。

## 复现

问题可以在不调用真实上游的情况下由路径解析和配置归一化稳定复现：

1. 在运行配置中把 `/cc` 的缓存策略改成 `no_cache`，把 `/na` 改成
   `current_high_cache`，重启或执行运行配置迁移。
2. 观察旧实现的路径策略解析：`/cc` 仍会被内置默认分支当成高缓存，
   `/na` 会被迁移逻辑恢复成无缓存，导致页面配置与实际行为不一致。
3. 将提示词引导范围设置为非 `/cc` 的入口，调用对应
   `messages` 和 `count_tokens`；旧实现只识别 `/cc`，新入口不会按配置生效。
4. 在 `/v1`、`/cc`、`/ha`、`/na` 间切换相同缓存类型，旧实现的路径缓存
   命名空间仍由内置路径名单决定；自定义路径却可能使用独立命名空间。

本轮新增的配置解析测试使用同一组场景验证修复后的预期：显式路径覆盖优先，
迁移不覆盖显式 `/na`，命名空间由字段控制，提示词和 `count_tokens` 共享路径规则。

## 当前源码链

### P0: `/cc` 提示词引导路径特判（已修复）

- `src/anthropic/prompt_steering.rs`
  - 已移除按 `/cc` 字符串判断的 `is_cc_endpoint()`。
  - `routeMode` / `routeRules` 通过共享路径规则匹配器决定提示词是否生效。
- `src/model/config.rs`
  - 旧 `cc_only` 只作为兼容输入，归一化为 `route_rules + allow_list + [/cc]` 默认配置。
- `src/anthropic/handlers.rs`
  - `messages` 和 `count_tokens` 均传入实际入口，使用同一套配置规则，不再由 `count_tokens_cc` 单独决定策略。

### P0: `/na` 缓存策略被强制（已修复）

- `src/model/config.rs`
  - 内置默认只在路径没有显式配置时补齐；显式 `/na` 配置不会被迁移覆盖。
  - `migrate_builtin_no_cache_routes()` 不再把 `/na` 写回无缓存，也不再删除显式 usage 覆盖。
- `ui/src/features/runtime/runtime-sections.tsx` 与 `admin-ui/src/components/runtime-config-panel.tsx`
  - `/na` 的页面策略由当前配置合并结果决定，可切换无缓存、高缓存或 Kiro-RS Tool。

### P1: 内置路径缓存命名空间特判（已修复）

- `src/model/config.rs`
  - 缓存命名空间由 `独立路径缓存空间`（`routeNamespace`）决定。
  - 内置路径默认值只用于初始化；显式 `true/false` 优先于路径名称。

### P1: 前端配置解释和文案路径特判（已修复）

- 两套 UI 仍列出内置入口作为可配置项和默认建议，这是入口清单，不是不可变策略。
- 提示词页面显示 `按路径规则`、`提示词路径模式`、`提示词路径规则`，并支持自定义内置或 `/dfcache/*` 入口。
- “应用到外部池”和 “count_tokens 同步计入”均说明使用同一套路径规则，不再写死 `/cc`。

## 不属于本问题的固定路径

以下固定路径可以保留，但不能决定业务策略：

- Axum 注册内置入口：`/v1`、`/cc/v1`、`/ha/v1`、`/na/v1`。
- Handler 传递真实入口用于记录、路径配置匹配、usage 归属。
- 外部上游 Anthropic-compatible 协议路径：`/v1/models`、`/v1/messages`。
- 测试、文档、loadtest 默认参数中的示例路径。

## 根因

历史上内置路由同时承担了两个角色：

1. 路由注册：让固定入口实际存在。
2. 策略选择：不同入口默认绑定不同缓存、usage 和提示词行为。

后续虽然增加了 `cachePolicy.pathOverrides`、`reportedUsage.pathOverrides`、外部池路由规则等配置，但旧的路径语义仍散落在后端和前端，导致“配置驱动”与“路径驱动”混用。

## 选定方案

### 方案原则

- 保留内置路由地址，不把 `/v1`、`/cc/v1`、`/ha/v1`、`/na/v1` 改成可删除或可重命名。
- 把内置路由默认策略变成配置初始化值，而不是运行时强制逻辑。
- 所有运行时决策先解析出路径配置，再按配置执行。
- `/cc`、`/ha`、`/na` 可以配置成任意缓存策略；配置后重启和 reload 不应被迁移覆盖。
- 提示词引导从“仅 `/cc` 路径”改成“按配置的路径规则”或“按兼容 profile”。

### 后端修复

1. 新增通用内置路由元数据，仅用于默认配置和 UI/文档同步：
   - `/v1`
   - `/cc`
   - `/ha`
   - `/na`
2. `CachePolicyConfig::with_builtin_path_defaults()` 只在缺省时补默认策略：
   - `/v1`、`/cc`、`/ha` 默认 `current_high_cache`
   - `/na` 默认 `no_cache`
   - 如果用户已经显式配置某路径，不覆盖。
3. 移除运行时迁移中强制 `/na` 为 no-cache 的逻辑。
4. 移除 `resolve_cache_policy_for_path()` 中基于 `/v1|/cc|/ha|/na` 的缓存 namespace 特判，改为配置字段控制是否使用路径 namespace。
5. 扩展 `PromptSteeringConfig`，增加路径规则配置，例如：
   - `routeMode`: `allow_all` / `allow_list` / `deny_list`
   - `routeRules`: 路径前缀规则
   - 默认保持历史行为：只允许 `/cc`
6. 保留 `PromptSteeringScope::ClaudeCodeProfile` 与 `AllRoutes` 兼容旧配置，但不再通过 `is_cc_endpoint()` 写死判断；`cc_only` 迁移为默认路径规则 `/cc`。
7. `count_tokens` 使用同一通用提示词规则，不再需要通过专用 `/cc` 函数表达策略。

### 前端修复

1. “缓存策略与路径绑定”允许 `/na` 切换到高缓存或 Kiro-RS Tool。
2. 默认策略仍可展示内置建议，但不把建议作为不可变行为。
3. “提示词引导”文案从“仅 /cc 路径”改为“按路径规则”，并暴露路径规则配置。
4. 路径归一化和别名处理集中到通用规则，避免每个内置路径散落判断。

## 验收矩阵

### 后端单元/集成

- `/cc` 配置为 `no_cache` 后，`cache_policy_for_path('/cc/v1/messages')` 返回 no-cache。
- `/na` 配置为 `current_high_cache` 后，重启/迁移/归一化不会改回 no-cache。
- `/cc` 配置为 `current_high_cache` 且需要独立 namespace 时，namespace 行为由配置决定，而不是路径名决定。
- `/v1`、`/cc`、`/ha`、`/na` 的默认策略仍保持历史默认值。
- 提示词引导默认仍只作用于 `/cc`，但这是默认路径规则，不是硬编码路径判断。
- 修改提示词路径规则后，`/v1`、`/ha`、`/na`、`/dfcache/team` 都能按配置启用或禁用。
- `count_tokens` 与 messages 使用相同提示词路径规则。
- 外部池路径规则继续按配置匹配，不被本修复破坏。

### 前端

- 页面能把 `/na` 从无缓存改成高缓存并保存。
- 页面能把 `/cc` 改成无缓存策略并保存。
- 提示词引导不再显示“仅 /cc 路径”这种不可变语义。
- 路径规则 UI 支持 `/v1`、`/cc`、`/ha`、`/na`、`/dfcache/team`。

### 真实/动态验证

- 在隔离或本地服务上验证：
  - `/cc/v1/messages` 使用 no-cache 配置时，usage/cache 行为符合 no-cache。
  - `/na/v1/messages` 使用 high-cache 配置时，usage/cache 行为符合 high-cache。
  - `/v1/messages/count_tokens` 被路径规则开启提示词引导后，估算包含新增 system prompt。
  - `/cc/v1/messages/count_tokens` 被路径规则排除后，估算不包含新增 system prompt。

## 残余风险

- 完全的“路由能力表”还未建立；本轮目标是先消除已知路径驱动策略，不做全路由系统重写。
- 旧数据库中已经被迁移过的 `/na` 策略无法凭空恢复用户曾经配置过的值，只能保证后续不再强制覆盖。
- 默认策略仍会列出内置路径，这是兼容初始化，不代表不可变策略。
- 本轮未重启现网服务，未完成浏览器实际保存/刷新交互和真实 Claude CLI 动态热加载；这些属于后续 C1/C2 门禁。

## 修复后结果与验证证据

### 已修改范围

- 后端：`src/model/config.rs`、`src/anthropic/prompt_steering.rs`、`src/anthropic/router.rs`、`src/anthropic/handlers.rs`、`src/anthropic/middleware.rs`、`src/anthropic/mod.rs`、`src/main.rs`。
- 新版 UI：`ui/src/types/api.ts`、`ui/src/lib/runtime-config-defaults.ts`、`ui/src/features/runtime/runtime-page.tsx`、`ui/src/features/runtime/runtime-sections.tsx`。
- 旧版管理界面：`admin-ui/src/types/api.ts`、`admin-ui/src/components/runtime-config-panel.tsx`。

### 验证命令与结果

- `cargo fmt --check`：通过。
- `cargo check --all-targets --locked`：通过。
- `feature/tests/run-cargo-scoped.sh route-policy-handler-matrix -- cargo test --all-targets --locked builtin_routes_follow_runtime_cache_and_prompt_config_matrix -- --nocapture`：`1 passed / 0 failed`。
- `cargo test --all-targets --locked`：主套件 `1864 passed / 0 failed / 6 ignored`，`kiro_loadtest` `31 passed / 0 failed`。
- `pnpm --dir ui check && pnpm --dir ui build`：通过；仅保留既有 chunk size warning。
- `pnpm --dir admin-ui build`：通过。
- `node feature/tests/check-feature-docs.mjs`：`70` 份问题文档、`280` 条相对链接通过。
- `node feature/tests/prompt-control-independence.mjs`：两套 UI 的提示词总开关与 body conversion 独立性通过。
- `node feature/tests/prompt-default-parity.mjs`：Rust、UI、Admin UI 的任务质量默认值一致。
- `git diff --check`：通过。
- 冻结候选二进制 SHA-256：`fba89eb1e57947b481f38051341481662ca1c7f927a25c4ec167351cef0fcf77`。
- Claude Code CLI `2.1.220` 假上游套件通过：`bare-invoke`、`long-session`（`5 sessions / 110 turns / 100 tool pairs / leakMatches=[]`）和 `thinking-wire`（`60/60`）；thinking 子套件使用 Claude Code 包内真实二进制复跑，排除了 Volta shim 环境噪声。

### 关键行为覆盖

- `/cc -> no_cache`、`/na -> current_high_cache` 的显式路径配置不会被默认值或迁移覆盖。
- `routeNamespace` 显式开关决定缓存空间是否按入口独立，不由 `/v1`、`/cc`、`/ha`、`/na` 名称决定。
- 提示词路径规则可命中 `/v1`、`/cc`、`/ha`、`/na` 和自定义 `/dfcache/team`，`messages` 与 `count_tokens` 使用同一规则。
- 内置入口仍由 Axum 固定注册，但 handler wrapper 只传递真实入口，缓存、usage、提示词和外部池策略在运行时按配置解析。
- `builtin_routes_follow_runtime_cache_and_prompt_config_matrix` 通过同一个 Axum handler + 假 Kiro 上游验证了配置矩阵：`/cc -> no_cache`、`/na -> current_high_cache` 且共享缓存空间、`/ha -> current_high_cache` 且独立缓存空间；只有 `/ha` 命中提示词引导；四个内置入口的 `count_tokens` 均保持本地处理。

## 回滚

- 如果动态验证发现配置迁移兼容风险，可以保留新增提示词路径规则，同时临时恢复默认内置策略补齐；但不能恢复运行时 `is_cc_endpoint()` 或强制 `/na` no-cache 作为长期方案。
