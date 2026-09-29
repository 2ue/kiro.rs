# P25 thinking 参数校验缺口（display、interleaved 预算、user 侧 thinking 块）

Status: documented / 入口校验与规范化已实现 / residual-gaps-open
Severity: Low
Area: request
Discovered: 2026-09-28 协议互转审计
Verified-against: a4227c1 (2026-09-29)

> 2026-09-29 工作树更新（尚未提交，见 [00 修复计划：2026-09-29 方案复核](00-current-protocol-fix-plan.md)）：本地路径已改为所有请求在转换前执行历史 thinking 丢弃。条件是 `payloadGuardEnabled`、`payloadShaping.enabled` 和 `discardHistoricalThinking` 同时为真，这也是默认配置。受保护的当前工具续写 assistant 保留 Kiro 原生签名。所以，本文中"本地首发保留全部历史签名 thinking"这一前提，在默认配置下不再成立，只有关闭上述任一配置时才成立。

## 问题与影响

审计原结论是“thinking 参数没有校验”，依据是 `Thinking` 结构体（`src/anthropic/types.rs:82-127`）本身只做类型反序列化。
按源码核对，这个结论不成立：校验和规范化在请求入口、反序列化之前就已完成，而且大部分“宽松处理”是有意设计。
逐项结论如下：

| 场景 | 当前行为 | 结论 |
| --- | --- | --- |
| 未知 `type`（如 `mystery`） | 400 `thinking.type must be one of: enabled, adaptive, disabled` | 已校验，不是缺口 |
| 缺少 `type` 或 `thinking` 不是对象 | 400 `thinking.type is required and must be a string` / `thinking must be an object` | 已校验 |
| `budget_tokens: null` / 浮点 / 字符串 | 400 `thinking.budget_tokens must be an integer`，不会走到 serde 报错 | 已校验，不是缺口 |
| `budget_tokens` 超出 i32 | 400 `thinking.budget_tokens must be between 1024 and 2147483647` | 已校验 |
| enabled 且 `budget_tokens < 1024`（含 0、负数） | 静默提升到 1024；必要时把 `max_tokens` 抬到 1025 | 有意规范化 |
| enabled 且 `budget_tokens == max_tokens` | 静默改为 `max_tokens - 1` | 有意规范化 |
| enabled 且 `budget_tokens > max_tokens` | 有界扩大 `max_tokens`，否则把 budget 截到 `max_tokens - 1` | 有意规范化；与 interleaved thinking 语义有偏差（缺口 G2） |
| enabled 缺少 `budget_tokens` | 补默认值（至多 20_000，保留输出空间） | 有意规范化 |
| adaptive/disabled 携带 `budget_tokens` | 删除该字段 | 有意规范化 |
| `display` 任意值（含 `omitted`、非法值） | 不解析、不校验、不转发，Kiro 侧 display 由运营配置决定 | 缺口 G1 |
| user 消息里的 `redacted_thinking` / `thinking` | 静默丢弃（前者有 debug 日志） | 缺口 G3（只需观测） |

剩余缺口都是低影响：不会导致请求失败，只是客户端表达的意图没有被执行，而且没有任何可见信号。

> 2026-09-29 代码核对（HEAD a4227c1）：`request_entry.rs`、`request_facts.rs`、`types.rs`、`envelope.rs`、`converter/model.rs`、`kiro.rs`、`thinking.rs`、`content.rs`、`handlers.rs:11377-11399`、`converter.rs:3640` 的引用行号在 HEAD 上全部核对一致（`request_entry.rs` 的行号写作时已包含 3a1306d 的改动），G1/G2/G3 均仍存在。3a1306d 对 `request_entry.rs` 的改动只是让入口拒绝的 usage 记录额外带上 model、stream、max_tokens（`record_entry_request_error` 新增 `raw_probe` 参数），不改变校验与规范化逻辑，也不改变对外错误响应。已更正端到端复现里的笔误：budget 512 规范化后是 1024（`MIN_ENABLED_THINKING_BUDGET_TOKENS`，`request_facts.rs:428-436`），不是 200。

## 官方协议对照

