# 外部池非流式成功响应 0 计费复查与修复记录

日期：2026-07-23

本文件独立记录 `v0.0.113` 部署后仍出现“请求没报错但计费为 0”的问题。它补充并修正本目录内早先审计文档的结论：上一轮修复覆盖了“非流式 JSON 被 SSE header 误分类”和“正常 Anthropic message JSON 缺 usage 时注入估算 usage”的路径，但没有覆盖“HTTP 200 成功 body 既没有可识别 usage、又不是当前 parser 认为的标准 message JSON”的路径。

## 现网版本确认

生产容器已是本轮部署版本，不是旧镜像残留：

- image：`ghcr.io/2ue/kiro-rs:latest`
- version label：`0.0.113`
- revision label：`36b65ce509809120ba53bb46c6b536e3658a6129`
- app container created：`2026-07-23 09:19:28 +0800`
- DB 查询窗口起点：`2026-07-23 01:19:28+00`

## 部署后证据

只读 DB 查询显示，部署后新写入的数据仍有成功 0 计费：

- `2026-07-23 01:19:28+00` 到 `2026-07-23 02:27:11+00`
  - success 总数：`2152`
  - `output_tokens=0 AND estimated_cost_usd=0`：`795`
  - 这 `795` 条全部是：
    - `routeKind=external_pool`
    - `routeSubtype=external_direct_policy`
    - `stream=false`
    - `usageSource=request_estimate`
    - `rawUsage` 缺失
    - `externalPoolBilling` 缺失

按池拆分后，早期窗口的 0 计费集中在 `pool_id=4 kkkkyue`：

- `kkkkyue / claude-opus-4-8 / non-stream / request_estimate`：`494/494` 为 0
- `kkkkyue / claude-haiku-4-5-20251001 / non-stream / request_estimate`：`141/141` 为 0
- `kkkkyue / claude-sonnet-5 / non-stream / request_estimate`：`102/102` 为 0

继续看最近 30 分钟，问题不再只局限于 #4：

- DB 当前时间：`2026-07-23 03:01:47+00`
- 最近 30 分钟 success 0 计费：
  - `pool_id=4 kkkkyue / non-stream / request_estimate`：`106/106`，`rawUsage=0`，`externalPoolBilling=0`
  - `pool_id=15 apiv3.52codeflow / non-stream / request_estimate`：`79/79`，`rawUsage=0`，`externalPoolBilling=0`
- 同窗口 stream 成功记录仍正常：
  - #4 stream `local_prompt_cache`：`367/367` 有 `rawUsage` 和 `externalPoolBilling`
  - #15 stream `local_prompt_cache`：`330/330` 有 `rawUsage` 和 `externalPoolBilling`

代表样本：

- #4 成功 0 计费样本：
  - `req_01254b3jpRMUwahApJmdTEmy`
  - `created_at=2026-07-23 02:32:57+00`
  - `endpoint=/v1/messages`
  - `model=claude-sonnet-5`
  - `requestedMaxTokens=64000`
  - `total_input_tokens=53055`
  - `externalAttempts=[{poolId:4,status:200,action:success}]`
  - `rawUsage` 缺失，`externalPoolBilling` 缺失，`output_tokens=0`，`estimated_cost_usd=0`

- #15 成功 0 计费样本：
  - `req_01f69uUbT4VHgJA5uEuMCVN5`
  - `created_at=2026-07-23 03:01:59+00`
  - `endpoint=/cc/v1/messages`
  - `model=claude-sonnet-4-6`
  - `requestedMaxTokens=8`
  - `total_input_tokens=814`
  - `externalAttempts=[{poolId:15,status:200,action:success}]`
  - `rawUsage` 缺失，`externalPoolBilling` 缺失，`output_tokens=0`，`estimated_cost_usd=0`

这个 #15 小请求样本修正了早先“主要是 #4 高 max_tokens 长上下文触发”的判断：高 max_tokens/长上下文是明显放大因素，但不是唯一触发条件。

## 直连复核

直连只输出 body 指纹，不保存完整 body，不打印上游 key。

- #4 小样本直连当前返回 `429` JSON error envelope：
  - `status=429`
  - `json_type=object`
  - `top_keys=error,type`
  - `usage_paths=` 空
  - 结论：当前无法用 #4 直连拿到 200 success body；这个结果只说明 #4 现时存在限流/不可用状态，不能用于推断成功 0 计费样本 body。

- #15 小样本直连可正常返回标准 Anthropic usage：
  - `pool_id=15`
  - `model=claude-sonnet-5`
  - `status=200`
  - `content_type=application/json`
  - `top_keys=content,id,model,role,stop_reason,stop_sequence,type,usage`
  - `usage_paths=usage`

- #15 贴近样本参数直连也可正常返回标准 usage：
  - `pool_id=15`
  - `model=claude-sonnet-4-6`
  - `max_tokens=8`
  - `status=200`
  - `top_keys=content,id,model,role,stop_reason,stop_sequence,type,usage`
  - `usage_paths=usage`

因此不能说 #15 或 #4 永远不返回 usage。更准确的判断是：某些真实请求形态会拿到 HTTP 200 success body，但该 body 没有当前 parser 可识别的 usage，也不满足当前“标准 message JSON”估算条件。

## 根因

代码路径在 `src/external_pool.rs`：

