# P05 合法的 PascalCase / MCP 工具名也被改写成 `xxxHash<8hex>`，模型侧看到的工具名与 system prompt 不一致

Status: open / documented / not-fixed
Severity: Medium
Area: request
Discovered: 2026-09-28 协议互转审计

## 问题与影响

`sanitize_tool_name` 会先按 `_`、`-` 和所有非 ASCII 字母数字字符切分，再把第一段的首字母转成小写（`src/anthropic/converter/tools.rs:115-118`），后面各段首字母大写拼接（`src/anthropic/converter/tools.rs:119-121`）。`deterministic_mapped_tool_name` 只要发现清洗结果和原名不同，就会走 `shorten_tool_name`，生成 `前缀 + "Hash" + sha256(原名)[..8]`（`src/anthropic/converter/tools.rs:137-144`、`src/anthropic/converter/tools.rs:166-178`）。

所以本来就合法的名字也会被改写：

| 客户端工具名 | 发给 Kiro 的名字 | 改写原因 |
| --- | --- | --- |
| `Read` | `readHash9b9a8d05` | 首字母被转成小写 |
| `Bash` | `bashHashd1e9567d` | 同上 |
| `Edit` | `editHash464c4ffd` | 同上 |
| `Write` | `writeHash3f00927a` | 同上 |
| `Grep` / `Glob` | `grepHash16970ed6` / `globHash18d00f7d` | 同上 |
| `WebFetch` | `webFetchHash7bd75b80` | 同上 |
| `mcp__a__b` | `mcpABHasha9ac39f0` | 分隔符被删除 |
| `read_file` / `foo-bar` | `readFileHash…` / `fooBarHash…` | 分隔符被删除 |

表中哈希是在本机用 `printf %s <name> | shasum -a 256` 算出来的，和源码里的测试夹具一致（`src/anthropic/converter.rs:4786`、`src/anthropic/converter.rs:4817`）。现有单测也写死了这个行为：`test_tool_name_mapping_summary_distinguishes_sanitized_and_overlong_names` 断言 `"Bash"` 属于 "sanitized"（`src/anthropic/converter.rs:1400-1414`）。

影响：

1. **提示词和工具名对不上。** Claude Code 的 system prompt 和工具描述里大量写着 "Use the Read tool"、"use `Edit` tool"。代理自己追加的 Write/Edit 分块后缀也直接写 `` `Edit` tool ``（`src/anthropic/converter/tools.rs:16-19`）。模型实际能调用的却是 `readHash9b9a8d05` / `editHash464c4ffd`，只能靠模型自己把 `Edit` 和 `editHash464c4ffd` 联系起来。
2. **泄漏指纹和防御成本。** `xxxHash<8hex>` 是最显眼的泄漏指纹。项目为此加了多层兜底：默认 task prompt 里禁止输出 readHash/editHash/bashHash 的规则（`src/model/config.rs:262`，后来通过迁移从默认值里删掉，见 [prompt-policy 专题](../prompt-policy-tool-choice-and-count-tokens.md)），以及 `ToolTranscriptSanitizer` 对映射名的识别（`src/anthropic/transcript_sanitizer.rs:690-705`）。每个 Claude Code 请求都会走这条映射路径，所以这些兜底也在每个请求上生效。
3. **可观测性。** "工具名称已规范化/映射" 这条日志（`src/anthropic/converter.rs:584-592`）在 Claude Code 流量上几乎每个请求都会打出来，真正非法或超长的名字反而被埋在里面。

观察记录：2026-09-28 本次审计会话本身就经过代理转发，模型侧看到的工具名是 `readHash9b9a8d05`、`bashHashd1e9567d`、`editHash464c4ffd`、`writeHash3f00927a`、`agentHash11b39c93`、`skillHash6df1bb18`、`toolSearchHash3161b03b`，和上面算出的 sha256 前 8 位完全一致，而 system prompt 里用的仍然是 `Read`/`Bash`/`Edit`。这只是一次观察，不作为生产频率证据。

