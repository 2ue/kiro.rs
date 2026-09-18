# Claude Code ↔ Kiro 协议互转 P0 实现与真实验证证据

执行日期：2026-09-16 Asia/Shanghai
清理复核：2026-09-17 Asia/Shanghai
分支：`main`
基线：`5313bc1` (`chore(release): 0.0.163`)，标签 `v0.0.163`
冻结运行二进制 SHA-256：`e00a99d2c8528c66d45485ba5c979e2d820f04688793fdf9a0ccdfa9a219ec4e`
Claude Code CLI：`2.1.273`
证据级别：`source-verified` + `focused-pass` + `isolated-real-credential` + `fake-external-pass`

## 范围

本批次只处理 Claude Code/Anthropic 与 Kiro 协议互转的窄范围兼容，不改变收费、
余额、成本或 usage 计费口径，也不改变调度策略。

已落地：

- Kiro EventStream frame/decoder 的可回放合同：合法分片、截断 dirty EOF、坏
  prelude/message CRC、非法 header、坏帧后恢复和重复 frame；
- 已知 Kiro event key 的 typed nested/top-level payload fallback；
- `toolUseEvent.input` 同时接受字符串 fragment 和完整 JSON value，并将完整 value
  以紧凑 JSON 字符串交给现有流式拼接器；
- Claude tool name mapping 的完整序列化 payload 回归：结构化 tools、历史
  `tool_use` 和 tool-choice steering 使用同一 Kiro-safe 名称，不把非法原名发给 Kiro；
- Kiro event 到 Claude SSE 的 thinking → text → tool → usage lifecycle 合同；
- 未放宽 strict CRC/EOF 主边界；unknown event 和 malformed known nested payload
  仍保持诊断/fail-closed，不降级为空文本或伪造 tool call。

本轮运行时验证增加了：

- 使用用户提供的 Kiro 凭据文件做低并发真实本地凭据调用；原始凭据文件未修改，
  只在本证据中记录文件哈希和聚合统计；
- 使用独立临时 PostgreSQL/Redis、临时 `kiro-rs` 和 loopback fake Anthropic
  upstream 验证本地凭据路由、Claude Code CLI 消费和外部池 SSE 生命周期；
- 所有真实账号请求均限制为 `haiku`、`sonnet-4.5` 或 Claude Code 的 `sonnet`
  alias；没有对账号做批量余额检查或高并发压力。

## 改动文件

- `src/kiro/parser/frame.rs`
- `src/kiro/parser/decoder.rs`
- `src/kiro/model/events/base.rs`
- `src/kiro/model/events/assistant.rs`
- `src/kiro/model/events/tool_use.rs`
- `src/kiro/model/events/additional.rs`
- `src/kiro/model/events/context_usage.rs`
- `src/anthropic/stream.rs`
- `src/anthropic/converter.rs`
- `src/anthropic/converter/history.rs`
- `src/anthropic/converter/tools.rs`

## 验证命令与结果

所有 Cargo 命令均通过 `feature/tests/run-cargo-scoped.sh` 执行。每个 scoped
target 在命令结束后由 wrapper 清理。

| Scope | Command | Result |
|---|---|---|
| `proto-interop-test-parser` | `cargo test --locked kiro::parser::frame::tests` | 7 passed, 0 failed, 0 ignored |
| `proto-interop-test-decoder` | `cargo test --locked kiro::parser::decoder::tests` | 6 passed, 0 failed, 0 ignored |
| `proto-interop-test-events` | `cargo test --locked kiro::model::events` | 18 passed, 0 failed, 0 ignored |
| `proto-interop-test-stream` | `cargo test --locked anthropic::stream::tests::nested_kiro_events_round_trip_to_claude_sse_lifecycle` | 1 passed, 0 failed, 0 ignored |
| `proto-interop-test-converter` | `cargo test --locked mapped_tool_name_is_used_across_serialized_kiro_payload` | 1 passed, 0 failed, 0 ignored |
| `proto-interop-test-converter-all` | `cargo test --locked anthropic::converter::tests` | 148 passed, 0 failed, 0 ignored |
| `proto-interop-full-test` | `cargo test --locked` | main: 2113 passed, 0 failed, 6 ignored；`kiro_loadtest`: 31 passed, 0 failed |
| `proto-interop-release-build` | `cargo build --release` | passed；运行时使用的冻结 `kiro-rs` 二进制已在 wrapper 清理前复制并哈希 |
| `proto-interop-final-check` | `cargo check --all-targets --locked` | passed |
| `proto-interop-final-fmt` | `cargo fmt --all -- --check` | passed |
| local diff hygiene | `git diff --check` | passed |