- Claude Code 协议侧（接口要接受什么）：
  - `thinking.type` 取 `enabled` / `adaptive` / `disabled`；enabled 必须带整数 `budget_tokens`，要求 `budget_tokens >= 1024` 且
    `budget_tokens < max_tokens`。例外：开启 interleaved thinking（`anthropic-beta: interleaved-thinking-2025-05-14`）时，
    budget 是整轮的思考总量，可以大于 `max_tokens`。
  - `display` 可选，取值 `summarized` / `omitted`。`omitted` 时响应里的 thinking block 文本为空，只带 signature。
  - thinking / redacted_thinking 只属于 assistant 内容。user 消息里出现这些块时官方怎么处理，本次没有验证。
  - 非法参数返回 400，信封为 `{"type":"error","error":{"type":"invalid_request_error","message":...},"request_id":...}`。
    本项目入口错误使用同一形状（`src/anthropic/types.rs:9-24`、`src/anthropic/envelope.rs:198-211`）。
- Kiro 协议侧（上游要求）：
  - 原生 reasoning 路径只接收 `additionalModelRequestFields`：output_config 形态为
    `thinking:{type:"adaptive", display?}` + `output_config:{effort}`，reasoning 形态为 `reasoning:{effort}`
    （`src/anthropic/converter/model.rs:249-263`、`src/kiro/model/requests/kiro.rs:83-93`）。
    不接收 `budget_tokens`，这是 [thinking effort 映射](../thinking-effort-adaptive-upstream-mapping.md) 已确认的合同。
  - Kiro 请求类型里没有 `max_tokens` 字段（`src/kiro/model/requests/` 下无此字段）。所以 `budget_tokens` 与 `max_tokens` 的大小关系不会直接发给 Kiro，
    只通过“budget → effort 档位”或“budget → 提示词 `<max_thinking_length>`”间接生效。
  - Kiro 是否接受 `display:"omitted"`：没有抓包证据。当前只发送过 `summarized`。
  - thinking 签名是 Kiro 原生签名，代理原样透传；本文所有方案都不改动签名。

## 源码链与根因

请求入口 `handle_messages_endpoint`（`src/anthropic/handlers/request_entry.rs:5` 起），顺序固定，本地账号和 raw external 路由共用：

```text
apply_missing_max_tokens_policy_with_probe            (request_entry.rs:43-61)
normalize_raw_reasoning_protocol_with_probe_and_limit (request_entry.rs:87-122; request_facts.rs:361-514)
  enabled:  <1024 -> 1024 (432-436); max<=1024 -> 1025/1024 (438-445)
            == max -> max-1 (446-448); > max -> 有界扩大或截断 (449-472)
            缺省 -> min(20_000, max-1024) 且 >=1024 (420-427)
            字段存在但非整数 -> 不处理，留给校验 (415-419)
  adaptive/disabled: 删除 budget_tokens (483-487)
  未知 type -> 不处理 (490)
validate_raw_reasoning_protocol_with_probe            (request_entry.rs:129-142; request_facts.rs:208-325)
  type 枚举 (279-285); budget 必须整数 (288-299); 1024..=i32::MAX (300-303); < max_tokens (304-311)
raw external 路由                                     (request_entry.rs:198 起)
parse_messages_payload_with_probe -> serde MessagesRequest (request_entry.rs:294, 557-581)
validate_typed_reasoning_protocol                     (request_entry.rs:583-625，与 raw 校验同口径的二次保护)
```

- `budget_tokens: null`：规范化因“字段存在但不是整数”跳过（`request_facts.rs:415-419`），raw 校验返回
  `thinking.budget_tokens must be an integer`（`request_facts.rs:291-293`）。`Thinking` 的 `deserialize_budget_tokens` 对 null 会报
  `Invalid JSON body: invalid type: null, expected i32 ...`（`types.rs:122-127`、`request_facts.rs:191-192`），但它在 raw 校验之后才运行，HTTP 入口到不了这里。
  客户端实际收到：HTTP 400，`{"type":"error","error":{"type":"invalid_request_error","message":"thinking.budget_tokens must be an integer"},"request_id":"..."}`。
