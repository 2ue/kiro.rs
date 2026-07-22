# 工具字段边界值触发 400（空 description / input_schema 为 null）

Status: `historical-fix-verified / unified-candidate-cli-mcp-gate-pending`

Severity: P1

> 规划归类（2026-07-12）：本文保留为 `COR-006` 的当前复现/来源证据。文中的修复建议不是目标实现权威；profile 分离、raw passthrough 和验收行为以 `docs/plantree/plans/system-architecture-modernization/decisions/012-tool-definition-compatibility-and-reversible-schema-mapping.md` 为准。

- 状态：已修复并通过真实调用验证（2026-07-12）：空/空白 `description` 转换为非空占位；`input_schema:null` 入口容忍为空 map
- 严重级别：高 —— 两类合计占生产近 12 小时非成功请求的 74%（208/281）
- 影响端点：全部 `/v1`、`/cc/v1`、`/ha/v1`、`/na/v1`、`/dfcache/*`（共用同一套请求处理逻辑，端点仅 usage 上报不同）
- 本质：同一族缺陷 —— 对工具字段的边界值（空串 / null）缺乏入口兜底。本文覆盖两个子问题：
  - 问题 A：工具 `description` 为空串 → 上游 400（201 条，71.5%）
  - 问题 B：工具 `input_schema` 显式为 `null` → 入口反序列化 400（7 条，2.5%）

# 问题 A：空 description 触发上游 Invalid tool use format

## 现象

请求里任一工具的 `description` 为空字符串（或客户端未传，`#[serde(default)]` 后为空串）时，上游 Kiro/Bedrock 以 `Invalid tool use format / REQUEST_BODY_INVALID` 拒绝**整个请求**，返回 HTTP 400。

代理对外返回：
```json
HTTP 400
{"type":"error","error":{"type":"invalid_request_error",
 "message":"The request body is invalid. Simplify the message, tools, tool results, files, or images and retry. ..."}}
```

特征：
- 请求可以很小（生产样本 `finalBytes` 460–503，`inputTokens` 3–24），与 payload 超长无关。
- usage 记录 `errorType: tool_use_format_bad_request`、`routeSubtype: local_error_no_fallback`。
- 代理侧 `toolUseFormatDiagnostics` 全部为 0（当前诊断不覆盖空 description，属盲区）。

## 根因

唯一决定变量是 `description` 是否为空串，与 `input_schema.properties` 是否为空无关：

| description | properties | 结果 |
|---|---|---|
| `""` 空串 | `{}` 空 | 400 ❌ |
| 有内容 | `{}` 空 | 200 ✅ |
| `""` 空串 | 有属性 | 400 ❌ |
| 字段缺失（转换后 `""`） | 任意 | 400 ❌ |
| `"   "` 纯空格 | 任意 | 200 ✅ |
| `"."` 单字符 | 任意 | 200 ✅ |

上游只做「非空」存在性校验，纯空格/单字符即可通过 —— 代理侧可安全兜底。

代码位置：
- `src/anthropic/converter/tools.rs:298-336`：description 从客户端原样透传，只做「Write/Edit 追加后缀」与「10000 字符截断」，对空串无兜底。
- `src/anthropic/types.rs:314-326`：`description: String` 带 `#[serde(default)]`，客户端不传即空串；同一类型中的非可选 map 也解释了显式 `input_schema:null` 的入口失败。
- 诊断盲区：`toolUseFormatDiagnostics` 覆盖了重复工具名、空 tool_use id、孤儿 tool_result 等，但无「空 description」项。

## 复现 case

前置：本地服务已启动（示例 `127.0.0.1:9022`），API Key 见 `config.json`（示例 `sk-kiro-rs-local-debug`）。本地测试账号仅支持 sonnet，模型用 `claude-sonnet-4-20250514`。

### Case 1：空 description，必现 400

