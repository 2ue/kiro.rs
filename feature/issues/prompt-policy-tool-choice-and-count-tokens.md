# Prompt Policy, Tool Choice, And Count Tokens

Status: `backend-and-ui-total-master-focused-pass / browser-cli-count-tokens-pending`

Severity: P0/P1

Related cases: A06-A07, D01-D05, F05

## 问题与影响

UI 的“启用提示词引导”历史上只控制 language/task/custom system prompt；converter 和 parsed-body pipeline 仍可能在 master OFF 时注入 tool-choice、thinking、Write/Edit 分块兼容提示，或按模型后缀/文本信号自动补 thinking。用户关闭总开关后仍会看到代理新增的协议提示，开关语义与实际行为不一致。

结构化 `tool_choice` 过滤和客户端已经提供的 `thinking`/`output_config` 不能被总开关删除，否则会改变 Anthropic 请求语义。真正需要总开关控制的是代理新增内容：所有 operator/compatibility prompt，以及自动 thinking 触发。

## 根因与所有权

根因是“新增提示”和“结构化转换”没有明确分层：request-level `prompt_steering` 只在 `prompt_steering.rs` 生效，converter 的四个注入 helper 与自动 thinking pipeline 没有读取同一个总开关。

当前所有权为：

- 总提示开关：`promptSteering.enabled` 控制语言、任务质量、custom、tool-choice、thinking、Write/Edit 分块等代理新增提示，以及模型后缀/文本信号触发的自动 thinking。
- 子开关：在总开关开启时进一步控制各提示位置。
- 结构化协议：`bodyConversion.toolChoiceSteering` 负责工具过滤；客户端显式 `tool_choice`、`thinking`、`output_config` 不删除，原生 reasoning 仍按能力合同映射。
- count_tokens：只复用同一 request-level operator prompt 总开关和 scope，不伪造结构化输入。

hash 工具名只可能出现在旧 task prompt 内容里，不是开关耦合的根因。

## 稳定复现

1. 设置 `promptSteering.enabled=false`，保持三个 prompt 子开关和 `bodyConversion.*` 为 true，发送带 `tool_choice` 的请求并使用 `Write`/`Edit` 工具；修复前仍会出现 `<tool_choice>`、分块 system/description 和 thinking 兼容提示。
2. 使用 `-thinking` 模型名或 `ultrathink` 文本，关闭总开关；修复前会自动补 `thinking`，修复后不得新增，客户端显式 `thinking` 则必须保留。
3. 发送显式 `thinking` + `output_config.effort=max`，关闭总开关；最终 native wire 仍应保留客户端能力映射，不得静默删除或降级。
4. 对相同 request 分别调用 messages 与 count_tokens，记录总开关 ON/OFF 下实际新增的 operator prompt 和 token 口径。

当前聚焦入口：

```bash
cargo test operator_prompt_master_disables_all_proxy_prompt_additions -- --nocapture
cargo test operator_prompt_master_off_preserves_structured_tool_filtering -- --nocapture
cargo test disabled_prompt_master_suppresses_automatic_thinking_additions -- --nocapture
cargo test runtime_config_migration_updates_legacy_default_task_quality_prompt_only -- --nocapture
```

完整门禁为 master ON/OFF x scope x local/external profile x tool_choice x thinking/chunked 子开关，每格 5 轮；再执行两 UI 交叉 save-refresh，而不是用 build 代替配置 round trip。

## 方案比较与选定方案

只改 UI 文案不能阻止 converter 和 parsed-body pipeline 继续注入。选定方案是：

- `promptSteering.enabled` 是所有代理新增提示的总开关，关闭时四类 prompt injection 和自动 thinking trigger 均为 false。
- 结构化 `tool_choice` 过滤、schema/name mapping、客户端显式 thinking/output_config 映射继续遵守请求语义；总开关不删除原始字段。
- scope、external、count_tokens 对 request-level operator prompt 使用一套明确规则；UI 明确说明“总开关控制新增提示，不删除结构化语义”。
- 配置迁移删除具体 `readHash/editHash/bashHash` 内置 task prompt，但只精确替换已知默认字节；带用户追加、空白变化或自定义内容的配置不自动改写。
- 两 UI 分区编辑并独立 round-trip promptSteering 与 bodyConversion，只保存用户实际编辑的字段权威。

