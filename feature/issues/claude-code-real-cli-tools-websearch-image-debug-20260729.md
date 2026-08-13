# Claude Code 真实调用：tools / WebSearch / image 调试初步分析

> 2026-07-29 修正：本文记录的是一次 external-pool-heavy 的历史排查，不能作为当前“只用本地账号 7/8 调试”的权威结论。当前本地账号专项证据见 [Claude Code local-account WebSearch/tools/image analysis - 2026-07-29](claude-code-local-accounts-websearch-tools-image-analysis-20260729.md)。本地账号 7/8 当前均可用 `claude-sonnet-4.5` 返回成功；旧文中“本地 Kiro social 凭据不可用 / tools 尚未成功闭环 / 图片缺真实证据”等判断已被后续本地账号测试修正或细化。

Status: `historical-external-pool-pass / superseded-for-local-account-diagnosis / retained-for-misdiagnosis-history`

Severity: P0/P1（历史排查严重级别）。本文下方记录的“本地凭据全禁用、工具/图片未闭环”等判断只描述当次 external-pool-heavy 环境；当前本地账号诊断已由上方修正摘要和本地账号专项文档覆盖。

Last observed: 2026-07-29 Asia/Shanghai

## 2026-07-29 本地账号修正摘要

本节是对本文历史外部池结论的修正。当前用户要求的是本地账号真实调用，不再使用外部池判定三类问题。

- 当前 `ccman cc current` 为 `local-kiro-rs-9022-current`，URL `http://127.0.0.1:9022/cc`；Claude CLI 版本为 `2.1.220 (Claude Code)`；服务监听 `127.0.0.1:9022`。
- 运行时 external pools 已为本轮本地账号测试关闭；关闭前备份为 `/tmp/kiro-runtime-before-local-only-9022.json`。
- 本地可用账号不是旧文中的“全部 disabled”。当前确认可用的是 credential `7` 和 credential `8`，均为 `social` / `KIRO FREE` / not disabled，且测试端点均可调用 `claude-sonnet-4.5` 返回 `local-ok`。
- 模型层必须按 usage/log 中的 upstream model 判定。`claude-sonnet-4.5`、`sonnet`、`claude-sonnet-4.6` 当前都能成功，但 `sonnet` 和 `4.6` 实际 upstream 均解析/回退到 `claude-sonnet-4.5`；响应体 echo 的 `model` 不能单独作为真实上游模型依据。
- WebSearch 不是“完全不可用”：纯 Anthropic native `web_search_20250305` 单工具请求成功，request `req_01yPTQ3uUhHq89z8FGZQycZ9` 返回 `server_tool_use` 和 `web_search_tool_result`。这段描述是 2026-07-29 的历史状态；2026-07-31 的本地账号 focused 验证已经证明 `web_search_YYYYMMDD` 泛化、mixed native server-side 执行和当前 Claude CLI `WebSearch` 都能工作，当前权威结论见 [Claude Code local-account WebSearch/tools/image analysis - 2026-07-29](claude-code-local-accounts-websearch-tools-image-analysis-20260729.md)。
- tools 不是最小路径解析全坏：直接 forced tool request `req_017oazMg5ptjHU64BX1CSYAW` 成功返回 `tool_use echo_value`；真实 Claude CLI `Bash` 与 `Read` 工具也能完成 tool_use/tool_result 闭环。剩余风险集中在工具名规范化/反向映射、tool_choice、长历史 pairing、schema 边界和 prefill 丢弃后的调试可观测性。
- 图片不是合法图普遍失败：合法 inline base64 PNG 连续三次成功，声明 `image/jpeg` 但实际 PNG 的请求也被修正 media type 后成功；真实 Claude CLI `Read` 图片返回 `Red`。坏图/伪图会在本地 `handler_preflight/local_body_prepare` 阶段 400 拒绝，这是正确行为。间歇性识别问题更可能来自坏/截断字节、remote/file source materialization、大小/并发/deadline、payload guard 裁剪或 tool_result image placeholder 边界。