## 官方协议对照

项目定位：kiro.rs 基于 Kiro 对外提供 Claude Code / Anthropic 协议接口。客户端（如 Claude Code CLI）发出的工具名，就是 Claude Code 协议里的工具名。代理把请求转换成 Kiro 协议，发往最终上游 Kiro；Kiro 返回的 `toolUseEvent.name` 再被反向映射回客户端原名。工具名映射是否需要、怎么映射，取决于 **Kiro 上游实际执行的校验规则**。

### Kiro 上游真实报文（2026-07-06，用户提供，已确认来自真实 Kiro 上游）

```text
bad_request: {"error":{"type":"<nil>","message":"上游 API 400: {\"message\":\"Mantle request failed with status 400:
{\"type\":\"error\",\"request_id\":\"req_xhwq6...\",\"error\":{\"type\":\"invalid_request_error\",
\"message\":\"***.***.custom.input_schema.properties: Property keys should match pattern '^[a-zA-Z0-9_.-]{1,64}$'\"}}\",
\"reason\":\"TOOL_SCHEMA_INVALID\"} (request id: 202607062048306419040328268d9d6P2RSXmwZ)"},"type":"error"}
```

这份报文说明了三点：

1. **报错对象是 property key，不是工具名。** 路径 `***.***.custom.input_schema.properties` 就是现有专题里的 `tools.N.custom.input_schema.properties`。这类问题已经由可逆的 schema key 映射处理了，见 [tool-property-key 专题](../tool-property-key-invalid-400-tool-schema-invalid.md)。实现位于 `src/anthropic/tool_schema_keys.rs`，默认正则由 `src/model/config.rs:131` 的 `default_tool_schema_key_validation_regex` 提供，Kiro 侧的分类在 `src/kiro/provider.rs:2052` 的 `TOOL_SCHEMA_INVALID`。它不属于本文的问题。
2. **Kiro 上游在内部做了 Anthropic 风格的请求校验。** 错误由 Kiro 后端的 "Mantle" 服务返回，内层格式是 Anthropic 错误信封：`{"type":"error","request_id":"req_...","error":{"type":"invalid_request_error",...}}`，字段路径也是 Anthropic 的 `tools.N.custom...` 形式。因此有理由推断，Kiro 对工具**名**也执行了 Anthropic 的工具名约束，即 `^[a-zA-Z0-9_-]{1,64}$`。这一点是推断：目前还没有专门针对工具名的上游报文。
3. **property key 正则允许 `.`，工具名正则大概率不允许。** 两者不能共用一个正则。本文的工具名规则必须单独定义为 `[a-zA-Z0-9_-]`，并且不包含 `.`。

补充：外层包装 `{"error":{"type":"<nil>",...}}` 和 `上游 API 400: ... (request id: ...)` 不是 kiro.rs 的错误格式，源码里没有这段文案。它来自采样链路上的中间转发层。真正的 Kiro 上游报文是内层 `Mantle request failed ...` 那一段。

据此，改写合法工具名不能以"规避上游约束"为理由。`Read`、`Bash`、`mcp__a__b`、`foo-bar` 都满足 `^[a-zA-Z0-9_-]{1,64}$`。真正需要映射的只有含 `$`、`.`、CJK、空格等字符的名字，或者超长的名字。

### 其他参照

