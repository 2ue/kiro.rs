# 工具 schema property key 非法触发 400 / 可逆映射修复

Status: `historical-fix-verified / unified-candidate-cli-mcp-long-history-gate-pending`

Severity: P1

> 规划归类（2026-07-12）：本文保留为 `COR-007` 的复现、修复和验证证据。目标行为以 `docs/plantree/plans/system-architecture-modernization/decisions/012-tool-definition-compatibility-and-reversible-schema-mapping.md` 为准；本文记录当前主线实现对该目标的落地状态。

- 状态：已修复并通过真实本地服务调用验证。默认 `sanitize` 仅清理不匹配正则的 schema property key，并在响应侧把 `tool_use.input` 递归映射回客户端原始 key。
- 2026-07-13 追加修正：诊断字段 `invalidToolSchemaPropertyKeys` 只统计真正的 `properties` key，不再把 `$defs` 定义名、`patternProperties` 正则 key、`dependentSchemas` 依赖 key 误报为非法 property key。
- 严重级别：中高 —— 客户端工具 schema 含非法属性名时，修复前上游拒绝整个请求。
- 影响端点：全部 `/v1`、`/cc/v1`、`/ha/v1`、`/na/v1`、`/dfcache/*`（共用同一套请求处理逻辑，端点仅 usage 上报不同）。
- 相关配置：`bodyConversion.toolSchemaKeyMapping`、`bodyConversion.toolSchemaKeyValidationRegex`。
- 默认合法性正则：`^[a-zA-Z0-9_.-]{1,64}$`。

## 现象

请求里任一工具的 `input_schema.properties` 出现不合规的属性键（property key）时，上游会拒绝整个请求，返回 HTTP 400。

真实上游错误串（账号走 Anthropic/Mantle 上游时）：

```text
reason: TOOL_SCHEMA_INVALID
tools.26.custom.input_schema.properties: Property keys should match pattern '^[a-zA-Z0-9_.-]{1,64}$'
```

代理对外曾返回：

```json
HTTP 400
{"type":"error","error":{"type":"invalid_request_error",
 "message":"The request body is invalid. Simplify the message, tools, tool results, files, or images and retry. ..."}}
```

修复前代理侧对 property key 没有清洗、拒绝或反向映射，因此非法 key 会透传上游。修复后默认在本地转换阶段处理，不再依赖上游 400 才暴露问题。

## 根因

Anthropic/MCP 客户端允许的 JSON Schema property key 空间比 Kiro 上游正则更宽，旧 converter 没有在 request scope 建立可逆 schema-key map，也没有同步更新 `required`/dependency 引用与 response `tool_use.input`。只做单向字符串替换会让请求返回 200 但客户端拿不到原始参数，比直接 400 更隐蔽。

## 上游约束（实测结论）

property key 必须匹配 `^[a-zA-Z0-9_.-]{1,64}$`。这是上游硬性约束，非本程序所加。

本地 sonnet 实测：

| property key | 匹配正则 | 修复前结果 |
|---|---:|---:|
| `user_name` | ✅ | 200 |
| `a.b-c_1`（字母数字 + `.` `-` `_`） | ✅ | 200 |
| `bad key`（空格） | ❌ | 400 |
| `path/to`（斜杠） | ❌ | 400 |
| `用户名`（中文） | ❌ | 400 |
| `ns:key`（冒号） | ❌ | 400 |
| `a@b`（@） | ❌ | 400 |
| 65 个字符 | ❌ | 400 |

允许：字母 `a-zA-Z`、数字 `0-9`、`_`、`.`、`-`，长度 1–64。

禁止：空格、`/`、`:`、`@`、中文、emoji、空 key、超 64 字符等。

## 为什么不能只做字符串替换

上游返回的工具参数 key 与发给上游的 schema property key 一致：

| 发给上游的 schema property key | 上游返回 `tool_use.input` 的 key |
|---|---|
| `distinctive_key_name_zzz` | `distinctive_key_name_zzz` |
| `bad_key`（模拟清洗后形态） | `bad_key` |

