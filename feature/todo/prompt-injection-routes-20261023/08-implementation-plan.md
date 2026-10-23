# 08 实施计划

按步骤顺序执行，每一步结束时 `cargo check --all-targets` 必须通过。步骤 1–6 是后端，7–8 是前端，9 是文档，10 是验证。全部完成后合成一次提交，不推送。

## 步骤 1：配置模型（新文件 `src/model/prompt_injection.rs`）

- `InjectionId`：14 个成员，serde 用 camelCase；提供 `ALL`、`supports_external_pool()`、`affected_by_strict()`。
- `ItemSetting { enabled: bool, external_pool: Option<bool> }`；路由覆盖用 `ItemOverride { enabled: Option<bool>, external_pool: Option<bool> }`。
- `PromptInjectionConfig { apply_to_count_tokens, defaults: BTreeMap<InjectionId, ItemSetting>, texts: BTreeMap<TextKey, String>, path_overrides: BTreeMap<String, BTreeMap<InjectionId, ItemOverride>> }`，所有 map 都用 `deny_unknown_fields` 或强类型 key 拒绝未知项。
- `RouteFeaturesConfig { defaults: RouteFeatureSet, path_overrides: BTreeMap<String, RouteFeatureOverride> }`；`RouteFeatureSet { thinking_model_name, thinking_trigger: ThinkingTrigger, channel_leak_guard, channel_leak_guard_stream, model_driven_web_search }`。
- `ThinkingTriggerMode` 增加 `Off`。
- 默认文本目录 `src/model/prompt_injection/catalog.rs`：把 `DEFAULT_TASK_QUALITY_PROMPT`、身份、语言、`SYSTEM_CHUNKED_POLICY`、Write/Edit 后缀、`THINKING_OUTPUT_POLICY`、tool_choice 各变体、`EMPTY_TURN_NUDGE`、WebSearch `GUIDANCE` 集中到这里，原文件改为引用。目录项同时带说明字段（分类、位置、依赖、建议、关闭影响），供管理接口返回。
- `Config` 新增 `prompt_injection`、`route_features`、`claude_models_only`。`prompt_steering`、`thinking_trigger_mode`、`bodyConversion.chunkedToolPolicy/thinkingPromptControls` 改成只反序列化（`skip_serializing`）的 `Option` 字段，供迁移读取。

## 步骤 2：迁移（`Config::migrate_legacy_prompt_steering`）

- 触发条件是"存在旧字段"，不看迁移版本号。原因：`Config::load` 会直接把版本号设成最新（`config.rs:7436`），按版本号判断会让文件配置漏掉迁移。
- 调用点有三处：`Config::load`、`apply_runtime_config_migrations`、PG 热加载（`manager.rs:2800`）。
- 规则按 03 第 5 节实现。实现完成后旧字段清空，下次保存时只写新结构。
- 版本号仍然升到 13，表示"这份配置已经由新代码保存过"。

## 步骤 3：请求级解析（新文件 `src/anthropic/prompt_injection.rs`，替换 `prompt_steering.rs`）

- `ResolvedInjections::resolve(endpoint, upstream, profile, &config)` 和 `ResolvedRouteFeatures::resolve(endpoint, &config)`。
- 每一项返回 `Allowed { text }` 或者 `Blocked(Reason)`，原因有四种：`Off { source }`、`Strict`、`ExternalPoolUnsupported`、`ExternalPoolOff`。
- system 块的拼装（A1/A2/A4、A5）沿用原来的格式和 marker，保证缓存前缀不变。

## 步骤 4：运行时接入

- `AppState`、`RequestRuntimeConfig`（`handlers.rs:930`）、`router.rs`、`middleware.rs`：删除 `prompt_steering`、`thinking_trigger_mode`，换成 `prompt_injection`、`route_features`、`claude_models_only`。
- `ConverterOptions`：删除 `prompt_steering`、`explicit_reasoning_request`，增加 `injections: ResolvedInjections`；`inject_*()` 改为读对应项。
- `ConversionResult` 增加 `applied_injections: Vec<InjectionId>`，记录本次实际注入了哪些项；请求日志输出 `prompt_injections` 字段。
- 按 03 第 3 节的表逐项改造各注入点，其中 A7 增加 Write/Edit 判断。
- `parsed_body_pipeline.rs`：去掉 `prompt_additions_enabled`，D5/D6 读路由功能；`BodyStageState` 里的 thinking 阶段计划保持不变。
- 外部池分支（`handlers.rs:7735-7865`）、count_tokens（`handlers.rs:13871`）、空轮重试（`handlers.rs:10797`）、WebSearch（`model_driven_websearch.rs`）、输出过滤（`handlers.rs:7363`）、模型列表（`handlers.rs:7372`）都改为读新结构。