- **Anthropic Messages API（已验证事实，来自现有问题文档）：** 工具 schema 的 property key 必须匹配 `^[a-zA-Z0-9_.-]{1,64}$`，见 [tool-property-key 专题](../tool-property-key-invalid-400-tool-schema-invalid.md)。Anthropic 对工具**名**的官方约束是 `^[a-zA-Z0-9_-]{1,64}$`（公开文档；本仓库里没有保存对应的实测报文，属于经验推断）。`Read`、`mcp__a__b` 两边都合法。
- **Kiro / CodeWhisperer（间接证据，本仓库未独立验证）：** 同级仓库 sub2api 在 `../sub2api-kiro/backend/internal/pkg/kiro/translator.go:2057-2065` 引用了 AWS 官方服务模型（`aws/aws-toolkit-vscode` 的 `user-service-2.json`）对 `ToolName` 的约束：`pattern "[a-zA-Z0-9_-]+"`、`max 64`。并注明实测故障样本是 `$WEB_SEARCH`、`$MUTLI_1.N.1-Read`（含 `$` 和 `.`），以及"连字符是合法的，不要替换"。sub2api 只替换非法字符，只在字符集有变化或超长时才加哈希后缀（`translator.go:2100-2125`），`Read`、`mcp__a__b` 都原样透传。
- **长度：** Rust 和 sub2api 都用 63（`src/anthropic/converter/tools.rs:82`、`translator.go:33`），比服务模型的 64 少 1，偏保守。
- **大小写（未验证）：** `src/anthropic/converter.rs:521` 的注释说 "Kiro 匹配工具名称时忽略大小写"，本仓库没有找到对应的实测证据。

## 源码链与根因

请求侧：

1. `convert_tools` 调用 `allocate_selected_tool_names`，逐个执行 `deterministic_mapped_tool_name(&tool.name)`（`src/anthropic/converter/tools.rs:300-372`，调用点在 `src/anthropic/converter/tools.rs:321-325`）。
2. `deterministic_mapped_tool_name` 在 `sanitized != name` 时调用 `shorten_tool_name(&sanitized, name)`（`src/anthropic/converter/tools.rs:137-144`）。
3. 历史里的 `tool_use.name` 通过 `map_tool_name` 走同一个算法（`src/anthropic/converter/history.rs:340`，函数在 `src/anthropic/converter/tools.rs:181-199`）。历史占位工具也用映射后的名字（`src/anthropic/converter.rs:519-543`）。
4. `tool_choice` 前缀里使用 Kiro 侧的名字（`src/anthropic/converter/tools.rs:413-443`、`src/anthropic/converter/tools.rs:459-465`）。

响应侧反向映射：

- 流式：`src/anthropic/stream.rs:3522-3527`（结构化 tool_use）、`src/anthropic/stream.rs:3053-3058`（字面 invoke 泄漏恢复）。
- 非流式：`src/anthropic/handlers.rs:10726-10729`、`src/anthropic/handlers.rs:10920-10923`。

根因（来自 git 历史）：

- `551b91f`（2026-03-31）只对超长名字做 `prefix_<8hex>` 缩短，那时合法名原样透传。
- `df60bef`（2026-05-29，"feat: improve model catalog and pricing management"）引入 `sanitize_tool_name`、`TOOL_HASH_MARKER = "Hash"`，并把条件改成 `sanitized != name || len > 63`。同一个 commit 把单测 `map_tool_name("short_name")` 的期望从 "原样透传" 改成了 `map_tool_name("shortName")`，并新增 `mcp__server-name__read_file` 必须被映射、输出只能是 `[A-Za-z0-9]` 的断言（`git show df60bef -- src/anthropic/converter.rs`）。时间线见 [tool-hash 时间线证据](../../evidence/tool-hash-and-leak-timeline-20260716.md)。
- 这个 commit 和同期文档（`docs/archive/request-and-protocol-history/kiro-400-improperly-formed-request-analysis.md`、`docs/archive/external-project-learning-history/kiro-gateway-account-manager-learning-analysis.md:103-112`）只说明它是从同类项目吸收来的 "Kiro-safe camelCase" 规范化。**没有找到 Kiro 因为 `_`、`-` 或大写首字母拒绝请求的报文。** 目前所有已知的上游 400 样本都是 `$`、`.`、CJK 或超长这类非法字符。
- 结论：把分隔符和首字母大写当作非法字符，是一个没有证据支撑的**过度规范化**。首字母小写只是 camelCase 风格化的副作用，不是在规避某个上游约束。hash 本身（确定性、防碰撞、可反查）是合理的，问题在于触发条件太宽。

