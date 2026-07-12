# Runtime usage/error follow-up（2026-07-13）

本文记录 `docs/feature/` 与 `tmp/analysis-usage-llm-errors/` 复核后的当前待办、已修复项、错误提示策略，以及 `/ha`、`/cc` usage 异常样本的解释。口径为当前本地工作区，不代表已发布到生产。

## 1. 本轮已确认并处理

| 项 | 结论 | 当前处理 |
|---|---|---|
| 空工具 `description` | 真实问题，旧生产窗口最大根因 | 已有修复：空/空白 description 归一为非空占位；保留发布后观察 |
| `input_schema:null` | 真实问题 | 已有修复：入口反序列化把 explicit null 归一为空 map |
| schema property key 非法 | 真实兼容问题 | 已有默认可逆 sanitize；本轮修正 diagnostics 误报，避免把 `$defs`/`patternProperties`/`dependentSchemas` map key 算作 property key |
| tool name 合法化与响应还原 | 真实兼容问题 | 源码已有 request-local 映射；文档已补状态 |
| `/cc` / `/ha` usage 展示输入异常过大 | 真实 reported usage policy 漏应用问题 | 已修复：`sample-max` 会始终压低展示 input；只有已有 cache-read 证据时才把差额转入 `cache_read_input_tokens`，没有证据时不伪造 cache read |
| `prompt is too long` | 真实外部池上限问题 | 已修复分类与 public message；parsed external route 不再把 request input tokens 恒置 0；新增 `externalPoolMaxInputTokens` 预检，默认 1,000,000，超过时本地拒绝、不发外部池 |
| `messageStatus` 丢失 | 真实观测盲区 | 已解析 `messageStatus`，usage latencyTrace 写入 `upstreamMessageStatus`、`sawUpstreamCompleted`、`stopReasonSource` |
| “开场白后 end_turn 空转” | 模型/CLI 时序行为，代理只能观测 | 新增 `suspectedIntentPreambleEndTurn` usage-only 诊断，不改变 SSE |
| 错误归一化过度 | 真实下游体验问题 | 允许 Kiro 官方上游结构化 JSON message 经敏感词过滤后透出；外部池、本地调度、账号和内部错误继续脱敏 |
| 坏图/伪图 `IMAGE_FORMAT_UNSUPPORTED` | 客户端输入错误，但代理可提前拦截 | 新增 base64 图片轻量结构校验：必须可解码并符合 PNG/JPEG/GIF/WebP 最低结构；伪图/截断 PNG 本地拒绝 |

## 2. 仍然待办

| 优先级 | 待办 | 原因 | 验收口径 |
|---|---|---|---|
| P0 | 本地真实服务回归 `/cc/v1` 工具/schema、错误提示、usage trace | ✅ 本轮完成基础真实调用；后续发布前可按同命令复跑 | 临时端口 release 服务；direct SSE + Claude CLI；不碰生产 9022 |
| P0 | C0 gate：fmt、diff check、cargo check/test、release build、两套 UI build | ✅ 本轮完成 | 全部通过，记录命令与结果 |
| P1 | stream idle timeout 首输出前重试的响应提交重构 | 当前 stream 会先向下游发送 `message_start`，再读取上游；小改硬重试会造成下游已提交后换请求，存在重复/乱序风险 | 独立设计“延迟 initial events/响应提交”或等价协议安全机制；在此之前保持超时错误可观测，不做 unsafe retry |
| P2 | 发布后生产 recurrence check | tmp 是旧版本窗口；必须看新版本是否复发 | 查询新 app/revision 的 usage/error/debug |

## 3. 错误提示策略

### Kiro 官方上游

允许把官方上游结构化 JSON 里的 message/reason/code 透给下游，但必须先做安全过滤：

