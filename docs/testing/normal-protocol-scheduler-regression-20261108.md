# 当前改动：正常调度与 Claude 协议回归验证

- 验证日期：2026-11-08（UTC；本机时区 America/Los_Angeles）
- 验证目标：确认当前未提交改动不破坏正常账号调度、Anthropic Messages 协议、SSE 生命周期、工具调用和 Claude Code CLI 链路。
- 生产边界：59138 只执行 `GET /healthz`；没有重启、改配置、写 PostgreSQL/Redis、改账号状态或向生产发送推理请求。
- 开发实例：59139，PID `379231`，由开发循环 PID `379200` 管理。
- 开发二进制：`/root/.cache/kirotest-dev/kiro-rs-dev`，SHA-256 `037b6df53c13483d0a86bb6e45720f9d95b4df5223db6d36d936cb03b14f83ae`。
- 开发配置：`/tmp/kirotest-dev/config/config.json`；凭证文件使用开发实例隔离路径。
- 仓库基线：HEAD `2e625e52d2b2225c487d6ff027ce29110f997511`；当前工作树包含 12 个未提交 Rust 文件改动，源码 diff SHA-256 `3aa7ce274628877402cdb9dda2c28d1e27a8c1f8b172e15ca0dbb4974ede4ef1`。
- 编译方式：沿用 debug 增量开发循环，`cargo build --bin kiro-rs -j 2`，成功后只原子替换并重启 59139；本轮未执行 release 冻结编译。

## 1. 资源与边界检查

开发循环的资源门禁为：可用内存低于 4 GiB、可用 swap 低于 8 GiB、1 分钟 load 不低于 8 或 59138 健康检查失败时暂停编译；恢复阈值为内存 6 GiB、swap 10 GiB、load 小于 6 且 59138 恢复 200。

本轮结束时：

- 59138 `/healthz`：HTTP 200，`{"status":"ok"}`。
- 59139 `/healthz`：HTTP 200，`{"status":"ok"}`。
- 机器：约 11 GiB 可用内存、8.3 GiB 可用 swap，1 分钟 load 约 0.64。
- 59139 进程仍存活，RSS 约 172 MiB；开发循环没有产生 orphan 进程。

## 2. C0 本地测试

使用项目提供的 scoped runner，没有直接使用裸 `cargo`：

```bash
feature/tests/run-cargo-scoped.sh normal-protocol-c0 -- \
  bash -lc 'cargo fmt --check && \
            cargo test --bin kiro-rs anthropic::stream::tests --no-fail-fast && \
            cargo test --bin kiro-rs anthropic::websearch::tests --no-fail-fast && \
            cargo test --bin kiro-rs anthropic::handlers::tests --no-fail-fast'
```

结果：

- `cargo fmt --check`：通过。
- stream 单测：145/145 通过。
- WebSearch 单测：36/36 通过。
- handlers 单测：247/247 通过。
- scoped target 清理完成。
- `git diff --check`：通过。

## 3. 真实 Claude 协议矩阵

### 3.1 原始 Opus 矩阵

原始报告：`/tmp/normal-protocol-20261108T134518Z/protocol-matrix.json`。

该矩阵使用 `claude-opus-5-5`，用于检查协议边界；但当前开发账号只剩两个可调度账号，且它们声明支持的模型是 Sonnet/Haiku 等，不包含 Opus 5.5。因此其中的 dfcache 503 是账号模型能力问题，不是 SSE 转换失败。

| 场景 | 结果 | 说明 |
|---|---:|---|
| `/cc/v1/messages` 流式 | 200，SSE 完整 | `message_start → content_block_start → content_block_delta → content_block_stop → message_delta → message_stop` |
| `/cc/v1/messages` 非流式 | 200 | `message`、text block、usage、`end_turn` 完整 |
| `/v1/messages` 流式 | 200，SSE 完整 | 生命周期和 usage 完整 |
| 未配置 `/dfcache` 路由 | 404 | `not_found_error`，公开消息正确 |
| `/dfcache/tune-ss6-99` 流式 | 503 | 请求 `req_01mR4uGipCUrHnmG6A8ipRp1`；日志为“没有支持当前模型的可用账号（可用：2/65）” |
| 非法模型 `model-does-not-exist` | 502 | 请求 `req_01f1qnwxxBnk3KAzkLhDZfvL`；见第 5 节 |