对审计原述的修正：

- 审计说 "长度 ≤64"。当前常量和 sub2api 都是 63，建议保留 63，不要放宽到 64。
- 审计把 `src/model/config.rs:~262` 当作 "现有缓解"。这段是旧版 `task_quality_policy` 常量里的文本（`src/model/config.rs:255-264`），当前默认值已经通过配置迁移去掉了具体的 hash 字样（见 [prompt-policy 专题](../prompt-policy-tool-choice-and-count-tokens.md) 的 migration v6 记录）。它是遗留防御，不是现行机制。

## 复现

### 最小复现（单测）

放进 `src/anthropic/converter.rs` 的 `mod tests`（`src/anthropic/converter.rs:647`），复用已有的 `test_tool`（`src/anthropic/converter.rs:3953-3965`）。修复前第一个测试通过（锁定现状），第二个测试失败：

```rust
#[test]
fn audit_p05_current_mapper_rewrites_kiro_safe_pascal_case_names() {
    // 现状锁定：合法名被改写
    assert_eq!(deterministic_mapped_tool_name("Read"), "readHash9b9a8d05");
    assert_eq!(deterministic_mapped_tool_name("Bash"), "bashHashd1e9567d");
    assert!(deterministic_mapped_tool_name("mcp__a__b").contains(TOOL_HASH_MARKER));
}

#[test]
fn audit_p05_kiro_safe_names_are_sent_verbatim() {
    for name in ["Read", "Bash", "WebFetch", "mcp__a__b", "read_file", "foo-bar"] {
        assert_eq!(deterministic_mapped_tool_name(name), name, "{name}");
    }

    let tools = Some(vec![test_tool("Read"), test_tool("mcp__a__b")]);
    let mut reverse_map = HashMap::new();
    let converted =
        convert_tools(&tools, &None, &mut reverse_map, ConverterOptions::default()).unwrap();
    let names = converted
        .tools
        .iter()
        .map(|tool| tool.tool_specification.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["Read", "mcp__a__b"]);
    assert!(reverse_map.is_empty(), "合法名不应产生反向映射");

    // 真正非法或超长的名字仍然要映射
    for name in ["$WEB_SEARCH", "a.b", "工具", &"x".repeat(64)] {
        let mapped = deterministic_mapped_tool_name(name);
        assert_ne!(mapped, name);
        assert!(mapped.len() <= TOOL_NAME_MAX_LEN);
        assert!(mapped.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
    }
}
```

### 端到端复现

1. 在测试实例上用 Claude Code（`/cc/v1/messages`）发起任意一次会带工具的对话。
2. 打开 debug 日志或 payload 诊断，看 Kiro 请求体里 `userInputMessageContext.tools[*].toolSpecification.name`：值是 `readHash9b9a8d05`、`bashHashd1e9567d` 等，而 system 消息里是 `Read`、`Bash`。
3. 看日志 "工具名称已规范化/映射"：Claude Code 自带的每个工具都会计入 `sanitized_tool_name_count`。

## 修复方案

### 候选方案

| 方案 | 做法 | 优点 | 缺点 |
| --- | --- | --- | --- |
| A. 合法名直通（推荐） | 名字匹配 `^[A-Za-z0-9_-]{1,63}$` 时原样返回，其余沿用现有 camelCase + Hash 算法 | 改动最小；非法名的映射结果和现在一样，已有会话历史里的非法名映射保持连续 | 非法名仍会被 camelCase 化（可以接受） |
| B. 全面对齐 sub2api | 非法字符替换成 `_`，字符集有变化或超长时追加 `_<8hex>` | 和社区实现一致，可读性更好 | 非法名的映射结果全部变化，legacy 识别面扩大，收益有限 |
| C. 只修首字母 | 去掉 `to_ascii_lowercase` | 覆盖了 `Read`/`Bash` | `mcp__a__b`、`read_file` 仍被改写；没有解决根因 |

