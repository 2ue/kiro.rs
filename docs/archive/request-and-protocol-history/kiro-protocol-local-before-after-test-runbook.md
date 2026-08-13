# Kiro 协议改造本地前后对比测试 Runbook

日期：2026-06-16  
目的：协议修改完成后，在本地用同一批真实请求对比改造前/改造后的行为变化。  
约束：不在生产代码中写“改造前/改造后”双分支；前后对比通过基线日志、请求样本、Claude Code CLI 真实会话完成。  
模型约束：本地凭据为 free，只测试 Sonnet 系列，不测试 Opus/Haiku。

## 1. 测试目标

这套测试要回答四个问题：

1. `profileArn` 改造是否解决 header/query/body 混用导致的上游 400/403/500。
2. Claude Code CLI 真实使用是否保持流畅：多轮、长会话、MCP、agent、tools/search 都能跑通。
3. 是否消除或不再放大已知错误：`assistant-prefill final message is not supported`、`TOOL_USE_RESULT_MISMATCH`、`Expected toolResult blocks`、重复输出、工具 XML 泄漏。
4. 是否没有引入模型行为退化：不改 system prompt、不改用户消息语义、不做跨 family 模型降级，只用 Sonnet。

## 2. 前后对比原则

- 改造前先采集 baseline，不改代码、不清理关键日志。
- 改造后用完全相同的配置、凭据、模型、prompt、MCP 配置、Claude Code CLI 参数复测。
- 每个 case 都保存三类证据：客户端输出、服务端日志、HTTP/API 调用结果。
- 不通过简单 mock 代替真实 Claude Code CLI；mock 只用于定位小函数和复现边界。
- 不用 Opus/Haiku。所有 Claude Code CLI 和 curl 请求显式指定 `sonnet` 或 `claude-sonnet-*`。

## 3. 测试环境准备

当前本地信息：

- 服务配置：`config.json` 端口为 `127.0.0.1:9022`。
- Claude Code 兼容入口：`http://127.0.0.1:9022/cc`，实际 API 为 `/cc/v1/messages`。
- 本地 API key：`config.json` 的 `apiKey`，当前为 `sk-kiro-rs-local-debug`。
- `ccman cc current` 当前已经指向 `local-kiro-rs-9022`，URL 为 `http://127.0.0.1:9022/cc`。
- 本机 `cc` 被 Volta 覆盖，Rust 测试/构建建议显式使用 `/usr/bin/cc`。

推荐环境变量：

```bash
export KIROS_BASE_URL="http://127.0.0.1:9022"
export KIROS_CC_BASE_URL="http://127.0.0.1:9022/cc"
export KIROS_API_KEY="sk-kiro-rs-local-debug"
export CC=/usr/bin/cc
export CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc
export RUSTFLAGS='-C linker=/usr/bin/cc'
```

启动依赖：

```bash
docker compose -f docker-compose.local-infra.yml up -d
```

启动服务并保存日志：

```bash
mkdir -p .local-run/protocol-before-after
RUST_LOG=info,kiro_rs=debug cargo run -- --config config.json \
  2>&1 | tee .local-run/protocol-before-after/server-before.log
```

改造后复测时使用：

```bash
RUST_LOG=info,kiro_rs=debug cargo run -- --config config.json \
  2>&1 | tee .local-run/protocol-before-after/server-after.log
```

## 4. ccman 切换验证

确认当前 Claude Code 服务商：

```bash
ccman cc current
```

如果不是本地服务，添加或切换：

```bash
ccman cc add \
  --name local-kiro-rs-9022 \
  --desc "local kiro.rs /cc" \
  --base-url http://127.0.0.1:9022/cc \
  --api-key sk-kiro-rs-local-debug \
  --switch
```

验收：`ccman cc current` 显示 URL 为 `http://127.0.0.1:9022/cc`。

## 5. 改造前 baseline 采集

### 5.1 健康和模型列表

```bash
curl -sS "$KIROS_BASE_URL/healthz" | tee .local-run/protocol-before-after/before-healthz.json

curl -sS "$KIROS_CC_BASE_URL/v1/models" \
  -H "Authorization: Bearer $KIROS_API_KEY" \
  | tee .local-run/protocol-before-after/before-cc-models.json
```

