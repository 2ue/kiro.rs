# 08 错误、异常、并发、多模态与文本边界

本分册包含五组用例：E 错误与异常、C 并发与调度、F fake 辅助（本分册新开的组，前缀 `CC-F-`）、I 图片/PDF/Notebook 输入、X 文本编码与输出边界。

- F 组只用于真实账号难以稳定制造的上游异常（坏帧、半帧、流中途失败等），通过 fake upstream（模拟 Kiro 上游）或 fake Anthropic server 注入，不消耗真实额度，可随时穿插执行。F 组的结果只作为辅助证据，CLI 行为仍以真实 `claude` 二进制的表现为准。
- 「来源」列取值：既有场景（来自此前真实测试与问题记录中已出现过的场景）、新增（用户要求）（与用户本轮要求直接对应）、新增（补充）（为完整覆盖补充的用例）。
- `ccp`、`$WORK`、`$RUN_ID` 等变量和函数见 01-environment-and-observability.md；问题登记与回归流程见 09-issue-record-and-fix-loop.md。

## E 错误与异常

公开错误的统一要求：HTTP 状态码与 Anthropic 错误类型对应（400 invalid_request_error、401 authentication_error、403 permission_error、404 not_found_error、413 request_too_large、429 rate_limit_error、500 api_error、529 overloaded_error），响应带 `request-id`，message 为英文公开文案并带 error id，不含内部术语（账号、凭据、号池、pool、外部池、调度器、fallback、hidden、api_key、bearer、`sk-` 等）；CLI 能正确显示、按类型决定是否重试。错误响应的 request id 要能与服务端日志对应。

### E.1 认证、请求格式与路由

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-E-01 | P0 | 既有场景 | 服务正常运行 | `KIRO_TEST_KEY=bad ccp 'hi'`；另用 curl 带错误 key 请求 `/cc/v1/messages` | 错误的 `ANTHROPIC_API_KEY` | 401 authentication_error，CLI 提示认证失败并退出 | 不重试；有 request id，且能在服务端日志中找到对应记录 |
| CC-E-02 | P1 | 既有场景 | 不设置任何 key | 同 CC-E-01；另用 curl 不带 key 请求 `/cc/v1/messages` | 无 `ANTHROPIC_API_KEY` / `ANTHROPIC_AUTH_TOKEN` | CLI 提示需要登录或 401；curl 返回 401 authentication_error | 同上 |
| CC-E-03 | P1 | 新增（补充） | 非 bare 模式 | 用 `ANTHROPIC_AUTH_TOKEN` 替代 `ANTHROPIC_API_KEY` 运行 A-01 | `ANTHROPIC_AUTH_TOKEN` | 成功 | 走 Bearer 认证路径 |
| CC-E-04 | P0 | 新增（用户要求） | 免费 plan 账号 | `ccp --model claude-opus-4-5 'hi'` | `--model claude-opus-4-5` | 清晰 4xx，CLI 显示模型不可用 | 同 CC-M-05；账号不被冷却/禁用；不在所有账号间轮询打满 |
| CC-E-14 | P1 | 既有场景 | curl | 分别发送：畸形 JSON（malformed JSON）、缺 `messages`、`messages` 为空、role 非法、content 类型非法、非法 `thinking.type`（本地参数校验拒绝），各一次 | 请求体变体 | 400 invalid_request_error | 错误结构符合协议，带 request id 和 error id；文案为统一英文，不含内部细节或内部转换措辞 |
| CC-E-15 | P1 | 新增（补充） | curl | 分别发送无效图片 base64、不支持的 media_type | 图片 content 变体 | 400 invalid_request_error | 已知现象：上游对不支持的图片格式返回 IMAGE_FORMAT_UNSUPPORTED；预期：本地或映射后统一为 400，文案不含内部细节 |
| CC-E-16 | P1 | 既有场景 | curl；可构造大量或超大图片、文件 | 发送总体超过 `MAX_MESSAGES_BODY_SIZE`（50MiB）的请求；再用 CLI 附带足够多的大文件复现一次 | 请求体 > 50MiB | 413，`error.type` 为 request_too_large；CLI 提示请求过大，不触发压缩 | 已知现象：以前 413 的 `error.type` 为 invalid_request_error；预期：协议要求的 request_too_large |
| CC-E-17 | P2 | 既有场景 | curl；CLI 或插件 | 访问：未知路由 `/cc/v2/messages`、未定义缓存路由 `/dfcache/x/v1/messages`、未注册接口 `GET /v1/models/{id}`、拼错的路径、`DELETE /v1/messages` | HTTP 方法与路径变体 | 返回 JSON 信封的 404 not_found_error；方法不匹配返回 405；均带 `request-id` | 已知现象：未注册路径返回框架默认的空 body 404/405，无 request-id、不经过认证，SDK 解析失败；预期：统一 JSON 错误信封 |
| CC-E-21 | P1 | 既有场景 | 服务正常运行 | `ccp --model claude-nonexistent-9 'hi'`；再用写法错误的模型名（如 `claude-sonet-4-5`）重复一次；记录 CLI 提示 | `--model claude-nonexistent-9` | 目标行为：返回 404 not_found_error，CLI 给出模型相关提示（如引导 `/model`）；若当前版本采用错误遮蔽策略，下游收到统一的临时失败错误（如 503 api_error，只带"稍后重试"和 error id）。两种情况都不得泄露凭据、外部池、回退、api_key、bearer、`sk-` 等内部信息 | 已知现象：未知模型先透传上游，上游 invalid model 被映射成 400 "The requested model is not available for this endpoint."，CLI 只显示通用 API Error。记录实际状态码与文案；服务端日志保留 model_not_found 证据；两种目标行为之间的取舍需在问题记录中明确 |