当前排障应以本地账号专项文档为准；本文下方 external pool 记录只保留为历史证据，用于解释早期误判如何发生。

## 历史运行态（external-pool-heavy pass）

- `ccman cc current` 已切到 `local-kiro-rs-9022-current`，URL 为 `http://127.0.0.1:9022/cc`。
- Claude Code CLI 版本：`2.1.220 (Claude Code)`。
- 本地服务：`kiro-rs` PID `59668` 监听 `127.0.0.1:9022`。
- 当前冻结二进制 SHA-256：`bd45abee44102e20176d1985fb10c2663f52728b8aae38a6a160ec98caae3f9d`。
- 本地 Postgres `public.credentials` 中 6 个 social 凭据均为 disabled，runtime reason 均为 `TemporarilySuspended`。
- 临时 external pool `#1 ccman-mygoband-kkk-debug` 当前 enabled、`requestBodyMode=normalized`、`streamResponseMode=event_passthrough`、`autoDisablePolicy=disabled`、status endpoint 显示 `dispatchable=true`。
- external pool model mapping rules 暂未包含 `claude-sonnet-5 -> claude-sonnet-5`，但 `modelMappingRequireMatch=false`，且当前远端对 `claude-sonnet-5` 直连最小请求可成功。

## 当时已确认现象

### 1. 本地 Kiro social 凭据不可用

已对多个 DB 中 enabled 候选做真实 `/cc/v1/messages` 调用，结果均为上游 403 `temporarily_suspended`，服务自动禁用。当前所有本地凭据 `disabled=true`，后续任何本地路线失败都可能先被这个环境问题放大。

该结论只说明当前本机 credentials 不可用于成功路径；不能据此判断 tools / WebSearch / image 的产品逻辑已坏或已好。

### 2. 无工具请求可经 external fallback 成功

代表请求：

- request id: `req_01QGjHz6WT9Zyub2xXGCo3YH`
- endpoint: `/cc/v1/messages`
- requested model: `sonnet`
- resolved upstream model: `claude-sonnet-5`
- status: success
- routeKind: `external_pool`
- routeSubtype: `external_fallback_preflight`
- fallbackReason: `local_all_disabled`
- externalPoolId: `1`
- external outbound model: `claude-sonnet-5`
- inferenceAttempts: `externalAttempts=1`, `localAttempts=0`

这证明服务、`ccman`、API key、external fallback 的最小无工具路径可以连通。

### 3. 真实 Claude CLI 默认请求带 10 个工具并失败

执行过真实 CLI：

```bash
claude --print --verbose --output-format=stream-json --include-partial-messages --model sonnet 'Reply with exactly: cli-pong'
```

结果：CLI 超时前持续收到 `api_retry`，`error_status=503`，最多观察到 attempt 9。

服务端对该请求的关键指纹：

- endpoint: `/cc/v1/messages`
- model: `claude-sonnet-5`
- stream: `true`
- message_count: `2`
- system_message_count: `3`
- tool_definition_count: `10`
- current_tool_count: `10`
- request_bytes: about `81792`
- converter 日志：`检测到末尾非 user 消息（prefill），静默丢弃`
- converter 日志：`工具名称映射: 10 个超长名称已缩短`
- converter 日志：`skipping unsupported additionalModelRequestFields for model claude-sonnet-5`

代表 usage record：

- request id: `req_01dhNKDS7yUL9MsVmrQYi4iW`
- routeKind: `local_credential`
- routeSubtype: `local_rescue_after_external`
- stream: `true`
- localPreflight.reason: `external_error`
- externalPoolId: `1`
- externalStatus: `403`
- externalErrorType: `auth_error`
- externalResponseErrorType: `permission_error`
- externalError: `external upstream rejected account authorization`
- inferenceAttempts: `externalAttempts=1`, `localAttempts=0`
- 最终 public status: `503`

