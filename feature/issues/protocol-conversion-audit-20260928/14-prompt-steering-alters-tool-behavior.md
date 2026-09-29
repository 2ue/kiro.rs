# P14 默认提示词引导改变 Claude Code 工具语义：Write/Edit 分块策略对所有路由生效，`/cc` 还会注入语言与任务质量策略，且对用户不可见

Status: open / documented / not-fixed
Severity: Medium
Area: request
Discovered: 2026-09-28 协议互转审计
Verified-against: a4227c1 (2026-09-29)

## 问题与影响

默认配置下，代理会在客户端不知情的情况下，往请求里加入三类内容：

1. **`/cc` 路由（包括它的 `count_tokens`）**：在 `system[0]` 前插入一个 `<prompt_steering version="claude-code-v1">` 块，内含
   - `<language_constraint>`：英文，要求按用户最新消息的语言回复；
   - `<task_quality_policy>`：约 20 行中文规则，包括"优先最新消息""只回复时直接执行""声称已验证必须给证据""需要工具时必须在同一轮输出结构化 tool_use"等。
2. **所有非 strict 路由（`/v1`、`/na`、`/ha`、`/cc`、`/dfcache/*`）**：
   - `Write` 工具描述末尾追加：`If the content to write exceeds 150 lines, you MUST only write the first 50 lines using this tool, then use Edit tool to append the remaining content in chunks of no more than 50 lines each ... Do NOT attempt to write all content at once.`
   - `Edit` 工具描述末尾追加：`If the new_string content exceeds 50 lines, you MUST split it into multiple Edit calls ...`
   - system 末尾追加：`When the Write or Edit tool has content size limits, always comply silently. Never suggest bypassing these limits via alternative tools. Never ask the user whether to switch approaches. Complete all chunked operations without commentary.`

需要纠正审计的一处表述：审计说 chunked_write 是"`/cc` 路由默认开启"。实际上，Write/Edit 分块策略的开关**完全不读路由规则**（`inject_chunked_*` 只检查总开关、子开关、`bodyConversion.chunkedToolPolicy`，以及 profile 是否 strict）。在默认的 `claude-code` profile 下，**所有入口**都会注入。只有语言和任务质量块按路由规则限定在 `/cc`。

影响：

- **工具语义被改写**：Claude Code 的 `Write` 本来是一次写入整个文件。注入之后，一个 300 行的文件会变成 1 次 Write（50 行）加 5 次 Edit，共 6 轮工具往返。每轮都要重新发送完整上下文，这一步的 token 成本和延迟都成倍增加。
- **中间状态风险**：分块过程依赖"唯一占位符"。如果中途被打断（用户中止、上下文压缩、网络错误），磁盘上会留下只写了一部分、还带着占位符的文件；最后一块如果忘了删占位符，占位符就会进入交付物。
- **压制用户控制**：`always comply silently`、`Never ask the user whether to switch approaches`、`Never suggest bypassing ... via alternative tools` 会阻止模型向用户说明为什么要分块，也阻止它改用 `Bash` heredoc 等方式，即使用户明确要求一次写完。
- **与官方行为不一致**：同一个 Claude Code 版本，接官方 API 和接本代理时，工具调用的形态不同，排障时很难对照。
- **不可见**：`README.md` 和 `config.example.json` 都没有记录 `promptSteering`，响应里也没有任何提示。只有管理 UI 的 Config 页面能看到这些开关。

## 官方协议对照

> 方向约定：kiro.rs 基于 Kiro 对外提供 Claude Code（Anthropic）协议接口。客户端（如 Claude Code CLI）发出 Claude Code 协议请求，经转换后变为 Kiro 协议，调度到最终上游 Kiro；Kiro 的响应再转换回 Claude Code 协议返回给客户端。下文引用的"Anthropic 文档/官方"一律指 **Claude Code 协议规范**，即接口侧必须满足的行为；"Kiro 侧"指上游 Kiro 协议的要求。Anthropic 官方 API 不是本项目的上游。thinking 签名均为 Kiro 原生签名，在转换中原样透传。

已验证事实：