### E.2 容量、限流与账号异常

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-E-05 | P0 | 既有场景 | 上游 429 / 配额耗尽（用小 `requestAdmission.rpm`，或在账号额度耗尽时观察）；长会话、流式、多工具会让同一账号承压 | 连续发请求，直到所有账号临时冷却或限速，再发一次请求 | `requestAdmission.rpm` 调小 | 429 rate_limit_error 并带 `retry-after`；CLI 退避重试后成功或给出清晰提示 | 所有账号冷却时不误报"所有凭据已禁用"，请求不一直挂着；记录 CLI 的重试次数；服务端不把 429 放大成更多上游请求（retry-budget） |
| CC-E-06 | P1 | 新增（补充） | `requestAdmission.maxConcurrentRequests` 调小 | 并发 5 个 `ccp` | 并发数 5 | 部分 429 或排队后成功 | 错误类型与文案；恢复后正常 |
| CC-E-18 | P1 | 新增（补充） | 选 access token 即将过期的账号，或 Admin 强制刷新 `POST /credentials/{id}/refresh` | 会话期间持续对话，跨过 token 过期点 | 无 | 刷新透明，请求不失败 | 刷新失败时账号被标记并切换其他账号 |
| CC-E-19 | P1 | 新增（补充） | 多账号 | 对话中通过 Admin 将当前正在使用的账号设为 `disabled`，继续对话 | 无 | 调度到其他账号，会话不受影响 | 若历史含原生签名 thinking，跨账号回放不报错 |
| CC-E-20 | P2 | 既有场景 | 多账号；上游对当前账号返回 `INVALID_MODEL_ID` / `MODEL_UNAVAILABLE`（400 或 404） | 观察调度行为；再发一条普通格式错误（schema、工具、图片 400）对比 | 无 | 只有在还有其他可用账号时才换账号重试一次，同一账号不重发；普通的格式、schema、工具、图片 400 只发一次就失败 | 不把账号误判为坏账号；防止普通请求错误被放大成多次上游调用 |
| CC-E-22 | P1 | 既有场景 | 测试实例；可用返回 529 的 fake upstream 对比 | 通过 Admin 禁用全部凭据后用 CLI 发请求，记录重试次数与提示；再用返回 529 的模拟上游对比 | 全部凭据 `disabled` | 返回 529 overloaded_error 加 `retry-after`，CLI 显示过载类提示并重试，不崩溃 | 已知现象："账号全部冷却或用尽、attempt 预算耗尽、熔断、provider 未就绪"一律返回 503 api_error，CLI 拿不到过载语义。另观察：连续 529 时 CLI 是否自动切换 fallback 模型（免费 plan 只记录现象） |
| CC-E-25 | P1 | 既有场景 | 有一个会返回 402 额度耗尽的账号（真实耗尽账号或 fake upstream） | 让调度选中该账号后发请求 | 无 | 对外规范化为 502 类公共错误，文案不含号池、调度等内部术语；不被当成协议错误 | 已知现象：额度耗尽的账号曾让 thinking 和 tool-use 用例无法完成；记录是否换到其他账号 |

