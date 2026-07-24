# Thinking Effort, Adaptive Mode, And Upstream Mapping

Status: `fixed / final-candidate-validated / monitor future CLI-or-Kiro-schema changes`

Severity: P0 protocol correctness / P1 capability compatibility

Related cases: A05, A09, C01, D01, D07, F05

## 影响面与用户可见现象

待验证问题是：Claude Code CLI 发来的 `output_config.effort` 是否在本项目中被静默限制为 `high`，`max` 是否丢失；`thinking.type=adaptive` 是否没有进入请求模型、在 body rewrite/converter 中被删除，或未映射到 Kiro 官方上游请求。任何一种情况都会让客户端选择的思考能力与实际调用不一致。

用户可见现象不一定包含固定字符串，可能表现为：同一模型的 `max` 与 `high` 行为、延迟和 usage 完全相同；显式 adaptive 请求退化为普通回答；thinking alias 只注入提示词但没有真实 thinking；主动设置与模型自动触发结果不一致；UI 开关、CLI 参数、请求字段和上游 wire body互相覆盖；响应声称有 thinking token，但请求侧没有启用相应能力。

本专题只负责请求侧能力与强度映射。thinking 正文泄漏、signed/redacted 完整性、stream block 和 `thinking_tokens` 终止安全仍由 [thinking 与签名内容安全](thinking-and-signed-content-safety.md) 负责。

## 当前事实与待验证假设

当前已经完成真实 Claude CLI 入站捕获和不依赖数据库的 converter-to-provider 最终 HTTP wire capture，但尚未完成冻结 kiro.rs 服务的完整 handler capture 与受控真实 Kiro 上游验证。已确认事实：

- Claude Code CLI 2.1.197 在 absent/`low`/`medium`/`high`/`xhigh`/`max` 六档各 15 个独立 session 中都发送 `thinking.type=adaptive`；absent 默认 `output_config.effort=high`，其余逐值原样发送。CLI 入站没有 `max -> high` clamp。
- 2026-07-21/22 C0d 继续复核又用当前安装的 Claude Code CLI 2.1.197 对 absent/`low`/`medium`/`high`/`xhigh`/`max` 六档各 5 个独立 session 重新捕获 raw Anthropic body。结果与前述一致：`thinkingVariants` 恒为 `{ "type": "adaptive" }`，absent 默认 `{ "effort": "high" }`，显式 `max` 仍是 `{ "effort": "max" }`，没有 CLI 侧 clamp；日志 SHA-256 为 `95faf7c33eee5e8a3286d18c3bb304679bd18670ceb8032f498c93a5b1f9b0e9`。见 [C0d evidence](../evidence/final-candidate-c0d-static-cli-load-ui-20260721.md)。
- 当前项目 serde/normalizer 接受上述五个显式 effort；native converter 根据模型发现 schema 生成 `additionalModelRequestFields.output_config.effort` 或 `reasoning.effort`，不会把 Anthropic `thinking` 未经合同直接复制到 Kiro wire。
- 修复前 CLI endpoint 会删除 `additionalModelRequestFields.thinking`，IDE endpoint 会在 `output_config` 存在时注入 adaptive/summarized。当前两端都只改写各自声明的 origin/profile 路径，保留已有 model-owned 字段且不发明新字段。
- 当前开发级最终出站矩阵已把 `thinking.type=adaptive + output_config.effort=max` 经 production converter、`KiroRequest`、provider 和 CLI/IDE endpoint 发送到 loopback fake upstream。CLI/IDE x stream/non-stream x 5 轮共 20 次均得到 `output_config.effort=max`，没有变为 `high`，且 fake schema 未声明 `thinking` 时最终 body 不含该字段。
- 本机官方 Kiro IDE 1.0.165 bundle 从 `ListAvailableModels.additionalModelRequestFieldsSchema` 动态读取 effort enum/default/path，并构造 `output_config` 或 `reasoning`，未观察到模型请求层强制注入 adaptive thinking wrapper。它是静态交叉证据，不是网络抓包。

仍待验证的假设：

- 完整 kiro.rs handler 的数据库能力缓存、raw body prefilter、runtime config、model alias 和请求路由是否与当前直接组合测试保持一致，仍需要冻结服务 runner 证明。
- Kiro 官方上游不一定使用与 Anthropic 相同的字段名或取值；不能未经证据把 Anthropic JSON 原样透传，也不能假设 Kiro 只支持到 `high`。
- “think hard”“ultrathink”、thinking model alias、CLI 配置、API 显式字段和模型自动行为可能走不同路径；只测一个 prompt 不能证明协议正确。