这说明完整 Claude CLI payload 至少进入过 external pool，但外部供应商对该完整请求返回了授权类 403；随后服务 rescue 到本地，而本地凭据全禁用，最终对 CLI 表现为 503/retry。

### 4. 最小 tools 请求经过服务时没有进入 external pool

带 1 个简单 Anthropic tool 的 `/cc/v1/messages` 通过本服务请求失败：

- request id: `req_01gr7zHZYakYsYBETQanR2bf`
- endpoint: `/cc/v1/messages`
- requested model: `sonnet`
- resolved upstream model: `claude-sonnet-5`
- tool_definition_count: `1`
- current_tool_count: `1`
- status: error
- routeKind: `local_credential`
- routeSubtype: `local_error_no_fallback`
- error: `所有账号均已禁用（0/6）`
- inferenceAttempts: `externalAttempts=0`, `localAttempts=0`
- log: `fresh local state permits fallback but no external pool is ready for this route reason`

同一临时 external provider 直连最小 Anthropic tool 请求可以返回 `tool_use`，所以“最小工具请求失败”目前更像服务内 fallback readiness/eligibility 问题，而不是远端完全不支持 tools。

### 5. WebSearch 目前只覆盖原生单工具探测

当前代码的 WebSearch 探测条件是：

- `tools.len() == 1`
- `tool.name == "web_search"`
- `tool.type == "web_search_20250305"`

Claude Code CLI 请求中出现的是客户端工具 `WebSearch`，且通常和 `Bash`、`Read`、`Write`、`WebFetch` 等 10 个工具一起发送。现有 `has_web_search_tool` 逻辑不会识别这种形态，测试也明确断言“多个工具时不应该被识别为纯 websearch 请求”。

因此“WebSearch 完全不支持”很可能不是旧的 `web_search_20250305` native MCP 问题，而是 Claude Code 客户端 `WebSearch` tool 的协议形态没有被单独支持或没有被外部池透传验证。

### 6. 图片路径尚缺本轮真实成功/失败证据

已有旧专题说明：坏图/伪图会导致上游 `IMAGE_FORMAT_UNSUPPORTED`，当前源码已有轻量结构校验和 media_type 修正。但本轮还没有完成以下真实调用：

- 合法 PNG/JPEG 经 `/cc/v1/messages` 成功识别；
- Claude Code CLI 图片输入或程序化图片 block 成功；
- tool_result 中图片 block 的转换/透传成功；
- 远程 URL / file source 在 safe image processing 下成功 materialize。

所以“图片识别不稳定”当前只能列为待复现问题，不能直接归因到已知坏图问题。

## 可能原因树

### A. 环境凭据问题是确定存在的阻塞

本地 Kiro social credentials 全部 disabled 且 runtime reason 为 `TemporarilySuspended`。任何没有被 external pool 接住的请求都会直接变成 `local_error_no_fallback` 或 rescue 后 503。

影响：

- 会掩盖 tools / image / WebSearch 本身的问题；
- 会让 Claude CLI 因 503 自动重试，制造大量相似失败；
- 会让“解析异常”看起来像模型失败，但实际可能根本没发到可用上游。

处理方向：

- 继续真实调用时优先走 external pool 或找真正可用的 Kiro 凭据；
- 记录 usage 时必须区分 `localAttempts=0`、`externalAttempts=0/1`。

### B. 带 tools 的 parsed fallback readiness 可能误判 external pool 不可用

最小 tool 请求的关键矛盾是：

- external pool status endpoint 显示 pool #1 `dispatchable=true`；
- pool #1 `supportedModels=[]`，按规则应匹配任意模型；
- pool #1 `requestBodyMode=normalized`，而 parsed fallback 对 normalized body 应该可用；
- 但请求 `req_01gr7zHZYakYsYBETQanR2bf` 在 local all disabled 后记录 `externalAttempts=0`，并打出 “no external pool is ready”。

