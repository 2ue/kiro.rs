# P19 旧版/带日期的模型 ID 被静默升级到另一个模型，响应的 `model` 字段仍回显请求值，客户端无法察觉

Status: open / documented / not-fixed
Severity: Low
Area: request
Discovered: 2026-09-28 协议互转审计
Verified-against: a4227c1 (2026-09-29)

## 问题与影响

请求 `claude-3-5-sonnet-20241022`、`claude-opus-4-1-20250805`、`claude-3-5-haiku-20241022` 这类 Kiro 目录里没有的旧模型 ID 时，代理在默认 `compatible` 模式下会把它解析成**另一个模型**再发给 Kiro，例如：

| 请求模型 | 默认解析结果（seed 目录下） | 依据 |
| --- | --- | --- |
| `claude-3-5-sonnet-20241022` | `claude-sonnet-4.5` | 单测 `resolver_maps_legacy_dated_models_to_seeded_kiro_models`（`src/anthropic/model_capabilities.rs:2565-2584`） |
| `claude-sonnet-4-20250514` | `claude-sonnet-4` | 同上 |
| `claude-opus-4-1-20250805` | `claude-opus-4.5` | 同上 |
| `claude-3-7-sonnet-20250219` | 候选 `claude-sonnet-4.5` → `4-5-20250929` → `4.6` | `src/anthropic/model_capabilities.rs:1361-1365` |
| `claude-3-5-haiku-20241022` | 候选 `claude-haiku-4.5` | `src/anthropic/model_capabilities.rs:1371-1375` |

响应里的 `model` 始终是请求值：

- 非流式：`handle_non_stream_request` 收到的 `model` 参数是 `&payload.model`（`src/anthropic/handlers.rs:6804-6812`），写入 `"model": model`（`src/anthropic/handlers.rs:11160`）。
- 流式：`handle_stream_request` 同样拿到 `&payload.model`（`src/anthropic/handlers.rs:6772-6780`），`StreamContextTemplate.model = model.to_string()`（`src/anthropic/handlers.rs:8127`），`message_start.message.model` 使用的就是它（`src/anthropic/stream.rs:2336`）。
- `x-kiro-rs-warnings` 不包含模型重映射信息（`src/anthropic/converter.rs:215-275`），而且默认关闭。重映射只出现在 info 日志 "Kiro upstream model mapping applied to request payload"（`src/anthropic/handlers/local_body_pipeline.rs:285-295`）和 usage 记录的 `upstream_model` / `model_resolution_*` 字段里（`src/anthropic/handlers.rs:4856-4865`）。

影响：

- 客户端以为在用 3.5 Sonnet，实际跑的是 Sonnet 4.5：能力、thinking 支持、输出上限、计费口径都不同，出了问题难以归因。
- 和之前的决策有冲突：[P003 真实账号模型 400 专题](../real-account-model-invalid-400-20260901.md) 定下的规则是 "显式 Claude minor 版本不再在没有明确 mapping 时静默改成另一个 minor"。但 `claude-opus-4-1-20250805` 是显式的 4.1，仍被映射成了 4.5（原因见下文）。
- 严重度定为 Low：旧模型在 Kiro 上本来就不可用，升级在多数情况下是 "能用" 的兜底；主要问题是**不透明**。

## 官方协议对照

> 方向约定：kiro.rs 基于 Kiro 对外提供 Claude Code（Anthropic）协议接口。客户端（如 Claude Code CLI）发出 Claude Code 协议请求，经转换后变为 Kiro 协议，调度到最终上游 Kiro；Kiro 的响应再转换回 Claude Code 协议返回给客户端。下文引用的"Anthropic 文档/官方"一律指 **Claude Code 协议规范**，即接口侧必须满足的行为；"Kiro 侧"指上游 Kiro 协议的要求。Anthropic 官方 API 不是本项目的上游。thinking 签名均为 Kiro 原生签名，在转换中原样透传。

- **Anthropic（已验证事实，官方文档）：** 响应的 `model` 字段是 "处理这个请求的模型"。官方对别名（例如 `claude-sonnet-4-5` → 带日期的快照）返回的是请求的别名还是解析后的快照，本仓库没有抓包记录，**未验证**。对于不存在或已下线的模型 ID，官方返回 `404 not_found_error`（经验推断），不会自动升级。
- **Kiro：** 模型目录来自 `ListAvailableModels`，缺失时退回 seed 目录（`src/anthropic/model_capabilities.rs:1151-1158`）。旧的 3.x ID 不在目录里。