### E.3 上游错误、流式错误与客户端中断

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-E-07 | P1 | 既有场景 | 上游 5xx 或非长度类异常（如 InternalServerException）；真实上游偶发，或 fake upstream 复现 | 长会话中持续观察；fake 环境下注入 5xx | `kiroUpstreamStreamRetryEnabled` 开/关 | 预输出阶段可重试时服务端重试一次后成功；已输出后变为 SSE error 事件，CLI 显示 API Error 并按错误重试 | 只有"输出长度超限"改为正常结束（见 CC-X-05），其他上游异常仍按错误处理。已知现象：真实上游验证中多次出现瞬时 500，不影响对压缩或转换修复的结论；签名重试后第二次请求返回 502 的情况单独记录到 CC-E-26 |
| CC-E-08 | P1 | 既有场景 | fake upstream 的 stream idle 场景，或把 `kiroUpstreamStreamIdleTimeoutSecs` 调小 | 发长输出请求，让上游空闲超时 | `kiroUpstreamStreamIdleTimeoutSecs` | 收到 SSE error 事件，CLI 提示并可重试 | 期间有 `ping` 保活；不无限挂起；不报成成功或空回答。完整异常矩阵见 CC-F-04 |
| CC-E-09 | P0 | 新增（补充） | 交互式会话 | 长输出中按 ESC 中断流 | 无 | CLI 立即停止，可继续输入 | 服务端记录 client drop；账号 in-flight 立即回落（`/api/admin/credentials/runtime`）；无 FD 泄漏 |
| CC-E-10 | P0 | 既有场景 | `-p` 模式或交互式流式回复中 | 运行中 Ctrl+C 中断进程；另测断网后重连再用 `-c` 继续 | 无 | CLI 停止；服务端释放租约和连接，没有残留；重连后会话能继续，usage 记录正确 | 客户端断开不当作上游故障重试，释放账号占用；in-flight 回落；真实 CLI 门禁要求覆盖取消、重连和最终 usage |
| CC-E-11 | P1 | 新增（补充） | 请求进行中 | 停止 19023 实例（本用例覆盖重启），观察 CLI | 无 | CLI 报连接错误并重试 | 实例恢复后 `-c` 继续成功（CC-B-11） |
| CC-E-12 | P1 | 新增（补充） | 无 | `ANTHROPIC_BASE_URL` 指向不存在的端口，运行 A-01 | `ANTHROPIC_BASE_URL` | CLI 连接错误，退出码非 0 | 纯客户端行为 |
| CC-E-13 | P1 | 新增（补充） | 在 CLI 与服务之间放可注入延迟/断连的代理（如 toxiproxy） | 发长输出请求并注入网络抖动 | 延迟/断连参数 | CLI 超时重试 | 服务端对下游慢读的背压处理 |
| CC-E-23 | P1 | 既有场景 | 构造本地无法转换的请求（如 reasoning 无法承载的组合） | 用 CLI 发送该请求 | 视构造方式而定 | CLI 显示带具体原因的 `API Error: 400`，exit 1，当前 turn 停止 | 不被包装成工具错误让模型继续。请求协议层错误与工具执行失败不是同一层，不能为了"不中断"而伪装成功 |
| CC-E-24 | P0 | 既有场景 | 交互式会话 | 在会话中触发一个客户端协议错误（工具配对、schema、大小问题）或上游 400（如 reasoning 不可用、请求错误），然后继续下一轮普通对话 | 无 | 返回 400 和规范化错误，会话能继续下一轮；错误描述请求结构、工具配对、schema 或大小问题 | 不被当成 5xx 反复换凭据重试，也不变成 503；不因为一个协议 400 跨账号或跨池重复发送。已知现象：错误文案曾暴露内部转换用语，客户端协议错误曾引发重试风暴。记录时区分 HTTP 层错误、Anthropic 错误信封、Kiro 上游错误和 CLI 行为 |
| CC-E-26 | P2 | 既有场景 | 真实上游出现签名失效后的重试，或 fake upstream 复现 | 第二次尝试已返回 200 并向客户端输出（例如大量 reasoning 帧）后，读取响应体出错 | 无 | 如实返回流错误；记录中把第一次的签名错误和最终的流读取错误分开 | 已知现象：第一次签名失败，第二次输出一段后报 "error decoding response body"，也出现过第二次请求返回 502；没有证据表明所有流读取错误都由签名失效引起 |
| CC-E-27 | P1 | 既有场景 | 可复用本组其他用例制造的错误 | 依次触发各类上游错误（4xx、5xx、网络、额度），收集 CLI 显示与 curl 原始响应 | 无 | CLI 看到的错误只有 request id 或 error id 和简短原因 | 没有号池、凭据、调度器等内部词，也没有原始上游错误信封；不兼容的上游错误结构不能直接透传给客户端 |