- 允许：`Invalid tool use format. (reason: REQUEST_BODY_INVALID)`、`Could not process image (reason: IMAGE_FORMAT_UNSUPPORTED)` 这类模型/请求级错误。
- 拦截：包含 `credential`、`bearer token`、`api key`、`client secret`、`refresh token`、`access token`、`账号 #`、`凭据`、`外部池`、`scheduler`、`调度` 等内部或敏感词的 message。
- 截断：public upstream message 最多 1024 bytes。

### 外部池

外部池继续不透 raw message。原因：

- 外部池不是官方可信上游，可能返回广告、推广、HTML、非协议 JSON、第三方请求 ID 或内部池名。
- 当前策略：下游只拿 public message + error id；raw message 只留 usage/internal logs。
- 对 `prompt is too long` 这种常见且可归类错误，下游 public message 改为上下文过长语义，但不暴露外部池原文和第三方 request id。
- 对已知超长请求，`externalPoolMaxInputTokens` 会在转发前本地拒绝，避免注定失败的外部池往返。

### 本地调度/账号/内部错误

继续归一化，不把 credential、fallback、scheduler、lease、capacity snapshot、外部池名等内部词给下游。

## 4. `/ha` 与 `/cc` 样本：为什么“上报输入”异常大

样本 A（`/ha`）：

- endpoint：`/ha`
- 请求模型：`claude-opus-4-8` → alias 到 `claude-opus-4.8`
- `max_tokens=64,000`
- usage：`上报输入=317,054`、`cache write=28,779`、`output=1`、`内部成本输入=345,833`
- payload guard breakdown：`totalBytes=3,831,046`，`historyBytes=3,731,585`，`historyEntries=556`，`historyImagesBytes=3,142,500`，`currentToolsBytes=98,854`，`currentToolCount=53`
- guard：`maxBytes=0`，`flattenedHistoryToolUses=222`、`textifiedHistoryToolResults=222`，未 trim/drop

样本 B（`/cc`）：

- endpoint：`/cc`
- 请求模型：`claude-opus-4-6` → alias 到 `claude-opus-4.6`
- `max_tokens=16,384`
- usage：`上报输入=104,005`、`cache write=5,266`、`output=412`
- 用户说明：payload guard 配置与样本 A 相同。

结论：

1. 这不是 schema key 清洗/映射导致的 token 膨胀。
2. 请求体本身极大：样本 A 是 3.8MB，其中 3.1MB 是历史图片，另有 556 条历史与 53 个当前工具定义。`src/token.rs` 对 base64 图片按图片尺寸/默认图片 token 估算，不把 3.1MB base64 全量当文本；因此 317k input tokens 更可能来自长历史、多图片、多工具的真实上下文规模，而不是 base64 文本双算。
3. `guard.maxBytes=0` 不是简单等价于“payload guard 总开关关闭”。当前默认 `payloadGuardMode=on_too_long` 时，首发请求使用 `maxBytes=0`（不预裁剪），只有上游返回 payload/context too-long 后才用实际 `payloadGuardMaxBytes` 裁剪并重试。样本是成功请求，所以没有触发第二阶段裁剪。
4. 真正的 reported usage bug 在策略应用：`/cc` 与 `/ha` 默认策略是 `input sample-max 96`。旧实现为了避免首轮伪造 cache read，在 `moveDeltaToCacheRead=true` 且没有 cache-read 证据时，直接跳过 input sampling，导致 `317,054` / `104,005` 这种本地估算值被当成“上报输入”展示给下游。
5. 当前修复后：只要路径策略启用 `sample-max`，展示 input 都会被压到配置上限内；只有响应已有 cache-read 证据时，少掉的 input delta 才会转入 `cache_read_input_tokens`。没有 cache-read 证据时，`cache_read_input_tokens` 保持 0，不伪造缓存读取。
6. 原始大输入不会丢：usage 诊断字段仍保留本地估算 / raw usage。也就是说页面应该能同时表达“本地估算输入很大”和“返回给下游的展示 input 已按策略压低”。
7. `内部成本输入 = 上报输入 + cache write` 是本系统历史兼容/费用估算口径，不是 Anthropic/Kiro 响应中的独立字段。样本 A 中 `317,054 + 28,779 = 345,833`，与页面一致；修复后该公式仍成立，但 `上报输入` 不应再是 317,054。
8. `output=1` 只表示该轮最终上报的输出 token 很少；`max_tokens=64,000` 是上限，不代表模型一定输出 64k。

