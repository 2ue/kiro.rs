# sub2api → kiro-rs(59138) 失败分析：403 forbidden 归因与参数兼容模块设计

- 日期：2026-11-09（北京时间）
- 线上版本：kiro-rs `0.0.241`（镜像 revision `80efc8dc`），源码基线 `0.0.243`（`539870fc`）
- 数据窗口：2026-11-08 11:00 ~ 2026-11-09 11:00（北京时间）
- 数据来源：159 机器 sub2api `ops_error_logs` / `usage_logs` / `accounts`；本机 kiro-rs `usage_records` / `route_scenarios` / `external_upstream_pools` / 容器日志
- 性质：仅分析与设计，不含代码修改

## 1. 问题 2：minesushua 账号出现 "Upstream access forbidden"

### 1.1 结论

先前"zslnh 外部池欠费导致 minesushua 报 403"的归因是错的。实际情况：

1. minesushua 的请求没有进入外部池。全部走 kiro-rs 本地凭据（`routeKind=local_credential`）。
2. "Upstream access forbidden, please contact administrator" 是 sub2api 自己生成的文案，不是 kiro-rs 或外部池返回的。
3. 真正的原因是 kiro-rs 本地凭据空回或流中断，kiro-rs 在 SSE 流里发出 `type:error` 事件；sub2api 的 pool_mode 账号把"流内错误事件"统一记成上游 403，再按 403 映射成 forbidden 文案返回给用户。

### 1.2 路由链路

| 层 | 配置 | 证据 |
|---|---|---|
| sub2api 分组 | minesushua（账号 2342）属于分组 122【自营】KIRO官号满血号池-1、207 Claude KIRO月卡、137【自营】KIRO官号满血按次号池1、51【自营】KIRO官号逆向满血分组 | `account_groups` |
| sub2api 账号 | `base_url=http://<59138>/dfcache/minea`，`pool_mode=true`，`pool_mode_retry_count=3`，`pool_mode_retry_status_codes=[401,403,429]` | `accounts.credentials` |
| kiro-rs 路由场景 | 场景 `日常` 的 `external.routeMode=allow_list`，`routeRules=["/cc","/dfcache/dfv1","/ha","/na","/v1"]`；`/dfcache/minea` 不在名单内 | `route_scenarios` id=1 |
| kiro-rs 实际路由 | 24h 内 `/dfcache/*` 全部路径走外部池次数 = 0；`/dfcache/minea` 60206 次全部 `local_credential` | `usage_records` |

zslnh 的 403 `INSUFFICIENT_BALANCE` 真实存在（157 次），但只出现在 `/na`(85)、`/cc`(45)、`/ha`(14)、`/v1`(10) 这几个允许外部池的入口，与 minesushua 无关。

### 1.3 逐条核对

用 sub2api `ops_error_logs.upstream_errors` 里的 kiro-rs request-id 回查 kiro-rs `usage_records`，minesushua 150 条 forbidden 中匹配到 145 条：

| kiro-rs 实际状态 | kiro-rs 错误 | 条数 |
|---|---|---|
| `stream_error` / local_credential | `upstream eventstream ended with only usage/metadata events and no assistant, reasoning, or tool output`（空回，已换号重试 6 次） | 136 |
| `stream_error` / local_credential | `Encountered an unexpected error when processing the request`（上游流内错误） | 6 |
| `stream_error` / local_credential | `upstream eventstream ended with incomplete tool input buffer(s)` | 3 |

sub2api 侧同一请求记录为：

```text
upstream_errors[0].kind = "stream_error"
upstream_errors[0].upstream_status_code = 403        ← sub2api 赋的值，kiro-rs HTTP 头是 200
upstream_errors[0].detail = {"error":{"message":"The response was interrupted ...","type":"overloaded_error"},"type":"error"}
最终返回 = 502 "Upstream access forbidden, please contact administrator"
```

### 1.4 sub2api 侧行为（闭源镜像 `weishaw/sub2api:latest`，按数据推断）

