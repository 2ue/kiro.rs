# WebSearch 工具错误稳定性修复与验证

更新时间：2026-09-27 16:32 +0800

## 结论摘要

这次本地修复的目标不是删除 `user query` 预检，也不是把所有 WebSearch 错误伪装成成功；目标是把已经进入 MCP/WebSearch 执行阶段的可恢复失败，按 Anthropic 工具协议返回为 `web_search_tool_result_error`，让 Claude Code CLI 收到一个完整的 `HTTP 200` assistant message，而不是把整个请求变成 `502/429/503` API 错误、进入 CLI 重试或中断路径。

最终策略：

|场景|当前处理|原因|
|---|---|---|
|正常非空 WebSearch query|保持成功路径，返回 `server_tool_use` + `web_search_tool_result` + 摘要文本|真实账号 direct native 已验证会返回有效搜索结果|
|空 query / 当前 user turn 缺失可搜索文本|保持 MCP 前 `400 invalid_request_error`，`mcpAttempts=0`|反事实证明绕过后真实 MCP 会返回 JSON-RPC `-32602`，没有可用 WebSearch 内容|
|MCP 执行层可恢复失败：HTTP 400/429/5xx、header/body timeout、disconnect、JSON-RPC error、`isError=true`|改为 `HTTP 200`，内容中包含 `web_search_tool_result_error`，并结束本轮 assistant message|这是工具执行失败，不应默认让 Claude Code CLI 的 API 请求失败|
|MCP 协议/边界坏包：malformed JSON、id/jsonrpc 不匹配、missing result、non-UTF8、响应过大、结果结构非法、通用 protocol error|保持硬失败，目前为 `502 api_error`|这类不是“搜索服务没搜到/临时不可用”，而是 kiro.rs 与上游协议状态不可信|
|外部 fallback 可用|仍优先走既有 external fallback，然后才合成工具错误|不改变已有回退优先级|

这次修改只发生在本地工作区。远端生产实例没有被修改、重启或写入。

## 执行计划与进度

本轮沿用前置生产审计的计划文档：`tmp/prod-evidence/20260927-083000-<redacted-host>/PLAN.md`。该计划已经覆盖远端最近 8 小时 usage 错误逐条导出、C003 真实账号 WebSearch 验证、反事实空 query 探针、脱敏归档和“仅分析时不改代码”的边界。真实本机路径保留在未跟踪的 `tmp/` 证据目录中，不写入可跟踪文档。

在用户进一步要求“设计合理返回、尽可能在合理语境下保证请求不中断，并充分测试”后，本地进入修复阶段。新增执行状态如下：

|阶段|状态|说明|
|---|---|---|
|确认问题边界|完成|区分空 query 预检、MCP 执行失败、协议坏包、Claude CLI 中断行为|
|代码修复|完成|只改 WebSearch 错误返回、usage 记录、测试矩阵|
|本地单元/handler 矩阵|完成|覆盖 stream/non-stream、可恢复/硬失败、隐私泄漏、恢复后正常请求|
|冻结候选二进制 C0|完成|全量 Rust test、loadtest、release build、fmt/diff 均通过|
|真实账号 direct native WebSearch|完成|正常/流式/空 query/旧 query 不复用/当前 query 成功均验证|
|真实 Claude Code CLI|部分覆盖|CLI 2.1.280 正常退出，但没有向 kiro.rs 发 native WebSearch；这是能力协商限制|
|Claude CLI 对协议形状的反应|完成|用 fake Anthropic server 验证 `HTTP 200 + tool_error` 完成，`HTTP 502` 进入重复 retry/失败路径|
|生产发布|未做|本轮没有发版、没有推送、没有改远端|

## 问题边界

### 1. 空 `user query` 是真实无效输入，不是误判

前置 C003 先证明了现有程序会在 MCP 前拒绝空 query，但这只能证明当前实现。后来补做了反事实探针，绕过 `extract_search_query`，直接向真实 MCP 发送 `tools/call web_search` 且 `arguments.query=""`。

结果是：

- MCP 确实被调用 1 次；
- MCP HTTP status 是 200；
- JSON-RPC 返回 `error.code=-32602`，`message=Invalid tool parameters provided`；
- `result=null`；
- 没有可解析的 `WebSearchResults`；
- 没有正常搜索结果的 `title/url` 内容。