## C 并发与调度

免费账号数量和额度有限，并发用例只做小规模（≤ 4 个 CLI 进程），大规模压测不在本文件范围。外部池默认关闭，只有 CC-C-07 做对照。

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-C-01 | P1 | 新增（补充） | 3 个独立 `$WORK` | 同时开 3 个 `ccp` 会话，各跑 3 轮带工具的任务 | 无 | 全部成功 | 会话间历史不串；记录每个请求的 request id 与账号分配 |
| CC-C-02 | P1 | 新增（补充） | 无 | 2 个交互式会话 + 1 个 `--bg` 后台会话同时运行 | `--bg` | 全部成功 | 调度排队合理，TTFT 可接受 |
| CC-C-03 | P1 | 新增（补充） | 同一会话目录 | 两个进程同时 `-c` | `-c` | 记录客户端行为 | 服务端无异常 |
| CC-C-04 | P1 | 新增（用户要求） | 无 | 并行子代理（CC-G-04）叠加另一个会话 | 无 | 成功 | 账号并发上限（`credentialMaxConcurrentRequests`）下排队而非失败 |
| CC-C-05 | P2 | 新增（补充） | 本组其他用例结束 | 检查 `/api/admin/credentials/runtime` | 无 | in-flight 全部为 0 | 无残留 lease；服务 RSS/FD 回落 |
| CC-C-06 | P1 | 既有场景 | 调小本地容量上限；可选开启外部池并让其返回 400/429/5xx/超时 | 本地容量打满时发请求；外部池异常时发请求；过程中让一个客户端中途断开 | `requestAdmission.maxConcurrentRequests` 等 | 不出现重试风暴；客户端断开后正确释放资源；首字延迟正常，流式输出不顿挫 | 完整回归的稳定性观察点：统计每个下游请求对应的上游请求数、TTFT、in-flight 回落 |
| CC-C-07 | P2 | 既有场景 | 开启至少两个外部池（仅对照用），其中一个健康；可用 fake 让某个池出错 | ① 外部池流在 `message_start` 之后直接 error，没有任何内容块、thinking 或 tool_use；② 外部池已发出内容块、thinking 或 tool_use 之后再出错 | 外部池配置 | ① 在同一请求内换到其他外部池恢复，客户端只看到一个 `message_start` 和完整输出；外部直连只在外部池之间重试，不回退到本地账号；② 不重放整个请求，备用池不被命中，客户端收到流错误，不伪造 `message_stop` | 已知现象：两个外部池的流式请求约有 2/12 到 5/12 出现"空回"或刚开始就报错，同期非流式请求全部成功；以前流错误发生在响应建立之后，不会换池。已输出的内容可能已被客户端保存或执行，重放会产生重复内容 |

## F fake 辅助