### 推荐方案

采用方案 A，并参照 property key 的做法，增加一个可配置的工具名校验规则，再加一层"上游校验失败时的确定性兜底"。

**设计要点：**

- 与 property key 使用同一套治理模式：配置化正则、合法内容原样透传、非法内容可逆映射、请求级反向映射表。
- 但工具名和 property key 各用一个正则，互不共用。
- 上游约束目前是推断出来的，不是实测结论。所以要保留旧算法作为配置项和自动兜底，出错时不会让用户看到 400。

**配置（`src/model/config.rs`，与 `toolSchemaKeyMapping` / `toolSchemaKeyValidationRegex` 并列）：**

| 配置项 | 默认值 | 含义 |
| --- | --- | --- |
| `bodyConversion.toolNameMapping` | `verbatim_if_valid` | 取值 `verbatim_if_valid`：符合正则的原样透传，其余走映射。取值 `legacy_camel_hash`：保持当前行为，所有名字都 camelCase 化并附加 Hash 后缀。 |
| `bodyConversion.toolNameValidationRegex` | `^[a-zA-Z][a-zA-Z0-9_-]{0,62}$` | 工具名合法性正则。长度上限 63，与现有常量一致，比推断的 64 保守一位。要求首字符为字母，兼容现有的 `tool` 前缀逻辑。不包含 `.`。 |

**实现步骤：**

1. 新增 `fn is_kiro_safe_tool_name(name, regex) -> bool`。正则只在配置加载时编译一次，与 `tool_schema_keys.rs` 复用正则的方式相同。
2. `deterministic_mapped_tool_name` 增加模式参数：当模式为 `verbatim_if_valid` 且名字命中正则时，直接返回原名；其余情况完全沿用现有的 camelCase + Hash 算法。这样非法名的映射结果与现在一致，已有会话历史里的非法名映射保持连续。
3. **上游兜底（针对推断不成立的情况）：**
   - 在 `src/kiro/provider.rs` 的 400 分类里，新增一个工具名校验失败的识别项。匹配条件：`reason=TOOL_SCHEMA_INVALID`，且错误路径以 `tools.N.custom.name` 结尾或包含 `name: ... should match pattern`。
   - 命中后，本请求用同一账号重试一次：以 `legacy_camel_hash` 模式重新生成请求体，也就是当前行为，这条路径已被生产验证可用。重试只执行一次，不换号，不进入 external fallback。
   - 记录 metric `tool_name_verbatim_rejected`，并用 warn 日志输出被拒绝的工具名（脱敏后），用于反向确认上游的真实工具名规则。
   - 这与仓库现有的"确定性 400 仅做一次定向修复重试"模式一致，见 `raw-max-tokens`、`reasoning malformed stripped retry` 等先例。

**其余步骤：**

4. `summarize_tool_name_mapping` 里"sanitized"的判定改成 `!is_kiro_safe_tool_name(original)`（`src/anthropic/converter/tools.rs:201-220`）。
5. **Legacy 识别：** 新增 `legacy_camel_hash_mapped_tool_name(name) -> Option<String>`，返回旧算法对合法名的映射结果，并在 `ToolTranscriptSanitizer::new` 里和 `legacy_overlong_mapped_tool_name` 一起加入已知名集合（`src/anthropic/transcript_sanitizer.rs:690-705`）。这样客户端历史里已经泄漏的 `readHash9b9a8d05` 仍能被识别和清理。
6. **大小写碰撞：** 修复后 `Read` 和 `read` 可能同时原样透传。`allocate_selected_tool_names` 按小写做碰撞检测（`src/anthropic/converter/tools.rs:326-361`），会把这种情况直接判成本地 400。现在 `Read` 会先被映射成 `readHash…`，所以不会碰撞。为了不引入新的拒绝，碰撞时应让后出现的名字回退到 hash 映射，而不是报错。
7. 如果响应里出现旧映射名（上游从历史里复述），反向映射表查不到。可以在请求侧给 reverse map 补上 legacy camel-hash 条目，指向原名，成本很低。