## 源码与协议链

审计必须从原始字节到实际 wire body 逐层建立字段所有权：

1. Claude Code CLI 生成的 `/cc/v1/messages` 原始 body，包括 unknown-field、stream/non-stream 和 count_tokens 差异。
2. Anthropic request types、request entry/raw prefilter、body processing、prompt steering、payload guard 和 local/external profile。
3. model alias 与 thinking-capability 解析、`BodyConversionConfig`、thinking/prompt 相关 runtime 配置及两套 UI save-refresh。
4. Anthropic-to-Kiro converter、CLI/IDE endpoint rewrite、Kiro request structs 和 JSON whitespace/single-pass helper。
5. reqwest 发往 fake/官方 Kiro endpoint 的最终 headers、Content-Length、SHA-256 和 redacted JSON body。
6. response thinking block/delta、final usage、`thinking_tokens`、请求/解析模型与上游 latency 的对应关系。

最终源码审计要列出所有包含 `thinking`、`effort`、`reasoning`、`adaptive`、`budget_tokens`、thinking alias、prompt controls 的字段、默认、迁移、开关和调用点，不能只检查用户指出的两个 key。

## 复现方法

### 原始 Claude CLI capture

使用隔离 `HOME`、`CLAUDE_CONFIG_DIR`、临时项目和临时 kiro.rs 端口，记录 Claude Code CLI 版本与脱敏原始 request body。测试显式 CLI/config/API 入口以及普通 prompt、要求深入思考、工具决策和长会话中的被动触发。不得触碰现有 `9022` 或读取真实 credential 文件。

### Fake Kiro wire capture

让冻结候选只连接 loopback raw HTTP fake upstream；按请求记录最终 path、headers、body byte SHA-256 和经过结构化脱敏的以下字段：

```json
{
  "output_config": { "effort": "<captured-or-absent>" },
  "thinking": { "type": "<captured-or-absent>" }
}
```

同时记录 Kiro 实际协议中的等价字段，避免只在 Anthropic body 中找到值却没有真正发送到上游。local CLI/IDE、external raw/normalized、stream/non-stream、messages/count_tokens 每格至少 5 轮。

### 强度与触发矩阵

- effort：absent、`low`、`medium`、`high`、`max`、大小写/空白/未知值；每格 5 轮。
- thinking：absent、disabled、enabled + budget、`adaptive`、畸形组合；每格 5 轮。
- 触发：显式字段、CLI 配置/参数、thinking alias、普通 prompt、`think hard`/高难任务、tool-use 前决策、长会话续问；主动与被动证据分开。
- 配置：prompt master ON/OFF，thinking compatibility prompt 子开关 ON/OFF，body conversion thinking controls ON/OFF，local/external profile；总开关关闭时禁止代理新增 thinking 提示，但客户端显式 thinking/output_config 仍按能力合同映射。
- 故障：不支持的模型/字段、400、429、500、partial stream 和恢复；错误不得换号放大或静默降级。

## 官方与开源交叉验证

优先级依次为：当前 Claude CLI 的真实请求、当前 kiro.rs 最终 wire capture、Kiro 官方客户端/可观察协议、公开文档。2026-07-21 重新查阅的公开资料与本地 capture 一致：Kiro CLI `/effort` 文档列出 reasoning effort levels（<https://kiro.dev/docs/cli/chat/effort/>），Kiro model docs 记录 Opus adaptive thinking 支持（<https://kiro.dev/docs/cli/models/>），AWS Bedrock Claude adaptive thinking 文档把 `effort` 放在独立 `output_config` 对象而不是 `thinking` 内（<https://docs.aws.amazon.com/bedrock/latest/userguide/claude-messages-adaptive-thinking.html>），Claude Platform 文档也把 `output_config.effort` 与 `thinking: { "type": "adaptive" }` 作为相关但分离的结构化控制（<https://platform.claude.com/docs/en/build-with-claude/effort>、<https://platform.claude.com/docs/en/build-with-claude/adaptive-thinking>）。GitHub 上的 Kiro 开源项目和逆向实现只作为交叉证据，必须记录仓库、commit、文件和访问日期，不能把第三方猜测当官方规范。