工具请求的第一次 400 使用了 `input_schema.additionalProperties=false`。去掉该字段后，同一路径返回完整 `tool_use` 流，包含多个 `input_json_delta`，终止原因为 `tool_use`。因此第一次 400 是测试构造触发的上游工具 schema 兼容边界，不是当前代理 SSE 回归。

thinking 请求声明 `{"type":"adaptive"}` 时返回 HTTP 200、普通 text block；本次上游没有生成 reasoning，因此没有 `thinking_delta`、`signature_delta` 或 thinking block。入口和响应协议正常，但该次没有验证真实 thinking 输出。

### 3.2 使用本地账号支持模型的调度矩阵

为排除账号池模型能力造成的假失败，使用开发 Admin 只读快照中明确支持的 `claude-sonnet-4-5`，顺序低频发送 5 个请求。原始结果：`/tmp/normal-protocol-20261108T134518Z/local-matrix.json`。

| 场景 | HTTP | SSE / body 检查 | 结果 |
|---|---:|---|---:|
| `/cc/v1/messages` 流式 | 200 | 1 个 `message_start`、1 个 `message_stop`、block 起止配对、2 个 text delta、`end_turn` | 通过 |
| `/cc/v1/messages` 非流式 | 200 | `message`、text block、usage（input 4109、output 3）、`end_turn` | 通过 |
| `/v1/messages` 流式 | 200 | 完整 SSE 生命周期、usage 非零、`end_turn` | 通过 |
| `/dfcache/tune-ss6-99/v1/messages` 非流式 | 200 | `message`、text block、usage（input 4109、output 3）、`end_turn` | 通过 |
| `/cc/v1/messages` 强制工具调用 | 200 | `tool_use` block、4 个 input JSON delta、`stop_reason=tool_use` | 通过 |

**本地支持模型矩阵：5/5。** 59139 日志对应的本地凭证为 2528、2529，均返回上游 HTTP 200；请求完成后 `inFlightRequests=0`，没有残留占用。另做 2 路并发 Sonnet 4.5 流式冒烟，结果 2/2 HTTP 200，两个请求都完整收到 `message_start`/`message_stop`，随后 `globalInFlightRequests=0`、`queuedRequests=0`、`inUse=0`。原始结果：`/tmp/normal-protocol-20261108T134518Z/concurrency.json`。

## 4. 调度状态只读快照

报告目录：`/tmp/normal-protocol-20261108T134518Z/admin/`。读取接口：

- `GET /api/admin/credentials/summary`
- `GET /api/admin/credentials`
- `GET /api/admin/credentials/runtime`
- `GET /api/admin/external-pools/status`
- `GET /api/admin/config/runtime`

快照摘要：

| 指标 | 值 |
|---|---:|
| 总账号 | 65 |
| 正常 / 可调度 | 2 |
| quota exhausted | 62 |
| disabled / unavailable | 63 |
| cooling | 0 |
| in-flight | 0 |
| queued | 0 |
| 全局并发上限 | 1000 |
| 单凭证并发上限 | 3 |
| RPM | 500 |

仅剩账号 2528、2529 未禁用，支持模型列表包含：`claude-sonnet-4.5`、`claude-haiku-4.5` 等，不包含 `claude-opus-5-5`。因此 `req_01mR4uGipCUrHnmG6A8ipRp1` 的 503 是“路由请求模型与剩余本地账号能力不相容”，不是调度器把可用账号错误判定为 cooling/in-flight，也不是协议转换错误。

外部池状态显示启用且可调度的池为 `jinnyapi`、`apijing`、`zslnh`。`/dfcache/tune-ss6-99` 属于本地路由；本地没有匹配 Opus 的账号时，外部 fallback 并不能保证同一模型成功，不能把外部池的错误当成本地调度回归。

重试使用支持模型后恢复：

- `claude-sonnet-4-5`：HTTP 200，由账号 2528 完成。
- `claude-sonnet-4.5`：HTTP 200，由账号 2529 完成。
- 两个请求正文均返回 `route-ok`，`end_turn`，usage 非零。

## 5. 异常请求归因

### 5.1 dfcache 503

失败请求：`req_01mR4uGipCUrHnmG6A8ipRp1`。

日志链路明确记录：

```text
没有支持当前模型的可用账号（可用: 2/65）
没有可调度账号
```

