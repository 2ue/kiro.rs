# 09 遗漏点与对策

本篇集中列出 01–08 篇没有覆盖、或只简单带过的问题，逐条给出结论。实现时以本篇为准，与前文冲突的地方都已同步修改。

## A. 请求链路

| # | 遗漏点 | 现状 | 对策 |
|---|---|---|---|
| A1 | **原始外部池直通先于解析** | 入口在请求体不需要规范化时，会先尝试原始外部池直通和预检（`request_entry.rs:262-296`），这时还没有解析请求体，也没有强制映射 | 外部池默认不受强制映射影响（D2），行为与现状一致，不需要改。只有 `applyToExternalPool=true` 时才需要处理：命中强制映射的请求跳过原始直通，改走解析后的路径，由外部池兜底并使用目标模型 |
| A2 | **本地池快速失败按原始模型判断** | `maybe_local_pool_unavailable_fast_fail_response` 用 raw probe 的 model 判断本地池是否可用（`request_entry.rs:416-435`）。外显模型在账号列表里没有时会误判为"本地池不可用" | 强制模式下，这一步改用**目标模型**判断。目标模型在入口按外显模型和规则算出（纯函数，不依赖 payload） |
| A3 | **外部池兜底上下文携带原始请求体** | `ExternalFallbackContext` 在强制改写前就已构造（`handlers.rs:8263`），payload 中是外显模型 | 默认不作用于外部池，外部池仍收到外显模型，符合预期。`current_local_dispatchable(model)` 用于判断本地池能否接单，调用方传入的已经是 `payload.model`（改写后就是目标模型），不需要另改 |
| A4 | **token 估算与注入的先后** | 输入 token 在注入**之后**统计（`local_body_pipeline.rs:484` 统计的 payload 已含身份提示等 system 注入） | 对外 usage 口径需要先保存一份"客户端原始请求"的 token 数（见 C1） |
| A5 | **压缩请求的窗口预算** | 用 `get_context_window_size`（不查 catalog），Sonnet 4.5 恰好得到 200K（`handlers.rs:1489`） | 改成按目标模型查 catalog，`max_input_tokens_for(upstream)`，查不到再兜底，不依赖巧合 |
| A6 | **cache 路由键和模拟缓存的模型** | 模拟缓存、最小可缓存长度、creation control 都按 `upstream_model` 计算（`handlers.rs:5009`、`:6146`） | 本来就按目标模型，不用改。注意 Haiku 4.5 的最小可缓存长度是 4096，Sonnet 4.5 是 1024。短 prompt 映射到 Haiku 后，按真实规则就不会命中缓存，这是正确行为 |
| A7 | **代码里按模型名字面判断的点** | 全仓库约 60 处，大多在 model_capabilities、converter、provider 里 | 强制改写后这些地方拿到的都是目标模型，保持现状即可。只有 02 第 2 节列出的三处（`-thinking` 后缀、外显模型默认思考、display 默认值）必须在改写**之前**处理 |
| A8 | **thinking 提示的 `-thinking` 判断** | `converter/thinking.rs:71/99` 用 `req.model.ends_with("-thinking")` 决定是否加可见思考策略 | 改写后目标模型没有这个后缀，所以在入口把它记成请求标志 `display_requested_thinking`，转换时读这个标志 |
| A9 | **diagnostics capture 与日志** | 记录 `requestedModel` 和 `upstreamModel` | 只在服务端可见，保持现状。capture 新增 `displayModel` 字段，方便排查 |
| A10 | **请求 key 的模型限制** | 现有 key 策略不按模型限制（`request_key_limits.rs`），无影响 | — |
| A11 | **websearch 模板与模型驱动路径** | 两条路径都直接用 `payload.model` 作为响应的 model（`websearch.rs:1386/1419`、`model_driven_websearch.rs:259/352`） | 改用 `display_model`，否则响应里会出现目标模型 |
| A12 | **非 messages 入口** | `/v1/messages/count_tokens`、`/v1/models`，以及 OpenAI 兼容和 Responses 入口 | count_tokens 在强制模式下不做 system 注入；模型列表见 B4；OpenAI 兼容入口最终都走 messages 管线，会自动生效 |

## B. 对外暴露

| # | 遗漏点 | 对策 |
|---|---|---|
| B1 | **`stop_details`、`refusal`** | 外显模型可能返回 refusal，目标模型不会，不需要模拟。当前代码不输出 `stop_details`，无影响 |
| B2 | **响应 id、签名格式** | `msg_` id 与模型无关；合成签名的长度与模型无关。无影响 |
| B3 | **usage 字段形态** | 外显模型的 usage 可能带 `cache_creation`（5m/1h 拆分）、`service_tier`、`server_tool_use`、`inference_geo`，现有组装已经按协议输出。强制模式不增减字段，保证同一路由下映射前后形态一致 |
| B4 | **`/v1/models` 的窗口值** | 列表中外显模型的 `max_input_tokens`、`max_tokens` 取自上游目录。如果某个外显模型（如 `claude-opus-5-5`）在目录里存在，展示的就是上游真实值；不存在时，会因为解析不到而不列出。**对策**：强制模式下，列表按"官方外显模型表"补齐并覆盖窗口值（1M/128K 等）。官方表内置在 K0 卡片配置里，不依赖账号目录 |
| B5 | **`GET /v1/models/{id}` 的 404 文案** | `model: {id}` 回显的是请求 id，没有泄漏 |
| B6 | **错误文案中的上游模型 ID** | 上游返回 INVALID_MODEL_ID 等错误时，`model_not_found_message(requested_model)` 已经用 usage_context.model 回显，这个值必须改成 display_model。所有对外错误统一做一次替换：把出现的目标模型 ID 和 Kiro 点号 ID 换成 display_model |
| B7 | **`x-kiro-rs-warnings`** | 默认关闭。打开时写的是转换改写的计数，不包含模型名。强制模式不写入任何相关项 |
| B8 | **响应时延与首字时间** | Haiku 明显更快，属于残留指纹。**不做人为延迟**：会拖慢用户体验，而且各渠道本来速度差异就大，人为延迟意义不大 |
| B9 | **公开号池页** | 冷却记录的 model 改为显示 display_model 对应的模型族名（见 05） |
| B10 | **下游 sub2api 计费的 model** | sub2api 记录的是它发给本服务的请求模型，即外显模型，不受影响。本服务 usage 记录的 `model` 字段保持外显模型 |

