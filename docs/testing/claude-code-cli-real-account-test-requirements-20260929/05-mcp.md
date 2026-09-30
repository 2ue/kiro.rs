# 05 MCP

本分册用例编号沿用 `CC-MCP-<序号>`。CC-MCP-01 ~ CC-MCP-23 为原有编号，CC-MCP-24 ~ CC-MCP-30 为本次吸收既有场景后新增，不新开组前缀。

「来源」列取值：既有场景（来自历史测试中已出现过的场景）/ 新增（用户要求）/ 新增（补充）。

## 测试 server 准备

在 `$EVID/mcp/` 下准备测试 server（不入库，结束后停止所有 server 进程）：

| server | 类型 | 工具/能力 | 用途 |
| --- | --- | --- | --- |
| `ping` | stdio（换行分隔 JSON-RPC，每条消息一行 JSON；不要用 `Content-Length` 头分帧，当前 CLI 无法连接这种写法的 server，属于测试 server 问题而非代理问题） | `ping` 返回固定串、`fail` 返回错误、`slow` 可配置延迟、`big` 返回指定大小文本、`echo` 原样返回参数 | 基础调用、错误、超时、大结果、参数保真 |
| `schema` | stdio | 复杂 schema 工具：嵌套 object、数组对象、`enum`、`oneOf/anyOf`（嵌套层与根级各一个）、`nullable`、`required/properties` 为 null、`$ref/$defs`、`additionalProperties`（字典型与 `false`）、`format`、带点号/连字符/空格的属性名（如 `"bad key"`）、中文 description、空 description、无参数工具、超长 description（>4000 字符，另备约 10000 个中文字符的版本）、名称接近 64 字符、PascalCase 与 kebab-case 名称、含 `.` 或 `$` 的非法名称 | schema 转换、属性键映射、工具名映射、description 边界 |
| `http-demo` | Streamable HTTP（本地端口，如 `127.0.0.1:19180/mcp`） | 同 ping | http 传输 |
| `sse-demo` | SSE（如 `127.0.0.1:19181/sse`） | 同 ping | sse 传输 |
| `res` | stdio | resources（`res://doc/1` 文本、`res://img/1` 图片）与 prompts（带参数的 prompt 模板） | 资源与 prompt |
| `rich` | stdio | `rag` 返回 `search_result` 块或文本 `document` 块（带来源 URL 与标题）；`pic_url` 返回 source 为 url 的 image 块；`pic_file` 返回 source 为 Files API file_id 的 image 块；`read` 工具（小写，用于与内置 `Read` 冲突） | 结果内容类型、工具名大小写冲突 |
| `many` | stdio | 80 个工具 | 工具定义预算（`toolDefinitionsBudgetBytes`、`compressToolDefinitions`） |
| `fs` | stdio，官方 filesystem MCP server，根目录指向测试仓库副本 | `list_directory` 等 | ToolSearch 发现与只读调用 |

MCP 配置文件示例（`$EVID/mcp/mcp-all.json`）：

```json
{
  "mcpServers": {
    "ping":   { "type": "stdio", "command": "node", "args": ["ping.mjs", "--delay=0"], "env": { "PING_TOKEN": "demo-not-secret" } },
    "schema": { "type": "stdio", "command": "node", "args": ["schema.mjs"] },
    "http-demo": { "type": "http", "url": "http://127.0.0.1:19180/mcp", "headers": { "X-Demo": "1" } },
    "sse-demo":  { "type": "sse",  "url": "http://127.0.0.1:19181/sse" },
    "res":    { "type": "stdio", "command": "node", "args": ["res.mjs"] },
    "rich":   { "type": "stdio", "command": "node", "args": ["rich.mjs"] }
  }
}
```

通用命令：`ccp --strict-mcp-config --mcp-config $EVID/mcp/<cfg>.json --allowedTools 'mcp__<server>__<tool>' ...`。

通用判定：`system/init` 中 MCP server 状态为 connected；工具名 `mcp__<server>__<tool>` 原样出现在 CLI 的 tool_use；参数与 server 实际收到的一致（server 侧记录收到的 arguments）；结果（含错误）以 tool_result 回传且下一轮正常；debug log 有 server connected、tool dispatch、completed 记录；服务日志无 400/429/500 或 `Improperly formed request`。