所以空 query 的问题不是“本地预检太保守”。如果放掉这个边界，它也不会变成正常搜索，只会多消耗一次 MCP attempt、账号租约、网络往返和错误归因空间。

因此本轮不删除轻量 query 校验，也不扩展成语义审查。它只应该确认：最后一个当前 user turn 存在非空文本；去掉已知搜索前缀后仍非空；不复用历史旧 query。

### 2. MCP 执行失败不等于整个 Claude API 请求必须失败

另一类问题是：正常 query 已经进入 MCP，但上游/MCP 因限流、临时不可用、JSON-RPC tool error、超时或断连失败。这些失败本质上是“server tool 调用失败”，不是请求体结构错误，也不是 kiro.rs 与 Claude Code 的 Anthropic 协议必然坏掉。

如果这类失败直接返回 `502/429/503`，真实 Claude Code CLI 会把它当 API 层失败处理。fake 协议验证中，`HTTP 502` case 已观察到 CLI 反复发起 `api_retry`，并进入 `error_during_execution / aborted_streaming` 路径。相反，`HTTP 200` 且 body/SSE 内含合法 `server_tool_use` 和 `web_search_tool_result_error` 时，CLI 正常完成，`resultIsError=false`，`terminalReason=completed`。

因此合理的协议形状是：对可恢复的 WebSearch 工具执行失败，返回 `HTTP 200` assistant message，显式告诉客户端这个工具调用失败，但本轮消息完整结束。

### 3. 这不等同于本地 `Read` 工具失败后的自动换方案

Claude Code 中本地 `Read` 工具失败时，模型通常会收到一个工具失败结果，然后在后续推理轮里选择其它方式。当前 kiro.rs native WebSearch 路径不是完整 agent loop；它是服务端拦截 WebSearch、调用 MCP、再合成 assistant response。

本轮修复能保证“当前 HTTP/SSE 请求不因为可恢复搜索失败而 API 中断”，但不会自动触发第二轮模型推理去换工具或重新规划。若未来要做到和本地工具失败完全一样，需要新的架构：把 `web_search_tool_result_error` 注入上下文后，再发起一次受预算和 deadline 约束的模型 inference loop。这个不在本次修复范围内。

## 代码修改

### `src/anthropic/websearch.rs`

新增工具级错误结果：

- `WebSearchOutcome::Failure` 增加 `tool_error: Option<WebSearchToolErrorOutcome>`；
- 新增 `WebSearchToolErrorOutcome`，保存已生成的 response、估算 output tokens、公开 `error_code` 和内部 reason；
- 新增 `WebSearchToolErrorCode`，目前映射为：
  - `InvalidRequest` -> `invalid_tool_input`
  - `RateLimit` -> `too_many_requests`
  - scheduler/timeout/attempt/upstream/部分 protocol-ish recoverable -> `unavailable`

新增可恢复/硬失败分界：

- 可恢复：`websearch_mcp_invalid_request`、`websearch_mcp_invalid_tool_input`、`websearch_mcp_rate_limit`、`websearch_mcp_timeout`、`websearch_mcp_body_timeout`、`websearch_mcp_body_read`、`websearch_mcp_upstream_error`、`websearch_mcp_rpc_error`、`websearch_mcp_tool_error`、`websearch_mcp_scheduler_unavailable`、`websearch_mcp_attempt_limit`；
- 硬失败：`websearch_mcp_malformed_json`、`websearch_mcp_invalid_envelope`、`websearch_mcp_missing_result`、`websearch_mcp_non_utf8_response`、`websearch_mcp_response_too_large`、`websearch_mcp_invalid_search_result`、`websearch_mcp_protocol_error`。

新增协议形状：

- 非流式：`HTTP 200` JSON message，包含 `server_tool_use`、配对的 `web_search_tool_result`、`content.type=web_search_tool_result_error`、最终说明文本、`stop_reason=end_turn`；
- 流式：`message_start` -> text block -> `server_tool_use` -> `web_search_tool_result` -> text block -> `message_delta(stop_reason=end_turn)` -> `message_stop`；
- `web_search_tool_result` 现在带 `tool_use_id`，与 `server_tool_use.id` 配对，测试中已覆盖；
- JSON-RPC `-32602` 现在归为 `invalid_tool_input`，不再和通用 `websearch_mcp_rpc_error` 混在一起。