如进行真实 Kiro 上游验证，只允许冻结候选、低并发小样本、脱敏 capture 和明确 attempt 上限；先通过 fake capture，禁止把真实 key/token/body 写入报告。真实调用用于确认支持矩阵，不用于负载测试。

## 根因判定规则

只有字段级证据完整后才定根因：

- CLI 未发送：记录为客户端能力/触发差异，不归因代理丢字段。
- 入口收到但 converter 前消失：归因本项目解析或 body pipeline。
- converter 保留但 endpoint rewrite 消失：归因 Kiro 映射/重写。
- wire body 显式从 `max` 变 `high`：必须找到负责 clamp 的代码、配置或上游兼容规则。
- wire body 不含 adaptive 等价语义：必须区分“不支持并明确报错”“有文档化映射”“静默退化”。只有最后一种是必修 P0。

## 候选方案与选定原则

不能预先选定“总是透传 Anthropic 字段”或“总是注入 adaptive”。选定原则是：客户端结构化意图优先；只有 Kiro 协议有明确等价语义时映射；不支持的取值要规范拒绝或以显式、可观测、经验证的兼容策略处理，不能静默 clamp/drop；operator prompt 开关不得改变结构化能力。

若 Kiro 仅支持有限 effort 集合，兼容表必须是具名、可测试的映射，并在 usage/diagnostics 中记录请求值、映射值和原因，不记录用户 prompt。若 Kiro 支持 adaptive，则保留/生成它的条件必须由结构化协议能力控制，而不是文本 prompt 或语言增强总开关。

## 验收与性能

- 原始 CLI body、converter 中间事实和最终 Kiro wire body三方字段可逐格对账。
- `max` 不得无证据静默变 `high`；adaptive 不得无证据静默丢失。
- 主动/被动触发、alias、stream/non-stream、messages/count_tokens、local/external 每类至少 5 轮。
- thinking block/delta 与 final usage 必须是真实输出证据，普通可见 prose 不算 thinking。
- 无效/不支持组合返回规范私有信息安全错误；attempt/hit 受共享预算限制，错误后 normal 5/5 恢复。
- clean body 保持既有 byte/value identity；新增映射不得引入第二次全 body DOM round trip，1 KiB-5 MiB p95/RSS 必须满足 body 性能门禁。

## 修复与验证结果

真实 Claude CLI 入站已完成三次 `6 档 x 5 轮`，合计 90 个独立 session、90 个 Messages hit、0 unknown/invalid/timeout。结果证明 CLI 原样发送 `max` 并始终发送 adaptive；runner 修复 timeout timer 后 wall time 从错误的 72.1 秒降为 15.27 秒，最终 SHA 的第三次为 13.48 秒，字段结果不变，所有 child/port/temp 清理通过。完整数据见 [Claude CLI thinking/effort 入站证据](../evidence/thinking-effort-claude-cli-ingress-20260717.md)。

信号清理随后复现了两条 runner 缺陷：只等待 Claude process-group leader 会让后代在删除后重建 TEMP_ROOT；signal cleanup 与主循环并发时也可能在删除后进入下一 case。修复为 shutdown gate + 完整 PGID drain 后，旧 runner 曾用前后 PID 比较证明 `9022` 未变化；该做法已按当前安全合同作废。2026-07-18 当前 runner 不再读取既有 9022 listener，只按数值拒绝该端口并验证 owned fake port。重跑结果为：`node --test feature/tests/thinking-effort-claude-cli-capture-signal.test.mjs` 通过，HUP/INT/TERM 各 3 轮共 9/9 cleanup；`node feature/tests/thinking-effort-claude-cli-capture.mjs` 通过，`6 档 x 5 轮 = 30/30` session，0 count_tokens、0 unknown、0 invalid JSON，报告字段包含 `protected9022ProbeSkipped:true`。

更深源码审计新增两个 P0：`output_config` 单独启用 native reasoning 时，请求侧可能真实产生推理字段，但 stream/non-stream response pipeline 仍只按 `payload.thinking` 决定是否输出 reasoning，造成付出推理成本却吞 thinking/usage；历史 signed reasoning 当前会被降普通 content 或删除，而本机官方 Kiro bundle 已存在同模型 `reasoningContent.reasoningText{text,signature}` 透传与 signature-invalid 单次重试逻辑。两项都需要独立 API/history/fault 矩阵，不能由 Claude CLI 始终同时发送 adaptive+output_config 的 happy path 掩盖。