记录点：

- HTTP 是否成功。
- 模型列表是否只选择 Sonnet 做后续测试。
- 服务日志中是否出现 `ListAvailableModels 失败`、`profileArn`、`403`、`Invalid token`。

### 5.2 最小 Claude Code 兼容请求

```bash
curl -sS "$KIROS_CC_BASE_URL/v1/messages" \
  -H "Authorization: Bearer $KIROS_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model":"sonnet",
    "max_tokens":256,
    "messages":[{"role":"user","content":"用一句话回答：本次请求是否成功？"}]
  }' \
  | tee .local-run/protocol-before-after/before-cc-minimal.json
```

记录点：

- 是否有 `assistant-prefill final message is not supported`。
- 是否有上游 500 包裹 400 的情况。
- 响应是否正常结束，是否重复输出同一句话。

### 5.3 流式工具调用基础请求

这个 case 用 curl 固定 tool schema，避免 Claude Code CLI 的交互变量影响 baseline。

```bash
curl -sS "$KIROS_BASE_URL/v1/messages" \
  -H "x-api-key: $KIROS_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model":"sonnet",
    "max_tokens":1024,
    "tools":[{
      "name":"get_city_time",
      "description":"返回城市当前时间",
      "input_schema":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}
    }],
    "messages":[{"role":"user","content":"调用工具查询上海时间，然后用中文简短说明。"}]
  }' \
  | tee .local-run/protocol-before-after/before-tool-use.sse
```

记录点：

- 是否出现结构化 `tool_use`。
- 是否泄漏 `<tool_use>`、`<invoke>` 或 XML 片段到 text。
- 是否重复生成相同 tool_use。

### 5.4 Claude Code CLI 非交互 smoke

```bash
claude -p --model sonnet --output-format stream-json --verbose \
  --debug-file .local-run/protocol-before-after/before-claude-smoke.debug.log \
  "请用三点列出当前目录这个项目是什么，不要改文件。" \
  | tee .local-run/protocol-before-after/before-claude-smoke.stream.jsonl
```

记录点：

- debug 日志是否命中 `/cc/v1/messages`。
- 输出是否完整、无重复段落。
- 是否出现 `server_error`、`bad_request`、assistant-prefill。

### 5.5 Claude Code CLI 工具/search 测试

使用内置工具读取/搜索，不允许编辑，验证工具调用和工具结果回传。

```bash
claude -p --model sonnet --output-format stream-json --verbose \
  --allowedTools "Read,Grep,LS" \
  --debug-file .local-run/protocol-before-after/before-claude-tools.debug.log \
  "请搜索 src/kiro 里 profileArn 相关逻辑，列出三个关键文件和原因。不要改文件。" \
  | tee .local-run/protocol-before-after/before-claude-tools.stream.jsonl
```

记录点：

- 是否能多次调用工具并继续回答。
- 是否有 `TOOL_USE_RESULT_MISMATCH` / `Expected toolResult blocks`。
- 是否出现重复 tool result 或重复最终段落。

### 5.6 MCP 测试

创建临时 MCP 配置，使用本地 stdio server。优先使用稳定、无网络依赖的 server；如果本机没有对应包，先记录环境缺失，不把它算作协议失败。

示例配置文件：

```bash
cat > .local-run/protocol-before-after/mcp-filesystem.json <<'JSON'
{
  "mcpServers": {
    "fs": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "."]
    }
  }
}
JSON
```

执行：

```bash
claude -p --model sonnet --output-format stream-json --verbose \
  --mcp-config .local-run/protocol-before-after/mcp-filesystem.json \
  --debug-file .local-run/protocol-before-after/before-claude-mcp.debug.log \
  "使用 MCP 查看当前目录文件，说明 README 和 docs 目录是否存在。不要改文件。" \
  | tee .local-run/protocol-before-after/before-claude-mcp.stream.jsonl
```

记录点：

