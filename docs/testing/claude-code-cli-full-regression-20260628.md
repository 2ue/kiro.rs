# Claude Code CLI 与 Kiro 代理回归测试报告（2026-06-28）

本文记录 2026-06-28 对当前未提交工作区的协议、调度、cachePoint、thinking、tool-use、MCP、异常和资源压力验证。测试只使用隔离端口：

- 本地代理：`127.0.0.1:19022`
- fake Kiro upstream：`127.0.0.1:19080`
- 管理密钥：`admin123`

本轮没有启动或重启日常开发端口 `9022`。

## 结论

- 当前代码下，Claude Code CLI 能通过 `/cc/v1/messages` 收到真实 `thinking_delta`、`signature_delta`、`text_delta` 和非零 usage。
- `sonnet-thinking`、完整 `claude-sonnet-4-6-thinking`、以及 Claude Code CLI 的 `--effort high` 都能触发 thinking 链路。
- Claude Code CLI Bash tool-use、MCP tool-use 成功回传、MCP tool-use 错误回传均能完成 round-trip。
- cachePoint 开启后能按工具 `cache_control` 插入 Kiro `cachePoint`；上游拒绝 cachePoint 时会自动去掉 cachePoint 并重试一次。
- 高并发、突发异常、异常恢复、stream idle、client drop、RPM、dfcache 的隔离压测没有发现进程卡死或持续 FD 泄漏。
- 真实 Claude Code CLI 客户端已经验证；真实 Kiro 上游模型质量、图片识别、大文档理解、真实上游长会话智商效果没有在本轮跑，因为那会消耗真实账号并影响真实账号状态。

## 关键注意事项

本机全局 `~/.claude/settings.json` 里存在 `env.ANTHROPIC_BASE_URL=https://okmcode.com`。所有 Claude Code CLI 指向本地代理的命令必须加：

```bash
--setting-sources project,local
```

否则 CLI 会打到全局配置里的服务，测试结论会被污染。

## 构建与静态验证

已运行：

```bash
cargo fmt
CLANG=$(xcrun --find clang)
SDKROOT=$(xcrun --show-sdk-path)
CC="$CLANG" CC_aarch64_apple_darwin="$CLANG" CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER="$CLANG" cargo test --locked --no-default-features
pnpm --dir ui check
pnpm --dir ui build
CC="$CLANG" CC_aarch64_apple_darwin="$CLANG" CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER="$CLANG" cargo build --locked --no-default-features --bin kiro-rs --bin kiro_loadtest
```

结果：

- Rust 主测试：`732 passed`
- `kiro_loadtest` 单测：先前 `9 passed`；新增 MCP fake 工具选择回归后当前为 `11 passed`
- UI check：通过
- UI build：通过，仅 Vite chunk size 警告
- 二进制构建：通过

## 隔离服务

代理：

```bash
target/debug/kiro-rs \
  --config .local-run/loadtest-config-20260627215353.json \
  --credentials .local-run/empty-credentials-20260627215353.json
```

fake upstream 常规启动：

```bash
target/debug/kiro_loadtest \
  --fake-listen 127.0.0.1:19080 \
  --fake-only true \
  --fake-kiro-eventstream true \
  --scenario normal-stream \
  --fake-delay-ms 120 \
  --fake-capture-dir .local-run/fake-captures-final-normal-current
```

最终状态：`19022` 代理保持运行，`19080` fake upstream 已恢复为 `normal-stream`。

## Claude Code CLI Thinking

### `sonnet-thinking`

命令：

```bash
ANTHROPIC_API_KEY=admin123 \
ANTHROPIC_BASE_URL=http://127.0.0.1:19022/cc \
CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 \
claude --bare --setting-sources project,local \
  -p --verbose --output-format stream-json --include-partial-messages \
  --debug-file .local-run/claude-cli-sonnet-thinking-localsettings.debug.log \
  --model sonnet-thinking \
  "Think briefly, then reply with exactly: cli model thinking ok" \
  > .local-run/claude-cli-sonnet-thinking-localsettings.stream.jsonl
```

证据：

- `.local-run/claude-cli-sonnet-thinking-localsettings.stream.jsonl`
- 包含 `thinking_delta`
- 包含 `signature_delta`
- 包含 `text_delta`
- 结果 usage 非 0：`input_tokens=19`、`output_tokens=9`
- `modelUsage.sonnet-thinking.outputTokens=9`

### 完整 thinking 模型名

命令输出：