- 7 天内全平台 `kind=stream_error` 共 25235 条，`upstream_status_code` 全部为 403，且全部来自 `pool_mode=true` 账号；非 pool_mode 账号 0 条。
- 推断：pool_mode 账号把"HTTP 200 后流内出现 error 事件、且尚未向客户端输出内容"的情况，伪装成 403，借助 `pool_mode_retry_status_codes` 里的 403 触发重试或换号；重试都失败后，按 403 文案返回 "Upstream access forbidden"。
- 145/150 条只有 1 次上游尝试，说明分组 122 里同优先级（priority=1）只有 minesushua 一个账号，没有可换的号，直接失败。
- 这个推断无法通过源码确认（sub2api 为闭源二进制，`strings` 只能确认文案存在）。

### 1.5 影响与修复方向

| 方向 | 做法 | 位置 |
|---|---|---|
| 根因（kiro-rs） | 降低本地凭据空回率：按凭据统计 `usage_only` 空回，集中出现的号降权、冷却或禁用（24h 共 794 次，集中在 `/dfcache/minea` 的 opus-5-5、sonnet-5-5、opus-5） | kiro-rs 调度 / 凭据健康 |
| 文案误导（sub2api） | pool_mode 下流内错误不应落成 forbidden 文案，应透传 kiro-rs 的原始 error message 与 `type`（`overloaded_error`） | sub2api，非本仓库 |
| 可用性（sub2api） | 分组 122 中 priority=1 只有 minesushua 一个账号，失败后无同级可换；可给 2335(minea-59138) 等同级账号设相同优先级 | sub2api 账号配置 |
| 可选（kiro-rs） | 在首个下游事件前就失败的空回，改为返回非 200 的 HTTP 错误（如 502/529），而不是 200 + 流内 error；这样 sub2api 会走普通 HTTP 错误分支，文案正确 | kiro-rs 流式响应提交时机 |

## 2. 问题 8：`does not support non-default top_p / temperature`

### 2.1 来源

这两条 400 不是 kiro-rs 产生的，请求根本没有到达 59138。

| 证据 | 结果 |
|---|---|
| kiro.rs 源码全仓搜索 `non-default` / `does not support` | 无命中 |
| 线上 kiro-rs 0.0.241 二进制 `strings` | `non-default` 计数 0 |
| 线上 kiro-rs 日志 2h 内 `top_p` | 0 条 |
| sub2api 二进制 `strings` | 有模板 `%s does not support non-default top_p` / `... temperature`，以及写死的 `claude-sonnet-5-5 / claude-haiku-5-5 does not support non-default top_p/temperature` |
| sub2api `ops_error_logs` | `error_phase=request`、`error_source=client_request`、无 `upstream_status_code`、`upstream_errors` 为空 |

24h 共 28 次：用户 535(16)、389(9)、258(2)、508(1)，均为 `Go-http-client/2.0`，账号挂在 59138 的 `/cc`、`/na`、`/v1`、`/onlylocal-按次`。

### 2.2 结论

- 要让这类请求"兼容而不是报错"，开关在 sub2api，不在 kiro-rs。sub2api 是闭源镜像（`weishaw/sub2api`），判断条件和是否有开关本次无法确认。
- kiro-rs 侧现状（即使请求到了）：
  - 本地凭据路径：`MessagesRequest`（`src/anthropic/types.rs:731-765`）没有 `temperature/top_p/top_k` 字段，serde 解析时静默丢弃，不报错不告警（`src/anthropic/openai/mod.rs:8` 注释也说明了这一点；已有 issue `feature/issues/protocol-conversion-audit-20260928/10-request-fields-and-beta-headers-silently-dropped.md`）。
  - 外部池路径：normalized 模式只覆盖 10 个顶层字段（`src/external_pool/body_pipeline.rs:188-199` `NORMALIZED_TOP_LEVEL_FIELDS`），其余字段以客户端原始 JSON 为底原样透传；raw 模式同样透传。若外部池背后也是会拒绝采样参数的网关，就会收到同类 400。
- 所以 kiro-rs 需要的不是"修 bug"，而是把"本地静默丢弃 + 外部池原样透传"这两种隐式行为变成显式、可配置的策略（见第 4 节）。

