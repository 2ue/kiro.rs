# kiro-account-manager enhanced implementation log

本文档记录基于 `docs/kiro-account-manager-enhanced-learning-analysis.md` 落地到当前 `kiro.rs` 的每一个具体改动点。

记录规则：

1. 每个独立改动点单独编号。
2. 每个编号记录改动文件、改动原因、改动前行为、改动后行为、测试方式和测试结果。
3. 本次实现不要求兼容历史本地文件数据；但不主动破坏现有 `/v1`、`/cc/v1`、`/ha/v1`、`/na/v1` 路径级缓存上报和凭据调度策略。

## Change 001: 建立增强实现变更记录

- 状态：已完成
- 改动文件：
  - `docs/kiro-account-manager-enhanced-implementation-log.md`
- 改动原因：
  - 用户要求“每改动一点”都要记录到单独文档，包含详细改动、改动后变化和如何测试。
- 改动前行为：
  - 只有学习分析文档，没有本次实现过程的逐点落地记录。
- 改动后行为：
  - 新增本实现日志，后续每个改动点都会以独立编号追加记录。
- 测试方式：
  - 文档类改动，无运行时测试。
- 测试结果：
  - 已复查确认文档存在，并记录本次所有具体改动点。

## Change 002: 补齐 Kiro event-stream 兼容解析

- 状态：已完成
- 改动文件：
  - `src/kiro/model/events/additional.rs`
  - `src/kiro/model/events/base.rs`
  - `src/kiro/model/events/mod.rs`
  - `src/anthropic/stream.rs`
- 改动原因：
  - 外部项目显示真实 Kiro/Amazon Q 流里可能出现 `messageMetadataEvent.tokenUsage`、`meteringEvent.usage` 和 `codeEvent`。
  - 当前项目此前只从 `metadataEvent.tokenUsage` 读取权威 usage，`messageMetadataEvent` 中的 token usage 会被忽略。
  - `meteringEvent` 此前被解析成空 payload，`codeEvent` 此前属于未知事件。
- 详细改动：
  - `MessageMetadataEvent` 新增可选 `token_usage` 字段，字段名按上游 camelCase 解析为 `tokenUsage`。
  - 新增 `MeteringEvent { usage }`，保留 Kiro credits/usage 信息，当前只做 debug 记录，不参与计费硬逻辑。
  - 新增 `CodeEvent { content }`，并把 `codeEvent` 纳入 `EventType` 和 `Event`。
  - 流式转换层收到 `messageMetadataEvent.tokenUsage` 时，与 `metadataEvent.tokenUsage` 一样更新最终 usage。
  - 流式转换层收到 `codeEvent` 时按普通 assistant 文本内容下发。
- 改动前行为：
  - 只有 `metadataEvent.tokenUsage` 会作为权威 token usage。
  - `messageMetadataEvent.tokenUsage` 即使存在也不会影响最终 usage。
  - `meteringEvent` 丢弃 payload。
  - `codeEvent` 会被视为未知事件，不会输出内容。
- 改动后行为：
  - `metadataEvent.tokenUsage` 和 `messageMetadataEvent.tokenUsage` 都能作为权威 usage 来源。
  - `meteringEvent.usage` 可被解析和 debug 观测，但不改变调度、不改变费用统计。
  - `codeEvent.content` 能转换为 Anthropic SSE 文本 delta，为后续 Amazon Q CLI 风格 endpoint 预留兼容。
- 如何测试：
  - 运行事件反序列化测试：`CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features message_metadata_usage_deserializes_token_usage`
  - 运行流式 usage 覆盖测试：`CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features test_message_metadata_usage_overrides_final_usage`
  - 运行 codeEvent 转文本测试：`CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features test_code_event_is_forwarded_as_text_content`
- 测试结果：
  - `message_metadata_usage_deserializes_token_usage` 已通过。
  - `test_message_metadata_usage_overrides_final_usage` 已通过。
  - `test_code_event_is_forwarded_as_text_content` 已通过。