- `.local-run/claude-cli-full-sonnet-thinking-localsettings.stream.jsonl`

证据：

- 模型：`claude-sonnet-4-6-thinking`
- 包含 `thinking_delta`
- 包含 `signature_delta`
- 包含 `text_delta`
- 结果 usage 非 0：`input_tokens=2`、`output_tokens=9`

### `--effort high`

命令输出：

- `.local-run/claude-cli-effort-high-localsettings.stream.jsonl`

证据：

- CLI 使用普通模型 `claude-sonnet-4-6`
- 请求仍携带 adaptive thinking 配置
- 代理保留该配置并转换到 Kiro history
- 结果 usage 非 0：`input_tokens=22`、`output_tokens=9`

## Claude Code CLI Tool Use

### Bash tool-use

命令：

```bash
ANTHROPIC_API_KEY=admin123 \
ANTHROPIC_BASE_URL=http://127.0.0.1:19022/cc \
CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 \
claude --bare --setting-sources project,local \
  --dangerously-skip-permissions --allowedTools Bash \
  -p --verbose --output-format stream-json --include-partial-messages \
  --debug-file .local-run/claude-cli-tool-use-localsettings-rerun.debug.log \
  --model sonnet \
  "Use Bash to print cli tool ok, then finish." \
  > .local-run/claude-cli-tool-use-localsettings-rerun.stream.jsonl
```

证据：

- 第一轮返回 `tool_use`，工具名 `Bash`
- `input_json_delta` 为 `{"command":"echo cli tool ok"}`
- CLI 执行 Bash 后回传 `tool_result`，内容 `cli tool ok`
- 第二轮正常完成
- `num_turns=2`
- 结果 usage 非 0

### MCP tool-use 成功

使用本地 MCP server：

- 配置：`.local-run/cc-real-tests/mcp-config.json`
- server：`.local-run/cc-real-tests/mcp-ping-server.js`

命令：

```bash
ANTHROPIC_API_KEY=admin123 \
ANTHROPIC_BASE_URL=http://127.0.0.1:19022/cc \
CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 \
claude --bare --setting-sources project,local \
  --strict-mcp-config \
  --mcp-config .local-run/cc-real-tests/mcp-config.json \
  --tools "" \
  --allowedTools mcp__kiro-local-test__ping \
  --dangerously-skip-permissions \
  -p --verbose --output-format stream-json --include-partial-messages \
  --debug-file .local-run/claude-cli-current/mcp-ping-current-fixed.debug.log \
  --model sonnet \
  "Use the kiro-local-test MCP ping tool, then reply with exactly: current mcp ok" \
  > .local-run/claude-cli-current/mcp-ping-current-fixed.stream.jsonl
```

证据：

- `.local-run/claude-cli-current/mcp-ping-current-fixed.stream.jsonl`
- 实际 tool_use：`mcp__kiro-local-test__ping`
- 实际 tool_result：`mcp-pong-kiro-local`
- debug log 记录 `Tool 'ping' completed successfully in 4ms`
- 第二轮正常完成
- `num_turns=2`
- 结果 usage 非 0：`input_tokens=20`、`output_tokens=15`

### MCP tool-use 错误回传

输出：

- `.local-run/claude-cli-current/mcp-tool-current.stream.jsonl`
- `.local-run/claude-cli-current/mcp-tool-current.debug.log`

证据：

- fake upstream 首次选择 `mcp__kiro-local-test__fail`
- MCP server 返回 `mcp-fail-kiro-local`
- CLI 将错误作为 `tool_result` 回传给代理
- 第二轮正常完成
- 结果 usage 非 0

这个场景验证错误 tool_result 不会让代理卡死或破坏后续请求。

## cachePoint

当前 19022 隔离代理 runtime 中：

```json
{
  "kiroCachePointEnabled": true,
  "kiroCachePointToolsOnly": true,
  "kiroCachePointRecordPlan": true
}
```

### 插入验证

请求带 Anthropic tool `cache_control`：

```json
{
  "model": "claude-sonnet-4-6",
  "max_tokens": 64,
  "stream": true,
  "messages": [{"role": "user", "content": "call tool if needed"}],
  "tools": [{
    "name": "echo",
    "description": "Echo text",
    "input_schema": {
      "type": "object",
      "properties": {"text": {"type": "string"}},
      "required": ["text"]
    },
    "cache_control": {"type": "ephemeral"}
  }]
}
```

证据：