- 规范化是有意的：`request_facts.rs:332-350` 的注释写明目的（某些兼容客户端会发送边界值，本地拒绝会让选路和账号无关），
  [真实账号 model invalid 400](../real-account-model-invalid-400-20260901.md) 记录了根因和真实账号验证，
  [raw max_tokens 校验](../raw-max-tokens-invalid-400-20260902.md) 补齐了 `max_tokens` 的类型和范围校验。已有单测覆盖：
  `raw_reasoning_protocol_repairs_zero_and_negative_thinking_controls`（`request_facts.rs:1569`）、
  `raw_reasoning_protocol_leaves_unknown_or_ambiguous_controls_for_validation`（`request_facts.rs:1673`）、
  `typed_reasoning_protocol_rejects_unknown_and_ambiguous_controls_for_five_rounds`（`request_entry.rs:1201`）。

budget 到 Kiro 的映射（使用规范化之后的值）：

- 原生路径：`effort_from_budget_tokens`：`<=4000` low、`<=16000` medium、`<=64000` high、其余 xhigh（`model.rs:158-165`）；
  模型不支持 xhigh 但支持 max 时用 max（`model.rs:209-211`），都不支持则本地报错（`model.rs:212-214`）。显式 `output_config.effort` 优先（`model.rs:171-187`）。
- 兼容提示词路径：enabled 写 `<max_thinking_length>{budget}</max_thinking_length>`，adaptive 写 `<thinking_effort>`（`src/anthropic/converter/thinking.rs:49-59`）。

### G1：`display` 被忽略

- `Thinking` 没有 `display` 字段（`types.rs:82-91`），raw 校验也不读它（`request_facts.rs:276-322`）。
- 发给 Kiro 的 `display` 只由 `force_visible_thinking` 决定（`model.rs:251-254`），而它来自运营配置 `prompt_steering` 和
  `thinking_trigger_mode`（`src/anthropic/handlers.rs:11377-11399`），与客户端的 `display` 无关。
- 结果：`display:"omitted"` 的请求照样拿到完整 thinking 文本；`display:"bogus"` 也返回 200。

### G2：interleaved thinking 下的 budget 被截断，effort 档位随之下降

规范化不看 `anthropic-beta`（`src/anthropic/` 下没有 interleaved 相关判断）。当 `budget > max_tokens` 且无法有界扩大时，budget 被截到 `max - 1`（`request_facts.rs:468-470`），
effort 档位跟着变。例：`claude-sonnet-4.6`，`max_tokens=32000`，`budget_tokens=100000`：
候选 max 100001 超过 2 倍上限 64001（`request_facts.rs:457-463`），budget 被截成 31999，映射为 `high`；
未截断时 100000 会映射为 xhigh 并落到 `max`（现有单测 `enabled_large_budget_maps_without_parse_time_truncation`，`src/anthropic/converter.rs:3640`，在 `max_tokens=128000` 下得到 `max`）。
在官方语义里，interleaved thinking 下这个请求是合法的。由于 Kiro 不接收 `max_tokens`，这个截断对上游并无必要。

### G3：user 消息里的 thinking 块被静默丢弃

`redacted_thinking` 只写 debug 日志（`src/anthropic/converter/content.rs:80-84`），`thinking` 落到 `_ => {}`（`content.rs:85`）。
Claude Code 不会产生这种形状。丢弃本身合理（Kiro user 消息没有 reasoning 字段），但与 [P06](06-image-only-history-user-empty-content.md) 叠加时，
只含这类块的历史 user 会变成空 content。

## 复现

### 最小复现（单测）

G1，放入 `src/anthropic/request_facts.rs` 的 `mod tests`，沿用 `validate_raw_reasoning_protocol` 的写法：

