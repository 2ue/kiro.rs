# P26 合并模型目录后，别名解析到只有部分账号支持的模型，请求被派到不支持该模型的账号

Status: partially-mitigated-in-f965f1d (one alternate retry + model cooldown); catalog-aware dispatch open
Severity: Medium
Area: request + scheduling
Discovered: 2026-09-29，导入支持 Opus/1M 的新账号后做回归时发现
Verified-against: f965f1d (2026-09-29)

## 问题与影响

2026-09-29 在测试实例导入了一个新账号 `#313`。它的 `ListAvailableModels` 返回了更新的模型：`claude-sonnet-5`、`claude-opus-5`、`claude-opus-5.5`，以及 `claude-opus-4.8` 等。导入之后，真实 Claude Code CLI 执行 `claude --model sonnet-thinking` 时，返回 `API Error: 400 The request body is invalid...`，而导入之前同一命令是正常的。

usage 记录如下：

- 请求模型 `sonnet-thinking` 被解析为上游模型 `claude-sonnet-5`。
- 调度器把请求派给了账号 `#303`，该账号的目录中没有 `claude-sonnet-5`。
- Kiro 返回 400，分类为 `model_invalid_bad_request`，只尝试了 1 次就直接失败。

影响：只要池中有一个账号的目录带来了更新的模型，所有解析到这个模型的别名请求（如 `sonnet`、`sonnet-thinking`）都有很大概率被派到不支持它的账号，导致 Claude Code 当前 turn 直接失败。

## 官方协议对照

- Claude Code 协议：`sonnet`、`opus` 这类别名表示"当前最新的该系列模型"，所以把别名解析到目录中最新的 sonnet 符合语义。
- Kiro 协议：模型是否可用取决于账号。同一个模型 ID，在一个账号上可用，在另一个账号上会返回 400 invalid model。

## 源码链与根因

1. 模型目录按凭据 cohort 发现后取并集（`src/kiro/provider.rs` 的 `merge_model_discovery_catalogs`），只要有一个账号有该模型，别名解析就会命中它。
2. 调度时的模型过滤以凭据的 `supported_models` 为依据。`supported_models` 为空的凭据（常见情况）被视为支持所有模型（`src/kiro/model/credentials.rs` 的 `supports_model`），所以不支持该模型的账号也会被选中。
3. 400 `model_invalid_bad_request` 原本属于确定性错误，不会换号（只有 404 的 invalid model 和 400 的 model unavailable 会换号）。

根因：别名解析依据的是全池并集目录，派发时却没有按账号级目录过滤，两者口径不一致。

## 复现

### 最小复现（单测）

`src/kiro/provider.rs` 的 `bad_request_retry_matrix`（fake bad request server 场景 `invalid_model`）：凭据池大于 1 时，400 invalid model 现在会换一个账号重试一次，共 2 次命中，最终仍然失败并返回原因。

### 端到端复现

1. 在测试实例（`127.0.0.1:19023`）导入一个目录中包含新模型、而其他账号没有该模型的账号。
2. 使用隔离的 `HOME` 和 `CLAUDE_CONFIG_DIR` 执行 `claude -p --model sonnet-thinking "Reply with only: alias-think-ok"`。
3. 修复前：400，usage 中上游模型为 `claude-sonnet-5`，账号不支持该模型，只有 1 次 attempt。

## 修复方案

### 候选方案

- A（已实现，缓解）：400 invalid model 在第一次失败后换一个账号重试一次，同时给拒绝的账号记录按模型的冷却，后续请求会避开它。重试只做一次，拼写错误这类真正无效的模型最多多打一次上游。
- B（推荐的根治方案，未实现）：按账号记录各自的发现目录，派发时优先选择目录中包含该模型的账号。别名解析也应该只考虑当前可调度账号的目录。
- C（运维侧）：对池中账号执行 supported models 同步（Admin `/credentials/{id}/supported-models/sync`），让调度器拿到明确的支持列表。

### 推荐方案

短期采用 A 加 C；中期实现 B。B 会改动调度器的候选过滤，需要单独做调度矩阵回归（参见 `feature/issues/scheduler-architecture-analysis-purpose-and-plan.md`）。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)。

- 单测：provider 全部测试通过，包括上面的重试矩阵。
- 真实上游验收（B 落地后）：导入的新账号只要支持新模型，`sonnet` 和 `sonnet-thinking` 别名请求就应该 100% 派到支持该模型的账号，并返回 200。

## 兼容性与风险

- A 只影响 400 invalid model 这一类错误，对真正无效的模型最多多一次上游调用，RPM 放大有上限。
- 如果池中只有极少数账号支持新模型，一次重试仍然可能落到不支持的账号上。冷却会逐渐让调度避开这些账号，但前几个请求仍可能失败，根治要靠 B。