- MCP server 是否启动成功。
- Claude 是否能调用 MCP tool 并继续回答。
- 是否出现 tool result mismatch、重复输出、XML 泄漏。

### 5.7 Agent 测试

使用 Claude Code 自定义 agent，不要求它真的改文件。

```bash
claude -p --model sonnet --output-format stream-json --verbose \
  --agents '{"protocol_auditor":{"description":"审计 Kiro 协议风险","prompt":"你只分析，不修改文件。重点看 profileArn、MCP、Claude Code tool use。"}}' \
  --agent protocol_auditor \
  --allowedTools "Read,Grep,LS" \
  --debug-file .local-run/protocol-before-after/before-claude-agent.debug.log \
  "请审计 src/kiro/endpoint 和 src/kiro/protocol.rs，输出三个最可能导致上游错误的点。" \
  | tee .local-run/protocol-before-after/before-claude-agent.stream.jsonl
```

记录点：

- agent 是否能发起工具调用。
- 长 system/agent prompt 是否触发 payload guard 异常。
- 是否出现重复最终回答。

### 5.8 长会话与多轮恢复

使用固定 session id，先制造上下文，再 resume。

```bash
SESSION_ID="11111111-1111-4111-8111-111111111111"

claude -p --model sonnet --session-id "$SESSION_ID" --output-format stream-json --verbose \
  --allowedTools "Read,Grep,LS" \
  --debug-file .local-run/protocol-before-after/before-long-1.debug.log \
  "第一轮：阅读 README.md 和 docs 目录，记住项目用途，回答 5 点。不要改文件。" \
  | tee .local-run/protocol-before-after/before-long-1.stream.jsonl

claude -p --model sonnet --resume "$SESSION_ID" --output-format stream-json --verbose \
  --allowedTools "Read,Grep,LS" \
  --debug-file .local-run/protocol-before-after/before-long-2.debug.log \
  "第二轮：基于上一轮上下文，搜索 profileArn，说明当前风险，不要重复上一轮原文。" \
  | tee .local-run/protocol-before-after/before-long-2.stream.jsonl
```

记录点：

- 第二轮是否保留上下文。
- 是否重复输出第一轮大段内容。
- 是否触发 assistant-prefill 或 tool mismatch。

## 6. 实施真实修改后的复测

修改完成后，先跑代码级测试：

```bash
cargo fmt --check
cargo test kiro::protocol -- --nocapture
cargo test kiro::endpoint -- --nocapture
cargo test kiro::provider -- --nocapture
```

如果本机 linker 仍被 Volta 覆盖，使用：

```bash
CC=/usr/bin/cc \
CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc \
RUSTFLAGS='-C linker=/usr/bin/cc' \
cargo test kiro::protocol -- --nocapture
```

然后按第 5 节完全相同命令复测，把文件名前缀从 `before-` 改为 `after-`。

## 7. 对比方法

### 7.1 错误关键词对比

```bash
rg -n "assistant-prefill|last message must be user|TOOL_USE_RESULT_MISMATCH|Expected toolResult|profileArn|Invalid token|bearer token invalid|500 Internal|403|400|524|duplicate|<tool_use|<invoke" \
  .local-run/protocol-before-after/*before* \
  | tee .local-run/protocol-before-after/before-errors.txt

rg -n "assistant-prefill|last message must be user|TOOL_USE_RESULT_MISMATCH|Expected toolResult|profileArn|Invalid token|bearer token invalid|500 Internal|403|400|524|duplicate|<tool_use|<invoke" \
  .local-run/protocol-before-after/*after* \
  | tee .local-run/protocol-before-after/after-errors.txt
```

通过标准：

- `assistant-prefill` 不出现。
- `TOOL_USE_RESULT_MISMATCH` / `Expected toolResult` 不出现；如果人为构造错误请求，应被映射为 400 且不重试风暴。
- profileArn 相关 400/403 不出现。
- 不出现 raw `<tool_use>` / `<invoke>` 泄漏。

### 7.2 Claude Code CLI 行为对比

对比 stream-json 输出：

