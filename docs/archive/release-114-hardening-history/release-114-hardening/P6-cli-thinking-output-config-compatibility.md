# P6 - Claude Code CLI / thinking / output_config 兼容性分叉

日期：2026-07-26

## 现象

你当前看到的本地 CLI 报错大意是：

- `output_config is only compatible with adaptive thinking or an omitted thinking field`
- 有时还伴随 `Chat Completions API (/v1/chat/completions) returned 404`

你怀疑的两个方向是：

1. `output_config.effort` 是否被静默压成 `high`。
2. `thinking.type` 是否没有被正确注入成 `adaptive`，或者在某些入口被错误保留成 `disabled`。

这个问题不能只看一个指纹。因为同一组字段在本仓库里至少经过四层：

1. Anthropic/raw 入口解析。
2. Anthropic `MessagesRequest` → Kiro-native 请求构造。
3. Kiro-native JSON 序列化前的兼容归一化。
4. CLI / IDE 入口对 `additionalModelRequestFields` 的二次变换。

## 当前已确认的行为

我在当前工作树上已经复核了这几条主路径：

- `src/anthropic/converter.rs`
- `src/anthropic/request_facts.rs`
- `src/anthropic/payload_guard.rs`
- `src/kiro/model/requests/kiro.rs`
- `src/kiro/endpoint/cli.rs`
- `src/kiro/endpoint/ide.rs`

当前代码已经做到以下几点：

- 显式 `output_config.effort=max` 没有被压成 `high`。
- `thinking.type=adaptive` 的 native wire 仍会保留。
- `thinking.type=disabled` 且存在显式 `output_config.effort` 时，Kiro-native 序列化会去掉不兼容的 sibling `thinking`，只保留 `output_config`。
- `thinking.type=disabled` 且没有显式 effort 时，不会偷偷生成 native reasoning fields。
- CLI 与 IDE 的 JSON 变换都走同一个兼容归一化函数，不是两套互相漂移的规则。

## 真实验证

我做了两层验证：

1. Rust 单测矩阵：
   - `cargo test --bin kiro-rs output_config -- --nocapture`
   - `cargo test --bin kiro-rs thinking -- --nocapture`

   这两组都通过，覆盖了：

   - explicit `high/max` wire 保留；
   - `disabled + explicit effort`；
   - `enabled + explicit effort`；
   - native output_config path；
   - CLI / IDE body transform；
   - payload guard 归一化；
   - stream / non-stream thinking 处理；
   - signed / unsigned / redacted thinking 的安全路径。

2. 真实 Claude Code CLI capture：
   - `claude --version` 为 `2.1.197 (Claude Code)`
   - `node feature/tests/thinking-effort-claude-cli-capture.mjs`

   这轮 capture 以真实 CLI 发起 30 个 session，抓到的结果是：

   - `totalCases = 30`
   - `totalMessageRequests = 30`
   - `byEffort` 覆盖 `absent / low / medium / high / xhigh / max`
   - 所有 case 里 `outputConfigVariants` 保留了对应 effort
   - 所有 case 里 `thinkingVariants` 都是 `{"type":"adaptive"}`
   - 没有把 `max` 静默压成 `high`
   - cleanup 正常完成

换句话说，当前这条主路径并没有出现你担心的那种“ effort 被 clamp、thinking 被漏注入”的现象。

## 根因判断

基于当前代码和验证，最像根因的不是“整个系统都不支持 `thinking + output_config`”，而是以下几类更窄的问题：

1. 某条入口没有走到同一套兼容归一化。
2. 某个上游/下游路径传的是另一种 body shape，`additionalModelRequestFields` 之外的字段没有被同样处理。
3. 某个模型能力分支拿到了不匹配的 reasoning schema，导致本该是 omitted/adaptive 的组合被保留成了非法组合。
4. 如果错误来自外部 upstream，而不是本地转换层，那么真正应该抓的是实际发出的 raw body 和 route，而不是只看 CLI 表象。

## 目前不建议再做的事情

- 不要把 `output_config.effort` 硬编码成 `high`。
- 不要把 `thinking` 机械地强行注入到所有路径。
- 不要只根据一个报错字符串判断整个协议层都错了。

这类组合必须按模型能力和入口协议来判定。

## 复现方式

如果你要重现当前协议链路，建议按下面顺序：

1. 先跑 Rust 协议单测：
   - `cargo test --bin kiro-rs output_config -- --nocapture`
   - `cargo test --bin kiro-rs thinking -- --nocapture`

2. 再跑真实 CLI capture：
   - `node feature/tests/thinking-effort-claude-cli-capture.mjs`

3. 如果仍有 400，再抓：
   - 实际 route；
   - 实际 raw request body；
   - `thinking` / `output_config` 的最终 wire；
   - 是谁把 non-adaptive thinking 保留进了上游。

## 结论

当前主路径已经对齐：

- `max` 不会被压成 `high`；
- `disabled` 不会在 native wire 里保留成不兼容组合；
- `adaptive` 和显式 effort 的组合可通过；
- 真实 Claude CLI capture 也没有复现这次担心的 clamp 问题。

所以，这个问题更像是“某个未覆盖入口或某个特定模型/route 分支的问题”，而不是统一的 thinking/output_config 设计错误。