```rust
#[test]
fn raw_reasoning_protocol_validates_thinking_display_for_five_rounds() {
    let valid: &[&[u8]] = &[
        br#"{"model":"m","max_tokens":4096,"messages":[],"thinking":{"type":"adaptive","display":"summarized"}}"#,
        br#"{"model":"m","max_tokens":4096,"messages":[],"thinking":{"type":"adaptive","display":"omitted"}}"#,
        br#"{"model":"m","max_tokens":4096,"messages":[],"thinking":{"type":"enabled","budget_tokens":2048,"display":"omitted"}}"#,
    ];
    let invalid: &[&[u8]] = &[
        br#"{"model":"m","max_tokens":4096,"messages":[],"thinking":{"type":"adaptive","display":"bogus"}}"#,
        br#"{"model":"m","max_tokens":4096,"messages":[],"thinking":{"type":"adaptive","display":1}}"#,
    ];
    for round in 0..5 {
        for &raw in valid {
            validate_raw_reasoning_protocol(&Bytes::copy_from_slice(raw))
                .unwrap_or_else(|error| panic!("round {round}: {error}"));
        }
        for &raw in invalid {
            // 当前实现失败：display 不做任何校验
            let error = validate_raw_reasoning_protocol(&Bytes::copy_from_slice(raw))
                .expect_err("invalid display must be rejected");
            assert!(error.contains("thinking.display"), "round {round}: {error}");
        }
    }
}

#[test]
fn raw_reasoning_protocol_null_budget_is_a_public_integer_error() {
    // 当前实现已通过：固定 null budget 的错误文案，防止回退成 serde 报错
    let body = Bytes::from_static(
        br#"{"model":"m","max_tokens":4096,"messages":[],"thinking":{"type":"enabled","budget_tokens":null}}"#,
    );
    let probe = probe_raw_messages_body(&body);
    assert!(normalize_raw_reasoning_protocol_with_probe(&body, &probe).expect("normalize").is_none());
    assert_eq!(
        validate_raw_reasoning_protocol(&body).expect_err("null budget"),
        "thinking.budget_tokens must be an integer"
    );
}
```

G2，放入 `src/anthropic/converter.rs` 的 `mod tests`，沿用 `enabled_large_budget_maps_without_parse_time_truncation` 的请求构造：

```rust
#[test]
fn clipped_budget_lowers_native_effort_bucket() {
    use super::super::types::{Message as AnthropicMessage, Thinking};

    let effort_for = |budget: i32| {
        let req = MessagesRequest {
            model: "claude-sonnet-4.6-thinking".to_string(),
            max_tokens: 32_000,
            messages: vec![AnthropicMessage { role: "user".to_string(), content: serde_json::json!("Hello") }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: Some(Thinking { thinking_type: "enabled".to_string(), budget_tokens: budget }),
            output_config: None,
            metadata: None,
        };
        convert_request_with_options(&req, ConverterOptions::default())
            .expect("convert")
            .additional_model_request_fields
            .expect("native fields")
            .output_config
            .expect("output config")
            .effort
    };
    // 入口规范化把 100_000 截成 31_999（request_facts.rs:468-470）后，档位从 max 降到 high
    assert_eq!(effort_for(100_000), "max");
    assert_eq!(effort_for(31_999), "high");
}
```

这条测试描述的是现状（通过），用于在修复 G2 时配合一条入口测试：带 interleaved beta 头时，
`normalize_raw_reasoning_protocol_with_probe_and_limit` 不再截断 budget，最终 effort 为 `max`。

### 端到端复现

```bash
BASE=http://127.0.0.1:19023/cc/v1/messages
H=(-H 'content-type: application/json' -H 'x-api-key: <key>' -H 'anthropic-version: 2023-06-01')

# 已校验：null budget -> 400 thinking.budget_tokens must be an integer
curl -sS "$BASE" "${H[@]}" -d '{"model":"claude-sonnet-4-5","max_tokens":2048,
  "thinking":{"type":"enabled","budget_tokens":null},"messages":[{"role":"user","content":"hi"}]}'

# 已校验：未知 type -> 400 thinking.type must be one of: enabled, adaptive, disabled
curl -sS "$BASE" "${H[@]}" -d '{"model":"claude-sonnet-4-5","max_tokens":2048,
  "thinking":{"type":"mystery"},"messages":[{"role":"user","content":"hi"}]}'

# 有意规范化：budget 512 -> 1024；日志 normalization_reason=raise_budget_to_minimum
curl -sS "$BASE" "${H[@]}" -d '{"model":"claude-sonnet-4-5","max_tokens":4096,
  "thinking":{"type":"enabled","budget_tokens":512},"messages":[{"role":"user","content":"hi"}]}'

# G1：display omitted 仍返回完整 thinking 文本；display bogus 返回 200
curl -sS "$BASE" "${H[@]}" -d '{"model":"claude-sonnet-4-6","max_tokens":4096,
  "thinking":{"type":"adaptive","display":"omitted"},"messages":[{"role":"user","content":"17*23=?"}]}'

# G2：interleaved 下 budget > max 被截断；日志 normalization_reason=clamp_budget_to_max_minus_one，
#     wire 采样中 output_config.effort=high
curl -sS "$BASE" "${H[@]}" -H 'anthropic-beta: interleaved-thinking-2025-05-14' \
  -d '{"model":"claude-sonnet-4-6","max_tokens":32000,
  "thinking":{"type":"enabled","budget_tokens":100000},"messages":[{"role":"user","content":"hi"}]}'
```