已有的 parser/events/stream focused 测试在本轮实现前后均未发现回归；converter
全测试包含新增 payload 扫描合同。

## 真实账号与外部池运行时验证

### 账号文件与隔离资源

| 项目 | 脱敏结果 |
|---|---|
| 凭据文件 SHA-256 | `4f8c943b8a669b1a81ae23d3c7d8fc688db5cd0bab69973d367b88f3334cd9b6` |
| 账号总数 | `235` |
| disabled | `0`（导入前聚合） |
| `authMethod` | `social=210`、`idc=10`、`api_key=15` |
| `apiRegion` | `us-east-1=212`、缺失=23 |
| subscription | 全部为 `KIRO FREE` |
| 运行服务 | `127.0.0.1:19023`，临时服务 PID `38605` |
| fake 外部池 upstream | `127.0.0.1:19080` |
| 临时 PostgreSQL | `kiro_cckiro_real_20260916_2035` |
| Redis | DB `15`，key prefix 仅用于本轮隔离 |

导入通过服务启动时的 PostgreSQL bootstrap 完成，共加载 `235` 个账号，没有调用
批量 Admin 导入接口，避免对每个账号触发额外余额检查。临时服务只使用独立数据库、
Redis DB/prefix 和临时配置，不接触项目指定的长期测试实例数据库。
服务启动后的 `/healthz`、`/readyz` 均为 `200`，PostgreSQL、Redis 和 Redis runtime
events readiness 均为 true。真实请求结束时，临时数据库聚合为 `disabled=10`、
可用 `225`；这只是本轮额度/账号状态隔离结果，不代表源文件账号被修改。

### C1 直接 `/cc/v1/messages`

请求覆盖 `haiku`、`sonnet-4.5`、Claude Code 常用 `sonnet` alias、thinking、
tool-use、流式/非流式和 malformed request。结果分类如下：

| Case ID | Case | 结果 | 解释 |
|---|---|---:|---|
| `REAL-C1-001` | `haiku` non-stream | `502` | 前几个真实账号返回 Kiro `402 quota exhausted`，服务按账号状态隔离并归一化下游错误 |
| `REAL-C1-002` | `haiku` stream | `502` | 同上；不是 parser/SSE 失败 |
| `REAL-C1-003/004` | `sonnet-4.5` non-stream/stream | `502` | 同上 |
| `REAL-C1-005` | `sonnet-4.5` thinking stream | `502` | 没有可用额度，不能据此宣称 thinking 真实通过 |
| `REAL-C1-006` | `sonnet-4.5` tool-use stream | `502` | 没有可用额度，不能据此宣称 tool-use 真实通过 |
| `REAL-C1-007` | malformed messages | `400` | `invalid_request_error`，本地请求校验生效 |

服务日志和 usage 记录确认的模型链路：

- `haiku` → `claude-haiku-4.5`；
- `sonnet-4.5` → `claude-sonnet-4.5`；
- Claude Code CLI 发来的 `claude-sonnet-5` → `claude-sonnet-4.5`。

### C2 真实 Claude Code CLI

Case ID：`REAL-C2-001`。使用隔离 `HOME`/`CLAUDE_CONFIG_DIR`、`--bare`、`--print`、
`--output-format=stream-json`、`--include-partial-messages`、
`--no-session-persistence` 和 `--model sonnet`。CLI 退出码为 `0`，收到
`result=1`、`assistant=1`、`usage=2`，stop reason 为 `end_turn`。服务端 usage
记录为：

- `status=success`；
- `routeKind=local_credential`、`routeSubtype=local_success`；
- upstream model 为 `claude-sonnet-4.5`；
- input/output usage 均为非零。