第一版 final-wire runner 经独立只读 review 判定为禁止执行：它继承环境，`KIRO_RS_POSTGRES_URL/KIRO_RS_REDIS_URL` 可覆盖隔离配置；把 CLI 删除 thinking、IDE 注入 adaptive 和 Opus `4.8 -> 4.7` 写成规范 oracle；只等 leader、signal 时不停止主循环；两空库、Redis prefix、fake Kiro 请求与异常 cleanup 均缺行为证明。当前 runner 已按“事实 capture、最小环境、外部 cwd、完整 PGID、stateful Redis foreign sentinel、精确 fake 协议、caller-owned DB create/drop harness”重构，并进一步完成 tracked-only source identity、有界 TERM/KILL、timer 取消、active socket ownership、direct Cargo output 拒绝和自然退出异常 fixture。纯 Node path/contract `11/11`、lifecycle `42/42` 已通过，见 [runner 安全硬化证据](../evidence/thinking-effort-kiro-wire-runner-hardening-20260717.md)。这只解除“runner 本身禁止执行”的基础设施阻断，不等于 frozen runtime 或真实 wire 已通过。

2026-07-18 当前源码重新验证进一步关闭了开发级 wire：`thinking` 名称过滤 `104/104`、`effort` 过滤 `17/17`；已有 provider 字节矩阵在 CLI/IDE、profile/no-op、compression ON/OFF、stream/non-stream、每格 5 轮下完成 80 次实际 loopback HTTP capture；新增组合测试把 Anthropic adaptive + explicit max 经 converter、`KiroRequest`、provider、CLI/IDE endpoint 发送 20 次，逐次确认最终 `output_config.effort=max`、没有 `max -> high`、没有发明未被 fake schema 声明的 `thinking`。首版组合测试因错误序列化 `ConversionResult` 编译失败，修正为与 `local_body_pipeline` 相同的 `KiroRequest` 构造后通过；另一次 `--exact` 过滤为 `running 0 tests` 已明确排除。全部 scoped target 在每轮后删除并释放预留。完整命令、红绿结果和性能边界见 [开发级最终 wire 证据](../evidence/thinking-effort-development-wire-20260718.md)。

2026-07-19 frozen real CLI gate 新增一条真实红绿链：pre-fix frozen binary `70c9741b...` 在 `bare-invoke-claude-cli.mjs` 的普通 `--model sonnet` case 下失败，Claude CLI 2.1.197 输出 `API Error: 400 model claude-sonnet-4 does not advertise a native reasoning effort field`。根因是 Claude CLI 即使普通请求也默认发送 `thinking.type=adaptive + output_config.effort=high`，而 fake model discovery 只广告无 reasoning schema 的 `claude-sonnet-4`；converter 旧逻辑在 native schema 缺失时因显式 effort 直接 400，兼容 thinking prompt fallback 没机会保留 effort。修复后 `build_additional_model_request_fields()` 对 `LegacyFallback` 无匹配、`Unknown`、`AuthoritativeAbsent` 和 `AuthoritativeInvalid` 返回 `None`，由兼容 prompt fallback 保留 `<thinking_effort>`；若兼容 prompt controls 同时禁用，仍明确报错。聚焦 Rust `reasoning-fallback-20260719-r3` 两个合同通过：无 native schema 时 effort 进入 synthetic history 且不伪造 native fields；已有 native schema 但 effort 不支持时仍不静默 remap。

同一修复后 frozen binary `e16df13a0...` 通过 `thinking-effort-kiro-wire.mjs`：CLI/IDE 两 endpoint × absent/low/medium/high/xhigh/max × 5 轮共 60 cases，inference=60，model discovery/schema=2，unknown/invalid/protocol violations=0，cleanup 全 true，report SHA-256 `439e1e69ec8407db9334a132dbd75aca0f4aa7c714441263529d615dcdf7336f`。该结果证明冻结服务在 fake Kiro schema 下不会把 `max` 截为 `high`，也不会伪造未声明字段。完整细节见 [2026-07-19 冻结 Claude CLI thinking/bare-invoke gate](../evidence/frozen-claude-cli-thinking-and-bare-invoke-20260719.md)。