```bash
curl -sS -X POST http://127.0.0.1:9022/v1/messages \
  -H 'content-type: application/json' \
  -H 'x-api-key: sk-kiro-rs-local-debug' \
  -H 'anthropic-version: 2023-06-01' \
  -d '{
    "model": "claude-sonnet-4-20250514",
    "max_tokens": 64,
    "messages": [{"role":"user","content":"Take a screenshot to see the current state of the screen."}],
    "tools": [{"name":"computer","description":"","input_schema":{"type":"object","properties":{}}}]
  }'
```
期望（修复前）：HTTP 400 `invalid_request_error`。修复后应恢复 200。

### Case 2：唯一改动=补上 description，恢复 200

```bash
curl -sS -X POST http://127.0.0.1:9022/v1/messages \
  -H 'content-type: application/json' \
  -H 'x-api-key: sk-kiro-rs-local-debug' \
  -H 'anthropic-version: 2023-06-01' \
  -d '{
    "model": "claude-sonnet-4-20250514",
    "max_tokens": 64,
    "messages": [{"role":"user","content":"Take a screenshot to see the current state of the screen."}],
    "tools": [{"name":"computer","description":"Control the computer via screenshots and actions.","input_schema":{"type":"object","properties":{}}}]
  }'
```
期望：HTTP 200，`stop_reason: tool_use`。

### 边界

- description 为纯空格 `"   "` 或单字符 `"."` → 200。
- 缺失 description 字段 → `#[serde(default)]` 变空串 → 400。

## 修复方案（已实施）

1. **转换器兜底**：`src/anthropic/converter/tools.rs` 组装 `ToolSpecification` 时，对 trim 后为空的 description 填入非空占位描述（包含工具名，工具名为空时用通用占位），过上游校验且不改变工具入参结构。
2. **入口容忍 null**：`src/anthropic/types.rs` 为工具 `input_schema` 增加 null-as-default 反序列化，显式 `null` 与缺失字段都落为缺省空 map。
3. **补诊断**：`toolUseFormatDiagnostics` 增加 `emptyToolDescriptions` 计数；同时补充 `invalidToolSchemaPropertyKeys` 计数用于相邻的 schema key 问题排查。
4. **单测**：覆盖 `input_schema` 为 null / 缺失 / 正常 map，以及空/空白/正常 description 的转换输出。

## 修复验证（2026-07-12）

- 基线真实调用（已启动的 `127.0.0.1:9022`）：
  - 空 `description`：HTTP 400。
  - 正常 `description`：HTTP 200。
  - `input_schema:null`：HTTP 400，入口错误 `invalid type: null, expected a map`。
  - 缺失 `input_schema`：HTTP 200。
- 修复后真实调用（临时 debug 服务 `127.0.0.1:19022`，验证后已停止）：
  - 空 `description`：HTTP 200。
  - 空白 `description`：HTTP 200。
  - 正常 `description`：HTTP 200。
  - `input_schema:null`：HTTP 200。
  - 缺失 `input_schema`：HTTP 200。
- 自动化验证：
  - `cargo +1.92.0 test tool_input_schema`：3 passed。
  - `cargo +1.92.0 test tool_description`：2 passed。
  - `cargo +1.92.0 test tool_use_format_diagnostics`：2 passed。
  - `cargo +1.92.0 test`：1126 passed。
  - `cargo fmt --check`、`git diff --check`：通过。
  - `cargo +1.92.0 build`：通过。
  - `cargo +1.92.0 build --release --locked`：先构建 `admin-ui/dist` 与 `ui/dist` 后通过；两套 UI 均为 release 嵌入产物，不能作为可选项跳过。

## 回归清单

- [x] Case 1（空 description）修复后返回 200，且转换后 description 非空。
- [x] Case 2（正常 description）保持 200。
- [x] 边界：空白 description 返回 200；缺失 description 由同一兜底路径保证非空。
- [x] 单测覆盖空/空白/正常 description。
- [x] `emptyToolDescriptions` 诊断在触发时计数正确。

# 问题 B：input_schema 显式为 null 触发入口反序列化 400