## 3. 问题 9：`output_config.effort must be one of: low, medium, high, xhigh, max`

### 3.1 来源

这条 400 是 kiro-rs 在请求入口返回的，请求没有发往任何上游；sub2api 只是把它记成上游 400。

| 位置 | 作用 |
|---|---|
| `src/anthropic/request_facts.rs:257-276` | raw 阶段校验（`validate_raw_reasoning_protocol_with_probe`），由 `src/anthropic/handlers/request_entry.rs:160-164` 调用，`entry_reason=invalid_reasoning_protocol` |
| `src/anthropic/handlers/request_entry.rs:798-812` | typed 阶段二次校验（`validate_typed_reasoning_protocol`），`reason=invalid_thinking_effort` |
| `src/anthropic/types.rs:153` | 合法值 `["low","medium","high","xhigh","max"]` |
| `src/anthropic/types.rs:714-719` | `parse_thinking_effort` 精确匹配：区分大小写、不去空格 |
| `src/anthropic/request_facts.rs:571-576` | 唯一的现有兼容：`"none"`（去空格、忽略大小写）会被删除 |

- 触发条件：`output_config.effort` 是字符串但不在 5 个值内，例如 `"High"`、`" high"`、`"minimal"`、`"auto"`、`""`。非字符串时报另一条 `output_config.effort must be a string`。
- 作用范围：校验在选路之前，所有入口（`/v1`、`/cc`、`/ha`、`/na`、`/dfcache/*`）、本地与外部池、所有模型一视同仁；`compatProfile` 不影响；无开关。

### 3.2 线上数据

| 来源 | 记录 |
|---|---|
| sub2api 24h | 4 次：用户 352（TauriTavern/2.4.0，`/dfcache/minea`，opus-4-6/4-8）3 次，用户 508（`/v1`，opus-4-7）1 次 |
| kiro-rs `usage_records` 7 天 | 北京时间 11-04 ~ 11-06：`/dfcache/onlylocal` 15 条、`/dfcache/minea` 4 条；`stage=request_entry`、`errorMessage=request rejected before upstream dispatch` |

客户端实际传的是哪个值：未确认。拒绝记录不保存原值；11-06 抓包里 effort 只出现了 5 个合法值和空串 `""`，空串会触发这条 400，可能是来源之一。

### 3.3 结论

- 这是 kiro-rs 可以直接兼容的点：在严格校验之前做一次 effort 归一化（大小写、空白、别名、未知值降级或删除），即可把这类 400 消掉。
- 落地前建议先在拒绝信息或 `record_entry_request_error` 的 metadata 中记下被拒的原值（截断），观察 1~2 天再定别名表。

## 4. 参数兼容模块（requestParamCompat）设计

### 4.1 现状：散落的参数处理

请求入口（所有路由、两种上游都生效），按执行顺序：

| 参数 | 位置 | 当前行为 | 可配置 |
|---|---|---|---|
| 缺 `max_tokens` | `request_entry.rs:572-608`，`request_facts.rs:691` | 拒绝或补默认值 | `missingMaxTokens`（`src/model/config.rs:5561`），全局 |
| `max_tokens` 范围 | `request_facts.rs:234-240` | 越界拒绝 | 否 |
| `output_config.effort = "none"` | `request_facts.rs:571-586` | 删除 | 否 |
| `thinking.type = between_tools` | `request_facts.rs:545-548`、`587-601` | 改为 `adaptive` | 否 |
| thinking 预算 | `request_facts.rs:365-540` | 自动修复；adaptive/disabled 删 budget | 否 |
| effort / thinking 严格校验 | `request_facts.rs:212-327`，`request_entry.rs:798-840` | 400 | 否 |
| 非标准 role | `request_entry.rs:672-707` | 并入 system / 丢弃未知 role | 否 |
| content 形状 | `request_entry.rs:730-790` | 归一化 | 否 |

本地凭据路径：