## 源码链与根因

模型解析有两套实现，它们的优先级需要先理清：

1. **本地 Kiro 主路径**：`resolve_request_model` → `ModelCapabilities::resolve_model_with_mapping`（`src/anthropic/handlers.rs:5706-5760`）→ `resolve_model_with_catalog_mapping_and_mode`（`src/anthropic/model_capabilities.rs:1139-1280`）。结果 `ModelResolution` 传给 `local_body_pipeline::prepare`（`src/anthropic/handlers/local_body_pipeline.rs:129`）→ `prepare_with_plan`（`:152`），再调用 `convert_request_with_resolved_model`（`src/anthropic/handlers/local_body_pipeline.rs:180`；3a1306d 新增的 reasoning 整形重试在 `:200` 用同一个 `model_resolution` 再转换一次；`src/anthropic/converter.rs:405-415`），**直接使用 `resolution.upstream_model`，不会调用 `map_model`**。
2. **`map_model`**（`src/anthropic/converter/model.rs:14-79`）只在下面几处被调用：
   - `convert_request_with_options`（`src/anthropic/converter.rs:395-402`）：生产代码里没有调用方，只有 `src/kiro/provider.rs:2875` 的测试夹具和 converter 自己的单测在用，**不在请求主路径上**；
   - `external_route_model_resolution`（`src/anthropic/handlers.rs:5762-5790`）：外部池路由在解析结果是 `ExactUpstream`/`PassThrough` 时，会用 `map_model` 再改写一次（调用点 `src/anthropic/handlers.rs:6349`、`src/anthropic/handlers.rs:6427`、`src/anthropic/handlers.rs:6462`）；
   - 外部池模型变体展开（`src/external_pool.rs:11456`、`src/external_pool.rs:11486`）。

所以审计原述 "converter.rs ~399 map_model 在主路径上把 claude-3-5-sonnet 映射成 sonnet-4.5" 需要修正：**本地主路径不经过 `map_model`**，但主路径的 resolver 通过另一套规则得到了同样的升级结果。`map_model` 的静默升级实际影响的是**外部池路由**。

主路径 resolver 的判定顺序（`src/anthropic/model_capabilities.rs:1139-1280`），默认 `ModelMappingConfig{enabled:true, auto_generate_rules:true}`（`src/model/config.rs:2649-2666`），默认模式 `Compatible`（`src/model/config.rs:2570-2580`）：

1. 精确命中目录 → exact（`:1160-1162`）
2. `[1m]` 或 `-thinking` 后缀剥离后命中 → alias（`:1176-1187`）
3. 配置规则 VersionEquivalent → 自动版本等价（同 family、major、minor）（`:1189-1204`）
4. 配置规则 Alias → `explicit_model_alias_families` → **`explicit_model_alias_candidates`**（`:1206-1227`，候选表在 `:1282-1377`）
5. 非 Compatible 模式直接 pass-through（`:1229-1231`）
6. 配置规则 Fallback（`:1233-1244`）
7. **显式 minor 版本 → pass-through**（`:1246-1253`）
8. family 候选 / family 兜底（`:1259-1277`）

根因：