## 修复方案

### 候选方案

A. 保持现有入口规范化不变，只补 G1/G2/G3（推荐）。
B. 恢复严格校验：`budget < 1024`、`budget >= max_tokens` 一律 400，贴近官方。会让
   [真实账号 model invalid 400](../real-account-model-invalid-400-20260901.md) 修掉的兼容客户端重新失败，而上游 Kiro 本来就不需要这对约束，不采用。
C. 把客户端 `display` 原样转发给 Kiro。Kiro 是否接受 `omitted` 没有证据，贸然转发可能换来上游 400，不采用。

### 推荐方案

本地校验（返回官方格式 400）与静默规范化的划分原则：请求的含义无法确定时本地 400；含义确定、只是数值不满足官方约束，并且 Kiro 本身不需要这条约束时，静默规范化并留下观测信号。

| 项目 | 处理 | 理由 |
| --- | --- | --- |
| 未知/缺失 `type`、非整数或越界 budget、非对象 thinking | 保持本地 400（已实现） | 意图无法确定；发给上游只会得到不可诊断的 400，而且确定性错误不应进入选路和重试 |
| `display` 非 `summarized`/`omitted`、非字符串 | 新增本地 400：`thinking.display must be one of: summarized, omitted` | 同上；放在 `validate_raw_reasoning_protocol_with_probe` 的 thinking 分支，raw 和本地路由同时生效 |
| `display:"omitted"` | 接受，暂不执行；计入告警 | 省略 thinking 文本需要在响应侧清空文本、保留 Kiro 签名，而空文本配原签名回放时 Kiro 能否校验通过没有证据；在拿到证据前保守返回完整文本，并在 `x-kiro-rs-warnings` 中写 `thinking-display-ignored=1` |

> 2026-09-29 代码核对（HEAD a4227c1）：上面“空文本配原签名回放”的风险在本地 Kiro 路径上是真实会发生的，不能假设历史 thinking 会先被整形掉。本地路径的 `discardHistoricalThinking` 只在 `guard_kiro_request` 的超限分支里生效（`src/anthropic/payload_guard.rs:582-584`，`size_limit_enabled && report.final_weight > max_weight && config.shaping.enabled` 时才调用 `apply_payload_shaping`）；常驻的 `apply_payload_safety_shaping`（`:2063-2078`）只丢超大历史图片。所以默认 `on_too_long` 模式下首次发送会保留历史中的 Kiro 原生签名 thinking，作为 `reasoningContent` 原样透传给 Kiro。外部池路径则不同：`apply_anthropic_payload_safety_shaping`（`:2080-2106`）在 `shaping.enabled` 下直接丢弃历史 thinking（`discardHistoricalThinking` 默认 true），这条路径上的回放不会被验证到。补证时必须走本地路径。另外 3a1306d 增加了两层兜底，会掩盖回放失败：本地转换遇到多个或混合的 native reasoning 块时，会丢弃历史 thinking 后重试（`src/anthropic/handlers/local_body_pipeline.rs:186-214`）；provider 遇到 `THINKING_SIGNATURE_INVALID`，或者 `Improperly formed request` 这类 400 malformed_request 时，只要存在 `thinking_signature_retry_body_builder`，就会去掉 reasoning 后用同一凭据重试（`src/kiro/provider.rs:12207`、`:13664-13677`）。补证时需要检查 attempts 里有没有 `thinking_signature_retry_same_credential` / `reasoning_malformed_retry_same_credential`，不能只看最终是否 200。
| budget < 1024、== max、缺省、adaptive/disabled 带 budget | 保持静默规范化（有意设计） | 已有真实账号证据；Kiro 不接收 budget/max_tokens，规范化不改变上游语义。建议把 `normalization_reason` 同时写入 `x-kiro-rs-warnings`（目前只有 info 日志，`request_entry.rs:93-104`） |
| budget > max 且带 `interleaved-thinking-*` beta | 不截断 budget，也不扩大 max；raw 校验在此条件下跳过 `budget < max_tokens` | 官方语义下合法；Kiro 不接收 max_tokens，截断只会让 effort 档位无故下降 |
| user 消息中的 `thinking`/`redacted_thinking` | 保持丢弃；新增 `user_thinking_blocks_dropped` 告警计数 | Kiro user 消息无 reasoning 字段；本地 400 会让偶发异常历史整段不可用，收益低于成本 |