因此如果代理把客户端的 `"bad key"` 改成 `"bad_key"` 发给上游，但不做响应侧反向映射：

```text
客户端 schema key = "bad key"
  ↓ 不可逆清洗
上游 schema key = "bad_key"
  ↓ 模型按上游 schema 填参
上游返回 input = { "bad_key": ... }
  ↓ 客户端按 input["bad key"] 读取
参数匹配失败
```

这类“请求成功但工具参数丢失”的结果比直接 400 更隐蔽，所以本修复必须同时满足：

- 合法 key 原样保留，不建映射；
- 非法 key 发给上游前变成唯一合法 key；
- `required`、`dependentRequired`、`dependentSchemas`、legacy `dependencies` 等同 scope 引用同步更新；
- stream / non-stream / leaked `<invoke>` 路径都把返回的 `tool_use.input` key 还原为客户端原始 key；
- 不能证明安全时本地明确报错，而不是静默改错或把原请求发给上游碰 400。

## 当前实现

### 配置开关

`BodyConversionConfig` 新增两个字段：

```json
{
  "bodyConversion": {
    "toolSchemaKeyMapping": "sanitize",
    "toolSchemaKeyValidationRegex": "^[a-zA-Z0-9_.-]{1,64}$"
  }
}
```

`toolSchemaKeyMapping` 支持三种模式：

| 模式 | 行为 |
|---|---|
| `sanitize`（默认） | 编译配置正则；只清理不匹配正则的 schema property key；合法 key 原样保留且不建映射；响应侧按 request-local map 还原。 |
| `reject` | 编译配置正则；发现第一个非法 property key 后，在本地返回明确 `invalid_request_error`，不清洗、不发上游。 |
| `disabled` | 不编译正则、不扫描、不清洗、不建映射；保留旧透传行为。 |

默认正则来自本文件记录的上游实测约束：`^[a-zA-Z0-9_.-]{1,64}$`。运行时 UI 的两套页面都暴露了该配置：

- `admin-ui/src/components/runtime-config-panel.tsx`
- `ui/src/features/runtime/runtime-page.tsx`

### 映射策略

映射只在请求内保存，不写 Redis，也不写全局状态。原因：

- schema key 映射只服务一次上游请求及其响应流，生命周期天然等于 request；
- 多会话、并发请求、多工具之间不共享映射，避免串 session / 串 tool；
- Redis 会增加网络往返、序列化、过期管理和残留风险，却不能提升正确性；
- 本地内存 map 随请求完成释放，性能和隔离性更好。

内部 map 形态：

```text
upstream_tool_name -> sanitized_schema_key -> original_schema_key
```

只对非法 key 生成映射；合法 key 不进入 map。

非法 key 不再做“去掉非法字符 / 大小写转换 / 前缀截断”这类不可逆清洗，而是生成唯一 hash 形态的合法 id：

```text
key<16 hex chars>
```

示例：

```text
"bad key" -> "key2fae6b21a8c4d901"   // 示例形态，实际值由 SHA-256 输入决定
```

hash 输入包含固定版本前缀、映射后的上游工具名、schema path、原始 key 和 attempt 序号。这样可以处理：

- 不同会话并发：map request-local，不共享；
- 不同工具：按 mapped/upstream tool name 隔离；
- 同一工具不同嵌套 path：hash 输入包含 path；
- 生成 key 撞到已有合法 key 或前一个生成 key：检测后增加 attempt；
- 多次 attempt 后仍不能生成匹配配置正则且不碰撞的 key：本地明确报错。

### schema 处理范围

实现会递归处理 JSON Schema 中真正代表对象属性名的位置：

- `properties`：key 参与校验/映射；
- 嵌套 object schema：递归处理；
- `$defs`、`patternProperties`、`oneOf`、`anyOf`、`allOf`、`items` 等子 schema：递归处理其内部的 `properties`；
- `required`、`dependentRequired`、`dependentSchemas`、legacy `dependencies`：同一 object scope 内同步重写被映射的 property name。