1. 非流式外部池成功响应会先排除明显 SSE/HTML/error envelope。
2. 然后进入 `process_non_stream_response_usage(...)`。
3. 旧逻辑只在两种情况下生成 `usage_capture`：
   - body 里已有可识别 usage 候选路径，例如 `$.usage`、`$.message.usage`、`$.data.usage` 等；
   - body 是当前识别的“正常非流式模型响应”，例如顶层 `type=message`、顶层 `content`、`message.content`、`data.content`、`response.content`。
4. 如果 HTTP 200 body 是其他 JSON wrapper、OpenAI-style `choices`、纯文本、或其他未识别成功体，旧逻辑返回空 `usage_capture`。
5. `external_pool_billing_from_capture(...)` 因为 `capture.raw` 为空返回 `None`。
6. `record_external(...)` 在 success 但 `billing=None` 时回退为：
   - `usageSource=request_estimate`
   - `compat_input_tokens=request_input_tokens`
   - `billable_input_tokens=request_input_tokens`
   - `output_tokens=0`
   - `pricing_available=false`
   - `estimated_cost_usd=0`
   - `rawUsage` 缺失
   - `externalPoolBilling` 缺失

所以现象不是 DB 自己算错，也不是单纯“上游没 usage”。直接原因是成功响应进入了“未识别 body 且无计费兜底”的代码空洞。

## 修复设计

修复落在 `src/external_pool.rs`：

1. 非流式 HTTP 200 成功响应，如果没有任何可识别 usage，也不是当前标准 message JSON，仍生成估算 `ExternalPoolBilling`。
2. 估算来源：
   - input tokens：请求侧估算 `estimated_external_request_input_tokens(...)`
   - output tokens：
     - 对常见 OpenAI-style `choices[].message.content`、`choices[].delta.content`、`choices[].text` 做文本估算；
     - 对常见 wrapper 字段如 `output_text`、`text`、`result`、`data.text`、`response.output_text` 做文本估算；
     - 仍无法识别时 output 记 `0`，但 input 成本仍可计费。
3. 标记诊断字段：
   - `externalPoolBilling.usageEstimated=true`
   - `externalPoolBilling.usageEstimateReason="unrecognized_success_body"`
   - `usageSource=request_estimate`
4. 下游 body 行为：
   - 如果 body 是 JSON object，则保留原字段并注入顶层 Anthropic-style `usage`，让下游标准 parser 不再拿不到 usage；
   - 如果 body 不是 JSON object，则不改 body，只做内部 billing，因为无法安全地给纯文本/二进制响应注入 JSON usage。
5. 旧路径保持不变：
   - 已有标准 usage 的 body 仍按原逻辑捕获/整形；
   - `skip_non_stream_usage_projection=true` 仍不重写已有 usage；
   - stream synthetic usage 路径不变；
   - SSE header + JSON body 修复路径不变；
   - HTML/error envelope 仍在前置校验中走错误/重试，不当 success 计费。

## 本地测试

新增测试：

- `non_stream_unknown_json_without_usage_injects_estimated_usage_and_billing`
- `non_stream_unknown_text_without_usage_records_estimated_billing_without_rewriting_body`
- `external_pool_fake_upstream_non_stream_unknown_json_records_estimated_billing`

已运行：

```bash
cargo fmt --all -- --check
git diff --check
RUSTUP_TOOLCHAIN=1.92.0 node scripts/ci/check-clippy-baseline.mjs
```

结果：

- fmt 通过；
- whitespace diff check 通过；
- clippy baseline 通过，实际 `764` warnings，baseline 允许 `764`。

```bash
RUSTUP_TOOLCHAIN=1.92.0 cargo test --locked non_stream_unknown
```

结果：`3 passed`。

```bash
RUSTUP_TOOLCHAIN=1.92.0 cargo test --locked external_pool::tests:: -- --test-threads=4
```

结果：`143 passed`。

```bash
RUSTUP_TOOLCHAIN=1.92.0 cargo test --locked --all-targets --no-default-features
```

结果：`1289 passed` + `26 passed`。

```bash
RUSTUP_TOOLCHAIN=1.92.0 cargo test --locked --all-targets
```

结果：`1289 passed` + `26 passed`。

```bash
RUSTUP_TOOLCHAIN=1.92.0 cargo build --release --locked
```

结果：通过，仅有既有 dead code warnings。

这组测试覆盖：

- 已有 usage 的 pass-through billing；
- current path policy usage projection；
- `skip_non_stream_usage_projection` 对已有 usage 的不整形；
- 非流式缺 usage 的估算注入；
- 未识别 JSON 成功体的估算注入；
- 非 JSON 成功体的内部 billing 兜底；
- SSE header + JSON body 非流式路径；
- stream synthetic usage 和 SSE usage projection。

## 本地证据位置

本轮只读生产证据根目录：

```text
tmp/prod-evidence/20260723-012743-kiro-prod/
```

关键文件：

- `raw/db/external_pool_body_probe_small.txt`
- `raw/db/external_pool_body_probe_pool15_small.txt`
- `raw/db/external_pool_body_probe_pool15_sonnet46_max8.txt`
- `probe_external_pool_body.sh`

这些 raw 文件仅留本地，不进入默认报告包。最终报告不得包含 SSH 密码、DB 密码、上游 API key 或完整上游 body。
