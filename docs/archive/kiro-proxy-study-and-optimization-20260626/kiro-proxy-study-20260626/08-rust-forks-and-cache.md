# Rust fork 与其他轻量项目分析

本篇覆盖：

- `/Users/yuanfeijie/Desktop/procode/kiro-research/pluto2sun__kiro2api`
- `/Users/yuanfeijie/Desktop/procode/kiro-research/TsinHzl__kiro2cc-proxy`
- `/Users/yuanfeijie/Desktop/procode/kiro-research/ndycode__kiro-rs`
- `/Users/yuanfeijie/Desktop/procode/kiro-research/cp-coder9__kiro-gateway`
- 其他低相关轻量 fork

这些项目大多不能整体替代当前项目，但有局部设计值得学习。

## `pluto2sun/kiro2api`

路径：`/Users/yuanfeijie/Desktop/procode/kiro-research/pluto2sun__kiro2api`  
最新提交：`53abfd6`，2026-06-19

### 关键文件

| 文件 | 作用 |
| --- | --- |
| `src/anthropic/true_cache.rs` | true response cache |
| `src/anthropic/input_cache.rs` | input cache savings policy |
| `src/anthropic/converter.rs` | Anthropic 转换 |
| `src/anthropic/stream.rs` | SSE 转换 |
| `src/kiro/token_manager.rs` | 轻量调度 |
| `src/kiro/provider.rs` | Kiro upstream 调用 |

### true response cache

`true_cache.rs` 做的是完整响应缓存：

- 解析 request body。
- 删除 `conversationState.agentContinuationId`。
- 删除 `conversationState.conversationId`。
- 递归归一化 `toolUseId` / `tool_use_id` / tool_use object `id`。
- canonical JSON 后 SHA-256。
- response 缓存在磁盘 fanout 目录。
- TTL 和 max response bytes 限制。

可学习点：

- volatile id 归一化非常有价值。
- tool_use_id 归一化可以用于当前项目 prompt cache fingerprint，而不是直接用于响应缓存。
- cache key 生成应先做结构化 JSON 归一化，不应该字符串替换。

不建议照搬：

- full response cache 对 Kiro/Claude Code 很危险。
- 可能缓存包含时间、文件状态、工具上下文、外部环境的回答。
- 对长会话、工具调用、MCP、WebSearch 不安全。
- 出错时会出现“请求看似成功但实际拿到旧答案”的严重问题。

建议当前项目：

- 暂不做全量 response cache。
- 借鉴 volatile id normalization，用在 prompt cache/high-cache fingerprint。
- 如果未来做 response cache，只允许极窄场景：
  - admin 显式开启。
  - 只对无 tools、无 images、无 web/search、无 file context 的纯文本请求。
  - TTL 很短。
  - usage 标记 `cache_hit=true`。

### input cache savings policy

`input_cache.rs` 有动态策略：

- savings ceiling。
- ceiling jitter。
- savings floor。
- full hit probability。
- scope：all/paid/free。
- free model list normalize。
- 参数用 atomic 存，热路径读不加锁。

当前项目已有 reported usage policy。可学习点：

- 配置热更新可用 atomic/frozen snapshot，避免每次请求锁大对象。
- 免费/付费模型 scope 的设计可用于 usage projection，但必须避免过度复杂。

## `TsinHzl/kiro2cc-proxy`

路径：`/Users/yuanfeijie/Desktop/procode/kiro-research/TsinHzl__kiro2cc-proxy`  
最新提交：`44d3985`，2026-06-26

它与当前项目结构相近，包含：

- `src/kiro/token_manager.rs`
- `src/kiro/provider.rs`
- `src/anthropic/converter.rs`
- `src/anthropic/stream.rs`
- `src/kiro/model/usage_limits.rs`
- `admin-ui` / `user-ui`

价值：