- 下游输出：`.local-run/cachepoint-current.sse`
- fake capture：`.local-run/fake-captures-mcp-current-fixed/fake_req_3.json`
- fake Kiro 收到的最终 body 中 `cachePointCount=1`
- 下游仍收到正常 tool-use stream

### 拒绝后重试

fake upstream 使用 `cache-point-reject` 场景。

证据：

- 下游输出：`.local-run/cachepoint-retry-current.sse`
- fake capture：
  - `.local-run/fake-captures-cachepoint-reject-current/fake_req_1.json`：`hasCachePoint=true`
  - `.local-run/fake-captures-cachepoint-reject-current/fake_req_2.json`：`hasCachePoint=false`
- 下游最终收到正常 text stream，没有暴露第一次上游拒绝的原始错误。

## 压测与异常矩阵

所有报告均来自 `127.0.0.1:19022` 代理和 `127.0.0.1:19080` fake upstream。

| 场景 | 报告 | 结果 | 延迟/资源结论 |
| --- | --- | --- | --- |
| 普通突发并发 300x80 | `.local-run/loadtest-reports/proxy-normal-burst-300x80-after-thinking-fix.json` | `300/300 success` | TTFB p99 `8217ms`；RSS `36.9MB -> 97.3MB -> 87.1MB`；FD `28 -> 163 -> 108`，后续检查可回落 |
| thinking 模型 60x20 | `.local-run/loadtest-reports/proxy-thinking-model-60x20-after-thinking-fix.json` | `60/60 success` | firstThinking p99 `1771ms` |
| 显式 thinking 60x20 | `.local-run/loadtest-reports/proxy-explicit-thinking-60x20-after-thinking-fix.json` | `60/60 success` | firstThinking p99 `1403ms` |
| RPM 限制 | `.local-run/loadtest-reports/proxy-rpm-limit-3-after-thinking-fix.json` | `1 success`、`7 x 429` | 限频生效，错误带 error id |
| dfcache 已配置 | `.local-run/loadtest-reports/proxy-dfcache-test-after-thinking-fix.json` | `8/8 success` | TTFB p99 `372ms` |
| 429 异常 | `.local-run/loadtest-reports/proxy-abnormal-clean-rate-limit429-after-thinking-fix.json` | `5/5 errors`，status `429` | error id `5` 个 |
| invalid tool format | `.local-run/loadtest-reports/proxy-abnormal-clean-invalid-tool-after-thinking-fix.json` | `5/5 errors`，status `400` | error id `5` 个 |
| malformed SSE | `.local-run/loadtest-reports/proxy-abnormal-clean-malformed-sse-after-thinking-fix.json` | `5/5 errors`，status `502` | error id `5` 个 |
| stream idle | `.local-run/loadtest-reports/proxy-abnormal-clean-stream-idle-after-thinking-fix.json` | `3/3 errors`，HTTP stream `200` with error event | total p99 `2269ms`，error id `3` 个 |
| 先异常再恢复 | `.local-run/loadtest-reports/proxy-recovery-error-burst-40x20-after-thinking-fix.json` + `.local-run/loadtest-reports/proxy-recovery-normal-90x30-after-thinking-fix.json` | 异常阶段 `40/40 errors`，恢复后 `90/90 success` | 恢复阶段 TTFB p99 `3049ms` |
| client drop | `.local-run/loadtest-reports/proxy-client-drop-clean-40x20-after-thinking-fix.json` | fake 返回 `200`，客户端主动断开 | FD `28 -> 55 -> 27`，未观察到泄漏 |
| tool-use stream | `.local-run/loadtest-reports/proxy-tool-use-stream-10x5-after-thinking-fix.json` | `10/10 success` | TTFB p99 `402ms` |

旧异常报告中有几份被 RPM 运行态污染：

- `.local-run/loadtest-reports/proxy-abnormal-invalid-tool-after-thinking-fix.json`
- `.local-run/loadtest-reports/proxy-abnormal-rate-limit429-after-thinking-fix.json`
- `.local-run/loadtest-reports/proxy-abnormal-stream-idle-after-thinking-fix.json`
- `.local-run/loadtest-reports/proxy-client-drop-40x20-after-thinking-fix.json`

最终验收只使用 `clean-*` 报告。

### 调度与资源回归追加验证（2026-06-28）

本轮在隔离目录 `.local-run/full-real-9022-20260628` 追加验证最新调度修复，代理端口仍为 `19022`，fake upstream 仍为 `19080`，未触碰日常开发进程 `9022`。