## Change 003: 增加上游风控/封禁错误独立凭据状态

- 状态：已完成
- 改动文件：
  - `src/kiro/token_manager.rs`
  - `src/kiro/provider.rs`
- 改动原因：
  - 外部项目识别了 `TEMPORARILY_SUSPENDED`、`AccountSuspendedException`、HTTP 423 locked 等账号风控/封禁信号。
  - 当前项目此前会把这类错误落入普通 401/403 失败计数或其他通用错误路径，后台不能准确区分账号状态。
- 详细改动：
  - 新增 `DisabledReason::TemporarilySuspended`、`DisabledReason::AccountSuspended`、`DisabledReason::AccountLocked`。
  - 新增公开枚举 `CredentialRiskControlReason`，供 Provider 把上游检测结果传给 TokenManager。
  - 新增 `MultiTokenManager::report_risk_controlled()`：
    - 立即禁用命中的凭据。
    - 设置独立 `disabled_reason`。
    - 设置 `failure_count` 到阈值，便于后台直观看到该凭据不可调度。
    - 清理该凭据 session binding、Redis cooldown/rate limit/in-flight 状态。
    - 写入 PgSQL `credential_runtime_state` 和 `credential_events`。
    - 发布凭据变更通知。
  - Provider 新增 `detect_risk_control_error(status, body)`：
    - 识别 JSON `reason` / `error.reason`。
    - 识别 `__type` / `exceptionType` / `code`。
    - 识别文本中的 temporarily suspended / account suspended / account locked。
    - HTTP 423 直接识别为账号锁定。
  - API 和 MCP 失败链路都在 402/401/403/429 分支前先检查风控状态。
- 改动前行为：
  - 明确风控/封禁类上游错误可能被当作普通凭据失败或普通 4xx。
  - 后台只能看到 `TooManyFailures`、普通错误信息或泛化的 403，无法可靠区分 suspended/locked。
- 改动后行为：
  - 明确风控/封禁类错误会禁用该凭据，并显示独立禁用原因。
  - 请求会继续故障转移到其他可用凭据；只有全部凭据都不可用时才向下游返回失败。
  - 普通 bearer token invalid 不会被误判为风控，仍保留原有强制刷新逻辑。
  - 普通 429 仍保持瞬态冷却策略，不会因为本改动被禁用。
- 如何测试：
  - 风控错误识别测试：`CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features detects_risk_controlled_upstream_errors`
  - TokenManager 状态测试：`CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features test_report_risk_controlled_disables_with_specific_reason`
- 测试结果：
  - `detects_risk_controlled_upstream_errors` 已通过。
  - `test_report_risk_controlled_disables_with_specific_reason` 已通过。

## Change 004: 模型 ID 向前兼容，避免未来 Claude 模型被静默降级

- 状态：已完成
- 改动文件：
  - `src/anthropic/converter.rs`
- 改动原因：
  - 外部项目对 `claude-(sonnet|haiku|opus)-...` 形式的未来模型 ID 选择原样透传。
  - 当前项目此前对未知 Sonnet/Opus 4 形态可能映射到已有旧版本，存在“用户以为调用新模型，实际被代理降级”的风险。
- 详细改动：
  - `map_model()` 继续保留当前 Claude Code alias：
    - `opus` / `opusplan` / `best` / `default` -> `claude-opus-4.7`
    - `sonnet` -> `claude-sonnet-4.6`
    - `haiku` -> `claude-haiku-4.5`
  - 已知 Kiro 模型版本继续归一化：
    - Sonnet 4.5 / 4.6
    - Opus 4.5 / 4.6 / 4.7
  - 新增原生 Claude family 判断：
    - `claude-sonnet-*`
    - `claude-opus-*`
    - `claude-haiku-*`
  - 对未知未来原生 Claude family 模型原样透传。
  - `-thinking` 和 `[1m]` 后缀仍会在映射前剥离。