## C. usage 与计费

| # | 遗漏点 | 对策 |
|---|---|---|
| C1 | **对外 input 的口径** | 强制模式下，对外 input = 客户端原始请求的 token 估算，算在注入和退避**之前**。原始请求超过 200K 时也照实报，与 04 第 4 节一致。之后仍走路由挂载的 usage 整形策略，上下限是最终硬约束 |
| C2 | **pricing_model** | 内部成本按上游模型（4.5）计算。对外价格由下游渠道决定，本服务不参与 |
| C3 | **tokenizer 系数** | 默认不放大（D5）。不放大时，外显模型 Sonnet 5 / Opus 4.7+ 的真实 token 会多 0–35%，下游看到的 token 偏少，但不会因此报错 |
| C4 | **旁路请求的成本** | L3 摘要、web_fetch 抓取，都只计入内部成本，不计入用户 usage |

## D. 稳定性与运维

| # | 遗漏点 | 对策 |
|---|---|---|
| D1 | **热更新** | 配置随 runtime_config 的 generation 自动刷新（`request_runtime_config`），开关和规则修改后立即生效。已在进行中的请求按开始时的配置跑完 |
| D2 | **目标模型被删除或无账号可用** | 保存配置时校验目标模型存在于上游目录。运行时没有可用账号时，按现有调度语义排队（见调度目标的约定），不回退到外显模型，也不报"模型不存在" |
| D3 | **账号限制列表** | 账号的 supported_models 和订阅档位列表必须包含目标模型，否则会被过滤掉。配置保存时提示"当前有多少账号能承接目标模型"，不阻止保存 |
| D4 | **配置包导出导入** | 新配置项进入运行时配置，自动随配置包导出（按自然键导入）。知识库条目同样进入配置 |
| D5 | **审计** | 开关变更、规则变更、知识库变更都写审计日志（复用现有 runtime config 审计） |
| D6 | **观测** | usage 记录新增字段 `forced_mapping`（bool）以及 `model_resolution_source = "forced"`。管理端可按此筛选，评估影响。这些字段只在管理端可见 |
| D7 | **灰度** | 用 `pathOverrides` 按路由逐步开启。先在测试路由验证，再扩大范围 |
| D8 | **回滚** | 关闭开关即可完全恢复，不需要迁移或重启 |
| D9 | **性能** | K1/K2 关键词检索在内存中完成（条目建议 ≤2000），单次检索 <1ms。输出侧的句级改写复用现有流式缓冲，不新增阻塞 |

## E. 行为一致性

| # | 遗漏点 | 对策 |
|---|---|---|
| E1 | **同一会话中途切换外显模型** | 见 08 第 3.1 节。目标模型不变时，缓存照常命中 |
| E2 | **子代理和后台任务** | 见 08 第 3.2 节。没有 Claude Code 系统提示时，K0 带完整型号 |
| E3 | **Claude Code 的 `/model` 菜单与状态栏** | 菜单和状态栏的内容来自客户端本地或 `/v1/models`（需开启 gateway discovery）。有了 B4，展示的就是外显模型的官方值 |
| E4 | **Claude Code 显示的上下文百分比** | 客户端按 1M 窗口计算。对外 input 按原始请求报（C1），百分比就和真模型一致 |
| E5 | **长会话被压缩后的连续性** | 退避只改动发给上游的内容，不改动客户端历史。客户端下一轮仍发送完整历史，服务端重新退避，结果是确定的 |
| E6 | **语言与风格** | 4.5 和 5.x 的行文风格不同（5.x 更简洁）。不在提示词里模仿风格，避免增加注入面。可以复用现有任务质量注入 |
| E7 | **安全拒答** | 4.5 的拒答边界和外显模型不同。不做处理，原样返回 |

## F. 测试

| # | 遗漏点 | 对策 |
|---|---|---|
| F1 | **mock 上游** | 用 `scripts/loadtest/kiro-mock-upstream.mjs` 加隔离库，做真实调度测试，覆盖强制映射后的账号筛选、冷却和计分 |
| F2 | **真实 Claude Code 端到端** | 用 `CLAUDE_CONFIG_DIR` 隔离客户端配置，跑长会话、多工具、压缩、子代理场景，扫描泄漏关键词（见本地真实测试环境的约定） |
| F3 | **对照组** | 身份、知识库、思考相关的提示词改动，都做"关闭 / 旧文案 / 新文案"三组遵从率对比 |
| F4 | **资源** | 全量测试同时只跑一个，复用共享编译目录，测完清理（见测试资源约定） |