Admin 快照同时显示 62 个账号 `quotaExhausted`，仅 2528/2529 可用，且两者支持模型列表不含 Opus 5.5。使用 Sonnet 4.5 重试后连续两次 200，故当前不修改调度代码。后续测试必须先按模型能力筛选账号，并在账号池不足时把该轮记为“环境容量失败”，不能记为协议回归。

### 5.2 非法模型 502

请求：`req_01f1qnwxxBnk3KAzkLhDZfvL`，模型 `model-does-not-exist`。

59139 日志显示：本地没有匹配账号后进入外部池；外部池尝试得到：

1. `jinnyapi`：404，`model_not_found`；
2. `apijing`：503，`model_not_found`；
3. `zslnh`：403，`INSUFFICIENT_BALANCE`。

最终公开状态被归一化为 502。这个结果是“非法模型 + 外部池余额/能力不一致”的组合，不足以证明当前改动引入回归。当前代码已有模型兼容和外部池 fallback 路径；后续若要改变状态码，应先补齐本地模型解析规范和独立的非法模型单测，暂不改。

### 5.3 工具 schema 400

最初测试 schema 带 `additionalProperties=false`，上游拒绝 forced tool choice；移除该字段后 tool_use 完整通过。当前 converter 的 schema 规范化和 tool_use SSE 在实际请求中正常，不改代码。

### 5.4 thinking 没有真实 reasoning

`thinking: {type: adaptive}` 的 HTTP 入口、200 响应和普通 SSE 生命周期正常。本次上游只生成 text，不能把“没有 thinking block”判定为代理丢失；需要上游真实产生 reasoning 的样本才能继续验证 thinking/signature。

## 6. Claude Code CLI

此前同一开发二进制已用隔离 `HOME` 和 `CLAUDE_CONFIG_DIR` 验证 Claude Code 2.1.283：

- 普通调用：通过；stream-json 为 system 2、stream_event 7、assistant 1、result 1；output tokens 10、input tokens 373、错误 0。
- Bash 工具调用：通过；`tool_use=1`，最终结果 `cli-tool-done`，没有 credential/fallback/scheduler 内部信息泄漏。
- `sonnet-thinking` 是 Claude CLI 自身不识别的模型别名，不能归因于代理；改用 `--model sonnet --effort high/max` 后均通过，`thinking_tokens=0`。

## 7. 结论

1. **正常调度：通过（在可用模型条件下）**。本地账号 2528/2529 被正常选中和释放，低频 5 请求全部完成；没有 queue、in-flight 泄漏或错误的 cooldown 判定。
2. **正常 Claude Messages 协议：通过**。`/cc` 流式/非流式、`/v1` 流式、`/dfcache/tune-ss6-99` 非流式均返回规范生命周期和 usage；工具调用完整返回 `tool_use` 与 `input_json_delta`。
3. **当前未发现由这批代码改动导致的正常协议回归**。C0 单测、真实协议矩阵、Claude Code CLI 均支持该结论。
4. **环境限制仍需记录**：59139 当前只有 2/65 本地账号可调度，Opus 5.5 路由会因账号模型能力不足返回 503；测试脚本必须使用可用模型或先检查账号能力。
5. **未决协议问题**：真实 thinking、CCTest 私有签名评分、WebSearch opaque 字段仍需专项证据；这些不影响本轮普通协议回归结论。

## 8. 证据文件

- `/tmp/normal-protocol-20261108T134518Z/protocol-matrix.json`
- `/tmp/normal-protocol-20261108T134518Z/scheduler-smoke.json`
- `/tmp/normal-protocol-20261108T134518Z/local-matrix.json`
- `/tmp/normal-protocol-20261108T134518Z/concurrency.json`
- `/tmp/normal-protocol-20261108T134518Z/admin/summary-after-concurrency.json`
- `/tmp/normal-protocol-20261108T134518Z/admin/summary.json`
- `/tmp/normal-protocol-20261108T134518Z/admin/credentials.json`
- `/tmp/normal-protocol-20261108T134518Z/admin/credentials-final.json`
- `/tmp/normal-protocol-20261108T134518Z/admin/pools.json`
- `/tmp/normal-protocol-20261108T134518Z/admin/config.json`
- `/tmp/kirotest-dev/logs/kirotest-dev.log`
- `/tmp/kirotest-dev/logs/dev-loop.stdout.log`

原始抓包和唯一事件归档按 `TASK.md` 的证据保留规则继续保留；本轮没有清理任何原始抓包。