| 参数 | 位置 | 当前行为 | 可配置 |
|---|---|---|---|
| `temperature` / `top_p` / `top_k` / `service_tier` / `context_management` | `types.rs:731-765` 无字段 | 静默丢弃 | 否 |
| `model` | `handlers.rs:7473-7500`，`model_capabilities.rs:1165-1240` | 映射，不支持则 400 | `modelMapping`、`modelResolutionMode`，全局 |
| effort → Kiro 档位 | `converter/model.rs:167-188` | 就近映射，等距取强 | `bodyConversion.nativeReasoningFields`，全局 |
| thinking 路由特性 | `parsed_body_pipeline.rs:30-50` | 按路由改写 | `routeFeatures.pathOverrides` |
| `stop_sequences` | `stop_sequence.rs` | 输出侧截断 | 否 |
| `output_config.format` | `structured_output.rs:41-100` | 转内部工具 | 否 |

外部池路径：

| 参数 | 位置 | 当前行为 | 可配置 |
|---|---|---|---|
| 未建模字段（temperature/top_p/top_k/service_tier/stop_sequences 等） | `external_pool/body_pipeline.rs:188-199` | 原样透传 | 否 |
| `model` | `external_pool/model_pipeline.rs:4-113` | 每池映射 | 每池 `modelMappingRules` 等 |
| `context_management` | `external_pool.rs:12425-12449` | 仅 `anthropic_passthrough` 档位按 beta 删除 | 每池 `headerProfile` |

问题：同一个参数在本地是"静默丢弃"，在外部池是"原样透传"，入口又对 effort 做"严格拒绝"，三种语义互不相知，也没有统一开关。

### 4.2 目标

1. 一个顶层配置管住所有"客户端参数兼容"行为，开关一处可见。
2. 每个参数显式声明处理方式，不再依赖结构体有没有这个字段。
3. 支持全局默认 + 按路由覆盖 + 按模型覆盖。
4. 现有散落逻辑迁入后默认行为不变（零配置等同现状）。

### 4.3 配置结构（挂在 runtime_config 顶层）

```jsonc
"requestParamCompat": {
  "enabled": true,
  "recordOriginalValue": true,          // 拒绝/改写时在 usage_records metadata 记录原值（截断）
  "defaults": {
    "temperature":  { "action": "drop_for_local_passthrough_external" },
    "top_p":        { "action": "drop_for_local_passthrough_external" },
    "top_k":        { "action": "drop_for_local_passthrough_external" },
    "outputEffort": {
      "action": "normalize",            // reject | normalize | drop | passthrough
      "caseInsensitive": true,
      "trimWhitespace": true,
      "aliases": { "none": "__drop__", "minimal": "low", "auto": "__drop__", "": "__drop__" },
      "onUnknown": "drop"               // reject | drop | default
    },
    "thinkingType":  { "aliases": { "between_tools": "adaptive" } },
    "maxTokens":     { "onMissing": "default_value", "defaultValue": 200000 }
  },
  "pathOverrides": {
    "/cc": { "temperature": { "action": "passthrough" } }
  },
  "modelOverrides": {
    "claude-sonnet-5-5": { "temperature": { "action": "drop" }, "top_p": { "action": "drop" } }
  }
}
```

`action` 统一语义：

| action | 含义 |
|---|---|
| `passthrough` | 不改动，本地路径仍受 Kiro 协议约束（等于丢弃），外部池透传 |
| `drop` | 两条路径都删除该字段，记一条 `x-kiro-rs-warnings` |
| `reject` | 返回 400（保留现有文案），用于严格模式 |
| `normalize` | 按规则改写（别名、大小写、范围钳制），改写失败时按 `onUnknown` 处理 |
| `drop_for_local_passthrough_external` | 现状兼容值：本地丢弃、外部池透传；作为采样参数的默认值，保证零配置不变 |

### 4.4 合并顺序与匹配

- 顺序：`defaults` → `pathOverrides`（按前缀从短到长叠加）→ `modelOverrides`。逐参数、逐字段覆盖，未写的字段继承上一层。
- 路由匹配复用 `reported_usage_path_matches`（`src/model/config.rs:6902`）：精确相等或前缀后紧跟 `/`；`/dfcache/<name>` 先经 `normalize_defined_cache_route`（`config.rs:6912`）。
- 模型匹配用客户端请求的模型名（如 `claude-sonnet-5-5`）。如需按上游模型匹配，需要放到模型解析之后，见 4.6 待定项。

