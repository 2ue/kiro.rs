# 企业/API-key 凭据 200 EventStream EOF 被误判 api_error

Status: `fixed-locally / focused-tests-passed / production-rollout-pending`

Severity: P0/P1

## 影响

- 影响对象：企业/API-key 类 Kiro 凭据（生产 usage 中以 `ksk_...` 形态脱敏出现）。
- 影响路径：`/cc/v1/messages`、`/ha/v1/messages`，以及共享的 Kiro EventStream → Anthropic stream/non-stream 转换路径。
- 用户可见表现：
  - 请求上游 HTTP 状态是 `200 OK`；
  - usage 记录仍显示失败，route 多为 `local_credential | local_error_no_fallback`；
  - 错误形态包括：
    - `upstream eventstream ended without a meaningful assistant, reasoning, or tool event`
    - `upstream eventstream ended with 1 incomplete tool input buffer(s)`
  - 业务侧看到类似 `api_error` / `api_protocol_error`，但上游积分/计量可能已经扣除。

## 与 personal/Gmail 问题的区别

这不是同一个指纹。

- personal/Gmail 问题主要是 `HTTP 200 + content-type: application/json + reason=api_protocol_error`，关键不确定点是 body 到底是真 JSON 错误 envelope，还是 JSON header 标错的 binary EventStream。
- 本问题是企业/API-key 类凭据已经进入 EventStream 解析路径，上游 HTTP 200，EventStream frames 被读到 EOF，但本项目自己的终端状态机过严或 tool EOF 容错不足，导致把“官方客户端可接受的响应”判为失败。

## 生产证据

本轮只读生产复核脱敏结论：

- `152.53.194.142`
  - 运行版本：`0.0.116`，revision `b37941bb5edaff668685bb9ed1a9509407f42518`。
  - 最近 8 小时仍有 `upstream_failure class=protocol_error upstream_status=200 content_type=json reason=api_protocol_error`。
  - 样本 credential 是 gmail/social personal 账号；这台机器主要印证 personal/Gmail 指纹仍存在，不是本企业号问题的主证据。

- `152.53.243.159`
  - 运行版本：`0.0.116`，revision `b37941bb5edaff668685bb9ed1a9509407f42518`。
  - 近 8 小时存在企业/API-key 类指纹：
    - `upstream eventstream ended without a meaningful assistant, reasoning, or tool event`
    - `upstream eventstream ended with 1 incomplete tool input buffer(s)`
  - 代表样本：
    - `/cc/v1/messages`
    - request model `claude-opus-4-8`
    - upstream model `claude-opus-4.8`
    - attempt stage `response_headers_received`
    - upstream status `200 OK`
    - raw usage 可见 input token 信号，output tokens 为 `0`
  - 另一类 `/ha/v1/messages` 样本出现第一 attempt `500`、第二 attempt `200` 后 EOF/empty-eventstream failure。

- `152.53.194.170`
  - 运行版本：`0.0.116`，revision `b37941bb5edaff668685bb9ed1a9509407f42518`。
  - 同样存在：
    - `upstream eventstream ended without a meaningful assistant, reasoning, or tool event`
    - `upstream eventstream ended with 1 incomplete tool input buffer(s)`
  - 说明该问题不是单机环境孤例。

注意：本文件不保存生产邮箱、API key、数据库连接串、原始 credential label 或任何 secret。

## 根因

根因是本项目近期为防止 silent truncation / fake success 加强 EventStream EOF 校验时，和官方 Kiro 客户端的兼容边界不一致。

本项目旧逻辑：

1. 若没有 `messageStatus=COMPLETED`；
2. 且没有 assistant 文本、reasoning、已完成 tool use；
3. 就把 EOF 判成 protocol failure。

这会误伤两类官方行为：

1. usage-only / metadata-only / context-only 终端：
   - 上游可能只返回 `contextUsageEvent`、`meteringEvent`、`metadataEvent` 或 `messageMetadataEvent`；
   - 官方客户端把这些作为正常 chunk/计量事件处理，不会因为没有 assistant 文本就抛错；
   - 本项目此前会返回 `upstream eventstream ended without a meaningful assistant, reasoning, or tool event`。