高风险代码点：

- [src/anthropic/handlers.rs](../../src/anthropic/handlers.rs): `build_external_fallback_context(...)` 的 `requires_normalized_body` 当前由 `request_history_contaminated` 传入，不一定等价于“带 tools 必须 normalized”。
- [src/anthropic/handlers.rs](../../src/anthropic/handlers.rs): `ExternalFallbackContext::has_eligible_external_pool_for_model(...)` 通过 `external_fallback_body_mode_filter(requires_normalized_body)` 决定 body mode filter。
- [src/anthropic/handlers.rs](../../src/anthropic/handlers.rs): `fallback_after_local_error_outcome_with_diagnostics(...)` 在 fresh local state 允许 fallback 后，又调用 `external_pool_ready_for_route_reason(...)`，这里返回 false 就会完全跳过 external。
- [src/external_pool.rs](../../src/external_pool.rs): `has_cached_eligible_pool_for_body_mode_and_model(...)` 依赖 `cached_static_pool_snapshot_for_local_route()`；如果快照为空、过期刷新 race、或 eligibility 字段没有及时反映 DB，会出现“status 可调度但本地热路径认为不可用”。
- [src/external_pool.rs](../../src/external_pool.rs): `ExternalRouteRequest::model_candidates_for_support()` 只包含 payload model 和 model_hint，不包含 `upstream_model`；当 external pool 配了非空 `supportedModels` 时，alias 后模型可能被错误排除。本次 pool `supportedModels=[]`，所以这不是当前唯一解释，但仍是相邻风险。

当前优先级最高的假设：

1. cached static pool snapshot 在某些请求阶段为空/过期/未刷新，热路径 fail-closed，导致 `externalAttempts=0`。
2. external pool 在一次 403 `auth_error` 后写入了某种 cooldown/runtime 状态，status endpoint 与 parsed fallback readiness 读取的缓存视图不一致。
3. model/body mode eligibility 对 parsed tool 请求使用了错误候选，尤其是 alias 后 `sonnet -> claude-sonnet-5` 与 pool mapping / support candidates 的组合。
4. 最小 tool 请求虽然 `requires_normalized_body=false` 时理论上 body filter 为 `None`，但后续 route/request preparation 仍可能要求 normalized；readiness 诊断不足导致看不出实际过滤原因。

### C. 完整 Claude CLI payload 对临时外部池触发 403

完整 CLI 请求比最小 tool 请求复杂得多：

- stream=true；
- 10 个工具；
- 3 个 system messages；
- 约 80KB 原始请求；
- 有尾部 assistant prefill；
- 有 output_config / 其他 Claude Code 字段；
- 工具名很长，需要本地 Kiro 路径缩短映射。

外部 provider 对最小 no-tool 和最小 tool 直连可用，不代表它接受完整 Claude Code stream-json payload。可能原因：

1. provider 对 `claude-sonnet-5` 的工具/流式/大 payload 组合做了权限限制，返回 403。
2. external normalized body 保留了 Claude Code 原始未建模字段，provider 将其判为无权限或不支持。
3. `event_passthrough` 对该 provider 的 SSE 形态不匹配，但当前失败发生在 HTTP 403，尚未进入 SSE 解码阶段。
4. 尾部 assistant prefill 在 local Kiro conversion 被丢弃，但 external normalized 路径可能仍保留 typed payload 中的 assistant prefill；需要抓取脱敏 body hash/shape 确认。
5. external pool mapping 没有显式 `claude-sonnet-5` 规则，虽然当前 fallback 会保留 processed model，但加规则可减少歧义。

### D. WebSearch 不是“原生 WebSearch MCP”单一问题

旧文档 [websearch-normalized-external-fallback-preflight.md](websearch-normalized-external-fallback-preflight.md) 解决的是 Anthropic 原生 server-side WebSearch：