修复点：本进程释放 Redis 并发 lease 后立即写入短 TTL 本地 tombstone，后台 Redis 状态同步会过滤这些刚释放的 lease，避免异步 Redis release 竞态把旧 lease 重新导入本地 in-flight。覆盖位置包括全量同步、按 id 同步和强制 apply。

关键报告：

| 场景 | 报告 | 结果 | 结论 |
| --- | --- | --- | --- |
| c16/r200 正常流式 | `.local-run/full-real-9022-20260628/25-after-local-tombstone-normal-c16-r200.json` | `200/200 success` | TTFB p95 `1457ms` |
| c32/r200 正常流式 | `.local-run/full-real-9022-20260628/25-after-local-tombstone-normal-c32-r200.json` | `200/200 success` | TTFB p95 `2872ms` |
| c64/r200 正常流式 | `.local-run/full-real-9022-20260628/25-after-local-tombstone-normal-c64-r200.json` | `200/200 success` | TTFB p95 `5773ms`；空闲后 FD/RSS 回落 |
| c128/r256，等待 3 秒 | `.local-run/full-real-9022-20260628/28-stress-clean-normal-c128-r256.json` | `248/256 success`，`8 x 429` | 429 来自 `credentialDispatchMaxWaitSecs=3` 的排队等待上限，不是泄漏 |
| c128/r256，等待 10 秒 | `.local-run/full-real-9022-20260628/30-stress-apiwait10-normal-c128-r256.json` | `256/256 success` | TTFB p95 `14239ms`，无排队超时 |
| 中途强杀代理 | `.local-run/full-real-9022-20260628/32-restart-kill-during-load-c64-r512.json` | 强杀期间大量 transport error | 符合测试预期，进程被杀后客户端连接失败 |
| 强杀后立即恢复 | `.local-run/full-real-9022-20260628/33-restart-immediate-recovery-c16-r64.json` | `64/64 success` | 重启后立即恢复正常调用 |
| 强杀后 c64 回归 | `.local-run/full-real-9022-20260628/34-restart-post-recovery-c64-r128.json` | `128/128 success` | 重启后高并发恢复正常 |
| 配置保存修复后 c16 | `.local-run/full-real-9022-20260628/35-cooldown-fix-normal-c16-r64.json` | `64/64 success` | 管理配置保存修复未影响业务接口 |
| 配置保存修复后 c64 | `.local-run/full-real-9022-20260628/36-cooldown-fix-normal-c64-r128.json` | `128/128 success` | TTFB p95 `5189ms`，FD 回到 `30` |

参数结论：

- `credentialDispatchMaxWaitSecs=3` 在 `dispatchGlobalMaxConcurrentRequests=64`、`credentialMaxConcurrentRequests=8`、16 个账号、c128 突发下会截断尾部排队请求，返回 429。
- 同配置下把 `credentialDispatchMaxWaitSecs` 调到 `10` 后，c128/r256 全部成功。代价是尾部 TTFB 升高，p95 约 `14s`；这是排队等待换成功率的结果。
- 大并发下不应盲目拉高全局并发；更稳的参数方向是根据上游真实吞吐设置账号并发、全局并发、队列长度和等待上限，让请求可排队但不过度堆积。

资源结论：

- tombstone 修复后，正常 c16/c32/c64 压测和异常矩阵后未观察到持续 FD 泄漏，空闲后 FD 回到 `30/31` 左右。
- 强杀代理后出现 2 条 `清理超时未释放的凭据并发占用 lease`，发生在 `credentialInFlightLeaseMaxSecs=20` 后，属于强杀遗留 lease 的预期自愈路径。
- Redis 热路径仍有少量 `75ms` 超时降级告警，当前隔离环境 Redis 在 `127.0.0.1:26379`，链路不是纯内存本地 Redis。降级没有导致并发槽污染或队列泄漏。
- usage writer 指标健康：`writerQueueAvailable=4096/4096`、`droppedPersistRecords=0`。少量 `PgSQL usage 批量写入耗时较长` 来自后台异步持久化，不阻塞下游接口。

### 运行配置保存兼容修复

问题：旧运行配置可能存在 `credentialMaxCooldownSecs` 小于某些错误类型基础冷却秒数的组合。运行时本来会用 `credentialMaxCooldownSecs` 作为上限截断冷却，但管理接口和 UI 之前错误地禁止这种配置，导致配置页“原样保存”失败。

修复：