2. tool input 已完整但 EOF 缺少显式 `stop:true`：
   - 上游可能已经发送完整 JSON tool input，但最后一个 stop frame 缺失；
   - 参考实现会在 EOF 时 flush 当前 tool input；
   - 本项目此前 non-stream 直接 502，stream 在生成 final events 前已经被终端协议检查拦截，无法执行 flush。

同时还有一个计量问题：

- `meteringEvent` 实际可能携带 `inputTokens/outputTokens`；
- 本项目原 `MeteringEvent` 只解析 `usage`，没有解析 token 字段；
- 因此 usage 只能走 context/local estimate，导致“上游扣费/计量存在，但本地输出 tokens 或 Kiro metering 不完整”的诊断盲区。

## 官方/参考实现对照

本地复核了官方 Kiro 扩展和参考 Go 实现：

- 官方 Kiro extension：
  - `metadataEvent`、`contextUsageEvent`、`meteringEvent` 都作为正常流事件处理；
  - streaming 路径可以 yield 空 content usage chunk；
  - non-stream 路径聚合 assistant content，若没有 assistant content 也不会仅因此抛出 protocol error。

- 参考 Go 实现：
  - EOF 时对缺少 stop 的 tool input 做 flush；
  - 解析 `meteringEvent` 中的 `inputTokens/outputTokens`。

由此判断：生产企业号样本中的 `HTTP 200 EventStream EOF` 不是单纯上游失败，更符合本项目 adapter 状态机过严导致的误判。

## 修复方案

### 1. 保留 JSON body fail-closed，不回退 personal/Gmail 修复

保留现有方向：

- `2xx + application/json` 不在 provider header 阶段直接判失败；
- 交给 handler body sniff：
  - 真 JSON error envelope 仍 fail-closed；
  - JSON header 标错但 body 是 binary EventStream 时正常解析。

### 2. usage-only / metadata-only EventStream 不再误判失败

修改 stream/non-stream 终端判断：

- 如果收到可信上游 side-channel event：
  - `metadataEvent`
  - `messageMetadataEvent`
  - `contextUsageEvent`
  - `meteringEvent`
- 即使没有 assistant/reasoning/tool 内容，也不再返回 protocol failure。

未知事件仍保持 fail-closed：

- 只有 `Unknown` frame 且无可信 side-channel event，仍失败；
- 有普通 assistant 文本但缺可信完成信号，仍失败，避免 silent truncation 被当成功。

### 3. 解析 `meteringEvent.inputTokens/outputTokens`

`src/kiro/model/events/additional.rs`：

- `MeteringEvent` 新增 `input_tokens`、`output_tokens`；
- 对应上游 camelCase 字段 `inputTokens/outputTokens`；
- 当 metadata usage 缺失或无信号时，用 metering token 填充 metadata usage fallback。

### 4. EOF tool input 容错 flush

stream：

- 记录 pending tool input 的 upstream tool name；
- EOF 终端判断时，若存在 pending tool buffer：
  - 全部 buffer 能解析成合法 JSON 且存在 block index，允许作为可 flush tool；
  - 任一 buffer 不是合法 JSON，仍 fail-closed；
  - 即使同时存在 `meteringEvent/contextUsageEvent`，坏 tool JSON 也不能被 usage side-channel 掩盖。
- final events 里 flush 完整 pending tool input：
  - 补发必要的 `input_json_delta`；
  - 发 `content_block_stop`；
  - 保持 tool name/schema key reverse mapping 和 `AskUserQuestion` CLI 参数修复。

non-stream：

- EOF 时先处理 pending tool buffers；
- buffer JSON 可解析则输出 tool_use content block；
- JSON 解析失败仍返回 502，并记录 parse error；
- 然后再执行缺完成信号判断。

### 5. 空 content 的 non-stream output token 不再被本地虚增为 1

原逻辑对空 content 调用 `estimate_output_tokens(&content)`，该函数历史上最小返回 `1`。

修复后：

- non-stream content 为空时，estimated output tokens 为 `0`；
- 有文本/工具/思考内容时仍走原估算；
- 如果上游 metadata/metering 明确给出正数 output tokens，仍优先使用上游值。

这避免 usage-only success 被本地记成 1 个 output token。