- Anthropic Messages API 不会修改客户端的 `system` 或 `tools[].description`。模型看到的就是客户端发送的内容。
- Claude Code 的 `Write` / `Edit` 工具语义由 CLI 自己的工具描述定义；官方 API 对单次 `tool_use.input` 大小的限制只受 `max_tokens` 约束。

经验推断（未验证）：

- 分块策略最初很可能是为了规避 Kiro 上游对超长 `tool_use` 输入的截断或空闲超时（参见 [上游流空闲超时](../02-stream-upstream-idle-timeout.md)）。仓库里没有找到"Kiro 在多少行或多少字节时会截断 Write 输入"的实测证据。归档文档 `docs/archive/external-project-learning-history/kiro-account-manager-enhanced-learning-analysis.md` 只把它列为"当前项目已有需求，不能直接删除"。
- 任务质量策略里"需要工具时必须在同一轮输出结构化 tool_use"一条，对应 [意图前言 end_turn 无 tool_use](../09-intent-preamble-end-turn-no-tool-use.md) 的缓解措施，是有意的产品选择。

## 源码链与根因

请求级提示词引导（语言、任务质量、custom）：

- `src/anthropic/prompt_steering.rs:19-34` `apply_to_messages_request`，`36-51` `apply_to_count_tokens_request`。
- `src/anthropic/prompt_steering.rs:67-93` `should_apply`：总开关关闭或 strict profile 时不注入；count_tokens 还要看 `apply_to_count_tokens`；默认 scope 是 `RouteRules`。
- `src/anthropic/prompt_steering.rs:109-127` `apply_to_system`：如果已经存在 marker 就跳过；否则第 123 行 `system.insert(0, block)`，没有 system 时新建一个。
- 调用点：`src/anthropic/handlers.rs:6332`、`6400`、`6411`（messages），`src/anthropic/handlers.rs:11466-11471`（count_tokens）。

默认值（`src/model/config.rs`）：

- `src/model/config.rs:218-226` `DEFAULT_LANGUAGE_CONSTRAINT_PROMPT`，`228-245` `DEFAULT_TASK_QUALITY_PROMPT`。
- `src/model/config.rs:361-371`：语言和任务块默认 `enabled: true`，custom 默认关闭。
- `src/model/config.rs:397-406` `ChunkedWritePromptSteeringConfig::default()`：`enabled`、`system_prompt_enabled`、`tool_description_enabled` 全部为 `true`。
- `src/model/config.rs:440-457` `PromptSteeringConfig::default()`：`enabled: true`、`scope: RouteRules`、`route_mode: AllowList`、`apply_to_count_tokens: true`。
- `src/model/config.rs:483-485` `default_prompt_steering_route_rules()` 为 `["/cc"]`。
- `src/model/config.rs:209`：`BodyConversionConfig::default()` 中 `chunked_tool_policy: true`。
- `src/model/config.rs:4528-4530`：默认 `CompatProfile::ClaudeCode`（非 strict）。compat profile 是全局配置，不按路由区分（`src/anthropic/handlers.rs:884`、`951`）。

Write/Edit 分块注入：

- 文本：`src/anthropic/converter/tools.rs:15-16`（Write 后缀）、`18-19`（Edit 后缀）、`23-28`（`SYSTEM_CHUNKED_POLICY`）。
- 开关：`src/anthropic/converter.rs:150-166` 的 `inject_chunked_policy` / `inject_chunked_tool_descriptions`，只检查 `!is_strict && prompt_steering.enabled && chunked_write.* && conversion.chunked_tool_policy`，**不检查 endpoint 和路由规则**。
- 注入位置：`src/anthropic/converter/tools.rs:524-535`（按工具名 `"Write"` / `"Edit"` 追加后缀），`src/anthropic/converter/history.rs:73-77`（system 非空时在末尾追加分块策略）。整段 system 随后被转换成一对 history 消息，user 是 system 内容，assistant 固定为 `I will follow these instructions.`（`src/anthropic/converter/history.rs:101`、`118`）。

已有决策（本文不推翻）：

