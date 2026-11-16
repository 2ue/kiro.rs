# 任务 s0-golden：usage 整形 golden 快照测试（阶段 0 行为冻结）

## 目标

为账号侧 usage 投影链路建立 golden 快照测试，作为后续删除 Kiro 代码时的回归依据。要求覆盖 6 种缓存策略 × 流式/非流式 × 有/无上游读取证据 × 有/无上游 1h 写入 = 48 个用例，快照文件落盘，测试两次运行结果一致。

## 允许改动的文件

- 新建 `src/external_pool/golden_tests.rs`
- 新建 `tests/golden/usage/*.json`（48 个或更多）
- `src/external_pool.rs`：只允许在现有 `#[cfg(test)] mod tests;` 附近加一行 `#[cfg(test)] mod golden_tests;`
- `src/external_pool/usage_projection.rs`：只允许新增上面说的 `#[cfg(test)]` 固定 seed 的辅助方法
- `src/external_pool/tests.rs`：只允许把 golden 测试需要复用的辅助函数（`test_pool`、`test_route`、`test_payload`、`projection_context_with_output_uplift`、`configure_usage_projection_test_path`、`event_data_value` 等）的可见性从私有改为 `pub(super)`，不改函数体

## 现有代码入口（先读这些）

- `src/external_pool.rs:15818` `fn project_usage_value(usage: &mut serde_json::Value, projection: Option<&ExternalUsageProjectionContext>, commit_cache_state: bool, response_output_estimate: Option<i32>) -> Option<ProjectedExternalUsage>`
- `src/external_pool.rs:14745` `process_usage_slots_in_sse_value`（流式槽位：`message.usage` 预览不提交，顶层 `usage` / `delta.usage` 提交）
- `src/external_pool/usage_projection.rs:233` `build_template` / `build_context`（投影上下文构造，含门槛判断）
- `src/external_pool/tests.rs:12241` `test_pool`、`:12991` `test_route`、`:13080` `test_payload`、`:14737` `projection_context_with_output_uplift`
- 既有 usage 投影测试 `src/external_pool/tests.rs:14910-17400`（函数名以 `usage_projection_` 开头），照它们的写法构造上下文
- 策略类型 `src/model/config.rs:3509` `PromptCacheStrategyType`：`NoCache`、`CurrentHighCache`、`KiroRsTool`、`StableSegmentCache`、`KiroRsToolPlus`、`BoundedHighCache`
- `ExternalRouteRequest` 的 `prompt_cache_strategy_type`、`prompt_cache_simulation_mode`、`reported_usage`、`kiro_rs_tool_cache_policy`、`kiro_rs_tool_plus_cache_policy`、`stable_segment_cache_policy`、`cache_hit_shaping_policy` 字段控制策略；`ExternalPool.usage_projection_mode` 必须设为 `CurrentPathPolicy`

## 用例矩阵

每个用例一个 JSON 文件，文件名 `<strategy>__<stream|nonstream>__<evidence|noevidence>__<1h|no1h>.json`，内容：

```json
{
  "strategy": "current_high_cache",
  "stream": true,
  "upstream_cache_read_evidence": true,
  "upstream_1h_write": false,
  "request": { "model": "claude-sonnet-4-5", "system_repeat": 500, "user_text": "hello" },
  "upstream_usage": { "input_tokens": 1200, "output_tokens": 350, "cache_read_input_tokens": 8000, "cache_creation_input_tokens": 0, "cache_creation": { "ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 0 } },
  "expected": { "reported": { ... }, "shaped": { ... } }
}
```