这证明 Claude Code alias、流式 SSE 消费、usage 归档和本地凭据成功路由可以闭环。
本轮没有把 `thinking` 或 tool-use 结果从额度耗尽的 direct case 推断为通过。

runner 的宽泛正则曾把 CLI 控制输出标记为 `publicLeak=true`。由于本轮运行没有保留
原始 CLI stdout 的最小复核证据，不能把该告警直接判定为产品泄漏，也不能把本轮
运行写成完整 runner leak gate 通过；服务端 usage 和下游 HTTP 响应未发现 refresh
token、access token、内部账号池或调度器术语。2026-09-17 已补 runner 泄漏扫描器
和静态合同：后续长会话报告会保留 stdout SHA-256、行数、字节数和最多 16 行脱敏
可疑行摘要；正常 CLI 控制 transcript 只作为 warning 证据，真实 secret、credential
JSON、API key、内部 pool/scheduler 术语仍 fail closed。该静态合同已通过，但没有
重跑完整 `5x20` 或 `5x100` 长会话动态门。

### C1 外部池协议链路

本轮外部池为 loopback fake Anthropic upstream，不连接真实第三方服务。复现
`EXT-C1-000` 先配置
`supportedModels=["haiku","sonnet-4.5"]` 时，两个请求均为 `503`：服务内部候选
模型归一化格式与临时池白名单格式不一致，池被判定为不可用。随后把临时池白名单
置空（表示 unrestricted，仅用于验证协议链路）后：

| Case ID | Case | HTTP | 路由/模型 | SSE/usage |
|---|---|---:|---|---|
| `EXT-C1-001` | external `haiku` non-stream | `200` | `external_pool` / upstream `claude-haiku-4.5` | text=`external-pong`，input `12` / output `2` |
| `EXT-C1-002` | external `sonnet-4.5` stream | `200` | `external_pool` / upstream `claude-sonnet-4.5` | `message_start → content_block_start → content_block_delta → content_block_stop → message_delta → message_stop`，output `2` |

这证明 fake 外部池的 Anthropic non-stream/stream 转换、模型映射、usage 和 SSE
terminal lifecycle 正常。该批次暴露的 `supportedModels` 格式问题已在后续模型命名
合同中处理：外部池对外和 discovery/过滤统一保存 Claude Code 模型命令，本地凭据
仍保留 Kiro upstream 模型名；自定义模型保持 exact-only，不做 family 匹配。

## 非行为性失败与复现

以下尝试没有进入 Rust 编译或测试逻辑，不计为代码失败：

1. 初次把两个过滤器参数同时传给 `cargo test`，Cargo 报只支持一个 positional
   filter。复现方式是同一命令追加两个测试过滤器；修正为单过滤器或模块过滤器后通过。
2. 初次两个 scoped Cargo 批次使用 `KIRO_VALIDATION_RESERVE_KIB=2097152`，磁盘
   admission 因保留量超过 floor 被拒绝。wrapper 随即释放 reservation，没有遗留
   target；改用 `KIRO_VALIDATION_RESERVE_KIB=1048576` 串行执行后全部通过。
3. 早期 test-only frame builder 曾触发借用检查 `E0502`；改为先保存 CRC 数值再写入
   mutable slice，之后 `cargo check --all-targets` 通过。

## 2026-09-17 后续补充：模型命名与 leak runner 合同

本补充只记录本轮后续修正和静态/源码合同，不把未重跑的动态长会话写成通过。

- 外部池 `supportedModels` 现在按 Claude Code 模型命令规范处理：
  `sonnet`、`sonnet-4.5`、`haiku-4.5` 等是系统对外和外部池筛选口径；
  本地账号凭据列表继续使用 Kiro upstream 形式，例如 `claude-sonnet-4.5`。
- 已补 legacy Claude 3.5 官方 ID canonicalization：
  `claude-3-5-sonnet-20241022` / `claude-3.5-sonnet` -> `sonnet-3.5`，
  `claude-3-5-haiku-20241022` -> `haiku-3.5`；非法 opus legacy 和自定义模型
  仍按 exact-only 边界处理。