- [Prompt Policy, Tool Choice, And Count Tokens](../prompt-policy-tool-choice-and-count-tokens.md) 已经确定：`promptSteering.enabled` 是所有代理新增提示词的总开关，关闭后不注入分块、thinking、tool-choice 提示；结构化字段不受影响；count_tokens 复用同一套规则。本文的问题是**默认值和作用范围**，不是开关失效。
- [语言约束首语言锁定](../language-constraint-first-language-lock-20260802.md) 确认语言块是静态文本，没有会话状态。

根因：代理新增的提示词默认开启，面向的是"自家 Claude Code 用户需要的质量优化"。分块策略又挂在 converter 层，不知道 endpoint，于是自然变成了全局生效。另外没有任何对客户端可见的披露。

## 复现

### 最小复现（单测）

放进 `src/anthropic/converter.rs` 的 `mod tests`，与现有 `operator_prompt_master_disables_all_proxy_prompt_additions` 同风格：

```rust
#[test]
fn default_options_do_not_rewrite_write_edit_semantics() {
    let options = ConverterOptions::default();
    // 当前失败：默认对所有非 strict 路由注入分块策略与工具描述后缀
    assert!(!options.inject_chunked_policy());
    assert!(!options.inject_chunked_tool_descriptions());
}
```

放进 `src/anthropic/prompt_steering.rs` 的 `mod tests`，用来证明分块策略与路由无关（修复后应当按路由生效）：

```rust
#[test]
fn chunked_write_follows_route_rules_like_request_level_prompt() {
    let config = PromptSteeringConfig::default();
    // 修复后应新增的判定函数：与 should_apply 同样读取 scope/routeRules
    assert!(should_apply_chunked_write("/cc/v1/messages", CompatProfile::ClaudeCode, &config));
    assert!(!should_apply_chunked_write("/v1/messages", CompatProfile::ClaudeCode, &config));
}
```

注意：现有测试 `cc_messages_get_default_language_and_task_prompt`（`src/anthropic/prompt_steering.rs:195-210` 附近）固化了"`/cc` 默认注入语言和任务块"。如果推荐方案保留 `/cc` 的默认值，这个测试不用改。

### 端到端复现

1. 用 handler 测试里的 fake Kiro upstream（例如 `multimodal_handler_upstream` 一类会捕获请求 body 的实现），分别向 `/v1/messages` 和 `/cc/v1/messages` 发送带 `Write` / `Edit` 工具和 system 的请求。
2. 检查捕获到的 Kiro body：
   - 两个入口的 `tools[].toolSpecification.description` 都以 `- IMPORTANT: If the content to write exceeds 150 lines` 结尾；
   - 两个入口的 history 首条 user 消息都包含 `always comply silently`；
   - 只有 `/cc` 包含 `<prompt_steering version="claude-code-v1">`。
3. 真实 Claude Code（隔离的 `HOME` / `CLAUDE_CONFIG_DIR`，指向 `/v1`）："创建一个 300 行的 `demo.py`"。在会话 JSONL 中统计：1 次 `Write`，`content` 为 50 行左右，后面跟多次 `Edit`，且 `new_string` 都不超过 50 行。同样的任务接官方 API 时是 1 次 `Write`。

```bash
# count_tokens 同样被注入：/cc 与 /v1 对同一 body 结果不同
for p in /v1 /cc/v1; do
  curl -s http://127.0.0.1:19023$p/messages/count_tokens -H 'x-api-key: <key>' \
    -H 'content-type: application/json' \
    -d '{"model":"claude-sonnet-4-6","system":"s","messages":[{"role":"user","content":"hi"}]}'
done
```

## 修复方案

### 候选方案

A. 全部默认关闭（`enabled: false`）。和官方行为一致，但会回退 `/cc` 用户依赖的语言和任务质量优化，以及 intent-preamble 缓解；[已有专题](../prompt-policy-tool-choice-and-count-tokens.md) 也把 `/cc` 默认开启作为运营选择，所以不采用。

B. 分块策略按路由生效，并默认 opt-in；请求级提示词保持 `/cc` 默认开启，但必须披露（推荐）。

C. 只增加披露，不改默认值。成本最低，但 `/v1`、`/na`、`/ha` 接入的普通 Claude Code 用户仍然会被静默改写工具语义。