使用 fake upstream 注入异常，服务端配置指向 fake 地址，CLI 仍为真实 `claude`。每条用例都要抓取 `--output-format stream-json` 与服务端日志。

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-F-01 | P1 | 既有场景 | fake upstream 返回上游 500 或协议错误，错误信息中含 `#1429` 凭据标签、`14290ms`、`4290 bytes` 之类数字 | 用 CLI 发请求，观察 CLI 提示与退避 | 无 | 只有真实 429 才显示限流并按 `retry-after` 退避，其他错误按真实类型展示 | 已知现象：代理用"错误字符串包含 429"判断限流，会把 5xx 伪装成 rate_limit_error 并附 retry-after |
| CC-F-02 | P0 | 既有场景 | fake upstream 在返回 200 响应头和部分推理/文本后读流失败 | 用 CLI 发请求，观察界面与 stream-json | `-p --output-format stream-json` | 保持 HTTP 200，关闭已打开的内容块，发送标准 SSE error 事件；不伪造 `message_delta`/`message_stop`；已输出内容后不换账号重放；CLI 不把该轮当成功，usage 记为流错误 | 这里的 200 只表示响应头已提交，不表示成功；重放会导致 CLI 收到重复或不连续的输出。记录不同 CLI 版本收到 SSE error 后是否自动继续（尚待验证） |
| CC-F-03 | P1 | 既有场景 | fake upstream 在流中途注入错误，错误前有待下发的缓冲文本（如工具标签嗅探缓冲中的内容） | 用 CLI 发请求 | `-p --output-format stream-json` | 错误前已缓冲的文本先下发，再发标准 SSE error，不发正常 `message_stop`；stream-json 能看到 error | 已知现象：旧逻辑先处理错误再刷缓冲，缓冲文本被吞掉，表现为输出一句后少一截内容并无声结束 |
| CC-F-04 | P1 | 既有场景 | fake upstream 依次模拟：200 却返回 JSON 异常、SSE/EventStream 帧损坏或 CRC 错误、半帧、帧重复、空 body、只发 `message_start` 就断开、流没有终态就 EOF、上游空闲超时 | 每种异常各发一次请求；另在其中一次过程中 Ctrl+C | 无 | 不报成成功或空回答；还没发 chunk 时返回普通 JSON 错误；已开始输出时发 SSE error，带 request id 和 error id，错误文案统一、不暴露上游原文；无终态 EOF 按协议失败处理（可重试时重试）；客户端断开不当作上游故障重试，并释放账号占用 | 已知现象：断流、坏帧或 JSON 异常曾被当成成功或空回答；返回 JSON 标签的 EventStream 在读头阶段被误判为协议错误。例外：EOF 时若已有上下文用量或计量事件，应算成功结束 |
| CC-F-05 | P2 | 既有场景 | fake upstream 正常结束但没有任何内容（合法的空 `message_stop`） | 用 CLI 发请求 | 无 | 视为已提交的正常结束，不因为是空回复而重试 | 与"首个输出前出错"区分开，避免误重试；统计上游请求次数为 1 |

## I 图片、PDF、Notebook 输入