验证重点：

- 本地真实服务回归要覆盖 `/cc` 与 `/ha`：构造长历史/工具定义请求，在没有 cache-read 证据时确认 final `message_delta.usage.input_tokens <= 96` 且 `cache_read_input_tokens=0`。
- 再覆盖已有 cache-read 证据的情况：确认 input delta 只在有读证据时进入 cache read。
- UI 文案继续强调“本地估算输入仅用于诊断；返回给客户端的用量以展示字段为准”。新 UI 使用“展示输入”，旧 admin-ui 使用“上报输入”，两者语义应在后续统一。
- 长上下文不会因 schema key 映射产生跨请求内存增长：映射为 request-local map，随请求释放；高内存风险主要来自大请求体、图片历史、工具定义和 usage/payload diagnostics，而不是 key hash 本身。

## 5. 性能/内存判断

schema key 清洗/映射的成本：

- 只对不合法 property key 建映射；合法 key 不清洗、不建映射。
- 映射生命周期是单请求，不写 Redis、不跨会话共享，避免多会话/多工具串映射。
- 内存量级与“工具 schema 中非法 key 数”成正比，通常远小于请求体、图片和历史消息。
- CPU 成本来自遍历工具 schema 与少量 hash/JSON key 重写；相比 3MB+ 长上下文、图片处理、上游网络延迟，通常不是瓶颈。

更值得关注的资源风险：

- 大历史图片和长上下文会提高 body parse、payload guard、token estimate、usage diagnostics 的 CPU/RSS。
- 外部池 max-input 预检不增加额外长上下文扫描：它复用已存在的 `request_input_tokens`，转发前只是一次整数比较。
- usage sampling 修复是 O(1) 数字改写，不随上下文长度增长；不会新增 Redis、不会新增跨会话映射状态。
- 图片轻量结构校验只扫描当前内联图片字节，不做完整像素解码；相比上游网络和大请求体解析，成本可控，但真实长上下文并发仍需看 RSS/FD。
- 高并发长上下文时，必须用临时 release 服务做低并发到小规模 burst 的 RSS/FD 观测。
- usage/payload diagnostics 不应在 success 路径无条件持久化大 JSON；当前仅在修改/超阈值/诊断需要时持久化，仍需回归确认。

## 6. 验证清单

- [x] `cargo test tool_schema_key_diagnostics_ignore_schema_map_keys`
- [x] `cargo test prompt_too_long_error_maps_to_input_length_message`
- [x] `cargo test official_kiro_upstream_400_message_is_exposed_without_internal_prefix`
- [x] `cargo test malformed_upstream_error_exposes_safe_official_message`
- [x] `cargo test assistant_message_status_marks_upstream_completion_without_changing_sse_shape`
- [x] `cargo test end_turn_with_tools_and_short_visible_text_sets_intent_preamble_diagnostic`
- [x] `cargo test reported_usage -- --nocapture`
- [x] `cargo test base64_image -- --nocapture`
- [x] `cargo test test_process_message_content -- --nocapture`
- [x] `cargo test external_pool_max_input_preflight -- --nocapture`
- [x] `cargo test external_public_error_reports_prompt_too_long -- --nocapture`
- [x] `cargo fmt --check`
- [x] `git diff --check`
- [x] `cargo check --all-targets`
- [x] `cargo test --all-targets`
- [x] `cargo build --release`
- [x] `admin-ui` production build
- [x] `ui` production build
- [x] 临时本地服务 direct `/cc/v1/messages`、`/ha/v1/messages`、`/v1/messages` 真实调用
- [x] Claude CLI `--output-format=stream-json` 普通/工具/usage 回归；错误路径用 direct API 回归
- [x] 低并发长上下文资源观测（RSS/FD/延迟）