不会把这些内容当作普通 property key 盲目改写：

- `patternProperties` 的正则 key；
- `$defs` 的定义名；
- `$ref` 字符串；
- 任意 schema 字符串值。

### 响应侧反向映射

响应处理按上游返回的 tool name 找到 request-local map，然后递归还原 `tool_use.input` 对象 key。

覆盖路径：

- 非流式 `Event::ToolUse` 聚合路径；
- `/cc/v1` / Anthropic SSE `input_json_delta` 流式路径；
- leaked `<invoke>...</invoke>` 工具调用提取路径。

流式路径只有当当前工具存在 schema key map 时，才会暂存该工具的 `input_json_delta` 并在 `stop` 时一次性输出还原后的 JSON；没有映射的工具保持原流式增量行为。因此性能影响只出现在“确实包含非法 schema key 的工具调用”上。

## 复现 case

前置：本地服务已启动（示例 `127.0.0.1:9022`），API Key 见 `config.json`（示例 `sk-kiro-rs-local-debug`）。本地测试账号仅支持 sonnet，模型用 `claude-sonnet-4-20250514`。

### Case 1：非法属性键（空格）

```bash
curl -sS -X POST http://127.0.0.1:9022/v1/messages \
  -H 'content-type: application/json' \
  -H 'x-api-key: sk-kiro-rs-local-debug' \
  -H 'anthropic-version: 2023-06-01' \
  -d '{
    "model": "claude-sonnet-4-20250514",
    "max_tokens": 64,
    "messages": [{"role":"user","content":"Call probe with bad key alpha and valid_key beta."}],
    "tools": [{
      "name":"probe",
      "description":"Records two values for validation.",
      "input_schema":{
        "type":"object",
        "properties":{
          "bad key":{"type":"string"},
          "valid_key":{"type":"string"}
        },
        "required":["bad key","valid_key"]
      }
    }],
    "tool_choice":{"type":"tool","name":"probe"}
  }'
```

期望：

- `sanitize` 默认模式：HTTP 200，返回 `tool_use.input` 包含原始 key `"bad key"` 与合法 key `"valid_key"`，不暴露 `key<hash>`。
- `reject` 模式：HTTP 400，本地错误信息指出违规工具、schema path、原始 key 和正则，不发上游。
- `disabled` 模式：保留旧行为，非法 key 可能被上游拒绝。

### Case 2：合法属性键

```bash
curl -sS -X POST http://127.0.0.1:9022/v1/messages \
  -H 'content-type: application/json' \
  -H 'x-api-key: sk-kiro-rs-local-debug' \
  -H 'anthropic-version: 2023-06-01' \
  -d '{
    "model": "claude-sonnet-4-20250514",
    "max_tokens": 32,
    "messages": [{"role":"user","content":"ping"}],
    "tools": [{"name":"probe","description":"probe tool","input_schema":{"type":"object","properties":{"user_name":{"type":"string"}}}}]
  }'
```

期望：HTTP 200；合法 key 原样传递，不建 schema-key 映射。

## 修复验证（2026-07-12）

### 真实本地服务调用

验证使用本地 release 二进制启动临时服务：

```text
KIRO_RS_HOST=127.0.0.1
KIRO_RS_PORT=19022
binary=./target/release/kiro-rs
```

验证后已停止临时服务，未触碰既有 `9022` 服务。

非流式 `/v1/messages`，默认 `sanitize`，真实上游调用结果：

```text
NON_STREAM_STATUS 200
NON_STREAM_TOOL_BLOCKS 1
NON_STREAM_TOOL_NAME probe
NON_STREAM_HAS_ORIGINAL_BAD_KEY True
NON_STREAM_HAS_VALID_KEY True
NON_STREAM_HASH_KEYS []
NON_STREAM_INPUT_JSON {"bad key": "alpha", "valid_key": "beta"}
```

流式 `/cc/v1/messages`，默认 `sanitize`，真实上游调用结果：