识别类用例一律使用带唯一合成标记的图片或文档，要求模型复述标记；只看到 HTTP 200 不算通过。

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-I-01 | P0 | 既有场景 | 生成 32x32 左红右蓝 PNG | 让 CLI 用 `Read` 读取该 PNG，问颜色布局 | 无 | 回答 `LEFT-RED-RIGHT-BLUE` 之类正确描述，usage 非 0 | 工具结果中的 image 块被正确转换，不被字符串化 |
| CC-I-02 | P1 | 既有场景 | 准备普通 RGB 截图的 PNG/JPEG/GIF/WebP、data URL 图片、伪造 PNG（扩展名为 png 但内容不是）、1x1 灰度带透明 PNG | 通过 CLI 发送 inline 图片与 `Read` 截图后放在 tool_result 中的图片；覆盖流式/非流式、1 张/2 张/多张 | 流式 / 非流式 | 标准图片均被识别；一两张图不会让 CLI 过早 autocompact（base64 不按普通文本计 token）；伪 PNG 本地拒绝且不计费；上游拒绝的图片不被请求体保护改写 | media_type 规范化（`safeNormalizeBase64MediaTypes`）。已知现象：1x1 透明 PNG 这类边缘格式会被上游以 IMAGE_FORMAT_UNSUPPORTED 拒绝，识别类测试应使用普通 RGB 截图 |
| CC-I-03 | P1 | 新增（补充） | 交互式会话 | 粘贴图片（Ctrl+V）或拖入图片路径 | 无 | 识别 | user content 中 image 块转换 |
| CC-I-04 | P1 | 既有场景 | 超过 5MB 或超高分辨率的图片 | 在 CLI 中附带该图片提问 | 无 | CLI 压缩或服务端按策略处理：超大图片被丢弃并留占位，模型知道图片被省略 | 不返回 5xx；记录 `oversizedImageHandling` 行为；超过 50MiB 请求体见 CC-E-16 |
| CC-I-05 | P1 | 新增（补充） | 5 张不同图片 | 一条消息附带 5 张图片要求对比 | 无 | 成功 | 图片顺序保持 |
| CC-I-06 | P1 | 既有场景 | 交互式会话 | 粘贴一张截图不打字直接回车；得到回复后再发"再描述一次"，再续聊几轮 | 无 | 第二轮及之后不 400，模型能再次描述第一轮图片 | 以抓包为准（CLI 可能自动附带 `[Image #1]` 文本）。已知现象：旧行为把只含图片的历史 user 消息转成空 content，此后每轮都可能 400，直到 compact 或 clear |
| CC-I-07 | P1 | 既有场景 | 10 页 PDF，第 7 页含唯一标记 | `Read` 该 PDF，问第 7 页内容并复述标记 | 无 | 回答正确，包含标记 | document 块在 tool_result 中不被整块字符串化；不能只看 HTTP 200 |
| CC-I-08 | P2 | 新增（补充） | 扫描版 PDF（纯图片） | `Read` 并提问 | 无 | 回答或说明无法识别 | 不报错 |
| CC-I-09 | P1 | 新增（补充） | 含输出图片的 `.ipynb` | `Read` 该 notebook | 无 | 成功 | notebook 输出中的图片正确处理 |
| CC-I-10 | P2 | 既有场景 | curl | 发送 `source.type=url` 的远程图片；分别测公网地址、内网地址、超大资源、多次重定向 | `safeDownloadRemoteSources` 开/关 | 只接受 inline 的模式下直接 400 并提示原因；允许下载时受大小、重定向和地址范围限制 | SSRF 边界：内网地址被拒；远程图片有 SSRF 和资源放大风险 |
| CC-I-11 | P2 | 既有场景 | curl；CLI | 上传 `/cc/v1/files` 后在 messages 中以 `file_id` 引用，以及 CLI `--file`；然后重启代理服务，再在会话中引用该文件 | `--file` | 重启前成功或清晰错误；重启后行为明确（清楚的错误或提示重新上传），不静默出现异常结果 | `safeMaterializeFileSources` 行为。已知现象：Files、prompt 缓存历史等存在进程内存里，重启后会丢；单用户部署下这也会改变 Claude Code 的行为 |
| CC-I-12 | P1 | 既有场景 | 临时目录放含标记的小文档和约 1800 行含标记的大文档 | 让 CLI 用 `Read` 找出两份文档中的标记 | 无 | 两者都准确返回标记；大文档被截断时 CLI 自动继续读取；usage 非 0 | 验证文件读取与长 tool_result 链路 |
| CC-I-13 | P1 | 既有场景 | 长会话中曾带过超过 5MB 的图片 | 继续对话若干轮；再在当前轮附带一张超大图片 | 无 | 不出现 IMAGE_SIZE_EXCEEDED；过大的历史图片被替换为占位文本；当前轮的超大图片返回明确的 invalid_request_error，提示压缩 | 已知现象：历史消息中的超大图片曾被原样透传，上游直接 400 |
| CC-I-14 | P1 | 既有场景 | 准备图片、大文档、小文档、超大 base64 图片；可用的内置工具 | 在 CLI 中组合 image+tools、document+tools、thinking+tools、超大 base64 图片各发一次 | thinking 开启 | 能正常对话，不因内容块类型或组合触发 "Improperly formed request"；图片超过上限时给出明确错误 | 这些内容在 Anthropic 协议下合法，但转换后可能被上游拒绝；"Improperly formed request" 的常见原因包括工具配对、工具 schema、多模态来源和请求体大小，失败时按这四类归因 |