```json
{"type":"web_search_20250305","name":"web_search"}
```

这次 Claude CLI 暴露的 `WebSearch` 更像普通客户端工具，由 Claude Code 本地执行搜索并回传 tool_result。当前代码会把它当普通 tool 处理，除非后续模型返回 tool_use 并由 CLI 执行。由于当前工具请求还没有成功进入可用上游，无法验证 WebSearch tool_use/tool_result 闭环。

可能原因：

1. 当前 `has_web_search_tool` 只覆盖原生单工具分支，对 Claude Code `WebSearch` 不做专门识别，这是设计限制，不是 regression。
2. 如果产品目标是“代理端提供搜索能力”，需要新增 Claude Code `WebSearch` 客户端工具到 server-side MCP/WebSearch 的桥接；这会改变 Claude Code 工具协议语义，风险较高。
3. 如果产品目标是“透传 Claude Code 自己的 WebSearch 工具”，则首要问题仍是普通 tools external/local 成功路径，而不是 `websearch.rs` native detector。

### E. 图片不稳定需要拆成 4 类复现

可能原因不能只看旧的 `IMAGE_FORMAT_UNSUPPORTED`：

1. 输入坏图/伪图：应在本地被明确拒绝或由上游 400，旧专题已有结构校验。
2. 合法 inline base64：应由 `body_processing` 修正 media_type，再由 `converter/content.rs` 生成 Kiro image 或 external normalized body。
3. remote/file source：safe mode 会下载/materialize，受 DNS/redirect/大小/并发/45s deadline 影响。
4. tool_result image：路径在 `converter/content.rs` 单独处理，可能和普通 user image 行为不同。

源码关注点：

- [src/anthropic/body_processing.rs](../../src/anthropic/body_processing.rs): safe/light image processing、remote source materialization、base64 media_type normalization。
- [src/anthropic/converter/content.rs](../../src/anthropic/converter/content.rs): inline image base64 解码、magic byte 检测、轻量结构校验、tool_result image 提取。
- [feature/issues/08-image-format-unsupported-400.md](08-image-format-unsupported-400.md): 旧的坏图/伪图专题，仍需真实 CLI image gate 补齐。

## 根因

本文的根因结论是历史结论，只适用于当时 external-pool-heavy 的排查环境：

- 当时本地凭据不可用或被判定不可用，导致很多请求先被环境状态放大。
- external pool 对完整 Claude Code payload 返回授权类 403，掩盖了本地 tools/WebSearch/image 的真实行为。
- 带 tools 的 parsed fallback readiness 存在“status endpoint 可调度但热路径认为无 external pool ready”的诊断缺口。
- WebSearch 分析只覆盖 native 单工具分支，不覆盖 Claude Code CLI `WebSearch` 客户端工具形态。
- 图片路径在本文中缺少本地账号真实成功/失败闭环，因此本文不能作为当前图片根因权威。

当前本地账号根因以 [Claude Code local-account WebSearch/tools/image analysis - 2026-07-29](claude-code-local-accounts-websearch-tools-image-analysis-20260729.md) 为准。

## 复现

本文历史复现分两类：

- External-pool-heavy 历史复现：使用当时的 external pool、完整 Claude CLI stream-json payload、`sonnet -> claude-sonnet-5` 路径，观察 403/503/retry/fallback readiness 行为。
- 当前不推荐复现：不要再用外部池失败来判断本地账号 7/8 的 tools/WebSearch/image 行为。

如果需要当前复现，应改用本地账号专项文档中的 local-only cases：

- credential `7` / `8`;
- model `claude-sonnet-4.5`;
- external pools disabled;
- direct pure native WebSearch、mixed WebSearch、direct tool、CLI Bash/Read、valid/invalid image controls.

## 方案

本文不再提供当前修复方案。历史方案只保留为误判来源和 fallback/external-pool 诊断参考。