- 后端只校验各冷却秒数必须大于 `0`，不再要求基础冷却小于最大冷却上限。
- UI 移除同样的前端拦截，并把“最大冷却时长”说明改成“连续出错时最多暂停多久”。
- 单测：`runtime_cooldown_validation_allows_base_values_above_max_cap`、`runtime_cooldown_validation_rejects_zero_values`。
- 隔离管理接口实测：`credentialMaxCooldownSecs=2`、`credentialStreamErrorCooldownSecs=5`、`credentialProtocolErrorCooldownSecs=10`、`credentialAuthErrorCooldownSecs=10` 可以通过 PUT 保存。

## usage 与 Claude Code CLI token 展示

当前 stream-json 结果中，最终 `result.usage` 和 `modelUsage` 均非 0。例：

- thinking：`output_tokens=9`
- Bash tool-use：`num_turns=2`，最终 usage 非 0
- MCP ping：`input_tokens=20`、`output_tokens=15`

需要注意：Claude Code CLI 的中间 `message_start` 和 `assistant` partial message 可能显示 `output_tokens=0`，最终 `message_delta.usage` 和 `result.usage` 才是完整值。之前看到长时间 `0 tokens`，如果发生在最终结果之前，可能是 CLI 聚合展示时机；如果最终 `result.usage` 仍为 0，才应继续查代理是否漏发 `message_delta.usage` 或 `usage` 字段。

## Claude Code CLI think 触发边界

验证时间：2026-06-28。

验证对象：

- 本地 Claude Code CLI：`2.1.156`。
- 本地代理：`http://127.0.0.1:9022/cc`。
- 只捕获请求体摘要，不保存完整 prompt、system prompt、工具 schema 或密钥。

请求体捕获结果：

| 场景 | 捕获目录 | CLI 实际模型 | thinking 字段 | output_config |
| --- | --- | --- | --- | --- |
| 普通 `--model sonnet` | `.local-run/claude-think-trigger-capture-20260628-204522` | `claude-sonnet-4-6` | `{type: adaptive}` | `{effort: high}` |
| 用户文本包含 `think` | 同上 | `claude-sonnet-4-6` | `{type: adaptive}` | `{effort: high}` |
| 用户文本包含 `think hard` | 同上 | `claude-sonnet-4-6` | `{type: adaptive}` | `{effort: high}` |
| 用户文本包含 `ultrathink` | 同上 | `claude-sonnet-4-6` | `{type: adaptive}` | `{effort: high}` |
| 显式 `--model sonnet-thinking` | 同上 | `sonnet-thinking` | `{type: adaptive}` | `{effort: high}` |
| 显式 `--model opus-thinking` | 同上 | `opus-thinking` | `{type: adaptive}` | `{effort: high}` |
| `--effort low` | `.local-run/claude-effort-capture-20260628-204907` | `claude-sonnet-4-6` | `{type: adaptive}` | `{effort: low}` |
| `--effort max` | 同上 | `claude-sonnet-4-6` | `{type: adaptive}` | `{effort: max}` |
| `--model sonnet-thinking --effort xhigh` | 同上 | `sonnet-thinking` | `{type: adaptive}` | `{effort: xhigh}` |

结论：

- Claude Code CLI 2.1.156 对普通 `sonnet` 请求也会发送 `thinking: {type: adaptive}`，默认 `output_config.effort=high`。
- 用户文本里的 `think`、`think hard`、`ultrathink` 不会让 CLI 改模型名，也不会自动调整 `output_config.effort`；这些词只是普通 prompt 内容。
- CLI 没有发现 `--no-think` 这类参数；显式“不思考”只能通过用户 prompt 表达，但不会改变请求体字段。
- 真正改变请求体 effort 的是 `--effort low|medium|high|xhigh|max`。
- 真正改变请求模型的，是 `--model sonnet-thinking`、`--model opus-thinking` 或其他显式 thinking 模型名。

当前代理实现边界：

- 普通 `adaptive`：只作为 Claude Code 兼容控制处理，不强制生成可见 `thinking_delta`，避免普通请求增加可见 thinking 块和额外输出 token。
- 显式 `*-thinking` 模型名：在 Kiro 上游没有原生 thinking 模型 ID 时，映射到基础模型，并加强上游提示，让输出中包含 `<thinking>...</thinking>`，再转换成 Anthropic SSE 的 `thinking_delta`。
- 显式 `thinking.type=enabled`：无论模型名是否带 `-thinking`，都按可见 thinking 处理。
- 显式 `thinking.type=disabled`：即使模型名带 `-thinking`，也不注入 thinking 控制，不输出 `thinking_delta`。