## X 文本编码与输出边界

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-X-01 | P0 | 新增（用户要求） | `$WORK/中文目录/说明.md` | 全中文对话，并对中文文件名/路径执行 Read/Edit | 无 | 正常 | 无乱码；工具参数中文路径正确 |
| CC-X-02 | P1 | 新增（补充） | 无 | 输入并要求复述日文、韩文、阿拉伯文（RTL）、emoji、组合字符、零宽字符 | 无 | 原样处理 | 多字节字符跨 SSE chunk 不损坏 |
| CC-X-03 | P1 | 新增（补充） | 无 | 让模型输出含 HTML 标签、`<br>`、XML、Markdown 表格的内容 | 无 | 原样输出 | 已知风险：输出中出现 `<br>` 等标签污染，或标签被吞；预期两者都不发生 |
| CC-X-04 | P1 | 新增（补充） | 无 | 让模型输出很长的单行（10 万字符的 base64） | 无 | 完整或按 max_tokens 截断 | 不卡住 |
| CC-X-05 | P1 | 既有场景 | 无 | 要求 8000 字文章；再让模型一次把 5000 行代码写进一个文件，以触发上游输出截断，观察 CLI 界面和结尾事件 | `-p --output-format stream-json` | 完整输出，`stop_reason=end_turn` 或 `max_tokens`；上游截断且已有输出时以 `message_delta`（stop_reason=max_tokens）和 `message_stop` 正常结束，CLI 保留已生成内容并能继续；截断发生在工具输入 JSON 中间时 stop_reason 仍为 max_tokens；完全无输出就截断时仍报错 | 流无停顿超过 idle 超时。已知现象：流式把上游 ContentLengthExceededException 转成 SSE error，无 `message_delta`/`message_stop`，文案被换成通用"请稍后重试"，CLI 显示 API Error 并完整重发，上游在同一位置再次截断形成循环。真实上游难稳定触发，主要依赖单元测试覆盖，真实测试以观察为主 |
| CC-X-06 | P1 | 新增（补充） | 无 | 用中文提问要求英文回答；再用英文提问要求中文回答 | 无 | 语言遵从 | 服务端不按首条消息的语言强制锁定回答语言 |
| CC-X-07 | P1 | 新增（补充） | 无 | 让模型在回答中复述"请继续"、"Tool results provided."、"<thinking>" 等敏感字面量 | 无 | 原样输出 | 不被 transcript sanitizer 误删 |
| CC-X-08 | P2 | 新增（补充） | 含大量空白、制表符、CRLF 的代码文件 | 要求 Edit 修改其中一处并保持格式 | 无 | 格式保持 | 服务端对 JSON 的空白压缩不改变工具参数语义 |
| CC-X-09 | P1 | 新增（用户要求） | 无 | 让模型只调用工具不输出任何文字 | 无 | 成功 | 空 text 块不导致请求失败或后续轮次 400 |
| CC-X-10 | P1 | 新增（用户要求） | haiku-4.5 | 跑 10 次工具任务，统计"声明要调用工具但以 end_turn 结束"的次数 | `--model claude-haiku-4-5` | 记录调用率 | 若频繁出现，按 09-issue-record-and-fix-loop.md 登记为"意图前言后 end_turn 未调用工具"问题 |
| CC-X-11 | P1 | 既有场景 | User-Agent 为 `claude-cli/2.1.193` 及以上的 CLI；另准备一个低于该版本的 UA 对照（curl） | 跑长任务，抓 SSE 统计空 delta | 无 | text、thinking、tool_use 活动块长时间无正文时分别收到空的 `text_delta`、`thinking_delta`、`input_json_delta` 保活；无活动块或版本不满足时仍发 `ping`；空 delta 不计 usage，不改变结束事件 | 已知现象：CLI 或 VSCode 插件的状态点长时间停住，因为以前只发 ping，而客户端 UI 不把 ping 当成进度 |
| CC-X-12 | P2 | 既有场景 | 抓包或 stream-json 带时间戳 | 长任务中统计 `text_delta` 间隔的 p50/p90/p99/max | 无 | 正常 p50 约几十毫秒；偶发几十秒的最大间隔能归因到工具执行、模型规划或上游无可见输出 | 用来排除网关自身攒输出（生成了却没 flush） |
| CC-X-13 | P1 | 既有场景 | 无 | 抓取 stream-json，检查最终 assistant 文本与工具调用 | `-p --output-format stream-json` | 不重复整段输出、不重复工具调用、不重复 final message | 重复处理 `content_block_delta` 会导致最终文本重复 |