- 可作为同源/近源 fork 的差异参考。
- 可以对比哪些功能被简化、哪些 UI/用户态能力保留。
- 对当前项目不构成“更优架构”参考。

建议：

- 只在需要查某个具体 bug 是否在 fork 中有不同修法时参考。
- 不建议按它重构当前项目。

## `ndycode/kiro-rs`

路径：`/Users/yuanfeijie/Desktop/procode/kiro-research/ndycode__kiro-rs`  
最新提交：`529fc2f`，2026-06-11

### 关键结构

它把 token manager 拆成目录：

- `src/kiro/token_manager/mod.rs`
- `admin.rs`
- `error.rs`
- `persist.rs`
- `refresh.rs`
- `report.rs`
- `selection.rs`
- `stress_tests.rs`
- `tests.rs`

这对当前项目最有参考价值。

当前项目的 `src/kiro/token_manager.rs` 已经过大，后续可以学习这种拆分方向：

- `selection.rs`：策略、打分、候选过滤。
- `capacity.rs`：RPM、并发、lease、queue。
- `cooldown.rs`：失败分类、cooldown/backoff。
- `session.rs`：sticky binding。
- `refresh.rs`：token refresh。
- `admin.rs`：管理 API snapshot/update。
- `persist.rs`：PgSQL/Redis 持久化。
- `report.rs`：success/failure/latency report。

注意：不要直接替换当前文件，应该先无行为变化地迁移测试覆盖，再拆。

## `cp-coder9/kiro-gateway`

路径：`/Users/yuanfeijie/Desktop/procode/kiro-research/cp-coder9__kiro-gateway`  
最新提交：`3d78e0a`，2026-06-25

这是 Python gateway，生产调度能力不如当前项目，但测试组织很值得学习。

### 关键文件

| 文件 | 作用 |
| --- | --- |
| `kiro/converters_openai.py` | OpenAI 转 Kiro |
| `kiro/converters_anthropic.py` | Anthropic 转 Kiro |
| `kiro/parsers.py` | AWS EventStream parser |
| `kiro/streaming_anthropic.py` | Anthropic streaming |
| `kiro/streaming_openai.py` | OpenAI streaming |
| `kiro/thinking_parser.py` | thinking FSM |
| `kiro/payload_guards.py` | payload guard |
| `tests/README.md` | 测试矩阵说明 |
| `tests/conftest.py` | 全局阻断真实网络调用 |

### 测试矩阵

`tests/README.md` 明确测试原则：

- 所有测试隔离真实网络。
- unit/integration 分开。
- 测 converter、parser、streaming、thinking、payload guard、account failover、network errors。
- 任何真实网络调用都失败。

当前项目之前用户指出“单测测试没有意义”，这里的启发不是只做 mock，而是建立分层测试：

- 单元测试：纯函数、payload guard、parser、selection score。
- 模拟上游集成测试：本地 fake Kiro server，覆盖 eventstream、200 JSON、stall、429、400。
- 真实账号 smoke：少量真实请求，只用于 release 前验证。
- 压测：并发/RPM/内存/TTFB。

当前项目需要的是这样的测试体系，而不是只加单元测试。

## 其他轻量项目

`caidaoli/kiro2api`、`hnewcity/KiroaaS` 等更多是轻量部署、桌面包装、基础代理形态参考。它们对当前项目的调度和生产化帮助有限。

可以学习：

- 简单部署说明。
- 用户 onboarding。
- 错误排查文档。

不建议：

- 不要学习其轻量状态存储。
- 不要为了 UI/易用性牺牲现网调度和错误归一化。

## 本篇结论

这些项目最有价值的不是“功能更多”，而是：

- `pluto2sun`：volatile id normalization，谨慎学习，不要直接上 full response cache。
- `ndycode`：Rust 模块拆分方向。
- `cp-coder9`：测试矩阵和网络隔离思路。
- `TsinHzl`：同源 fork 参考，不作为主架构学习对象。