9022 真实调用验证：

| 场景 | 验证目录 | 结果 |
| --- | --- | --- |
| 普通 direct API `sonnet` | `.local-run/post-restart-think-boundary-20260628-204725` | 无 `thinking_delta` |
| direct API 文本 `Do not think` | 同上 | 无 `thinking_delta` |
| direct API `sonnet-thinking` | 同上 | 有 `thinking_delta` 和 `thinking_tokens` |
| direct API `sonnet + thinking.enabled` | 同上 | 有 `thinking_delta` 和 `thinking_tokens` |
| direct API `sonnet-thinking + thinking.disabled` | 同上 | 无 `thinking_delta` |
| direct API `sonnet + thinking.adaptive + effort=max` | `.local-run/post-restart-adaptive-boundary-20260628-205712` | 无 `thinking_delta` |
| direct API `sonnet-thinking + thinking.adaptive + effort=max` | 同上 | 有 `thinking_delta` 和 `thinking_tokens` |
| direct API `sonnet + thinking.enabled` | 同上 | 有 `thinking_delta` 和 `thinking_tokens` |
| direct API `sonnet-thinking + thinking.disabled` | 同上 | 无 `thinking_delta` |
| CLI 普通 `sonnet` | `.local-run/ccman-9022-real-20260628/think-boundary-20260628-205027` | 无 `thinking_delta` |
| CLI prompt 包含 `think` | 同上 | 无 `thinking_delta`，该轮模型选择了 Bash 工具但未输出可见 thinking |
| CLI prompt 包含 `ultrathink` | 同上 | 无 `thinking_delta` |
| CLI `--effort low` | 同上 | 无 `thinking_delta` |
| CLI `--effort max` | 同上 | 无 `thinking_delta` |
| CLI `--model sonnet-thinking` | 同上 | 有 `thinking_delta` 和 `thinking_tokens` |

兼容限制：

- 当前 Kiro 上游不接受 `claude-sonnet-4-6-thinking` 作为真实 `modelId`，会返回 `INVALID_MODEL_ID`。因此本系统不能把 `*-thinking` 原样交给 Kiro，而是使用基础模型加控制提示实现兼容。
- 由于这是通过 Kiro 文本输出中的 `<thinking>` 片段转换出来的 unsigned thinking，没有原生 Anthropic `signature_delta`。当前验证中 `signature_delta=false` 是预期结果。

## SSO external_idp 无 clientSecret 验证

验证时间：2026-06-28。

代码改造：

- `external_idp` 账号刷新路径不需要 `clientSecret`，只使用 `clientId + refreshToken + tokenEndpoint`。
- 导入字段支持 `access_token/accessToken`、`expires_at/expiresAt/expired`、`issuerUrl/issuer_url`、`scopes/scope`。
- 当 `external_idp` 缺少 `tokenEndpoint` 时，系统会尝试从 Microsoft issuer URL 或 access token JWT `iss` 推导；如果只有 Microsoft MSAL refresh token 形态，则回退到 `https://login.microsoftonline.com/common/oauth2/v2.0/token`。
- 缺少或无效 `clientSecret` 不再把 `external_idp` 误判为 `idc`。
- `invalid_grant` / `refreshToken 已失效` 在 Admin 新增和刷新中归类为 `400 invalid_request`，不再误报 `500 internal_error`。

本地真实文件：

- `target/sso/external-idp-refreshed-rust-20260627-145624.json`
- 文件字段：`authMethod=external_idp`，有 `accessToken`、`refreshToken`、`clientId`、`tokenEndpoint`、`issuerUrl`、`scopes`，无 `clientSecret`。
- 文件内 access token `exp=2026-06-27T08:25:11Z`，验证时已过期。

9022 验证结果：

- POST `/api/admin/credentials` 导入该文件返回 `400 invalid_request`。
- Microsoft OAuth 返回 `AADSTS700082`：refresh token 发行时间 `2026-06-27T06:56:45Z`，因 12 小时未活动失效。
- 导入失败后没有新增同 `profileArn` 的账号。
- 负向回归：构造 `external_idp` 且不带 `clientSecret/tokenEndpoint` 的请求，返回 `400 invalid_request` 且错误包含 `invalid_grant`，不再出现 `clientSecret` 或 `tokenEndpoint` 校验错误。
- 重启 9022 后 Claude Code CLI 烟测通过：`.local-run/ccman-9022-real-20260628/05-post-sso-change-smoke.stream.jsonl`，返回 `post sso change ok`，最终 usage 非 0。