runner 同时移除了对既有 9022 listener 的前后 `lsof` 探测，只在随机端口分配器中按数值排除 9022，并重新通过当前源码 path/contract `11/11` 和 lifecycle `42/42`。本轮人工调试中有一次只读 `lsof` 查看 9022 listener，按安全合同作废且不计入任何证据；后续验证不再重复。thinking frozen wire 本轮复用当前项目专属隔离 PostgreSQL/Redis，通过临时 canonical psql wrapper 连接容器内 `psql`，并在结束时删除 wrapper、两个 caller-owned database 和 artifact root。

尚未完成 API 畸形组合、完整主动/被动触发与长会话、真实 thinking delta/usage、受控真实 Kiro 小样本和最终 release candidate。因此本专题仍保持发布阻断；当前可以确认开发级 converter/provider/endpoint 与 2026-07-19 frozen fake-upstream runtime 不会把 `max` 截为 `high`，但不能把该结论外推为当前全部生产配置、真实上游和未来 CLI 版本。

2026-07-22 当前工作树补齐完整 service wire 复核：先用 Claude Code CLI 2.1.197 对 absent/`low`/`medium`/`high`/`xhigh`/`max` 六档各 5 轮重新抓原始 CLI body，30/30 均为 `thinking.type=adaptive`；absent 默认 `output_config.effort=high`，显式 `max` 保持 `max`。随后用 frozen `kiro-rs` SHA `31b8c4749201b0f7666b63a9c268c0b75e21f6c1600b18c77bf39a7c6c249c2e`、caller-owned PostgreSQL 两空库、空 Redis DB9 和临时 Node `psql` wrapper 执行 `thinking-effort-kiro-wire.mjs`，CLI/IDE × 6 efforts × 5 rounds 共 60/60 pass；inference=60，model discovery/schema=2，unknown/invalid/protocol violations=0，cleanup 全 true，report SHA-256 `df9a2fe3e07a41fd9df5cd8716ab6270d8902e3a09f1c9f0a749fff7487170a3`。

本轮当前候选事实是：Kiro final wire 没有把 `max` 截成 `high`；当上游 schema path 为 `output_config.effort` 时，wire `thinking` 变体为 `null` 是当前 mapping 合同，代码不发明未被 model discovery 广告的 `thinking` 字段。若未来官方 schema 广告 `reasoning` 或 `thinking` 形式，需由 capability path 切换和新矩阵证明，不能无条件注入。完整当前复核见 [2026-07-22 回归证据](../evidence/final-regression-rerun-20260722.md)。

## 残余风险与回滚

未来 Claude CLI 或 Kiro 上游可能增加 effort/thinking 取值、改变默认或改名。实现必须保留 unknown-field 观测与明确 fail-closed/compatibility policy，不能通过硬编码当前 CLI 版本掩盖变化。

回滚不得恢复“关闭提示词引导即关闭所有 thinking/tool 协议能力”的旧耦合，也不得用无日志的 clamp 保持表面成功。若新映射与真实上游不兼容，应回滚到上一记录 binary，并保留明确错误与本专题阻断，直到重新完成 wire 矩阵。

### 2026-07-23 最终候选复核

v0.0.117 冻结候选执行两层证据：raw Claude CLI capture 30/30 表明 Claude Code CLI 2.1.197 原生发送 `thinking: {type:"adaptive"}`，absent 默认 `output_config.effort=high`，显式 `low/medium/high/xhigh/max` 均原样；Kiro thinking wire 60/60 pass，CLI/IDE 两入口、6 effort × 5 轮均无 `max -> high`、无 invalid wire JSON、无 protocol violations、无 unknown requests。最终 wire 使用上游广告的 native schema；未广告 thinking 字段时不发明 thinking 字段，避免与官方 schema 冲突。报告路径与 SHA 见 [最终发布门禁证据](../evidence/final-release-gate-20260723.md)。

### 2026-07-25 修订：output_config 路径补 adaptive thinking

用户追加复核问题后重新确认：Claude Code CLI 当前发送的是成对结构化意图：

```json
{
  "thinking": { "type": "adaptive" },
  "output_config": { "effort": "max" }
}
```

因此当前工作树把 `KiroReasoningFieldPath::OutputConfig` 的上游 wire 改为同时包含：