## 用例

### 连接、传输与基础调用

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-MCP-01 | P0 | 既有场景 | ping（换行分隔 JSON-RPC） | `claude -p --strict-mcp-config --mcp-config <配置> --tools "" --allowedTools mcp__ping__ping --dangerously-skip-permissions "Use the ping MCP ping tool, then reply with exactly: current mcp ok"` | stdio | 出现 tool_use `mcp__ping__ping`，tool_result 为 server 返回的固定串；第二轮返回指定文本 `current mcp ok` | 通用判定；`num_turns=2`，退出码 0，usage 非 0。若 server 连不上，先确认不是 `Content-Length` 分帧写法导致的测试 server 问题 |
| CC-MCP-02 | P0 | 既有场景 | ping | 同 MCP-01 的配置，分别让模型调用 `ping`（成功）和 `fail`（报错），报错后要求回复固定文本 | 工具错误 | 成功结果正常返回；`fail` 的错误作为错误 tool_result（`is_error`）回传给代理，模型说明工具失败，第二轮正常完成并返回指定文本 | 退出码 0，usage 非 0；代理不卡死，不影响后续请求。已知问题：MCP 错误曾被当成"无结果"的空成功；MCP 或 WebSearch 的辅助请求失败曾污染主调度器的凭据健康状态。预期：错误不伪装成成功；辅助失败后凭据健康状态不变（管理接口/日志中该凭据未被标记失败或降权） |
| CC-MCP-03 | P0 | 新增（用户要求） | http-demo | 调用 http 版 ping | http | 成功 | 同上 |
| CC-MCP-04 | P1 | 新增（用户要求） | sse-demo | 调用 sse 版 ping | sse | 成功 | 同上 |
| CC-MCP-05 | P0 | 新增（用户要求） | mcp-all | 同一轮要求分别调用 ping、http-demo、sse-demo 的工具 | 多 server、并行 | 3 个调用都成功 | 并行 MCP 工具配对正确 |
| CC-MCP-29 | P0 | 既有场景 | fs（非 `--bare` 模式）；另备 `--bare` 对照组 | `--mcp-config` 指向 fs，`--allowedTools "ToolSearch,mcp__fs__list_directory"`，prompt：`使用 MCP 查看当前目录文件并列出，不要改文件`；对照组同命令加 `--bare`，并用 ping 重复一次（期望回显 pong） | ToolSearch 发现、bare 模式 | CLI 先用 ToolSearch 发现 MCP 工具，再发出真实 MCP tool_use，拿到结果后完成回答（能看到 README.md 和 docs 目录，或回显 pong） | 最后一条 tool_use 与当前 tool_result 在同一 user turn 正确配对，不被误删；无重复输出、无 XML 泄漏；服务日志无 400/429/500 或 `Improperly formed request`。已知现象：`--bare` 模式下自定义 MCP 虽然 connected，但工具不暴露给模型，只能算连接检查，不计为调用通过 |