保留不变：

- 空 query / 当前 user turn 不可搜索仍在 MCP 前 `400`；
- MCP 响应坏包、id 不匹配、结果结构非法、超限响应仍硬失败；
- 正常成功 WebSearch 的结果摘要路径不变。

### `src/anthropic/handlers.rs`

新增 usage 记录语义：

- 可恢复 WebSearch tool error 从客户端视角记录为 `UsageRecordStatus::Success`；
- 同时保留内部诊断：
  - `error_type = "websearch_tool_error"`
  - `error_message = internal_reason`
  - `error_detail = "websearch_tool_error:<code>:<reason>"`
  - `errorMetadata.websearchToolError = { errorCode, internalReason, mcpAttempts }`
- stream 与 non-stream 都会在完成时记录 `downstreamStopReason=end_turn` 和 downstream committed；
- 外部 fallback 仍在 MCP failure 后优先尝试，只有 fallback 不生效才返回合成 tool error；
- 硬失败仍按原路径记录 `UsageRecordStatus::Error`、公共错误状态和 error id。

这个记录方式的含义是：HTTP 客户端请求完成了，但工具执行失败。后续 dashboard 如果默认认为 `status=success` 就不应出现 `error_type`，需要把 `websearch_tool_error` 当作“成功请求中的工具诊断”展示，而不是普通 API failure。

### `src/anthropic/handlers/tests.rs`

WebSearch MCP 错误矩阵改为覆盖两大类：

|模拟场景|期望 HTTP|期望工具错误|
|---|---:|---|
|`http-400`|200|`invalid_tool_input`|
|`http-429`|200|`too_many_requests`|
|`http-500`|200|`unavailable`|
|`header-timeout`|200|`unavailable`|
|`body-timeout`|200|`unavailable`|
|`disconnect`|200|`unavailable`|
|`jsonrpc-invalid-params`|200|`invalid_tool_input`|
|`jsonrpc-error`|200|`unavailable`|
|`is-error`|200|`unavailable`|
|`malformed`|502|无，硬失败|
|`non-text-content`|502|无，硬失败|
|`mismatched-id`|502|无，硬失败|
|`content-length-over-limit`|502|无，硬失败|
|`chunked-over-limit`|502|无，硬失败|

每个场景跑 5 轮，并交错 stream/non-stream。测试额外断言：

- response body 不泄漏私有 upstream marker；
- usage/logs 不保存 raw query；
- `server_tool_use.id` 与 `web_search_tool_result.tool_use_id` 匹配；
- recoverable tool error 的 usage 是 success，但带 `websearch_tool_error` 诊断；
- hard failure 的 usage 是 error；
- 错误后正常 WebSearch 请求仍能恢复成功。

## 验证结果

### 本地 Rust / 静态验证

|命令|结果|
|---|---|
|`feature/tests/run-cargo-scoped.sh websearch-tool-error-unit-2 -- cargo test anthropic::websearch::tests:: -- --nocapture`|通过，24 tests|
|`feature/tests/run-cargo-scoped.sh websearch-tool-error-handler-2 -- cargo test anthropic::handlers::tests::websearch_mcp_error_resource_and_recovery_matrix_returns_tool_errors_for_five_rounds -- --exact --nocapture`|通过，1 test|
|`feature/tests/run-cargo-scoped.sh websearch-tool-error-regression-2 -- cargo test websearch_ -- --nocapture`|通过，19 tests|
|`git diff --check`|通过|
|`feature/tests/run-cargo-scoped.sh websearch-fmt-check-final -- cargo fmt --check`|通过|
|`feature/tests/run-cargo-scoped.sh websearch-fmt-check-confirm -- cargo fmt --check`|通过|

### 冻结候选 C0

候选二进制：

- 路径：`/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T//kiro-cli-candidate.websearch.ePZ2E7/kiro-rs`
- SHA-256：`84f1acae7ffc8c6f9d630ff2d681d443d160857c96a097df3e05348b7e673bb7`

命令批次：