### 4.5 挂载位置

```text
handle_messages_endpoint
  ├─ normalize_raw_client_reasoning_aliases_with_probe   request_entry.rs:81
  ├─ ★ apply_request_param_compat(raw_body, endpoint, model, cfg)   ← 新模块
  ├─ thinking 预算修复                                   request_entry.rs:~118
  ├─ validate_raw_reasoning_protocol_with_probe          request_entry.rs:160
  └─ ... 选路 → 本地 / 外部池
```

- 放在严格校验之前，effort 修好后再交给校验，无需改动校验本身。
- 拿得到 endpoint、`raw_probe.model`、runtime_config，本地和外部池同时生效。
- 发生改写时照现有写法重新 probe，并置 `requires_normalized_body = true`（`request_entry.rs:229`、`367`）。
- 预归一化结果放进 `RequestRuntimeConfig`（`handlers.rs:975`、`1126`），按 generation 缓存，不在每个请求上重复解析。
- `request_facts.rs:571-586`（effort none）和 `request_facts.rs:545-601`（between_tools）迁入新模块，作为 `defaults` 中的别名表，原位置删除。

### 4.6 需要改动的文件（实施时）

| 文件 | 改动 |
|---|---|
| `src/model/config.rs:4981` | `Config` 增加 `request_param_compat` 字段与默认值 |
| `src/admin/types.rs:1878`、`2051` | `RuntimeConfigResponse` / `UpdateRuntimeConfigRequest` 增加字段 |
| `src/admin/service.rs:7183` | 运行时配置写回 |
| `src/anthropic/handlers.rs:975`、`1126` | `RequestRuntimeConfig` 增加预归一化字段 |
| `src/anthropic/handlers/request_entry.rs` | 在 81~118 之间调用新模块 |
| 新建 `src/anthropic/param_compat.rs` | 规则解析、合并、执行与单测 |
| `admin-ui` | 配置页增加一个分区 |

待定项：

1. `count_tokens` 走另一个入口（`handlers.rs:15254`），是否同步应用。
2. 是否需要递增 `runtime_config_migration_version`（未确认）。
3. 模型覆盖用客户端模型名还是上游模型名；后者需要把模块放到 `resolve_model_with_mapping` 之后，或拆成两段。
4. 按模型的 effort 白名单可复用 `KiroReasoningFieldCapability`（`model_capabilities.rs:29-112`），但它依赖账号 cohort，入口处能否取到未确认；第一版建议只用静态配置。
5. `missingMaxTokens` 是否迁入新模块统一管理，还是保留原字段做兼容别名。

### 4.7 对本次两个问题的覆盖

| 问题 | 新模块能否解决 | 说明 |
|---|---|---|
| 8 top_p / temperature | 部分 | 请求在 sub2api 就被拒，kiro-rs 收不到；需先在 sub2api 侧放行或丢弃。模块解决的是 kiro-rs 外部池透传同类参数的风险 |
| 9 effort 非法值 | 能 | `outputEffort.action=normalize` + 别名表 + `onUnknown=drop` 即可消除入口 400 |

## 5. 待办汇总

| 优先级 | 事项 | 归属 |
|---|---|---|
| P1 | 按凭据统计 `usage_only` 空回，集中出现的号降权、冷却或禁用（问题 2 根因） | kiro-rs |
| P1 | effort 拒绝时记录原值，观察 1~2 天 | kiro-rs |
| P2 | 实现 `requestParamCompat` 模块（第 4 节） | kiro-rs |
| P2 | pool_mode 下流内错误不要映射成 forbidden 文案；sonnet-5-5 / haiku-5-5 的 top_p / temperature 改为丢弃而非拒绝 | sub2api（非本仓库） |
| P3 | 分组 122 给 minesushua 配置同优先级的备用账号 | sub2api 配置 |
| P3 | 评估首个下游事件前的失败是否改为非 200 HTTP 错误返回 | kiro-rs |