### schema、名称与参数映射

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-MCP-06 | P0 | 既有场景 | schema | 逐个调用复杂 schema 工具，参数覆盖多层嵌套对象、数组、enum、null；对含非法属性键（如 `"bad key"`）的工具分别在 stream 与 non-stream 下各调用一次 | 复杂/嵌套 schema、属性键往返映射 | server 收到的参数与 schema 一致，参数结构完整；tool_use/tool_result 配对 | 属性名被 `toolSchemaKeyMapping=sanitize` 映射成合法 ID 发往上游，CLI 收到的 `tool_use.input` 恢复成原始键（如 `{"bad key": ...}`），不出现内部哈希键；合法键保持不变；映射只在单个请求内生效；不出现 400 invalid tool/schema。本用例也是交互和长会话兼容回归矩阵的一部分 |
| CC-MCP-07 | P1 | 既有场景 | schema | 分别调用：合法名 `mcp__srv__do_thing`、PascalCase、含连字符、接近 64 字符、含 `.` 或 `$` 的非法名工具；每类连续调用 5 轮 | 工具名映射 | 全部 200，调用成功 | 合法名原样发给上游，`tool_use.name` 与原名一致，CLI 看到原名而非哈希名；非法名在上游被映射，返回 CLI 时还原。已知现象：旧版会删除 `mcp__a__b` 的分隔符并加哈希。预期：只有含 `$`、`.`、CJK、空格或超过 63 字符的名字才需映射 |
| CC-MCP-08 | P1 | 既有场景 | schema | 调用空 description 工具、超长 description（>4000 字符）工具，以及描述约 10000 个中文字符的工具 | description 边界、长度口径 | 成功，无 400 | 超长描述被截断（`toolDescriptionMaxChars`）不影响调用。长度口径待验证：代理按 10000 字符截断，上游可能按字节计（约 10237 字节），而 10000 个中文字符约 30000 字节；记录上游是否接受，以确定上限按字符还是字节计 |
| CC-MCP-09 | P1 | 新增（用户要求） | ping `echo` | 参数包含中文、emoji、换行、引号、反斜杠、大整数、浮点、布尔、空数组 | 参数保真 | 原样回显 | 逐字段比对 |
| CC-MCP-23 | P2 | 新增（补充） | 同名工具存在于两个 server | 分别调用 | 命名冲突 | 调用到正确 server | 名称映射不冲突 |
| CC-MCP-24 | P1 | 既有场景 | rich（提供小写 `read`），内置工具保留 `Read` | 让模型分别调用内置 `Read` 与 MCP 的 `read` | 工具名大小写冲突 | 无本地 400，两个工具都能调用 | 合法名透传后服务端按小写做碰撞检测；冲突时后出现的名字回退为哈希映射，而不是报错；返回 CLI 时两者名字都还原正确 |
| CC-MCP-27 | P1 | 既有场景 | schema | 调用 input_schema 根级为 `oneOf`/`anyOf`/`allOf` 的工具，以及 `required`/`properties` 为 null 的工具 | 根级组合子、null 字段 | 不 400，工具可被调用 | 不出现 `TOOL_SCHEMA_INVALID` 或 `Improperly formed request`。已知现象：根级组合 schema 或 null 字段曾导致上游对整个请求返回 400；预期：根级整理为 object 后展平。注意这是比官方更宽容的降级（官方对根级组合子直接 400）："二选一必填"降级为都不必填，同名属性只保留第一个分支的类型；嵌套层组合子保留 |
| CC-MCP-28 | P1 | 既有场景 | schema | 调用参数含字典型 `additionalProperties`、`additionalProperties: false` 或 `format` 的工具 | 上游不支持的 schema 关键字 | 调用正常，无 400，server 收到的参数类型正确 | 上游遇到这些关键字会拒绝整个请求，代理全局删除它们。副作用：字典值类型和"禁止额外属性"约束丢失，模型可能传错类型；记录是否出现类型错误或多余属性 |
| CC-MCP-30 | P1 | 既有场景 | ping + schema，内置工具与 Task 子代理可用 | 同一会话中组合使用 MCP 工具（含需映射的名称和属性键）、内置工具（Read/Bash 等）和子代理（子代理内也调用 MCP 工具） | MCP + 内置工具 + 多代理 | 流程完成 | 工具名和参数键在主会话与子代理中都能无损往返；改名映射必须可逆，不能有损；无 400 |