```bash
KIRO_FROZEN_BINARY="$candidate_root/kiro-rs" \
  feature/tests/run-cargo-scoped.sh claude-cli-c0-websearch-2 -- \
  bash -lc 'cargo fmt --check && cargo test && cargo build --release && install -m 755 "$CARGO_TARGET_DIR/release/kiro-rs" "$KIRO_FROZEN_BINARY"'
```

结果：

- main tests：`2183 passed; 0 failed; 6 ignored`
- `kiro_loadtest`：`31 passed`
- release build：通过

### 指定本地实例

按 Claude CLI validation 约束，使用唯一项目测试实例：

- 地址：`127.0.0.1:19023`
- 配置：`tmp/thinking-budget-local/config.json`
- 当前进程：`kiro-rs` PID `77344`
- 启动方式：tmux session `kiro_cli_websearch_tool_error_20260927_161417`
- 冻结 binary：上面的 SHA-256 候选
- `/healthz`：通过
- `/readyz`：此前通过

### 真实账号 direct native WebSearch

证据目录：

- `tmp/websearch-tool-error-validation-20260927-161605/direct-runtime-key/summary.json`
- `tmp/websearch-tool-error-validation-20260927-161605/direct-runtime-key/usage-summary.compact.json`

结果摘要：

|用例|HTTP|结果|
|---|---:|---|
|`normal-nonstream`|200|JSON message，`server_tool_use=1`，`web_search_tool_result=1`，`web_search_result=1`，`stop_reason=end_turn`，usage 非零，`mcpAttempts=1`|
|`normal-stream`|200|SSE，`message_stop=1`，`server_tool_use=1`，`web_search_tool_result=1`，`web_search_result=1`，usage 非零，`mcpAttempts=1`|
|`empty-query-prefix-only`|400|`invalid_request_error`，无 tool blocks，usage 为 `websearch_missing_user_query`，`mcpAttempts=0`，`inferenceConsumed=0`|
|`stale-query-not-reused`|400|不复用历史旧 query，`websearch_missing_user_query`，`mcpAttempts=0`|
|`current-query-after-stale`|200|只使用当前 query，真实 WebSearch 成功，`mcpAttempts=1`|

这组验证说明：正常 query 可以成功返回预期搜索内容；空 query/旧 query 不复用不是 mock 结论，而是在真实账号 direct native 路径下成立。

### 真实 Claude Code CLI

证据文件：

- `tmp/websearch-tool-error-validation-20260927-161605/claude-cli-real/websearch.summary.json`

环境：

- Claude Code CLI：`2.1.280`
- base URL：`http://127.0.0.1:19023/cc`
- 命令使用隔离 `HOME` / `CLAUDE_CONFIG_DIR`
- 参数包含 `--tools=WebSearch --allowedTools=WebSearch`

结果：

- CLI exit code：0；
- kiro request id：`req_01L86EBxPBZ8iZiiNQuPcNjN`；
- 最终 usage 中 `server_tool_use.web_search_requests=0`；
- 工具名只看到 `telemetry`；
- CLI 没有通过 kiro.rs 发起 native WebSearch，而是输出了类似计划/文本。

因此这次真实 CLI 运行不能证明“真实 CLI 已经走过 native WebSearch tool error”。它只能证明候选服务没有破坏该 CLI 正常请求。native WebSearch 没触发是 CLI tool negotiation/model profile 兼容问题，需要单独处理。

### Claude CLI 协议反应验证

为了回答“如果 WebSearch 上游失败，Claude Code CLI 是中断还是继续”这个问题，补做了 fake Anthropic server 协议验收。它不替代 kiro.rs 逻辑测试，只验证 CLI 对响应形状的反应。

证据文件：

- `tmp/websearch-tool-error-validation-20260927-161605/claude-cli-fake-protocol/summary.rerun2.partial.json`

结果：

|case|CLI 结果|
|---|---|
|`tool-error-200`|CLI 完成；`apiRetryCount=0`；收到 `server_tool_use=1`、`web_search_tool_result=1`、`web_search_tool_result_error`；`message_stop=1`；`terminalReason=completed`；`resultIsError=false`|
|`http-502`|观察到重复 `api_retry`；中途主动中止以避免等待完整 retry ladder；partial summary 中 `apiRetryCount=8`，`resultSubtype=error_during_execution`，`terminalReason=aborted_streaming`|