```bash
for f in .local-run/protocol-before-after/*claude*.stream.jsonl; do
  printf '\n== %s ==\n' "$f"
  rg -n '"type":"(assistant|result|error|tool_use|tool_result)|server_error|bad_request' "$f" || true
done
```

人工验收点：

- 回答是否完整，不中途断裂。
- 多轮上下文是否连续。
- 工具调用是否自然，工具结果后能继续推理。
- 没有重复整段输出、重复 tool call、重复 final message。
- 不出现为了规避错误而明显变笨：答非所问、无视工具结果、只输出模板话术。

### 7.3 服务日志对比

重点看 attempt chain、credential 切换和上游 request id：

```bash
rg -n "Kiro API 凭据调用链路|attempt|retry|disable_and_retry|force_refresh|ListAvailableProfiles|ListAvailableModels|profileArn|request id|上游" \
  .local-run/protocol-before-after/server-before.log \
  > .local-run/protocol-before-after/before-server-important.log || true

rg -n "Kiro API 凭据调用链路|attempt|retry|disable_and_retry|force_refresh|ListAvailableProfiles|ListAvailableModels|profileArn|request id|上游" \
  .local-run/protocol-before-after/server-after.log \
  > .local-run/protocol-before-after/after-server-important.log || true
```

通过标准：

- 改造后同一 case 的 retry 次数不增加。
- 400 bad request 不被当成 5xx 反复换凭据。
- BuilderId/free 的 streaming 仍能成功，不因缺 `profileArn` 报 400。
- model list / usage / MCP/header 不因占位/fallback `profileArn` 报 403。

## 8. 最终验收矩阵

| 场景 | 改造前记录 | 改造后要求 |
| --- | --- | --- |
| `/cc/v1/models` | 保存 before 响应和日志 | 成功返回 Sonnet；无 profileArn 403 |
| `/cc/v1/messages` 最小请求 | 保存 before 响应和日志 | 成功；无 assistant-prefill；无重复输出 |
| `/v1/messages` tool use | 保存 before SSE | 结构化 tool_use 正常；无 XML 泄漏 |
| Claude Code CLI smoke | 保存 stream-json/debug | 命中本地 `/cc/v1/messages`；输出完整 |
| Claude Code tools/search | 保存 stream-json/debug | 多工具调用后继续回答；无 tool mismatch |
| MCP | 保存 stream-json/debug | MCP tool 可用；无重复/泄漏 |
| Agent | 保存 stream-json/debug | agent + tools 正常；无 prompt 行为退化 |
| 长会话 resume | 保存两轮输出 | 上下文连续；不重复大段历史 |
| 错误分类 | 保存 server log | 客户端协议错不触发 503/retry 风暴 |

## 9. 不通过处理

- 如果只在 before 失败、after 成功：记录为修复收益。
- 如果 before/after 都失败且错误相同：不是本次改造收益，另建问题。
- 如果 after 新增失败：回到对应改动点，优先检查 profileArn 用途、endpoint、TokenType、model mapping。
- 如果 after 输出质量下降但协议无错：检查是否误改 system prompt、messages、tool schema、model family 映射。

## 10. 2026-06-16 最终复测记录

本节记录本轮真实复测的实际命令产物和判读方式。测试产物保存在 `.local-run/protocol-before-after/`，不提交到版本库。

### 10.1 环境

- 服务进程：`./target/debug/kiro-rs -c config.json --credentials credentials.json`
- 服务地址：`http://127.0.0.1:9022`
- Claude Code 入口：`http://127.0.0.1:9022/cc`
- `ccman cc current`：`local-kiro-rs-9022`
- 服务日志：`.local-run/protocol-before-after/server-after-latest.log`
- 最终扫描起始行：`.local-run/protocol-before-after/final-server-start-line.txt`
- 模型：只用 `sonnet`

### 10.2 HTTP/API 产物

| 产物 | 场景 | 结果 |
| --- | --- | --- |
| `final-healthz.json` | `/healthz` | JSON 成功 |
| `final-cc-models.json` | `/cc/v1/models` | 返回 19 个模型 |
| `final-cc-minimal.json` | `/cc/v1/messages` 最小 Sonnet | 返回 `成功。` |
| `final-tool-use.json` | `/v1/messages` 自定义工具 schema | 返回结构化 `tool_use`，未泄漏 XML |

