# 03 后端设计：配置、解析、迁移

## 1. 配置结构

旧的 `promptSteering` 整体删除，另外新增两个顶层字段：`promptInjection`（注入项）和 `routeFeatures`（路由功能）。同时删除 `bodyConversion.chunkedToolPolicy` 和 `bodyConversion.thinkingPromptControls`，`identity.claudeModelsOnly` 改为顶层的 `claudeModelsOnly`，`bodyConversion.modelDrivenWebSearch` 移到 `routeFeatures`。

```jsonc
"promptInjection": {
  "applyToCountTokens": true,
  // 全局默认：路由没有单独设置时用这里的值。key 是 02 第 1 节的注入项 ID。
  "defaults": {
    "taskQuality":   { "enabled": false, "externalPool": false },
    "identityPrompt": { "enabled": false, "externalPool": false },
    "chunkedWritePolicy": { "enabled": true }
    // ……每个注入项都有一条；externalPool 只在支持外部池的 4 项上出现（A1/A2/A4/A5）
  },
  // 可编辑文本：缺省或为 null 时用内置默认。key 是"注入项 ID"或"注入项 ID.变体"
  "texts": {
    "taskQuality": null,
    "languageTurnReminder.explicit": null,
    "languageTurnReminder.followUser": null,
    "chunkedWriteToolSuffix.write": null,
    "chunkedWriteToolSuffix.edit": null
  },
  // 路由覆盖：按最长前缀匹配；没写的项跟随 defaults
  "pathOverrides": {
    "/cc": { "taskQuality": { "enabled": true } },
    "/dfcache/team": { "identityPrompt": { "enabled": true, "externalPool": true } }
  }
},
"routeFeatures": {
  "defaults": {
    "thinkingModelName": true,
    "thinkingTrigger": "real_request",   // real_request | always | off
    "channelLeakGuard": { "enabled": false, "stream": true },
    "modelDrivenWebSearch": false
  },
  "pathOverrides": { "/cc": { "modelDrivenWebSearch": true } }
},
"claudeModelsOnly": false
```

- Rust 端用 `enum InjectionId`（14 个成员）和 `BTreeMap<InjectionId, ItemSetting>` 表示。serde 遇到未知 key 直接拒绝，保存时不会把拼错的 key 静默丢掉。
- 路由前缀的规范化和匹配复用缓存策略那一套（`normalize_reported_usage_path_prefix` / `reported_usage_path_matches`），三个分区的匹配语义一致。
- `thinkingTriggerMode` 原来是顶层字段，现在并入 `routeFeatures.defaults.thinkingTrigger`。

## 2. 每个请求的解析

```rust
/// 一次请求可以注入的项，以及每项生效值的来源。注入、预览、日志都只读这个结构。
pub(crate) struct ResolvedInjections {
    items: [ResolvedItem; InjectionId::COUNT],
}
pub(crate) struct ResolvedItem {
    allowed: bool,
    source: SettingSource,   // Default | Route(prefix) | Strict
    text: Cow<'static, str>, // 只读项和"未修改"的项用内置默认文本
}

impl ResolvedInjections {
    pub(crate) fn resolve(
        endpoint: &str,
        upstream: UpstreamKind,          // Local | ExternalPool
        profile: CompatProfile,
        config: &PromptInjectionConfig,
    ) -> Self;
}

pub(crate) struct ResolvedRouteFeatures { /* thinking_model_name, thinking_trigger, channel_leak_guard, ... */ }
```

解析规则只有四条：

1. 先找最长前缀匹配的路由覆盖；路由上写了这一项就用它，没写就用 `defaults`。
2. 上游是外部池时，读这一项的 `externalPool` 值；不支持外部池的项一律不生效。
3. strict 模式下，02 第 3 节列出的项不生效，`source` 记为 `Strict`。
4. 依赖关系（B2 依赖 B1、D1 依赖 WebSearch、E1 依赖空轮重试）不在这里处理。这些条件在各注入点按请求内容判断，和"请求里有没有 Write 工具"属于同一类判断。

## 3. 注入点改造

| 位置 | 改造 |
|---|---|
| `prompt_steering.rs` | 改名为 `prompt_injection.rs`。拼 system 块时读 `ResolvedInjections` 的 A1/A2/A4/A5，块的格式和 marker 不变，保持缓存前缀不变 |
| `handlers.rs:7735-7865` 的外部池分支 | 直接用 `resolve(.., ExternalPool, ..)` 的结果，不再计算 `prompt_steering_for_external` / `identity_for_external` 两个布尔值 |
| `ConverterOptions` | 删除 `prompt_steering` 和 `explicit_reasoning_request`，改为持有 `ResolvedInjections`；`inject_*()` 改成读对应项 |
| `converter.rs:626-645` | 三段每轮提醒各自读 B4、A3、A6 |
| `converter/history.rs:80` | A7 增加条件：请求声明了 `Write` 或 `Edit` 工具 |
| `converter/tools.rs` | A8 的后缀文本从 `ResolvedItem.text` 读，Write 和 Edit 两个变体分开 |
| `parsed_body_pipeline.rs:30-47`、`handlers.rs:13655,13774` | 读 `ResolvedRouteFeatures`，不再看任何提示词开关 |
| `empty_turn_nudge.rs` | 文本和开关都从 E1 读；关闭时空轮重试仍然去掉 reasoning 字段和 B2，只是不追加提示 |
| `handlers/model_driven_websearch.rs:684` | D1 文本和开关从 `ResolvedInjections` 读，功能开关从 `ResolvedRouteFeatures` 读 |
| `identity_guard` 调用点（`handlers.rs:7363` 等） | 读 `channelLeakGuard` |
| `claude_models_only()`（`handlers.rs:7372`） | 读顶层 `claudeModelsOnly` |
| `local_body_pipeline.rs:431` 请求日志 | 增加字段 `prompt_injections`，内容是本次实际注入的项 ID 列表（不是"允许注入"的列表） |