- 两套页面测试下拉已保持同一边界：外部池测试使用 Claude Code 命名，本地账号测试
  使用 Kiro 命名。
- `feature/tests/claude-cli-leak-scanner.mjs` 被长会话 runner 复用，报告新增
  `stdoutByteLength`、`stdoutLineCount` 和 `stdoutEvidence`；`stdoutEvidence` 只含
  stdout hash、行/字节数、match 计数和脱敏可疑行摘要，不保留完整 transcript。

验证：

| Command | Result |
|---|---|
| `node --check feature/tests/claude-cli-leak-scanner.mjs` | passed |
| `node --check feature/tests/claude-cli-long-session-continue.mjs` | passed |
| `node --test feature/tests/claude-cli-leak-scanner.contract.test.mjs` | `4/4` passed |
| `node --test feature/tests/claude-code-model-name-contract.test.mjs feature/tests/claude-cli-leak-scanner.contract.test.mjs` | `5/5` passed |
| `rg -P -n "(?<!p)npm\\s+(?:run|--prefix|test|ci)|(?<!p)npm\\s+--prefix" README.md docs feature ui admin-ui package.json --glob '!**/node_modules/**'` | no matches |

资源：

- 本补充未启动 `kiro-rs`、fake upstream、Claude CLI、Chrome DevTools 或 Cargo；
- 未创建 Cargo target、PostgreSQL 数据库、Redis key 或监听端口；
- 只执行 Node 静态/合同测试，没有产生需要清理的 runtime 进程。

## 2026-09-17 后续补充：外部池错误消费边界

- `src/external_pool/retry_pipeline.rs` 新增受控路由错误判定：
  - `model_mapping_miss`：当前池的映射配置不匹配，只排除当前池，不在同池重放；
  - `model_unavailable`：当前池/模型组合不可用，即使上游用 HTTP 400 返回，也允许转到
    其它合格池；模型级 cooldown 关闭时仍通过稳定的分类消息保持该语义；
  - 两者都不污染普通协议错误、schema/tool/image 400 或 payload guard retry。
- 新增合同测试：
  - `external_pool_retry_boundary_same_pool_limit_caps_to_one_and_rejects_terminal_errors`
  - `external_pool_retry_boundary_cross_pool_separates_route_errors_from_protocol_and_request_errors`
  - `external_pool_bad_request_model_unavailable_fails_over_without_same_pool_replay`
- 验证结果：
  - `feature/tests/run-cargo-scoped.sh proto-interop-error-boundary-matrix -- cargo test --locked external_pool_retry_boundary`
  - `2 passed / 0 failed / 0 ignored`；
  - `feature/tests/run-cargo-scoped.sh proto-interop-error-boundary-http-test -- cargo test --locked external_pool_bad_request_model_unavailable_fails_over_without_same_pool_replay`
  - `1 passed / 0 failed / 0 ignored` at test level; the existing helper returned before
    creating PgSQL/Redis fixtures because `KIRO_RS_TEST_POSTGRES_URL` and
    `KIRO_RS_TEST_REDIS_URL` were unset in this workspace；
  - 前置 `cargo fmt --check`、`git diff --check` 通过；
  - wrapper 在测试结束后删除约 `2,090,948 KiB` 隔离 target 并释放 reservation。
- 这批验证没有启动 kiro-rs、fake upstream、Claude CLI、PostgreSQL、Redis 或监听端口，
  因此不产生临时运行时资源；真实第三方池和本地 provider 完整 HTTP 错误消费矩阵仍待
  后续低并发、脱敏验证。

## 运行时与秘密边界

- 因指定 `127.0.0.1:19023` 在启动前无监听，且
  `tmp/thinking-budget-local/config.json` 不存在，本轮按技能允许的例外建立
  一个临时 `kiro-rs` 实例；原因、端口、配置、数据库、Redis DB/prefix 和生命周期
  已在本证据记录。
- 原始凭据文件未修改；证据不含 email、token、refresh token、profile ARN、完整
  request/body/response、完整账号 JSON 或完整 CLI transcript。
- 用户提供的凭据文件保留在工作区供后续明确授权的测试使用，当前权限已收紧为
  `0600`，且未加入任何 evidence 或提交内容。