结论：当前项目已经支持无 `clientSecret` 的 `external_idp` SSO 账号格式；本次指定文件不能导入使用的原因是 Microsoft 判定 refresh token 已过期，不是本系统要求 `clientSecret`。

## 当前仍未覆盖的真实能力

以下能力本轮没有用真实 Kiro 上游验证：

- 真实模型智商效果。
- 图片识别。
- 大文档、小文档理解质量。
- 真实长会话上下文质量。
- 真实上游账号在长时间高并发下的限流恢复曲线。
- Claude Code CLI 交互式 TUI 中 agent 面板的主观流畅度。

这些能力需要真实账号和真实上游调用。为了不影响真实账号状态，本轮只验证了协议兼容性、流式输出、错误处理、调度压力、资源释放和 CLI 客户端行为。

## 残余风险

- 普通突发并发 300x80 的 TTFB p99 到 `8217ms`，主要来自隔离测试账号/队列压力和 fake upstream 延迟叠加；真实参数调优时应优先控制单账号并发、队列等待和 RPM，而不是盲目拉高全局并发。
- Redis 状态同步仍可能出现少量 100ms+ 慢日志，但当前观察不会污染并发槽/队列热路径。
- PgSQL usage 批量写入仍可能偶发较慢，但处于异步写入路径，不阻塞下游 stream。
- cachePoint 是默认关闭的试验能力；只有显式开启后才会改变上游 body。

## 9022 真实 Claude Code CLI 回归补测

验证时间：2026-06-28 17:00-17:31（Asia/Shanghai）。

验证对象：

- 本地服务：`127.0.0.1:9022`。
- 服务进程：`target/release/kiro-rs -c config.json --credentials credentials.json`。
- Claude Code CLI：`2.1.156`。
- CLI 代理方式：单次命令环境变量 `ANTHROPIC_BASE_URL=http://127.0.0.1:9022/cc` 和运行态请求 key；没有改 `ccman` 持久配置。
- 管理 key：`admin123`。

### 基础 CLI 调用

最小 `claude --bare -p --model sonnet --output-format stream-json --include-partial-messages` 通过 9022 成功：

- exit code：`0`。
- wall time：`4952ms`。
- first stdout：`324ms`。
- first visible text：`4876ms`。
- stream-json 事件：`system`、`stream_event`、`assistant`、`result`。
- `result.usage` 非 0，包含 `input_tokens`、`output_tokens`、`cache_creation_input_tokens`、`cache_read_input_tokens`、`service_tier`、`speed`、`modelUsage` 相关字段。
- 响应携带 request id。

### thinking 触发

`real_request` 模式：

- `--effort high` + prompt 包含 `ultrathink`：调用成功，但没有 `thinking` content block。
- `--effort low` 普通 prompt：调用成功，没有 `thinking` content block。

`always` 模式：

- 临时将 runtime `thinkingTriggerMode` 从 `real_request` 改为 `always`。
- 真实 CLI 调用返回 `thinking` content block 和 `thinking_delta`。
- `thinking_delta` 数量：`21`。
- thinking 文本长度：约 `438` 字符。
- `result.usage` 非 0。
- 测试结束后 runtime 已恢复为 `real_request`。

结论：当前 9022 运行态下，`real_request` 不会把 CLI 默认 adaptive thinking 强制变成可见思考；`always` 可以真实触发可见 thinking 输出。

### 工具调用

Claude Code CLI `Bash` 工具调用通过：

- prompt 要求执行 `printf kiro-tool-ok`。
- stream-json 中出现 `tool_use`、`tool_result`、第二轮 `assistant`。
- 工具名：`Bash`。
- `num_turns` 对应两轮请求，最终 `resultSubtype=success`。
- `result.usage` 非 0。

### MCP 工具调用

使用项目现有 `.local-run/cc-real-tests/mcp-ping-server.js` 和 `.local-run/cc-real-tests/mcp-config.json` 复测 9022：

- MCP `ping` 工具：成功返回 `mcp-pong-kiro-local`，随后模型返回 `current mcp ok`。
- MCP `fail` 工具：工具返回错误 `mcp-fail-kiro-local`，CLI 将错误作为 `tool_result` 回传，随后模型返回 `current mcp fail ok`。
- 两个场景 exit code 均为 `0`。
- 两个场景最终 `result.usage` 均非 0。
- debug log 记录 MCP server connected、tool dispatch start/end、tool completed 或 failed。