```text
STREAM_STATUS 200
STREAM_TOOL_NAMES ['probe']
STREAM_PARTIAL_COUNT 1
STREAM_COMBINED_INPUT {"bad key":"alpha","valid_key":"beta"}
STREAM_HAS_ORIGINAL_BAD_KEY True
STREAM_HAS_VALID_KEY True
STREAM_HASH_KEYS []
```

结论：默认清洗发给上游的非法 schema key，并在返回给客户端前正确映射回原始 key；合法 key 不受影响；hash key 未泄漏到客户端响应。

`reject` / `disabled` 的行为由单元测试覆盖。曾尝试用临时配置启动 `reject` 服务做真实调用，但该环境运行时配置由 PgSQL snapshot 覆盖文件配置，返回仍是默认 `sanitize` 行为，因此未把该次结果计入真实 `reject` 证据。

### 自动化验证

已覆盖：

- schema key mapper：只清理非法 key、合法 key 不建映射、`reject` 明确报错、`disabled` 不编译非法正则且不改 schema、hash-only 生成 key、碰撞规避、不同工具隔离；
- converter：默认 sanitize round-trip、reject、本地 disabled、自定义正则；
- stream：存在 schema map 时缓冲并反向映射 `input_json_delta`，`<invoke>` 提取也会反向映射；
- frontend contract：两套 UI 的 runtime config 类型与默认值一致；
- release：先构建 `admin-ui/dist` 和 `ui/dist`，再执行 release build，证明两套 UI 都作为 release 嵌入产物参与构建。

关键命令：

```bash
cargo +1.92.0 fmt --check
git diff --check
cargo +1.92.0 check
cargo +1.92.0 test
cargo +1.92.0 test --locked --all-targets --no-default-features
cargo +1.92.0 test --locked --all-targets
(cd admin-ui && pnpm build)
(cd ui && pnpm build)
node scripts/check-frontend-contracts.mjs
cargo +1.92.0 build --release --locked
```

## 性能与状态边界

- 不使用 Redis。映射只存在于当前请求的转换/响应上下文中，请求完成即释放。
- `disabled` 模式不编译正则、不扫描 schema。
- `sanitize` / `reject` 模式会对工具 schema 做一次递归扫描；只有非法 key 才计算 SHA-256 hash 并写入 map。
- 合法 key 不改写、不建映射；正常请求只承担 regex 校验和 schema 遍历成本。
- 流式路径只对存在映射的工具暂存 `input_json_delta`；没有非法 key 的工具保持原有增量输出，不增加 JSON 聚合成本。
- 并发多会话、多工具不会互相污染，因为 map 不共享、按上游 tool name 分组，并且 hash 输入包含 tool name 与 schema path。

## 回归清单

- [x] 合法 key 保持 200，且返回 `tool_use.input` key 与请求 key 一致。
- [x] 非法 key 在默认 `sanitize` 下恢复 200，返回参数 key 还原为客户端原始 key。
- [x] 返回给客户端的 `tool_use.input` 不包含内部 `key<hash>`。
- [x] 合法 key 不建映射、不被重命名。
- [x] 嵌套 `properties`、`required`、`dependentRequired`、`dependentSchemas`、legacy `dependencies` 同步处理。
- [x] 不改写 `patternProperties` 正则 key、`$defs` 定义名、`$ref` 字符串。
- [x] 多工具 map 隔离；清洗后 key 碰撞时 deterministic retry 或本地明确报错。
- [x] stream / non-stream / leaked `<invoke>` 路径均反向映射。
- [x] `reject` 模式本地明确报错，不清洗、不发上游。
- [x] `disabled` 模式保留旧透传行为。
- [x] 两套 UI 暴露配置，且 release build 要求 `admin-ui/dist` 与 `ui/dist` 都存在并嵌入。

## 关联

- `feature/issues/empty-tool-description-400-invalid-tool-use-format.md`（问题 A/B：空 description、`input_schema:null`，同一工具字段兼容性主题）。
- 生产样本：`req_01adUsZb5CJcXzzfYm3WFUYR`（07/07 04:54，账号 #462，`tools.26.custom.input_schema.properties`）。