## 现象

请求里任一工具的 `input_schema` 显式传 `null`（`"input_schema": null`）时，请求在**入口反序列化阶段**直接失败，返回 HTTP 400，根本未进入工具处理/上游调用。

代理对外返回：
```json
HTTP 400
{"type":"error","error":{"type":"invalid_request_error",
 "message":"Invalid JSON body: invalid type: null, expected a map at line 1 column <N>"}}
```

特征：
- 错误来自入口 JSON 解析，`errorSource: request_entry`、`routeKind: null`。
- 在 `tool-format-debug` 中**查无记录**（尚未进入工具处理阶段）。
- 无 `payloadGuardReport`。
- `column <N>` 随 body 大小变化（生产 7 条均为 31596，是大 body 里靠后某工具的 `input_schema:null`）。

## 根因

`src/anthropic/types.rs:326` 的 `input_schema` 是非 Option map，仅带 `#[serde(default)]`：
```rust
#[serde(default)]
pub input_schema: HashMap<String, serde_json::Value>,
```
`#[serde(default)]` **只在键缺失时生效**；客户端显式传 `null` 时，serde 走反序列化路径，对 `HashMap` 遇到 `null` 报 `invalid type: null, expected a map`，整个请求体解析失败。

与问题 A 同族：均为对工具字段边界值（空串 / null）缺乏入口兜底。

## 复现 case

### Case 3：input_schema=null，必现 400

```bash
curl -sS -X POST http://127.0.0.1:9022/v1/messages \
  -H 'content-type: application/json' \
  -H 'x-api-key: sk-kiro-rs-local-debug' \
  -H 'anthropic-version: 2023-06-01' \
  -d '{
    "model": "claude-sonnet-4-20250514",
    "max_tokens": 64,
    "messages": [{"role":"user","content":"hi"}],
    "tools": [{"name":"computer","description":"Control the computer.","input_schema":null}]
  }'
```
期望（修复前）：HTTP 400 `Invalid JSON body: invalid type: null, expected a map`。修复后应恢复 200。

### Case 4：对照，input_schema 缺失 → 200

```bash
curl -sS -X POST http://127.0.0.1:9022/v1/messages \
  -H 'content-type: application/json' \
  -H 'x-api-key: sk-kiro-rs-local-debug' \
  -H 'anthropic-version: 2023-06-01' \
  -d '{
    "model": "claude-sonnet-4-20250514",
    "max_tokens": 64,
    "messages": [{"role":"user","content":"hi"}],
    "tools": [{"name":"computer","description":"Control the computer."}]
  }'
```
期望：HTTP 200（`#[serde(default)]` 对缺失键生效）。

## 修复方案（已实施）

- 将 `input_schema` 反序列化改为容忍 `null`：自定义 deserializer 把 `null` 视作缺省空 map，或改为 `Option` + 取值时兜底空 map。同理可评估 `system`/`tool_choice` 等其他透传字段的 null 容忍度。

## 回归清单

- [x] Case 3（input_schema=null）修复后返回 200。
- [x] Case 4（input_schema 缺失）保持 200。
- [x] 单测覆盖 `input_schema` 为 null / 缺失 / 正常 map 三种反序列化输入。

# 关联

- `docs/kiro-400-improperly-formed-request-analysis.md`、`docs/kiro-small-payload-improperly-formed-fix-plan.md`（同为上游 400，根因不同）。
- 生产证据目录：`tmp/analysis-usage-llm-errors/`（app `0.0.101` / revision `737f9f1`）。

## 残余风险与回滚

历史真实调用与单测绑定旧候选，最终仍需当前统一 binary 上覆盖 Claude CLI、MCP nullable schema、缺失字段、空白 description 和多工具组合。空 map 占位可能不符合未来更严格上游 schema；若发生兼容回退，应本地给出明确错误，不能恢复 `null` 入口崩溃、空 description 上游 400 或跨账号无意义 retry。