说明：手写的 Content-Length framing 临时 MCP server 未能连接成功；改用 Claude Code 当前兼容的换行 JSON-RPC 测试 server 后通过。因此前者不是代理问题。

### agent / Task 调用

使用 `--agents` 定义临时 `quick` agent：

- 主会话调用 agent。
- agent 返回 `SUBAGENT-OK`。
- 主会话返回 `AGENT-FINAL-OK`。
- stream-json 出现 `Task` / `agent` 相关 tool use 和 tool result。
- `result.usage` 非 0。

### 多轮会话

使用临时 HOME 和固定 `--session-id`：

- turn 1：让模型记住 `kiwi-river-714`，返回 `TURN1-OK`。
- turn 2：`--resume` 同一 session，模型正确返回 `TURN2-kiwi-river-714`。
- 两轮都通过 9022，usage 非 0。

### 文件和图片读取

使用临时目录和 `Read` 工具：

- 小文档：读取 marker `SMALL-DOC-742` 成功。
- 大文档：约 1800 行，读取并定位 `LARGE-DOC-918273` 成功；CLI 因文件截断自动继续读取，符合预期。
- PNG 图片：32x32 左红右蓝，模型通过 `Read` 识别并返回 `LEFT-RED-RIGHT-BLUE`。
- 三个场景均通过 9022，usage 非 0。

### 错误路径

通过 9022 验证：

- 未授权 `/cc/v1/messages`：`401 authentication_error`，request id 存在。
- 错误 key：`401 authentication_error`，request id 存在。
- malformed JSON：`400 invalid_request_error`，request id 存在。
- 未配置 `/dfcache/not-configured`：`404 not_found_error`，request id 存在。
- 非法模型：`400 invalid_request_error`，对外错误为统一英文文案，并带 `error ID`。
- 抽样错误响应没有出现 `credential`、`pool`、`fallback`、`external pool`、`凭据`、`外部池` 等内部概念。

### 本地热路径压力

不打真实上游，混合请求 `1120` 条，concurrency `120`，连续两轮：

| 轮次 | 状态 | wall | RPS | p50 | p95 | p99 | request id | 内部词泄漏 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| round 1 | 800x200, 120x400, 120x401, 80x404 | `300ms` | `3733/s` | `25.77ms` | `64.47ms` | `74.76ms` | 0 缺失 | 0 |
| round 2 | 同上 | `200ms` | `5600/s` | `21.42ms` | `27.97ms` | `29.41ms` | 0 缺失 | 0 |

RSS 观察：

- round 1 前：约 `20MB`。
- round 1 后：约 `68MB`。
- round 2 后：约 `68.8MB`。

结论：第一轮有明显预热和分配，第二轮基本稳定，没有观察到持续增长。

### 真实上游小并发

真实 `/cc/v1/messages` 流式请求 4 条，concurrency `2`：

- 全部 `200`。
- 全部返回预期文本。
- 全部携带 request id。
- 全部返回 usage。
- first text：约 `1.86s` 到 `4.58s`。
- RSS 增量：约 `368KB`。

### usage 和日志

Admin usage 验证：

- `/api/admin/usage-records-paged` 有最近真实调用记录。
- `/api/admin/usage-summary` 可聚合。
- `/api/admin/usage-writer-stats`：`writerQueueAvailable=4096`，`droppedPersistRecords=0`。
- 最近记录包含唯一 `id`、endpoint、model/upstreamModel、status、usage source、latency trace、raw usage、reported usage。
- 抽样未发现错误信息重复堆叠。

服务健康：

- `/healthz`：`200`，约 `1ms`。
- `/readyz`：`200`，Postgres、Redis、Redis runtime events 均 ready。
- runtime 测试后保持 `thinkingTriggerMode=real_request`，`definedCacheRoutes=0`。

日志：

- 未发现 panic / ERROR。
- 有一条 Redis 调度热路径 `75ms` 超时后降级本地调度的 WARN，发生在本地压力测试期间；后续未重复出现。

### 本轮未做

- 没有用真实上游做大规模长时间压测，避免打爆真实账号或影响账号状态。
- 没有用交互式 TUI 截图验证 agent 面板视觉显示；本轮验证的是 `stream-json` 协议和真实工具/agent/MCP 行为。