- 改动前行为：
  - 未来 Sonnet/Opus 模型可能被映射成旧的 4.5/4.7。
  - Haiku family 统一映射到 `claude-haiku-4.5`。
- 改动后行为：
  - 已知版本和 alias 行为不变。
  - 未来形态的 Claude 原生模型 ID 不再静默降级，直接传给 Kiro 上游。
  - 非 Claude 模型仍返回 unsupported。
- 如何测试：
  - 运行模型映射测试：`CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features test_map_model_future_claude_models_pass_through`
- 测试结果：
  - `test_map_model_future_claude_models_pass_through` 已通过。
  - `test_map_model_claude_code_aliases` 已通过，确认旧 alias 行为未变。
  - `test_map_model_thinking_suffix_haiku` 已通过，确认 `haiku-thinking` 仍映射到既有 `claude-haiku-4.5` 边界。

## Change 005: 增加 Kiro 上游模型能力同步和 /models 合并目录

- 状态：已完成
- 改动文件：
  - `src/kiro/model/available_models.rs`
  - `src/kiro/model/mod.rs`
  - `src/kiro/endpoint/mod.rs`
  - `src/kiro/endpoint/ide.rs`
  - `src/kiro/provider.rs`
  - `src/anthropic/model_capabilities.rs`
  - `src/anthropic/mod.rs`
  - `src/anthropic/middleware.rs`
  - `src/anthropic/router.rs`
  - `src/anthropic/handlers.rs`
  - `src/storage/postgres.rs`
  - `src/admin/service.rs`
  - `src/admin/handlers.rs`
  - `src/admin/router.rs`
  - `src/main.rs`
  - `admin-ui/src/types/api.ts`
  - `admin-ui/src/api/usage.ts`
  - `admin-ui/src/hooks/use-usage.ts`
  - `admin-ui/src/components/model-pricing-panel.tsx`
- 改动原因：
  - 外部项目会调用 Kiro `ListAvailableModels` 获取真实模型能力和 token limits。
  - 当前项目 `/v1/models` 只有静态硬编码列表，新模型或上下文窗口变化需要发版才能反映。
  - 用户要求结合实际情况优化和能力补齐，并且不能让模型计价/能力影响调度。
- 详细改动：
  - 新增 Kiro `ListAvailableModels` 响应类型：
    - `KiroAvailableModelsResponse`
    - `KiroAvailableModel`
    - `KiroModelTokenLimits`
    - `KiroModelPromptCaching`
  - 扩展 `KiroEndpoint`：
    - `models_url(ctx, next_token)`
    - `decorate_models(req, ctx)`
  - `IdeEndpoint` 实现 `GET https://q.{region}.amazonaws.com/ListAvailableModels`：
    - `origin=AI_EDITOR`
    - `maxResults=50`
    - 有 `profileArn` 时带 `profileArn`
    - 分页时带 `nextToken`
    - 使用与 Kiro IDE 类似的 UA、Authorization、profile header。
  - `KiroProvider::list_available_models()`：
    - 逐个凭据尝试同步模型能力。
    - 使用 `acquire_context_for_credential` 获取 token。
    - 不占用请求并发 lease。
    - 不写入请求成功/失败调度状态。
    - 不禁用凭据。
    - 所有凭据失败时返回最后错误给上层记录状态。
  - 新增 `ModelCapabilitiesCatalog`：
    - 内置静态保底模型目录。
    - 支持从 Kiro 同步结果合并模型能力。
    - 支持导出 Anthropic `/models` 响应模型列表。
    - 支持记录同步错误但保留现有目录。
  - `/v1/models`、`/cc/v1/models`、`/ha/v1/models`、`/na/v1/models` 改为从共享 `ModelCapabilitiesCatalog` 返回：
    - 没有同步结果时返回静态保底。
    - 有同步结果时合并静态保底和上游模型。
  - PgSQL 新增表：
    - `model_capabilities`
    - `model_capabilities_sync_status`
  - Admin API 新增：
    - `GET /api/admin/model-capabilities`
    - `POST /api/admin/model-capabilities/sync`
  - Admin UI 模型页新增：
    - 模型能力状态卡片。
    - 手动同步模型能力按钮。
    - 模型能力表：模型、显示名、输入上限、输出上限、缓存能力、输入类型。
  - 启动流程新增：
    - 先从 PgSQL 加载模型能力目录。
    - Provider 初始化后异步同步 Kiro 上游模型能力。
    - 同步失败只写日志和状态，不阻塞启动、不影响调度。