## 当前实现与分批证据

- converter 四个注入 helper 和 parsed-body 自动 thinking trigger 统一读取 `promptSteering.enabled`；master OFF 时不注入 chunked/thinking/tool-choice 兼容提示。
- 结构化 `tool_choice` 过滤仍由 `bodyConversion.toolChoiceSteering` 决定；master OFF 的预期仍为 `none=0`、`any=N`、`named=1`。
- task prompt migration 使用旧内置文本的精确字节匹配；带自定义追加或前导空白的配置不迁移。
- 两套 UI 保存 normalization 已不再把 prompt 子开关镜像覆盖 `bodyConversion.*`，并分别展示 operator prompt 与协议转换配置；两套 production build 已通过。
- `operator_prompt_master_disables_all_proxy_prompt_additions`、`operator_prompt_master_off_preserves_structured_tool_filtering`、`disabled_prompt_master_suppresses_automatic_thinking_additions` 聚焦测试已通过；Rust prompt 组 7/7 通过。
- operator prompt 的 endpoint/scope/strict/count_tokens 矩阵 6/6 通过，其中正交格每格 5 轮；master OFF 的 protocol 子开关、`none/any/named` 和关闭 prompt 子开关仍保留 named 过滤三项均通过。
- Config JSON 对 master、三个 `bodyConversion` 能力和三个 prompt 子开关执行全部 128 组合 x 5 轮（640 次）round-trip，未发生字段覆盖。
- [`feature/tests/prompt-control-independence.mjs`](../tests/prompt-control-independence.mjs) 解析两套 TSX setter/save 路径并通过 2/2 源码合同；共享 API 类型 167 个一致。
- 2026-07-21 浏览器检查 `127.0.0.1:9025/admin/` 确认 Config 页已按源码分层暴露 prompt steering、tool-choice、thinking 与 payload controls；页面文案也明确把总开关描述为只控制新增提示，不删除结构化语义。这个证据只补齐 UI 可见性，`count_tokens` 端到端仍待继续复核。
- 继续复扫发现 Rust 默认 task prompt 已去除具体内部指纹，但两套 UI 默认仍保留 `Tool results...`、函数标签和 hash 示例；三方 parity 红测分别命中 6 类 marker。修复后 [`prompt-default-parity.mjs`](../tests/prompt-default-parity.mjs) 证明三方 exact bytes 一致且 marker 0，两套 production build 重新通过。
- runtime config migration 升至 v6：覆盖“已在 v5 被旧 UI 再次持久化”的 exact legacy V3 默认；只做逐字节相等替换，suffix、前导空白和任意 operator 自定义 prompt 均保持。聚焦 migration 测试通过。

隔离 PostgreSQL 的真实 Admin API save-refresh、两 UI 浏览器交叉 round-trip、external raw/normalized 和真实 CLI thinking/tool 行为仍未执行，因此本专题不能标记为 verified-fixed。源码合同不能替代浏览器 gate。

## 验收、性能、回滚与残余风险

master ON/OFF x scope x endpoint x tool_choice x count_tokens 正交矩阵每格至少 5 轮；none/any/named 的工具数始终为 0/N/1。两个 UI 分别保存后独立字段不变，raw/normalized external 行为符合合同，真实 CLI thinking/tool/usage 不因 operator master 意外改变。

关闭或打开 operator master 不得增加额外 body serialize/全历史扫描；结构化 tool filtering 只能与工具数线性相关。兼容风险主要是旧管理员把 master 当成只影响语言提示的开关；UI 现在明确总开关会抑制新增兼容提示，但不会删除客户端结构化字段。

回滚可以恢复旧 operator prompt 文案，但不得让 master 再控制结构化 tool_choice、thinking/chunked 协议能力。残余风险包括：旧数据库中被用户修改过但恰好字节等于历史默认的 prompt 无法区分来源；external raw profile 的 prompt 合同和 count_tokens 模拟仍需端到端证据；未来新增 protocol 子开关若再次嵌套到 operator master，可能重引入同类问题，因此正交 config test 应成为发布门禁。