```json
{
  "thinking": { "type": "adaptive" },
  "output_config": { "effort": "<low|medium|high|xhigh|max>" }
}
```

并在 `force_visible_thinking=true` 时发送：

```json
{
  "thinking": { "type": "adaptive", "display": "summarized" },
  "output_config": { "effort": "max" }
}
```

关键约束：

- 显式 `max` 不降级为 `high`。
- 这个变更只影响 native `output_config` reasoning wire，不改变模型路由、模型 alias 或下游 model 字段。
- `thinking.type=enabled/disabled` 与 `output_config` 的不兼容组合仍由 request facts fail-closed；`output_config` 只与 adaptive 或 omitted thinking 兼容。
- prompt 总开关不应决定结构化 `thinking/output_config` 是否能映射；它只控制文本提示增强。

新增 Rust 精确测试：

```text
explicit_max_output_config_effort_survives_authoritative_wire_conversion_five_rounds
native_output_config_visible_thinking_sets_summarized_display_for_five_rounds
```

本轮本地冻结候选还补了 direct live smoke：

```text
request: thinking.type=adaptive + output_config.effort=max
result: success
usage: input_tokens=13, cache_creation_input_tokens=7417, output_tokens=3
leak markers: []
```

该 smoke 证明当前代理不会把 `adaptive + max` 在入口误拒绝或降级；精确 wire 字段由上述 converter 测试与 C0 全量 Rust gate 覆盖。证据见 [Release C0 and Claude CLI smoke evidence 2026-07-25](../evidence/release-c0-cli-smoke-20260725.md)。

### 2026-07-25 最终候选验证补充

最终候选：

```text
kiro-rs sha256=25ea01fb741bdffb103fa95397f0fb29b60c8bffee9267741f563f388ae237a4
local service=existing 127.0.0.1:9022
Claude Code CLI=2.1.197
```

当前权威结论：

1. 对 native `output_config` reasoning path，最终 Kiro wire 必须携带：

   ```json
   {
     "thinking": {"type": "adaptive"},
     "output_config": {"effort": "max"}
   }
   ```

   旧 fake-schema 结论“未声明 thinking 时不发明字段”只适用于当时的旧模型发现夹具，不再作为当前 output_config path 的产品合同。

2. 显式 `output_config.effort=max` 不会被截断成 `high`。修复后的 provider body-capture 测试在 CLI/IDE、stream/non-stream、5 轮中验证 final wire 同时包含 `thinking.adaptive` 与 `output_config.max`。

3. Native Kiro adaptive thinking 不携带 Anthropic-only `budget_tokens`。`budget_tokens` 属于 Anthropic `thinking.enabled` 输入形态；Kiro output_config path 使用 `thinking.type=adaptive` + `output_config.effort`。

4. Claude Code CLI 2.1.197 的 `--effort medium/high/xhigh/max` 在本轮 ingress capture 里仍然不可直接从 visible JSON 字段区分：CLI 发送 `thinking.enabled + budget_tokens=31999`，没有 `output_config` 或 top-level effort。因此代理不能把 `budget_tokens=31999` 盲目推断成 `max`，否则会错误升级其他 effort 档。

5. prompt steering 总开关不再作为结构化 thinking/output_config 能力的唯一开关。文本语言增强、任务质量、tool_choice prompt、thinking prompt、chunked tool prompt 应由子开关控制；客户端显式结构化字段仍按能力合同解析/映射。

最终验证：

```text
cargo fmt --check: passed
cargo test --bin kiro-rs -- --test-threads=2: 1784 passed / 0 failed / 6 ignored
node feature/tests/check-feature-docs.mjs: passed
node --test feature/tests/*.test.mjs: 261 passed / 22 skipped / 0 failed
inventory-build-artifacts --gate: passed
direct thinking/adaptive/max: real thinking block=1, thinking_delta=1, thinking_tokens=4, success
Claude CLI --effort max: thinking block/delta present, text final-cli-think-ok, usage non-zero
load/chaos L3/L4/L5: passed
```

仍需持续监控的不是当前修复，而是未来 CLI/Kiro schema 变化：如果官方上游把字段从 `output_config` 改成 `reasoning` 或新增 effort 枚举，必须通过 model discovery capability 和 wire capture 更新映射，不允许无证据硬编码。