- 改动前行为：
  - `/models` 返回 handler 内硬编码模型列表。
  - 后台只有模型价格同步，没有 Kiro 模型能力同步。
  - 新模型发布后只能依赖代码更新。
- 改动后行为：
  - `/models` 总是有静态保底。
  - 如果 Kiro `ListAvailableModels` 同步成功，`/models` 会包含同步到的新模型。
  - 后台可查看和手动同步模型能力。
  - 模型能力同步失败不会影响 `/messages`、凭据调度、计价或缓存上报。
- 如何测试：
  - 模型能力 catalog 单测：`CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features sync_from_kiro_models_merges_with_static_fallback`
  - Admin UI 构建：`pnpm --dir admin-ui build`
  - PgSQL 持久化集成测试包含在全量 Rust 测试中：`postgres_persists_runtime_config_credentials_stats_usage_and_pricing`
  - 最终全量 Rust 测试：`CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features`
- 测试结果：
  - `sync_from_kiro_models_merges_with_static_fallback` 已通过。
  - `pnpm --dir admin-ui build` 已通过。
  - 全量 Rust 测试已通过：`CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features`，结果 `347 passed; 0 failed`。

## Change 006: 模型能力同步按实际凭据 ID 遍历，避免删除凭据后跳号漏同步

- 状态：已完成
- 改动文件：
  - `src/kiro/provider.rs`
- 改动原因：
  - 新增模型能力同步初版按 `1..=total_count` 遍历凭据。
  - 当前项目凭据存储在 PgSQL，删除/导入后凭据 ID 可能不连续，按数量推导 ID 会漏掉真实存在的凭据。
- 详细改动：
  - `KiroProvider::list_available_models()` 改为从 `token_manager.snapshot().entries` 读取实际凭据 ID。
  - 只遍历未禁用凭据。
  - 仍然保持模型能力同步不写入调度失败、不禁用凭据、不占用请求并发 lease。
- 改动前行为：
  - 如果当前有 2 个凭据但 ID 是 7、8，同步会尝试 1、2，导致拿不到真实凭据。
- 改动后行为：
  - 同步始终按实际未禁用凭据 ID 尝试，适配 PgSQL 自增 ID 和软删除后的跳号。
- 如何测试：
  - 编译级覆盖：`CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features sync_from_kiro_models_merges_with_static_fallback`
  - 最终全量 Rust 测试：`CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features`
- 测试结果：
  - `sync_from_kiro_models_merges_with_static_fallback` 已通过。
  - 全量 Rust 测试已通过：`CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features`，结果 `347 passed; 0 failed`。

## Change 007: 运行 Rust 格式化，收敛代码风格

- 状态：已完成
- 改动文件：
  - `src/admin/router.rs`
  - `src/anthropic/mod.rs`
  - `src/anthropic/model_capabilities.rs`
  - `src/anthropic/stream.rs`
  - `src/kiro/endpoint/ide.rs`
  - `src/kiro/model/mod.rs`
  - `src/main.rs`
  - `src/storage/postgres.rs`
- 改动原因：
  - `cargo fmt --check` 发现新增代码存在格式差异。
  - 保持仓库 Rust 代码格式统一，降低后续 review 噪音。
- 详细改动：
  - 仅由 `cargo fmt` 调整 import 顺序、换行和长表达式排版。
  - 没有改变任何字段、函数、路由、SQL 或运行时逻辑。
- 改动前行为：
  - 运行 `cargo fmt --check` 会失败。