### 6. websearch failure metadata 和 clippy 质量门禁

本轮顺手修复了前序 websearch 改动带来的质量门禁问题：

- websearch failure usage metadata 只记录脱敏 selection failure 与内部 reason；
- `WebSearchFailure` 将较大的 attribution 放入 `Box`，避免 `result_large_err` 回归；
- 不改变 websearch 对外行为。

## 修改文件

- `src/kiro/model/events/additional.rs`
  - `MeteringEvent` 解析 `inputTokens/outputTokens`。
- `src/anthropic/stream.rs`
  - usage-only terminal signal；
  - pending tool input 完整性判断；
  - EOF flush tool input；
  - metering token fallback。
- `src/anthropic/handlers.rs`
  - non-stream metering token fallback；
  - non-stream EOF tool flush 顺序；
  - 空 content output token 估算；
  - websearch failure metadata。
- `src/anthropic/handlers/tests.rs`
  - 新增 usage-only metering fixture；
  - 新增 incomplete tool without status fixture；
  - stream/non-stream 5 轮正向矩阵。
- `src/anthropic/websearch.rs`
  - Box failure attribution；
  - 简化 MCP tool_use_id 生成。

## 复现方法

不需要真实生产 credential 即可复现 adapter bug：

### A. usage-only EventStream

fake Kiro upstream 返回：

```text
HTTP 200
content-type: application/vnd.amazon.eventstream
frames:
  contextUsageEvent {"contextUsagePercentage":0.01}
  meteringEvent {"usage":0.24,"inputTokens":123,"outputTokens":0}
EOF
```

修复前：

- stream/non-stream 可能因为没有 assistant/reasoning/tool 内容而返回 protocol failure。

修复后：

- 返回 success；
- downstream `stop_reason=end_turn`；
- usage 记录 success；
- `kiro_metering_usage=0.24`；
- output tokens 保持 `0`，不会由空 content 本地估算成 `1`。

### B. tool input 完整但缺 stop

fake Kiro upstream 返回：

```text
HTTP 200
content-type: application/vnd.amazon.eventstream
frames:
  toolUseEvent {
    "name":"Bash",
    "toolUseId":"toolu_legacy_terminal",
    "input":"{\"command\":\"printf legacy-tool-ok\"}",
    "stop":false
  }
EOF
```

修复前：

- non-stream 返回 `upstream eventstream ended with 1 incomplete tool input buffer(s)`；
- stream 在 final flush 前被 terminal failure 拦截。

修复后：

- stream/non-stream 都返回 success；
- downstream `stop_reason=tool_use`；
- 输出 `tool_use` block；
- tool name 和 schema key 仍按映射还原。

### C. 坏 tool JSON 仍 fail-closed

fake Kiro upstream 返回：

```text
toolUseEvent {"input":"{","stop":false}
EOF
```

修复后仍应失败，不会被 `meteringEvent/contextUsageEvent` 掩盖成成功。

### D. unknown-only 仍 fail-closed

fake Kiro upstream 只返回未知事件：

```text
futureUnknownEvent {"opaque":"..."}
EOF
```

修复后仍失败，不返回空 success。

## 已完成验证

所有 Cargo 命令均通过 `feature/tests/run-cargo-scoped.sh` 执行，scoped target 均清理，未继续堆积仓库 `target/`。

### 格式与编译

```text
cargo fmt
git diff --check
feature/tests/run-cargo-scoped.sh eventstream-urgent-check3 -- \
  bash -lc 'cargo check --locked --bin kiro-rs'
```

结果：通过。

### clippy baseline

```text
feature/tests/run-cargo-scoped.sh eventstream-urgent-clippy3 -- \
  bash -lc 'rustup run 1.92.0 node scripts/ci/check-clippy-baseline.mjs'
```

结果：

```text
Clippy emitted 817 warnings; the checked-in baseline allows 849.
```

未更新 baseline。

### 协议 focused tests