实现要点：

1. `request_facts.rs` thinking 分支增加 `display` 校验，与现有 `type` 校验同风格；`Thinking` 增加 `display: Option<String>`，
   供转换器判断是否计入 `thinking-display-ignored`。注意 `Thinking` 的自定义 `Serialize`（`types.rs:100-117`）需要同步处理新字段。
2. `normalize_raw_reasoning_protocol_with_probe_and_limit` 增加参数 `interleaved_thinking: bool`（由 `request_entry.rs` 从请求头
   `anthropic-beta` 解析），为 true 时 `budget > max` 分支直接保留原值；`validate_raw_reasoning_protocol_with_probe` 同步放宽。
   `validate_typed_reasoning_protocol`（`request_entry.rs:617-622`）也要同步，否则二次校验会重新拒绝。
3. G3 在 `process_message_content` 的两个分支计数；函数签名目前不带 `ProxyWarnings`，可以让它多返回一个丢弃计数，由调用方累加。
4. 不改签名透传、不改 effort 映射表、不改 adaptive 默认 effort 的解析。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)，只使用 `127.0.0.1:19023` 指定测试实例和隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，Cargo 通过 `feature/tests/run-cargo-scoped.sh` 运行，不得影响本机正在运行的 Claude Code CLI 与服务。

- 新增 `raw_reasoning_protocol_validates_thinking_display_for_five_rounds`：修复前失败、修复后通过。
- 新增 `raw_reasoning_protocol_null_budget_is_a_public_integer_error`、`clipped_budget_lowers_native_effort_bucket`：修复前后都通过，作为护栏。
- 新增入口测试：带 `anthropic-beta: interleaved-thinking-2025-05-14`，`max_tokens=32000`、`budget_tokens=100000`，
  规范化返回 `None`、校验通过、最终 wire `output_config.effort=max`；不带该头时仍按现状截断。
- 回归：`request_facts::tests::raw_reasoning_protocol*`、`typed_reasoning_protocol_*`、
  `messages_entry_normalizes_omitted_or_conflicting_thinking_controls_before_dispatch`、
  `explicit_max_output_config_effort_survives_authoritative_wire_conversion_five_rounds` 全部通过。
- 端到端：上面 5 条 curl 的结果符合“推荐方案”表格；`display:"omitted"` 响应头含 `thinking-display-ignored`。
- 真实 Claude Code CLI（带 interleaved beta，`--effort max`）：thinking 正常输出，wire effort 不低于修复前。
- 补证：抓一次 Kiro 对 `thinking.display:"omitted"` 的真实返回，决定是否把 G1 升级为“转发给 Kiro”。空文本加原签名的回放补证只在本地路径上有意义，并且要确认没有触发 3a1306d 的去 reasoning 重试（见“推荐方案”表后的核对说明）。

## 兼容性与风险

- `display` 非法值由 200 变成 400，是唯一的“原来成功、现在失败”的变化。Claude Code CLI 不发送非法 display，影响面限于手写请求。
- interleaved 放宽只改变“budget > max”这一种请求：budget 不再被截断，effort 可能从 high 升到 xhigh/max，思考成本随之上升。这正是客户端的原始意图。
- raw external 路由与本地路由共用入口校验和规范化（`request_entry.rs:124-142`），放宽之后 raw 路由收到的请求体也会变化，需要在外部池路径单独验证。
- 顺带发现：[thinking effort 映射](../thinking-effort-adaptive-upstream-mapping.md) 写有“`thinking.type=enabled/disabled` 与 `output_config` 的不兼容组合由 request facts fail-closed”，
  但当前 `request_facts.rs` 和 `request_entry.rs` 中没有找到这条拒绝逻辑；enabled + `output_config.effort` 时，显式 effort 直接优先（`model.rs:171-187`）。
  这属于那份文档的范围，建议另行核对，本文不改动。
- 回滚：G1/G2/G3 相互独立，可以分别回滚。