- 改动后行为：
  - Rust 源码符合 `rustfmt` 格式。
  - 运行时行为不变。
- 如何测试：
  - 格式检查：`cargo fmt --check`
  - 最终全量 Rust 测试：`CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features`
- 测试结果：
  - `cargo fmt --check` 已通过。
  - 全量 Rust 测试已通过：`CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features`，结果 `347 passed; 0 failed`。

## Change 008: 非流式响应同步补齐新增 Kiro 事件兼容

- 状态：已完成
- 改动文件：
  - `src/anthropic/handlers.rs`
- 改动原因：
  - Change 002 先补齐了流式 `StreamContext` 对 `messageMetadataEvent.tokenUsage`、`meteringEvent.usage`、`codeEvent.content` 的处理。
  - 复查时发现非流式响应解析仍只消费 `assistantResponseEvent`、`metadataEvent`、`reasoningContentEvent`、`toolUseEvent` 等旧事件，新增事件在非流式路径没有完全生效。
- 详细改动：
  - 非流式解析收到 `codeEvent.content` 时追加到 assistant 文本。
  - 非流式解析收到 `messageMetadataEvent.tokenUsage` 时更新权威 `metadata_usage`。
  - 非流式解析收到 `meteringEvent.usage` 时输出 debug 日志，仍不参与调度、不参与计费硬逻辑。
  - 非流式 `metadataEvent.tokenUsage` 增加与流式一致的 debug 字段日志。
- 改动前行为：
  - 流式请求能使用 `messageMetadataEvent.tokenUsage` 覆盖最终 usage。
  - 非流式请求遇到同一事件时不会更新 usage。
  - 非流式请求遇到 `codeEvent` 不会输出对应文本内容。
- 改动后行为：
  - 流式和非流式对这三类 Kiro 事件的语义一致。
  - usage 优先级仍保持“真实 metadata/messageMetadata 优先，本地 high-cache 只做 fallback/下游投影”。
  - 不改变 `/v1`、`/cc/v1`、`/ha/v1`、`/na/v1` 路径级缓存上报策略。
- 如何测试：
  - 格式检查：`cargo fmt --check`
  - 事件解析相关单测：`CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features message_metadata_usage_deserializes_token_usage test_message_metadata_usage_overrides_final_usage test_code_event_is_forwarded_as_text_content`
  - 最终全量 Rust 测试：`CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features`
- 测试结果：
  - `cargo fmt --check` 已通过。
  - 全量 Rust 测试已通过：`CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features`，结果 `347 passed; 0 failed`。

## Change 009: 风控禁用分支不再额外累计会话软失败

- 状态：已完成
- 改动文件：
  - `src/kiro/provider.rs`
- 改动原因：
  - 风控/暂停/锁定类错误已经通过 `report_risk_controlled()` 立即禁用凭据、清理 session binding 和调度状态。
  - 复查时发现 API 风控分支随后还调用了 `maybe_exclude_after_soft_failure()`，会把已禁用的风控账号再计入同会话软失败排除，语义重复且污染会话软失败计数。
- 详细改动：
  - 移除 API 风控分支中的 `maybe_exclude_after_soft_failure()` 调用。
  - 保留 `unbind_session_if_bound_to()`、`report_risk_controlled()`、`finish_attempt()` 和故障转移逻辑。
- 改动前行为：
  - 上游明确风控时，凭据会被禁用，同时本次会话还可能记录一次软失败。
- 改动后行为：
  - 上游明确风控时只走风控禁用语义，不再叠加瞬态 429/408/5xx 用的软失败排除语义。
  - 普通 429/408/5xx 的软失败和排队/冷却逻辑不变。
- 如何测试：
  - 风控错误识别测试：`CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features detects_risk_controlled_upstream_errors`
  - 风控禁用状态测试：`CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features test_report_risk_controlled_disables_with_specific_reason`
  - 最终全量 Rust 测试：`CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features`
- 测试结果：
  - 全量 Rust 测试已通过：`CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features`，结果 `347 passed; 0 failed`。