- `strategy` 的 6 个取值对应 `PromptCacheStrategyType` 的 serde 名：`no_cache`、`current_high_cache`、`kiro_rs_tool`、`stable_segment_cache`、`kiro_rs_tool_plus`、`bounded_high_cache`。
- `upstream_cache_read_evidence=true` 时 `cache_read_input_tokens` 取 8000，否则 0。
- `upstream_1h_write=true` 时 `cache_creation_input_tokens=3000` 且 `cache_creation.ephemeral_1h_input_tokens=3000`，否则两者为 0。
- 流式用例：构造一个 `message_start` 事件（`message.usage`）和一个 `message_delta` 事件（`usage` 顶层含 output_tokens），依次经 `process_usage_slots_in_sse_value(..., rewrite=true)` 处理，`expected.reported` 记录 **message_delta 之后** 的 usage 对象（即最终提交值），另外记录 `expected.message_start_usage`。
- 非流式用例：构造完整响应 JSON `{"id":"msg_x","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"model":...,"usage":{...}}`，经 `project_usage_value(&mut usage, Some(&ctx), true, None)`，记录返回值的 `reported` 与 `shaped`。
- 每个用例使用独立的 `PromptCacheTracker` 与 `PromptCacheCreationController`（在 `test_route` 中新建），用例之间互不影响。
- `bounded_high_cache` 与 `kiro_rs_tool_plus` 使用各自策略的默认参数即可；`current_high_cache` 用 `test_route` 的默认值；`no_cache` 要把 `prompt_cache_simulation_mode` 设为 `Disabled`。
- 所有策略均设置 `reported_usage.default.enabled = true`，并对路径 `/cc` 加一条与现网一致的 override：input `sample-max 96` 且 `move_delta_to_cache_read=true`，cache_creation `sample-target 3000`、`normal_max_multiplier 1.2`（参考 `src/model/config.rs:1410-1434` 的内置默认）。查清楚字段的真实名字再写。
- uplift 参数固定：`uplift_percent=25`、`cost_floor_enabled=true`、`cost_floor_margin_percent=10`、`output_uplift_min_tokens=1000`、`output_uplift_percent=50`。

## 测试实现要求

- 在 `golden_tests.rs` 写一个 `#[test] fn golden_usage_snapshots()`：遍历 `tests/golden/usage/*.json`（用 `env!("CARGO_MANIFEST_DIR")` 定位），按文件内容构造上下文并执行，与 `expected` 比较；任何不一致时把全部差异收集后一起 `panic!`，差异信息包含文件名、字段名、期望值、实际值。
- 环境变量 `GOLDEN_UPDATE=1` 时不比较而是把实际值写回 JSON 文件的 `expected` 字段（保持其他字段与缩进不变，用 `serde_json::to_string_pretty`）。
- 第一次由你用 `GOLDEN_UPDATE=1 cargo test --bin kiro-rs -- golden_usage` 生成 48 个文件的 expected；然后不带环境变量再跑两次，必须都通过。如果两次结果不同，说明有非确定性来源（随机 seed、时间），你需要在测试里固定它（例如用例里加 `seed` 字段并传给上下文，已知非确定性来源：`src/external_pool/usage_projection.rs:172` 的 `bounded_seed: fastrand::u64(..)`，以及 `:417` 的 plus `seed`。允许在 `usage_projection.rs` 的 `build_context` 旁新增一个 `#[cfg(test)] pub(super) fn build_context_with_seed(..., seed: u64)` 或在 `ExternalUsageProjectionContext` 上加 `#[cfg(test)] pub(super) fn with_fixed_seed(self, seed: u64) -> Self`，只覆盖 seed 字段，不改任何算法），而不是放宽比较。
- 另写一个 `#[test] fn golden_usage_case_count()` 断言用例数 ≥ 48 且 6 种 strategy 各 8 个。
- 如果某种组合在该策略下"不整形"（例如 `no_cache` 下投影返回 None），expected 里记录 `"projected": false` 和原样 usage，这也是需要冻结的行为。

## 自检

```
cargo fmt --all
GOLDEN_UPDATE=1 cargo test --bin kiro-rs -q -- golden_usage
cargo test --bin kiro-rs -q -- golden_usage
cargo test --bin kiro-rs -q -- golden_usage
cargo test --bin kiro-rs -q -- external_pool::tests::usage_projection
ls tests/golden/usage/*.json | wc -l    # ≥ 48
git status --short                       # 只应出现允许改动的文件
```