结论：对可恢复 WebSearch 工具执行失败，`HTTP 200 + tool_result_error` 更符合“让请求不中断”的目标；直接 `502` 会进入 CLI 的 API retry/失败路径。

## 与生产审计的关系

前置生产审计仍然是最近 8 小时错误分析的主证据：

- 主报告：`tmp/prod-evidence/20260927-083000-<redacted-host>/summary/production-error-audit-20260927.md`
- 逐条错误台账：`tmp/prod-evidence/20260927-083000-<redacted-host>/summary/error-ledger.md`
- C003 direct native 与反事实：`tmp/prod-evidence/20260927-083000-<redacted-host>/summary/c003-websearch.md`、`tmp/prod-evidence/20260927-083000-<redacted-host>/summary/c003-websearch-counterfactual.md`

该审计已经覆盖 `178/178` 条非成功 usage 记录，每条都有 request id、时间、范围、账号/池、端点、模型映射、状态/HTTP、错误类型、耗时、attempts、问题编号和 raw 行号。

本次本地代码修复主要解决 P010 延伸出来的 WebSearch tool-error 返回策略；它不声称修复 P001-P009/P011 的外部池、payload、client drop、deadline 或长流解码问题。

## 已改与未改

### 已改

- 可恢复 MCP/WebSearch 执行失败返回合法 Anthropic tool result error；
- stream/non-stream 都生成配对的 `server_tool_use` 与 `web_search_tool_result`；
- JSON-RPC `-32602` 映射为 `invalid_tool_input`；
- usage 对 tool error 记录为“客户端请求成功 + 工具失败诊断”；
- handler 测试覆盖 recoverable vs hard failure、隐私、usage、恢复后成功；
- 本地候选服务已用冻结 binary 在 `19023` 跑过真实账号 direct native WebSearch。

### 没改

- 没改远端生产服务；
- 没删除空 query / 当前 user query 轻量校验；
- 没做 query 语义审查；
- 没对正常 query 增加 MCP 试探预检；
- 没把所有协议错误都吞成成功；
- 没实现第二轮模型 inference / 自动换工具；
- 没改 external pool retry、429 cooldown、deadline、client drop、payload guard；
- 没改 Claude CLI tool negotiation/model profile。

## 风险与后续建议

1. **Dashboard 语义风险。** usage 现在可能出现 `status=success` 且 `error_type=websearch_tool_error`。这是有意设计：客户端请求完成，工具失败。需要确认 UI/报表不会把它误算成普通 API 错误，也不会完全隐藏。

2. **协议兼容风险。** fake CLI 证明 Claude Code 2.1.280 接受该响应形状；真实 CLI 因没有发 native WebSearch，尚未完整覆盖“kiro 真实 MCP 失败 -> Claude CLI native WebSearch tool error”链路。后续应在能稳定触发 native WebSearch 的 CLI/profile 上补 C2/C3。

3. **语义能力限制。** 本次只保证不中断；不会让模型自动重新搜索、改用其它来源或继续推理。要做到本地工具失败那种“模型换方案”，需要实现二次 inference loop，并给出预算、deadline、usage、stream event 顺序和失败上限。

4. **硬失败边界要保持收紧。** malformed JSON、id mismatch、响应过大、结果结构非法等仍应硬失败。把坏协议也转成 `HTTP 200` 会掩盖上游/中间层损坏，并可能让客户端消费不可信内容。

5. **空 query 预检不应扩大化。** 保留非空结构校验即可。不要把这次结论扩展为“所有 query 都要预检”“为了省积分做语义校验”。

## 推荐落地顺序

1. 先审阅本地 diff 和本文档，确认 `status=success + websearch_tool_error` 的 usage 口径符合运营/报表预期。
2. 用现有 `19023` 候选继续做一个能够真实触发 native WebSearch 的 Claude CLI/profile 验证；若 CLI 仍不触发 native tool，先修能力协商。
3. 灰度部署到非生产或低流量环境，重点观察：
   - `websearch_tool_error` 数量；
   - `HTTP 5xx` WebSearch 错误是否下降；
   - Claude CLI 是否不再进入 API retry；
   - 正常 WebSearch 成功率是否不变；
   - hard failure 是否仍按 502/400 出现。
4. 再考虑做完整二次 inference loop。这个属于新架构工作，不应混入本次稳定性补丁。