- **显式 minor 保护的位置太靠后。** `claude-opus-4-1-20250805` 能被 `parse_claude_minor_version` 解析成 opus 4.1（`src/anthropic/model_capabilities.rs:1451-1492`），本应在第 7 步被保护。但第 4 步 `explicit_model_alias_candidates` 的硬编码表把它映射到了 `claude-opus-4.5`（`src/anthropic/model_capabilities.rs:1356-1360`），第 7 步根本走不到。更关键的是，第 4 步在第 5 步的 `allows_family_fallback` 检查**之前**，所以 `alias_only` 模式下也会发生这次跨 minor 升级。
- **旧命名格式识别不到 minor。** `claude-3-5-sonnet-*` 的格式是 `claude-<major>-<minor>-<family>`，而 `parse_claude_minor_version` 只认 `claude-<family>-<major>-<minor>`（`src/anthropic/model_capabilities.rs:1451-1460`），所以 3.5 不被当作显式 minor，会走 alias 或 family 的升级。
- **`claude-sonnet-4-20250514` 没有 minor**（日期段超过 3 位，不算 minor，见 `src/anthropic/model_capabilities.rs:1480-1482`），映射到 `claude-sonnet-4` 属于别名解析，是合理的。
- **`map_model` 的子串匹配很宽**：只要名字包含 `sonnet` 且包含字符 `4`（包括日期里的 4），就会返回 `claude-sonnet-4.5`（`src/anthropic/converter/model.rs:33-45`，`contains("4")` 在 `:38`）。所以 `claude-3-sonnet-20240229` 会被映射成 4.5，而 `claude-3-7-sonnet-20250219` 反而返回 `None`（名字里既没有 `4` 也没有 `3-5`）。审计原述 "opus 没有 minor → opus-4.7" 成立（`src/anthropic/converter/model.rs:56-67` 里 `contains("4")` 的分支，在 `:63`；只针对非原生格式，原生 `claude-opus-4-...` 会先走 `:46-55`），"未知版本透传" 对 `claude-<family>-<n>...` 这种原生格式成立（`src/anthropic/converter/model.rs:25-32`、`:46-55`、`:68-73`）。

审计原述中 `converter/model.rs:25-73` 的行号大致准确，函数完整范围是 `src/anthropic/converter/model.rs:14-79`。

> 2026-09-29 代码核对（HEAD a4227c1）：3a1306d 没有改动模型解析（`model_capabilities.rs`、`converter/model.rs`、`external_route_model_resolution` 均未变），本问题仍存在。`handlers.rs` 与 `model_capabilities.rs` 的引用行号在 HEAD 上均核对一致；已更新 `local_body_pipeline.rs`（转换调用 `:180`、映射日志 `:285-295`）和 `provider.rs`（测试夹具 `:2875`）的行号。补充：第 4 步在 `explicit_model_alias_candidates`（`:1222-1227`）之前还有 `explicit_model_alias_families`（`:1215-1220`）。

## 复现

### 最小复现（单测）

放进 `src/anthropic/model_capabilities.rs` 的 `mod tests`（`src/anthropic/model_capabilities.rs:1841`）：

```rust
#[test]
fn audit_p19_explicit_minor_is_upgraded_by_alias_table_even_in_alias_only_mode() {
    let models = seed_model_capabilities()
        .into_iter()
        .map(|item| item.model)
        .collect::<Vec<_>>();

    for mode in [ModelResolutionMode::Compatible, ModelResolutionMode::AliasOnly] {
        let r = resolve_model_with_catalog_and_mode("claude-opus-4-1-20250805", &models, mode);
        // 现状：显式 4.1 被静默改成 4.5，与 P003 的 "显式 minor 不跨版本" 规则冲突
        assert_eq!(r.upstream_model.as_deref(), Some("claude-opus-4.5"), "{mode:?}");
        assert_eq!(r.source, ModelResolutionSource::Alias);
    }

    let legacy = resolve_model_with_catalog("claude-3-5-sonnet-20241022", &models);
    assert_eq!(legacy.upstream_model.as_deref(), Some("claude-sonnet-4.5"));
    assert!(legacy.is_remapped());
}
```

`map_model` 的宽匹配放在 `src/anthropic/converter.rs` 的 `mod tests`：

```rust
#[test]
fn audit_p19_map_model_substring_matching_is_date_sensitive() {
    // 日期里的 "4" 触发 sonnet-4.5
    assert_eq!(map_model("claude-3-sonnet-20240229").as_deref(), Some("claude-sonnet-4.5"));
    // 3.7 没有 "4" 也没有 "3-5"，返回 None
    assert_eq!(map_model("claude-3-7-sonnet-20250219"), None);
}
```

### 端到端复现

1. 非流式请求 `model: "claude-3-5-sonnet-20241022"`：响应 `model` 为 `claude-3-5-sonnet-20241022`，但 info 日志显示 `upstream_model=claude-sonnet-4.5`，usage 记录里 `modelResolutionSource=alias`。
2. 同一请求打到外部池路由：对比外部池收到的 `model` 字段（`external_route_model_resolution` 改写后的值）。
3. 设置 `modelResolutionMode=alias_only` 后请求 `claude-opus-4-1-20250805`：仍然被映射到 4.5。