- Chrome DevTools、Pencil、MCP 和其他 Claude 进程均是本轮之外的长期进程，未误杀。
- Admin API 删除临时外部池返回 HTTP `200`；停止临时服务和 fake upstream 后，
  `19023`、`19080` 均无监听。
- Redis DB 15 本轮前缀删除前 `9` 个 key，删除后 `0` 个；未执行 `FLUSHDB`。
- 临时 PostgreSQL 数据库删除前存在 `1` 个，删除后为 `0` 个。
- 临时根目录、隔离 Claude HOME/config、候选二进制和 marker 均已删除。
- `node feature/tests/inventory-build-artifacts.mjs --gate`：
  `targets=0 reservations=0 target_processes=0 blockers=0`；空的仓库根 `target/`
  目录也已移除。
- `19023`、`19080` 端口检查和 `git diff --check` 均通过。
- `node feature/tests/check-feature-docs.mjs` 报告的是仓库既有 78 个 issue 文档的 23
  项历史 contract/link violation；本批次新增 plan/evidence 文档没有引入代码或协议
  测试失败。该历史文档清理不属于本次互转实现范围。

## 未验证项与残余风险

本证据不能证明真实 Kiro 上游接受所有 nested shape，也不能把单个真实账号成功
调用当成协议规范。以下保持 `not-run`、`partial` 或 `待真实证据`：

- C2/C3 真实 Claude Code CLI 的 thinking、tool-use、交互、长会话矩阵（本轮
  normal alias/usage 已通过，thinking/tool-use direct 因额度耗尽为 partial）；
- 真实外部第三方池账号；本轮外部池为 fake loopback upstream；
- fake-upstream 产品级本地/外部池完整路由组合在本轮新代码上的重放；
- trim 后 tool pair 重新收敛、schema profile、thinking 多块、WebSearch index、
  400 message 子分类到 retry/failover/cooldown 的行为；
- Kiro `origin`、`profileArn`、endpoint/region 组合的真实接受范围。
- 真实外部池 `supportedModels` 与第三方服务 discovery 的组合验证；
- unknown model 是否应在本地 exact allowlist 下提前拒绝，而不是继续下游调度。

因此本批次只把“已知 event key 的两种 payload shape、完整 JSON tool input、
工具名一致化、SSE lifecycle、真实 Claude Code normal alias/usage 闭环、fake
外部池 SSE 链路，以及本地 provider 的 `MODEL_UNAVAILABLE` 400/404 消费合同”
标记为已验证；不会把该 fallback 扩大到 unknown event 或未经抓包确认的字段。

## 2026-09-17 后续补充：本地 provider 400/404 消费合同

为把“错误分类”与“账号轮换消费”分开验证，本轮在既有 `FakeBadRequestServer`
矩阵中增加了 HTTP 404 的模型不可用场景：

- `model_unavailable`：HTTP 400，body reason=`MODEL_UNAVAILABLE`；
- `model_unavailable_404`：HTTP 404，body reason=`MODEL_UNAVAILABLE`；
- 两者都只在存在其它可用本地凭据且请求预算允许时切换账号；
- 同一账号不重复回放；池大小为 1 时只发起一次请求；
- 普通 malformed/schema/tool/image/body-invalid 400 仍只发起一次请求；
- 该路径不进入外部池 failover，也不把 `402 quota exhausted` 误判为协议错误。

验证：

| Scope | Command | Result |
|---|---|---|
| `proto-interop-provider-retry-boundary-404` | `feature/tests/run-cargo-scoped.sh proto-interop-provider-retry-boundary-404 -- cargo test --locked bad_request_retry_matrix_bounds_real_provider_http_hits` | `1 passed / 0 failed / 0 ignored`；fake provider HTTP 命中矩阵通过 |
| `git diff --check` | `git diff --check` | passed |

该测试使用本地 fake upstream 和内存态凭据池，不需要 PostgreSQL、Redis、Claude CLI
或真实 Kiro 账号；scoped Cargo target 在 wrapper 退出前已删除。该结果是源码级
消费合同，不代表真实 Kiro 上游一定使用相同的 404 body。