## 测试与验收

- 更新依赖旧行为的单测：
  - `test_tool_name_mapping_summary_distinguishes_sanitized_and_overlong_names`：`Bash`、`echo_value` 不再计入 sanitized（`src/anthropic/converter.rs:1400-1414`）。
  - `test_map_tool_name_sanitizes_separators_and_records_mapping`：`mcp__server-name__read_file` 改为透传（`src/anthropic/converter.rs:1363-1377`），另外补一个含 `.`/`$` 的非法样本。
  - `convert_tools_rejects_raw_name_that_collides_with_another_mapped_name_atomically`：非法样本从 `foo-bar` 换成 `foo.bar`（`src/anthropic/converter.rs:1417-1437`）。
  - 32 位哈希碰撞夹具（`src/anthropic/converter.rs:1440-1459`）超过 63 字节，仍走映射，预期不变，需要重跑确认。
- 所有用 `deterministic_mapped_tool_name("Bash")` 构造夹具的 sanitizer / handler 测试（`src/anthropic/converter.rs:4793`、`src/anthropic/converter.rs:5371-5372` 等）都要复核：它们模拟的是 "历史里有旧映射名"，应改用 legacy helper 构造。
- 新增 `audit_p05_kiro_safe_names_are_sent_verbatim`，以及 `Read` + `read` 同时存在时不报 400 的用例。
- 端到端：真实 Kiro 账号连续 5 轮 Claude Code 工具调用（Read/Edit/Bash/MCP），全部返回 200，请求体里工具名等于原名，响应里 `tool_use.name` 正确，没有新增 400。
- 回归：WebSearch、带 `$` 的 MCP 名、超长 MCP 名的 400 样本仍然被映射。
- 上游兜底：在 `src/kiro/provider.rs` tests 中用 fake upstream 返回 `{"message":"Mantle request failed with status 400: ...tools.3.custom.name: String should match pattern ...","reason":"TOOL_SCHEMA_INVALID"}`，断言同账号只重试 1 次、重试 body 中工具名为 legacy camel-hash、最终 200，且记录 `tool_name_verbatim_rejected`；返回 property key 类 `TOOL_SCHEMA_INVALID`（本文“Kiro 上游真实报文”样本）时不触发工具名兜底，仍走既有 schema key 处理。
- 配置：`toolNameMapping=legacy_camel_hash` 时全部输出与当前版本逐字节一致（复用现有 `Bash -> bashHashd1e9567d` 等夹具）。

## 兼容性与风险

- **上游约束没有 100% 实证。** Kiro 通过 Mantle 执行 Anthropic 风格校验（property key 真实报文为证），工具名规则 `^[a-zA-Z0-9_-]{1,64}$` 是据此的推断，未见工具名专属报文。上线前在测试实例用真实账号验证 `Read`、`mcp__a__b`、`foo-bar` 均 200；即使推断不成立，第 3 步上游兜底会自动回退到当前 legacy 映射，用户不会看到 400，代价是该请求多一次上游调用，并由 metric 暴露，可随时把 `toolNameMapping` 切回 `legacy_camel_hash`。
- **Prompt cache 一次性失效。** 工具名进入 tools 数组的字节，上线后所有进行中会话的前缀缓存会 miss 一轮。
- **历史里的旧名。** 客户端历史里的 tool_use 用的是原名，每次请求都会重新映射，所以请求内部是一致的。旧映射名只可能出现在历史的文本泄漏里，由第 5 步的 legacy 识别兜底。
- **回滚：** 撤掉直通判断就能完全恢复旧行为，没有持久化状态。