## 修复方案

### 候选方案

| 方案 | 做法 | 评价 |
| --- | --- | --- |
| A. 把显式 minor 保护提前，并改为透明（推荐） | 在 alias 表之前判断：显式 minor、目录里没有、也没有**用户配置**规则 → pass-through；从硬编码 alias 表里删掉跨 minor 的条目（`claude-opus-4-1-*` → 4.5）；旧 `claude-3-x` 命名按 "显式 minor" 处理 | 和 P003 决策一致；旧模型改为由上游返回真实错误 |
| B. 保留升级，增加披露 | 升级照旧，在响应头 `x-kiro-rs-model-resolved: <upstream>` 以及 warnings 里写明 | 兼容性最好，但语义依然是 "静默换模型" |
| C. 响应 `model` 回显 upstream | 改成返回实际模型 | 客户端能看到真实模型；但 Claude Code 等客户端可能用 `model` 做本地判断，风险未知 |

### 推荐方案

A + B 组合：

1. `resolve_model_with_catalog_mapping_and_mode`：把 `is_explicit_claude_minor_version(&base)` 的 pass-through 挪到第 4 步（硬编码 alias 表）之前。用户在 `modelMapping.rules` 里显式配置的 Alias/Fallback 规则仍然优先（第 3 步和第 4 步的配置规则部分），这样运营方可以显式声明 "4.1 → 4.5"。
2. 扩展 `parse_claude_minor_version`，识别旧格式 `claude-<major>-<minor>-<family>`（3-5、3-7），让它们也受显式 minor 保护。是否保留 "3.x → 4.5" 的兜底，由一个配置开关 `legacyModelUpgrade`（默认 true，保持现状）决定，而不是硬编码。
3. 不论最终结果如何，只要 `is_remapped()` 为真，就输出响应头 `x-kiro-rs-model-resolved: <upstream_model>`。这个头不受 `expose_proxy_warnings` 控制，信息量低且无敏感内容。
4. `map_model`：外部池路由改为复用主路径 resolver，或者至少把 `contains("4")` 这类子串匹配收紧成按 `parse_claude_minor_version` 解析的结构化匹配，避免日期里的数字导致误判。
5. 响应 `model` 字段暂时保持回显请求值（官方对别名的行为没有验证，不贸然改动）。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)，只使用 `127.0.0.1:19023` 指定测试实例和隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，Cargo 通过 `feature/tests/run-cargo-scoped.sh` 运行，不得影响本机正在运行的 Claude Code CLI 与服务。

- `audit_p19_explicit_minor_is_upgraded_by_alias_table_even_in_alias_only_mode` 改为修复后的期望：`claude-opus-4-1-20250805` → pass-through；配置了用户 Alias 规则时 → 规则目标。
- 更新 `resolver_maps_legacy_dated_models_to_seeded_kiro_models`（`src/anthropic/model_capabilities.rs:2565-2584`）里 opus-4-1 的断言；`claude-sonnet-4-20250514` → `claude-sonnet-4` 保持不变。
- `legacyModelUpgrade=false` 时，`claude-3-5-sonnet-20241022` → pass-through；为 true 时 → 4.5，并带 `x-kiro-rs-model-resolved` 头。
- `map_model` 单测：`claude-3-sonnet-20240229` 不再因为日期里的数字被映射。
- 回归 P003 的模型矩阵：显式 `claude-sonnet-4-6` 仍按 dash/dot 等价解析，不降级。
- 端到端：外部池和本地两条路由，对同一个旧模型 ID，解析结果、响应头、usage 字段一致。

## 兼容性与风险

- 还在使用 `claude-opus-4-1-*` 的客户端，修复后会收到上游的真实错误，而不是被升级到 4.5。需要在发布说明里提示运营方：如果需要保留升级，就配置显式的 Alias 规则。
- 默认保留 `legacyModelUpgrade=true`，3.x 客户端的行为不变，只多一个响应头。
- 新增响应头对 Anthropic SDK 没有影响（SDK 会忽略未知头）。
- 回滚：恢复判定顺序、关掉响应头即可，没有持久化状态。