## 7. 本轮验证证据（2026-07-13）

静态/构建：

- `cargo fmt --check` 通过。
- `git diff --check` 通过。
- `cargo check --all-targets` 通过，无 warning。
- `cargo test --all-targets` 通过：主程序 1130/1130，`kiro_loadtest` 26/26。
- `pnpm build` 通过：`ui/` 与 `admin-ui/` 两套生产构建均通过。
- `cargo build --release` 通过，release binary 成功构建并可启动。

真实本地服务（临时端口 `127.0.0.1:19022`，未触碰 live `9022`）：

- `/cc/v1/messages` 长上下文流式真实调用：`req_01B236A19zHpuT1y1pdLpJ1G`，final usage `input_tokens=12/cache_read=0/cache_creation=271/output=1`，SSE 顺序含 `message_start -> content_block_* -> message_delta -> message_stop`。
- `/ha/v1/messages` 长上下文流式真实调用：`req_01UFBqVPKrA3nkv6x2koWWWh`，final usage `input_tokens=23/cache_read=0/cache_creation=0/output=1`。
- 数据库落库核对：上述两条 raw `total_input_tokens=8607` 仍保留，展示口径 `compat_input_tokens=12/23` 已按 `/cc`、`/ha` `sample-max` 生效；没有 cache-read 证据时 `cache_read_input_tokens=0`，未伪造首轮缓存读取。
- `/cc/v1/messages` 非流式真实工具调用：`req_01nU6gYLh2NKnNTLzJJd5Djv`，非法 schema key `"foo-bar"`、`"中文 key"` 与合法 key `"legal_key"` 均按原始 key 返回，未泄漏内部 `key<hash>`。
- `/cc/v1/messages` 流式真实工具调用：`req_01D6LGwpSJiP8LzCXVcjqGky`，SSE `input_json_delta` 为 `{"foo-bar":"ok","legal_key":"legal","中文 key":"cn"}`，未泄漏内部 `key<hash>`；落库 `stopReasonSource=local_inferred_tool_use`。
- 坏图真实调用：`req_01ppmFifaRP5MYu2jBQH8s2N` 返回 HTTP 400 / `invalid_request_error`，message 为 `invalid image data for media_type: image/png`，未暴露账号/凭据/外部池/调度等内部词。
- `input_schema:null + 空 description` 真实调用通过：工具入口被归一化后不再触发 `invalid type: null, expected a map` 或空 description 的上游 400。
- 无效模型 direct API 返回 public error，message 包含可给下游定位的 request/error id，不含 credential、external pool、fallback、scheduler、bearer、api key、凭据、外部池、调度等内部词。
- Claude CLI 2.1.197 `--output-format=stream-json` 最小请求：真实经过 `http://127.0.0.1:19022/cc`，输出 `pong`，usage 非零，无内部词泄漏。
- Claude CLI Bash 工具请求：真实经过 `http://127.0.0.1:19022/cc`，CLI 收到 `tool_use` 并回传 `tool_result`，最终包含 `tool-ok`，usage 非零，无内部词泄漏。
- 低并发资源 smoke：3 并发、9 条 `/cc`/`/ha` 长一点上下文真实请求全部 HTTP 200；final `input_tokens` 全部 `<= 96` 且 `cache_read=0`。FD `31 -> 34 -> 31 -> 31` 回到基线；RSS `36.5MB -> 38.4MB -> 39.6MB -> 39.2MB`，未见 FD 泄漏或线性失控。部分请求被模型长输出放大到 3k+ output tokens，仍能正常结束并释放 FD。