### 10.3 Claude Code CLI 产物

| 产物 | 场景 | 判定 |
| --- | --- | --- |
| `final-claude-smoke.*` | smoke，读项目文件并回答 | success/completed |
| `final-claude-tools.*` | `Read/Grep/LS` 搜索 `profileArn` | success/completed |
| `final-long-1.*` / `final-long-2.*` | 两轮长会话 resume | success/completed |
| `final-claude-mcp-allowed.*` | MCP filesystem `mcp__fs__list_directory` | success/completed |
| `final-claude-agent-clean.*` | 自定义 agent + tools | success/completed |

长会话最终 session id：

```text
61a8eecd-9db0-41fa-842a-44cdbd621beb
```

### 10.4 环境事件与非协议失败

- 旧 session id `387d8932-8414-42f4-a955-4d86e012ecab` 被 Claude CLI 报 `already in use`，因此最终复测改用新 session id。这是本地 Claude CLI session 状态，不是 Kiro 上游请求失败。
- `final-claude-mcp.stream.jsonl` 第一次 MCP 运行未显式允许 `mcp__fs__list_directory`，被 Claude CLI 权限层拦截；随后 `final-claude-mcp-allowed.*` 使用 `--allowedTools "ToolSearch,mcp__fs__list_directory,mcp__fs__get_file_info,mcp__fs__read_file"` 重跑成功。
- `final-claude-agent*.stream.jsonl` 中 agent 曾先调用小写 `bash` / `read` / `glob` 这类不存在工具名，随后用正确大小写工具恢复并最终 success。这是 Claude CLI 工具名选择问题，不是代理协议错误。
- Debug 日志里的全局 MCP server 噪声，如 `exa`、`context7`、`tapd_mcp_http` 初始化问题，不作为 Kiro 协议失败；显式 filesystem MCP 已成功。
- 服务日志中有个别凭据 token refresh 瞬态失败和 AWS OAuth `500 Internal Server Error {"message":"Oops, something went wrong. Please try again later."}`，被记录为 auth transient cooldown。当前测试请求最终成功，且未出现用户报告的 `generateAssistantResponse` 非流式请求 500 或 `assistant-prefill` 错误。

### 10.5 最终扫描命令

```bash
rg -n "server_error|bad_request|assistant-prefill|last message must be user|TOOL_USE_RESULT_MISMATCH|Expected toolResult|profileArn is required|上游 API 调用失败|非流式 API 请求失败|500 Internal Server Error|ListAvailableModels 失败|ListAvailableProfiles 返回 403" \
  .local-run/protocol-before-after/final-* \
  .local-run/protocol-before-after/server-after-latest.log || true

python3 - <<'PY'
import json, pathlib
base = pathlib.Path('.local-run/protocol-before-after')
for p in sorted(base.glob('final-*.stream.jsonl')):
    successes = completed = denials = local_tool_errors = 0
    api_errors = []
    for line in p.read_text(errors='replace').splitlines():
        if '<tool_use_error>' in line:
            local_tool_errors += 1
        try:
            obj = json.loads(line)
        except Exception:
            continue
        if obj.get('type') == 'result':
            successes += obj.get('subtype') == 'success'
            completed += obj.get('terminal_reason') == 'completed'
            if obj.get('api_error_status') is not None:
                api_errors.append(obj.get('api_error_status'))
            denials += len(obj.get('permission_denials') or [])
    print(p.name, successes, completed, api_errors, denials, local_tool_errors)
PY
```

验收重点：

- 所有最终有效场景 `api_error_status = null`。
- 所有最终有效场景 `terminal_reason = completed`。
- 无 `assistant-prefill`、`last message must be user`、`TOOL_USE_RESULT_MISMATCH`、`Expected toolResult`、`profileArn is required`。
- MCP 使用显式 allowed tools 的通过产物为 `final-claude-mcp-allowed.*`。