```text
feature/tests/run-cargo-scoped.sh eventstream-urgent-tests-final -- bash -lc '
  cargo test --locked --bin kiro-rs metering_and_code_events_deserialize_without_extra_requirements -- --nocapture &&
  cargo test --locked --bin kiro-rs trusted_terminal_contract_rejects_silent_eof_and_keeps_legacy_terminals_for_five_rounds -- --nocapture &&
  cargo test --locked --bin kiro-rs handler_legacy_metadata_metering_and_complete_tool_are_trusted_terminals_for_five_rounds -- --nocapture &&
  cargo test --locked --bin kiro-rs handler_non_stream_untrusted_eof_fails_closed_for_five_rounds -- --nocapture &&
  cargo test --locked --bin kiro-rs handler_eventstream_postcommit_faults_never_retry_or_fake_success_for_five_rounds -- --nocapture &&
  cargo test --locked --bin kiro-rs websearch -- --nocapture
'
```

结果：

- `metering_and_code_events_deserialize_without_extra_requirements`: passed。
- `trusted_terminal_contract_rejects_silent_eof_and_keeps_legacy_terminals_for_five_rounds`: passed。
- `handler_legacy_metadata_metering_and_complete_tool_are_trusted_terminals_for_five_rounds`: passed。
- `handler_non_stream_untrusted_eof_fails_closed_for_five_rounds`: passed。
- `handler_eventstream_postcommit_faults_never_retry_or_fake_success_for_five_rounds`: passed。
- `websearch`: `26 passed / 0 failed`。

### personal/Gmail JSON-labeled EventStream 与外部池计费回归

```text
feature/tests/run-cargo-scoped.sh protocol-billing-regression-final -- bash -lc '
  cargo test --locked --all-targets provider_status_and_non_eventstream_matrix_is_private_typed_and_bounded -- --nocapture &&
  cargo test --locked --all-targets handler_binary_eventstream_with_json_content_type_is_body_sniffed_for_five_rounds -- --nocapture &&
  cargo test --locked --all-targets external_pool_billing_matches_dashed_opus_request_to_dotted_pricing_model -- --nocapture
'
```

结果：

- `provider_status_and_non_eventstream_matrix_is_private_typed_and_bounded`: passed。
- `handler_binary_eventstream_with_json_content_type_is_body_sniffed_for_five_rounds`: passed。
- `external_pool_billing_matches_dashed_opus_request_to_dotted_pricing_model`: passed。

```text
feature/tests/run-cargo-scoped.sh external-billing-final -- bash -lc '
  cargo test --locked --all-targets external_pool_billing_pass_through_uses_reported_cost_without_floor -- --nocapture &&
  cargo test --locked --all-targets external_pool_billing_tracks_raw_shaped_uplifted_costs -- --nocapture &&
  cargo test --locked --all-targets external_pool_billing_uses_output_uplift_as_final_reported_cost -- --nocapture &&
  cargo test --locked --all-targets estimate_matches_dashed_request_to_dotted_price_model -- --nocapture
'
```

结果：

- `external_pool_billing_pass_through_uses_reported_cost_without_floor`: passed。
- `external_pool_billing_tracks_raw_shaped_uplifted_costs`: passed。
- `external_pool_billing_uses_output_uplift_as_final_reported_cost`: passed。
- `estimate_matches_dashed_request_to_dotted_price_model`: passed。

## 当前限制

- 本轮没有对生产机器发真实模型请求，因为目标机器上相关凭据已经被禁用/不可用，且用户要求分析不能影响现网服务。
- 本轮没有重新执行完整 Claude Code CLI 长会话/tools/MCP/图片矩阵；该矩阵属于发布前完整 gate，当前紧急补丁先完成 adapter 级复现和 fail-closed 回归验证。
- 生产历史 usage 没有保存完整 EventStream frame 内容，因此生产侧只能证明“HTTP 200 + EventStream EOF + parser 误判指纹”，不能还原每一帧原始 payload。

## 发布后观测项

上线后应重点看：

- `upstream eventstream ended without a meaningful assistant, reasoning, or tool event` 是否下降；
- `upstream eventstream ended with ... incomplete tool input buffer(s)` 是否下降；
- 企业/API-key 类 credential 是否仍出现 `upstream_status=200 public_status=200` 但 usage error；
- `kiro_metering_usage` 是否保留；
- usage-only success 是否 output tokens 为 `0`，不再虚增；
- unknown-only / JSON error envelope 是否仍保持失败，而不是空成功。