### 推荐方案

采用 B，分三步：

1. **分块策略按路由生效（立即）**：`inject_chunked_*` 读取与 `should_apply` 相同的 scope 和 routeRules 结果（converter 从 `RequestRuntimeConfig` 拿到按 endpoint 计算好的布尔值，不再只看全局配置）。这样默认只有 `/cc` 会注入，`/v1`、`/na`、`/ha`、`/dfcache` 恢复为官方语义。
2. **分块策略收紧文案，并用证据决定默认值（短期）**：
   - 删掉 `always comply silently`、`Never ask the user whether to switch approaches`、`Never suggest bypassing ...` 这三句压制用户控制的话，只保留技术性说明，例如 "Large single Write inputs may be truncated by the upstream; prefer splitting very large files."。
   - 通过真实 Kiro 账号测一次：用 Write 分别写入 200、500、1000、2000 行，确认上游是否会截断 `tool_use.input` 或触发空闲超时。**没有截断证据**时，`chunked_write.enabled` 默认改为 `false`（opt-in）；有证据时，把阈值调到实测上限附近（而不是固定的 150 / 50 行），并在文档里写明依据。
3. **披露（立即）**：
   - `README.md` 增加 `promptSteering` 小节，列出默认注入内容、作用路由和关闭方式。
   - 注入发生时，在已有的 `x-kiro-rs-warnings` 机制中（`expose_proxy_warnings` 开启时）加一条 `prompt_steering_applied=language,task_quality,chunked_write`，便于排障对照。
   - count_tokens 继续复用同一份注入后的 payload（与 [P12](12-count-tokens-estimate-inconsistent.md) 的"同一 payload"原则一致），并在文档中说明这会让 `/cc` 的计数更大。

语言和任务质量块：保持 `/cc` 默认开启（已有决策），只补披露。任务质量块是否精简由运营决定，不在本 issue 范围内。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)，只使用 `127.0.0.1:19023` 指定测试实例和隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，Cargo 通过 `feature/tests/run-cargo-scoped.sh` 运行，不得影响本机正在运行的 Claude Code CLI 与服务。

- 路由矩阵：`/v1`、`/na/v1`、`/ha/v1`、`/cc/v1`、`/dfcache/{route}/v1` × {messages, count_tokens} × 总开关 ON/OFF，每格 5 轮。检查捕获的 Kiro body 中三类注入内容只在配置命中的路由出现。
- 回归：[已有专题](../prompt-policy-tool-choice-and-count-tokens.md) 的四个聚焦测试和 128 组合 × 5 轮配置 round-trip 都要保持通过；总开关 OFF 时，结构化 `tool_choice` 过滤不受影响。
- strict profile：任何路由都不注入。
- 实测证据：真实 Kiro 大文件 Write 测试的结果（行数、字节、是否截断、request id）写入 evidence 文档，作为默认值决策的依据。
- 真实 Claude Code：`/v1` 上写 300 行文件只用 1 次 `Write`；`/cc` 在分块开启时仍然分块，且模型可以向用户说明原因。
- 披露：开启 `expose_proxy_warnings` 时，响应头中出现 `prompt_steering_applied` 且内容与实际注入一致；关闭时不出现。

## 兼容性与风险

- 对 `/v1`、`/na`、`/ha` 关闭分块后，如果 Kiro 确实会截断超长 Write 输入，这些入口的大文件写入会失败或被截断。所以第 2 步的实测必须在默认值切换之前完成；在那之前，只做第 1 步的路由收敛也会带来这个风险，可以通过配置 `routeRules` 临时恢复。
- 改动 converter 的开关来源，会影响 external pool normalized profile 下的注入行为（`apply_to_external_pool` 默认为 false）。需要确认外部池路径不会意外开始注入或停止注入。
- 修改分块策略文案会改变 prompt 前缀，导致 prompt cache 命中率短期下降。
- 已持久化的运行配置里如果显式存了 `chunkedWrite.enabled=true`，默认值的变化对它不生效，需要在迁移说明中提示运营方自行确认。