## 步骤 5：管理接口

- `admin/types.rs`：`RuntimeConfigResponse` 和 `UpdateRuntimeConfigRequest` 换成新字段。`UpdateRuntimeConfigRequest` 保留一个 `prompt_steering: Option<serde_json::Value>`，收到时返回 400，提示旧版页面已停用这些字段。
- `admin/service.rs`：执行 02 第 1 节的文本校验；`route_scenario_routes` 收集路由时改用新的 pathOverrides。
- 新增 `GET /api/admin/prompt-injection/catalog`，以及 `POST /api/admin/prompt-injection/preview`。预览直接复用本地转换使用的 `ConverterOptions` 构造函数（从 `local_body_pipeline.rs` 抽出来共用），入参里的"模型是否支持原生 reasoning"由调用方指定，不做模型解析。

## 步骤 6：后端测试

- 改写现有测试：`converter.rs` 约 56 处，`prompt_steering.rs` 的测试迁到新模块，`config.rs` 约 88 处，`local_body_pipeline.rs` 3 处，`handlers/tests.rs` 3 个测试，以及 `external_pool/tests.rs`。
- 新增：
  - 迁移等价测试（07 第 1 节）：把旧判定函数整体复制到测试模块里作为对照，用固定种子生成 10,000 份随机配置。
  - 独立性测试、预览一致性测试、文本校验测试。
- 运行：通过 `feature/tests/run-cargo-scoped.sh` 跑全量，同一时间只跑一组。

## 步骤 7：新版前端 `ui/`

- `types/api.ts`、`lib/runtime-config-defaults.ts`：使用新类型，删除前端里的默认文本。
- 新页面 `features/prompt-injection/`：注入项、路由矩阵、不可关闭内容、预览四个 Tab，并接入侧栏路由。
- 路由编辑弹窗（`cache-policies-page.tsx`）：拆成三个 Tab，删除 `route-identity-editor.tsx`、`route-websearch-editor.tsx`，新增 `route-injection-editor.tsx`、`route-features-editor.tsx`。
- `route-policy-transfer.ts`：格式升到 v2，导入 v1 时转换 identity 和 webSearch。
- `runtime-page.tsx`：删除两个旧区块，加跳转链接；删除两个 bodyConversion 开关；在"模型"分区加 `claudeModelsOnly` 开关。

## 步骤 8：旧版 `admin-ui/`

- 删除提示词和身份相关的编辑区块与类型，保存时不再发送 `promptSteering`；对应位置提示去新版后台编辑。

## 步骤 9：文档

- 按 06 执行；同步更新 `config.example.json` 和 `README.md`。

## 步骤 10：验证

- 按 07 执行，结果写进 `09-results.md`。
- 本地实例用独立端口、独立 PG database、独立 Redis db，测完立即清理。

## 风险与应对

| 风险 | 应对 |
|---|---|
| 新旧版本同时部署时，旧实例保存配置会写回 `promptSteering`，并丢掉 `promptInjection` | 新实例热加载时会再迁移一次，结果和旧配置等价，但新实例上做过的修改会丢失。升级说明里写明：全部实例升级完成之前不要在页面上修改注入设置 |
| 迁移结果不等价 | 步骤 6 的对照测试覆盖；发现不等价时修正迁移规则，不改对照实现 |
| 默认文本搬家时字节不一致，导致缓存前缀失效 | 目录测试逐字节断言搬家后的默认文本与原常量一致，搬家前先把原常量的 sha256 记录下来 |
| 改动面大，测试改写量大 | 每一步 `cargo check` 通过后再进入下一步；测试集中在步骤 6 一次改完 |