为了让日志记录的是"实际注入"，各注入点注入成功后要在 `ConversionResult.applied_injections` 里登记一次。

## 4. 内置默认文本

所有默认文本集中在 `src/anthropic/prompt_injection/catalog.rs`。目录里每一项都写明：ID、分类、位置、触发条件、是否可编辑、默认文本或各变体、依赖、建议等级、关闭后的影响、是否支持外部池、是否受 strict 影响。

- 后端注入时读这里，管理端 `catalog` 接口返回的也是这里（见 [04](04-admin-and-ui.md)）。
- 前端不再硬编码任何默认文本，原来三处默认文本（Rust、`ui/src/lib/runtime-config-defaults.ts`、admin-ui）对齐的问题也就不存在了。
- 旧的 `LEGACY_*` 常量和迁移 v2–v12 中替换旧默认文本的逻辑保留。旧数据库要先走完这些迁移，才能进入第 5 节的 v13。

## 5. 迁移

新增运行配置迁移 v13，加载时执行一次，之后保存的都是新结构。旧结构由一个只用于反序列化的 `LegacyPromptSteering` 读取。

记号：`M` 是旧的 `promptSteering.enabled`；`R(e)` 是旧的路由规则对入口 `e` 的判定（包含 scope、routeMode、routeRules）。

| 新项 | 迁移后的全局默认 | 路由覆盖 |
|---|---|---|
| A1 / A2 / A4 | `M && 子项开关 && 规则在"没有命中任何前缀"时的结果`：AllowList 下为 false，DenyList 下为 true，AllowAll / AllRoutes 下为 true，ClaudeCodeProfile 下看当时 profile 是否为 claude-code 或 debug | AllowList：每条规则写成显式开；DenyList：每条规则写成显式关；`*` 视为 AllowAll |
| A1 / A2 / A4 外部池 | `M && 子项开关 && applyToExternalPool && 同上` | 同上，写入 `externalPool` |
| A3 | `M && languageConstraint.enabled && languageTurnReminder.enabled && 同上` | 同上 |
| A5 / A6 | 按旧 `identity` 的 enabled、routeMode、routeRules 计算，A5 再乘 `promptEnabled`，A6 再乘 `turnReminder.enabled` | 旧 `identity.pathOverrides` 的每个前缀，用旧的 `for_route(前缀)` 算出结果后写成显式值 |
| A5 外部池 | 再乘 `applyToExternalPool` | 同上 |
| A7 | `M && chunkedWrite.enabled && systemPromptEnabled && bodyConversion.chunkedToolPolicy` | 无 |
| A8 | 同上，`systemPromptEnabled` 换成 `toolDescriptionEnabled` | 无 |
| B1 / B2 | `thinking.enabled && bodyConversion.thinkingPromptControls`（不乘 M，与旧行为等价，原因见下） | 无 |
| B3 / B4 | `M && toolChoice.enabled && bodyConversion.toolChoiceSteering` | 无 |
| D1 / E1 | true（旧版本没有开关，一直注入） | 无 |
| D5 `thinkingModelName` | `M` | 无 |
| D6 `thinkingTrigger` | M 为 true 时取旧 `thinkingTriggerMode`，否则为 `off` | 无 |
| `channelLeakGuard` | 按旧身份规则计算，再乘 `outputGuard.enabled`；`stream` 取 `outputGuard.streamEnabled` | 同 A5 |
| `modelDrivenWebSearch` | 原值 | 原 `pathOverrides` 原样搬过来 |
| `claudeModelsOnly` | 取 `identity.claudeModelsOnly` | — |
| 文本 | 旧文本和内置默认逐字节相同时记为 null（跟随默认），否则原样保留 | — |

B1/B2 不乘 M 的原因：旧代码里 M 关闭时，只有客户端显式请求 reasoning 才会注入 B1。而在 M 关闭时，自动开启 thinking 的两条路径（D5、D6）都已经迁移为关闭，所以剩下需要 B1 的请求恰好都是显式请求 reasoning 的请求，结果等价。

迁移后有意改变、不再等价的只有两处，页面"变更说明"和升级说明里都会列出：

1. A7 只在请求声明了 Write/Edit 工具时注入。
2. 旧配置 scope 为 `claude_code_profile` 时，迁移按迁移那一刻的 compat profile 固化成固定值，之后修改 profile 不会再联动。

迁移等价性测试见 [07](07-verification.md) 第 1 节。