### 结果内容与资源

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-MCP-10 | P1 | 新增（补充） | ping `big` | 返回 100KB / 1MB 文本 | 大结果 | CLI 按 `MAX_MCP_OUTPUT_TOKENS` 截断或完整传递 | 服务端不崩溃；过大导致上游上下文过长时，按 07-context-compaction-tokens-cache.md 中的 too-long 处理（交给 CLI 压缩，不做静默裁剪） |
| CC-MCP-15 | P1 | 新增（补充） | res | 交互式中 `@res:res://doc/1` 引用资源，或让模型调用资源读取工具 | resources | 资源内容进入上下文 | 图片资源以 image 块传递时服务端正确处理 |
| CC-MCP-16 | P2 | 新增（补充） | res | 交互式执行 MCP prompt 斜杠命令 `/mcp__res__<prompt> 参数` | prompts | prompt 展开并执行 | 展开后的消息正常发送 |
| CC-MCP-25 | P1 | 既有场景 | rich `rag` | 调用返回 `search_result` 块（嵌套在 tool_result 内，以及顶层）或文本 `document` 块的工具查询，再追问"来源是什么、标题是什么" | search_result / document | 请求 200；模型能复述内容并说出来源 URL 与标题 | 已知现象：search_result 曾被整块转成 JSON，出处结构丢失；顶层 search_result 曾被直接丢弃。预期：出处信息保留到模型可见的内容中 |
| CC-MCP-26 | P1 | 既有场景 | rich `pic_url` / `pic_file` | 调用返回 image 块（source 为 url 或 Files API file_id）的工具，让模型描述图片 | tool_result 内远程图片 | 请求 200，模型能描述图片 | 嵌套在 tool_result 中的来源与顶层一样先下载物化，同样受 SSRF 防护（内网地址被拒）和图片数量预算约束。已知现象：嵌套 url/file 图片曾未物化，直接 400 `remote image URL source was not materialized before conversion` |

### 超时、故障与规模

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-MCP-11 | P1 | 新增（用户要求） | ping `slow`，`MCP_TOOL_TIMEOUT=3000` | 调用延迟 10s 的工具 | 工具超时 | CLI 报超时，模型继续 | 等待期间服务端没有请求挂起（工具执行在客户端） |
| CC-MCP-12 | P1 | 新增（用户要求） | `MCP_TIMEOUT=2000` + 启动很慢的 server | 启动 CLI | server 连接超时 | CLI 标记 failed，其余功能正常 | 请求 tools 中不含失败 server 的工具 |
| CC-MCP-13 | P1 | 新增（补充） | 配置一个不存在的 command | 启动 | server failed | 同上 | 同上 |
| CC-MCP-14 | P1 | 新增（补充） | 调用过程中 kill server 进程 | 工具调用 | 返回错误 | 会话可继续 | 同 MCP-02 |
| CC-MCP-17 | P1 | 既有场景 | many，另可叠加 mcp-all 多个 server | 80 个工具全部加载，调用其中第 70 个；再多轮使用工具（每轮都带完整工具定义与累积的工具结果） | 工具数量、请求体大小 | 调用成功；请求不因工具 schema 过大被上游拒绝 | 工具定义压缩后该工具仍可调用；请求体保护介入后工具调用仍完整；记录每轮请求体大小与压缩动作。背景：工具定义和工具结果容易把请求体撑大 |

### 配置、权限与模型组合

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-MCP-18 | P1 | 新增（用户要求） | ping | 通过 `claude mcp add`（写入隔离 config）而非 `--mcp-config` 添加 server，scope 分别为 local/project/user | 配置来源 | 均可用 | `.mcp.json` 项目配置首次需批准 |
| CC-MCP-19 | P2 | 新增（补充） | mcp-all | 交互式 `/mcp` 查看状态、重连、禁用某 server | 管理命令 | 状态正确 | 禁用后下一请求 tools 不含该 server |
| CC-MCP-20 | P1 | 新增（用户要求） | ping，不加 `--allowedTools`，默认权限 | 交互式调用 MCP 工具 | 权限询问 | 弹权限确认，允许后执行 | 拒绝时的 tool_result 处理正确 |
| CC-MCP-21 | P1 | 新增（用户要求） | ping + thinking（`MAX_THINKING_TOKENS=4096`） | thinking → MCP 调用 → thinking → 回答 | MCP + thinking | 成功 | 同 03-thinking.md 的 CC-T-21 |
| CC-MCP-22 | P1 | 新增（用户要求） | ping，haiku-4.5 | 同 MCP-01 | haiku | 成功 | 小模型工具调用可靠性：记录是否出现"只说要调用工具但以 end_turn 结束、没有发出 tool_use"的现象 |