当前方案入口：

- local-account WebSearch/tools/image 修复方向见 [local-account analysis](claude-code-local-accounts-websearch-tools-image-analysis-20260729.md)。
- native WebSearch fallback 见 [websearch-normalized-external-fallback-preflight.md](websearch-normalized-external-fallback-preflight.md)。
- WebSearch/MCP correctness/privacy 见 [websearch-mcp-protocol-usage-and-privacy.md](websearch-mcp-protocol-usage-and-privacy.md)。
- 图片坏图/伪图见 [08-image-format-unsupported-400.md](08-image-format-unsupported-400.md)。

## 残余风险与回滚

Residual risk:

- 本文仍包含大量 external-pool-heavy 历史事实，后续读者可能误把它当成当前 local-only 结论。
- 如果不保留顶部 supersession note，容易再次把“外部池授权失败”误判成“本地账号 tools/image/WebSearch 不可用”。

Rollback boundary:

- 不得删除历史记录中的 request id 和环境事实；它们解释了早期误判来源。
- 不得把本文状态改回当前权威，除非重新跑同一范围的本地账号验证并更新所有关联文档。

## 当前不能下的结论

- 不能说“tools 解析一定坏了”：目前最小 tool 请求已经被解析到 `current_tool_count=1`，真正失败发生在 fallback 没进入 external pool。
- 不能说“external provider 不支持 tools”：直连最小 Anthropic tool 请求已返回 `tool_use`；但完整 Claude CLI payload 仍可能被该 provider 拒绝。
- 不能说“WebSearch 已实现”：当前实现只覆盖原生 `web_search_20250305` 单工具，Claude Code `WebSearch` 仍未完成真实闭环。
- 不能说“图片问题已修复”：旧专题覆盖坏图/伪图结构校验，本轮还没有合法图片真实成功证据。

## 下一步验证计划

1. 给 external fallback readiness 增加临时脱敏诊断，至少记录 body_mode_filter、model candidates、cached static snapshot 命中情况、eligible pool count、被过滤原因。
2. 用最小 tool 请求复现 `externalAttempts=0`，确认为什么 pool #1 被认为 not ready。
3. 修复或绕过 readiness 后，先跑 C1.7 最小 tool `/cc/v1/messages`，必须返回有效 `tool_use`。
4. 重新跑真实 Claude CLI C2.1/C2.4：
   - `--print --output-format=stream-json`
   - 普通 exact response；
   - Bash tool `printf tool-ok`；
   - 记录 tool_use count、tool_result count、final usage。
5. WebSearch 分两路验证：
   - 原生 `web_search_20250305` server-side body 仍按旧专题走 MCP/fallback；
   - Claude Code CLI `WebSearch` 作为客户端工具能否 tool_use/tool_result 闭环。
6. 图片分四路验证：
   - 合法 inline PNG；
   - media_type 错但字节合法的 PNG/JPEG；
   - 坏图/伪图本地明确拒绝；
   - Claude CLI 或程序化 tool_result image。
7. 若继续使用临时 external pool，应先补一条显式 model mapping：`claude-sonnet-5 -> claude-sonnet-5`，减少定位噪音。

## 验收标准

- 本地凭据全 disabled 时，无工具和有工具请求都能按配置进入 external fallback，usage 中 `routeKind=external_pool` 且 `externalAttempts>=1`。
- 最小 tool 请求返回规范 Anthropic `tool_use`，Claude CLI 能执行 Bash tool 并回传 `tool_result`。
- Claude CLI 下游不看到 internal terms，例如 credential、fallback pool、upstream pool、scheduler。
- WebSearch 明确区分 native server-side WebSearch 与 Claude Code client-side `WebSearch`，两者各有独立通过/失败证据。
- 图片合法样本真实 200，坏图/伪图本地或上游错误分类稳定，不跨账号重试，不泄漏原始上游私有错误。
