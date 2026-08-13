# 外部池直连、模型映射与跨池重试异常（2026-08-04）

Status: `analysis-updated / cross-host-evidence-matrix-recorded / target-semantics-confirmed / p0-implemented / retry-cooldown-focused-verified / half-open-recovery-focused-verified / follow-ups-open`

Severity: `P0 fixed for body-mode/model routing and default header; P1/P2 follow-ups remain`

Scope: 现网 `152.53.243.159`、`152.53.194.170`、`152.53.194.142` 上外部池直连、Raw 透传/模型映射、本地凭证误参与、外部上游错误重试和跨池调度行为。用户要求先完成外部池 usage 修复与验证，再对本问题进行只读分析和必要修复评估。

这三台机器被用户描述为“当前疯狂报错”的共同受影响实例；本文件先以
159 机器的完整样本登记，后续必须把 170、142 的同类记录按相同字段矩阵
对照，不能因为先分析 159 就把问题范围误缩小为单机问题。

## 用户提供的原始问题

以下内容是用户在 2026-08-04 03:00 左右提供的现网现象整理。此阶段只记录事实和疑问，不把任何候选解释写成根因。

### A. 外部池显示“直连”仍出现本地凭证 fallback/错误

- 三台机器的相关路由配置被设置为外部池显示直连。
- 仍观察到 fallback 到本地凭证，或者页面/请求明细出现本地凭证错误。
- 用户明确要求：外部池直连不应因为外部池失败而误进入本地凭证；即使需要重试，也应先在当前外部池或其他外部池配置内处理。
- 需要确认“外部直连”“外部池模式”“外部 fallback”“本地凭证 fallback”“local rescue”之间的真实边界，以及页面显示的路由字段是否与实际发送链路一致。

### B. Raw 透传/优先映射下，带日期模型仍未按映射发送

用户反馈外部池配置为“body 透传”，并使用“优先映射”及以下模型映射：

```text
claude-3-5-haiku-20241022 -> claude-haiku-4-5
claude-3-5-sonnet-20241022 -> claude-sonnet-4-5
claude-3-7-sonnet-20250219 -> claude-sonnet-4-5
claude-opus-4-1-20250805 -> claude-opus-4-5
claude-3-5-sonnet-20240620 -> claude-sonnet-4-5
claude-opus-4-20250514 -> claude-opus-4-5
claude-sonnet-4-20250514 -> claude-sonnet-4-5
claude-haiku-4-5-20251001 -> claude-haiku-4-5
claude-opus-4-5-20251101 -> claude-opus-4.5
claude-sonnet-4-5-20250929 -> claude-sonnet-4.5
```

现网仍出现以下请求/上游模型组合或类似组合：

- `claude-haiku-4-5-20251001`
- `claude-3-5-haiku-20241022`
- `claude-opus-4-5-20251101`
- `claude-opus-4-6-thinking`
- `claude-opus-4-7`

用户疑问：

- 是否因为未开启“模型回写”导致“优先映射”只在本地解析层生效，没有真正写回外部请求体。
- Raw 透传是否应默认开启“模型回写/模型重写”。
- “body 透传”是否只表示正文尽量保留，而不应禁止已经配置的模型映射。
- 映射不到时，是否应保留原模型直接透传；映射到时，是否应发出映射后的目标模型。
- 页面“模型（请求）”“模型（上游）”“模型（本地解析）”“模型解析说明”分别应代表哪一阶段。

### C. 外部直连 502 后的重试、冷却和跨池行为

159 机器上的一条样本：

```text
时间：08/04 02:54:28
请求 ID：req_01obYvvuv1kBSRMT546bQZ5S
入口：/dfcache/team/v1/messages
模型（请求）：claude-opus-4-6
模型（上游）：claude-opus-4-6
外部账号：#17 yuenan
路由：外部直连 · external_pool · external_direct_policy
状态：错误
客户端错误类型：api_error
客户端状态码：502
客户端收到的错误：The request could not be completed right now. Please retry shortly.
错误类型：server_error
内部状态码：502
错误阶段：external_account
内部错误信息：external upstream server was temporarily unavailable
直连原因：explicit_direct
```

用户希望确认：

- 外部上游 502 是否应自动重试。
- 是否应在同一外部池更换账号重试。
- 当前外部池失败后，是否应在其他外部池配置继续尝试。
- 直连策略是否明确禁止回退本地凭证，但允许外部池之间重试。
- 外部账号进入冷却后，当前请求是否仍有可用重试预算。
- 外部池优先级为 20、30 时，为什么看起来没有继续调用低优先级池。
- 哪些错误默认可重试，哪些错误应立即返回；是否需要类似“池模式”的可配置重试错误类型、次数和冷却策略。
- 已经向下游发送响应后，哪些错误不能再重试；首字节前、首事件后和响应完成后的边界分别是什么。

用户还明确要求不要把“外部直连失败后回退本地凭证”当作默认答案：

- 外部直连显示为“直连”时，不能隐式切换到本地凭证；
- 若配置允许池模式，重试应优先在当前外部账号、当前外部池或其他外部池中完成；
- 只有路由配置明确允许的 fallback/rescue 才能改变凭证类型；
- 外部池优先级为 20、30 不应被错误解释为“只尝试第一条命中的配置后停止”，需要核对排序、候选池生成和重试预算。

### F. `usage` 记录的是请求决策结果，不是完整运行时配置快照

用户问“`usage` 不是记录了配置的吗”。当前代码和持久化结果表明，`usage_records.data` 记录的是这次请求的**决策结果**和**运行轨迹**，而不是完整的 `runtime_config`。

已明确存在的页面/数据字段包括：

- `路由`、`routeKind`、`routeSubtype`
- `Fallback 原因`、`直连原因`
- `模型（请求）`、`模型（上游）`、`模型（本地解析）`、`模型解析说明`
- `外部账号`、`外部池`、`externalAttempts`
- `rawUsage`、`externalPoolBilling`
- `localPreflight`、`localAttempted`

但这里没有完整的 `externalPools` 或 `runtime_config` 快照，也没有把所有开关、优先级、路由规则、冷却、映射和回写策略逐项写进每条 usage。要确认某条历史请求到底当时采用了什么配置，必须结合：

1. 当时的运行配置快照或数据库里的 `runtime_config`；
2. 同一版本代码的处理顺序；
3. 该条 usage 的 `routeSubtype` / `fallbackReason` / `directPolicyReason` / `externalAttempts` / `externalOutboundModel`。

换句话说，usage 能证明“这次请求走了什么”，不能单独证明“全局配置当时一定是什么”。

### D. 本地凭证无可用账号时的模型别名错误

159 机器上 `/dfcache/onlylocal/v1/messages` 出现：

```text
时间：08/04 03:02:20
请求 ID：req_01FCExhoorXMrbcp5vn7uMcd
模型（请求）：claude-opus-4-7
模型（上游）：claude-opus-4.7（alias）
模型解析说明：claude-opus-4-7 -> claude-opus-4.7
路由：本地错误 · local_credential · local_error_no_fallback
客户端状态码：503
错误阶段：local_account
内部错误信息：未提供
```

以及：

```text
时间：08/04 03:10:40
请求 ID：req_01SMA6CeaK7YSv1xFci7TUsg
模型（请求）：claude-haiku-4-5-20251001
模型（上游）：claude-haiku-4.5（alias）
模型解析说明：claude-haiku-4-5-20251001 -> claude-haiku-4.5
路由：本地错误 · local_credential · local_error_no_fallback
客户端状态码：503
错误阶段：local_account
内部错误信息：所有账号均已禁用（0/0）
```

用户疑问：

- 这类请求已经是“仅本地”配置时，是否应该按本地配置直接失败，而不能进入外部池。
- 模型解析显示的 `4.7`、`4.5` 别名是否改变了本地账号的真实请求模型，还是仅用于本地能力/定价分类。
- 带日期模型在页面上仍显示为别名，是否说明映射只发生在本地解析层，未发生在真正发送层。
- 本地账号没有可用容量时，是否存在无意义的重试、冷却或错误包装。

这两条记录是“仅本地”入口的反例样本，和 C 节的外部直连样本分开分析：
即使后续确认模型别名解析本身合理，也必须确认它不会改变“仅本地”路由的
凭证范围，更不能因为本地账号耗尽而自动扩展到外部池。

### E. 带日期模型和映射未命中的完整问题清单

除前述示例外，用户要求逐项核对以下带日期模型是否真正进入外部请求：

- `claude-haiku-4-5-20251001`
- `claude-3-5-haiku-20241022`
- `claude-opus-4-5-20251101`
- 以及同类带日期的 Claude 模型标识

需要区分四个阶段，而不是只看页面上一个字段：

1. 客户端发送的“模型（请求）”；
2. 本地解析/别名归一化产生的“模型（本地解析）”；
3. 按外部池映射决定的发送值，即真正的“模型（上游）”；
4. Raw 透传下原始请求体中最终发出的模型字段。

如果映射命中但只改变了第 2 阶段，仍属于用户认为的“没有映射”；如果
映射未命中，才应按配置决定保留原值透传或返回明确错误，不能静默使用一个
与外部池配置无关的本地别名。

## 跨机器事实矩阵

本节把用户给出的 159 样本、142 时间窗口和此前 170 证据放到同一套页面字段下，
避免只分析某一台机器或某一种“路由”。

| 机器 | 时间/请求 | 入口 | 路由 | 外部尝试 | 本地尝试 | 关键事实 |
| --- | --- | --- | --- | ---: | ---: | --- |
| `152.53.243.159` | `08/04 02:54:28` / `req_01obYvvuv1kBSRMT546bQZ5S` | `/dfcache/team/v1/messages` | 外部直连 · `external_direct_policy` | 1 | 证据待补 | 用户提供页面显示 `外部账号 #17 yuenan`、`内部状态码 502`、`错误阶段 external_account`；`usage` 只保存这次请求的决策轨迹，不保存完整配置快照，因此仍需用当时 `runtime_config` 和代码顺序核对“为什么其他请求会出现 `external_fallback_preflight`/`local_no_credentials` 风格记录”。 |
| `152.53.243.159` | `08/04 03:02:20` / `req_01FCExhoorXMrbcp5vn7uMcd` | `/dfcache/onlylocal/v1/messages` | 本地错误 · `local_error_no_fallback` | 0 | 本地路径 | 用户提供页面显示 `模型（请求） claude-opus-4-7`、`模型（上游） claude-opus-4.7（alias）`。这是仅本地入口样本，不能拿来证明外部直连失败回本地。 |
| `152.53.243.159` | `08/04 03:10:40` / `req_01SMA6CeaK7YSv1xFci7TUsg` | `/dfcache/onlylocal/v1/messages` | 本地错误 · `local_error_no_fallback` | 0 | 本地路径 | 用户提供页面显示 `内部错误信息 所有账号均已禁用（0/0）`。同样属于仅本地入口；问题点是页面模型字段和本地别名说明容易误导。 |
| `152.53.243.159` | `2026-08-02 19:59:38 UTC` / `req_01aqAYbzZH6a9r3Ps2Y9GDDA` | `/v1/messages` | 预检 fallback · `external_fallback_preflight` | 2 | 0 | 脱敏 JSONB 显示 `jinnyapi 502 retry_next` 后 `kkkkyue 502 retry_next`，最终 502；不是外部直连，也没有本地凭证尝试。 |
| `152.53.243.159` | `2026-08-02 19:53:00 UTC` / `req_01NMjqxNBn6hcXJnPcFsAzxv` | `/ha/v1/messages` | 预检 fallback · `external_fallback_preflight` | 1 | 0 | `jinnyapi 400 fail`，`错误类型 bad_request`，`retryable=false`；当前证据不足以盲目重试，缺口是“内部错误信息/上游处理诊断”不够。 |
| `152.53.194.170` | `2026-08-02 16:16:40 UTC` / `req_01BqsyBPKKxHsCnWbAdyXBCc` | `/ha/v1/messages` | 预检 fallback · `external_fallback_preflight` | 2 | 0 | 正向对照：`jinnyapi 502 retry_next` 后 `kkkkyue 200 success`，证明同一请求在某些路径下可以跨外部池成功。 |
| `152.53.194.170` | `2026-08-02 19:59:47 UTC` / `req_01rHLgUX8SLaLB8aHYq3zJN1` | `/v1/messages` | 预检 fallback · `external_fallback_preflight` | 2 | 0 | 两个外部池均 502 后最终返回 502；不是外部直连，也没有本地凭证尝试。 |
| `152.53.194.142` | `2026-08-04 08:01:40-08:02:01 +08`，边界 `req_01jwPrGwvtMN1gKkDuPU5K4u` | `/ha`、`/v1`、`/cc`、`/dfcache/team` | 外部直连 · `external_direct_policy` | 0 或 1 | 0 | `yuenan` 作为唯一 Raw 池大量 502；被冷却/排除后出现 `external_pool_unavailable`；本地错误主要来自 `/dfcache/onlylocal`，不是直连回本地。 |

当前能确认的事实是：

1. 已有 159/170 脱敏样本没有证明“外部直连失败后隐式回本地凭证”；它们能证明的是：某些记录的 `routeSubtype` 是 `external_fallback_preflight`，并且同条记录里出现了 `Fallback 原因 local_no_credentials`。这不是完整配置快照，而是这条请求在代码里走到了外部预检分支时记录下来的决策结果。
2. 142 当前样本也没有证明“外部直连失败后隐式回本地凭证”；`外部直连` 下本地尝试为 0。现有证据更强地支持“候选池筛选/路由条件/模型映射/冷却”导致外部池结果不同，而不是直连后偷偷回本地。
3. 用户看到的“本地错误”样本来自 `入口 /dfcache/onlylocal/v1/messages`，它被“外部池路由模式/外部池路由规则”排除，因此应单独归为仅本地路由可用性和页面字段解释问题。
4. 真正需要修复/增强的点不是把直连改回本地，而是：当前外部池候选生成把“请求正文模式”作为前置筛选；持续失败高优先级池缺少更强的池级熔断/手动恢复/可配置重试策略；外部池运行时缺少默认 `anthropic-version` header；页面“模型（上游）”和真实外部发送模型的关系不够清楚；`usage` 也需要明确标注其只是一条请求的运行轨迹，不是完整配置快照。

## 142 现场根因与验证结果

142 当前运行 `v0.0.130`（revision
`d05a959a923ccb0502bbe274acba5a9fdf540b9c`）。完整证据见
[142 外部池直连、模型映射与重试证据](../evidence/external-pool-direct-model-retry-142-20260804.md)。

已确认：

1. “外部直连策略”开启时，502 不会自动进入本地凭证；142 的
   `yuenan` 502 记录本地尝试为 0。出现的 `本地错误 · local_error_no_fallback`
   主要来自“外部池路由模式/外部池路由规则”明确排除的
   `/dfcache/onlylocal`，不是外部直连失败后的隐式本地 fallback。
2. 当前实现把“请求正文模式”放进外部池候选筛选。142 的 `yuenan` 是唯一
   “Raw 透传”池，`kkkkyue` 与 `jinnyapi` 是“标准处理”池；因此 Raw 请求在
   `yuenan` 502 后没有同模式后备池，最终产生“外部池不可用”。这解释了现场，
   但不再视为目标产品语义：用户已明确要求 Raw/标准处理是选池后的 body
   处理方式，不应决定候选池集合。
3. 现场窗口内 `yuenan` 502/重试下一个 419 次，`kkkkyue` 成功 136 次，
   `jinnyapi` 0 次，外部池不可用 40 次。本地错误 653 次来自仅本地入口。
4. “外部池最大重试次数”是上限，不会创造不符合“请求正文模式”的候选池。
   `yuenan` 只有一个 Raw 候选，所以单个请求实际只有 1 次外部尝试是预期结果。
5. 502 被正确标记为可重试并写入 10 秒“服务器错误”冷却；“外部池自动禁用”
   当前关闭，所以冷却结束后优先级 1 的 `yuenan` 会再次参与调度。这解释了
   “持续报错但仍被反复调度”，不是冷却失效。
6. `yuenan` 的 Raw “透传优先映射”与“重写顶层模型”已经真实生效；
   `claude-3-5-haiku-20241022` 成功发送为
   `claude-haiku-4-5-20251001` 的记录来自 `kkkkyue`，说明映射阶段和页面字段
   均需按池分别核对。
7. 142 的 `kkkkyue`、`jinnyapi` 实际缺少用户列出的三个带日期映射，且
   “必须命中映射”关闭；映射未命中时保留原模型透传是当前配置语义，不是
   “模型回写开关未打开”。
8. `/dfcache/onlylocal` 中出现
   `claude-opus-4-7 -> claude-opus-4.7`、`claude-haiku-4-5-20251001 ->
   claude-haiku-4.5` 是“模型（本地解析）”阶段，不等于外部池的“模型（上游）”
   发送值。
9. 本次差异不是 `/ha`、`/v1`、`/cc` 或 `/na` 某个路径的策略写死；运行时
   外部池准入、拒绝列表、直连和 fallback 都按“外部池路由模式/外部池路由规则”
   解析。内置路径的硬编码只负责注册入口和默认配置，不负责定义外部池调度行为。

本地通过 scoped Cargo 运行 `cargo test --locked external_pool::tests::external_pool`
结果为 `104 passed / 0 failed`，覆盖正文模式候选、严格优先级、Raw 映射回写、
映射未命中透传、错误重试和冷却。142 公网 `/healthz` 为 200；未携带明确授权的
`/v1/models` 为 401，因此没有把未认证探测当作业务接口成功验证。

## 后续分析边界

### 2026-08-05 半开放恢复与 Redis 联合故障补充

外部池优先级故障波已经用三个 loopback HTTP 假上游完成真实动态验证：高优先级
`yuenan` 持续 500 时，流量切换到 `kkkkyue`/`jinnyapi`；冷却结束后恢复池能获得
半开放探测；外部直连全失败期间没有新增本地凭证请求。根因是原先冷却结束后仍保留
瞬态失败罚分，健康池长期压过恢复池，导致恢复池无法再次观察成功；现已修复为冷却
结束后允许恢复探测，成功清除失败计数，失败重新冷却。详见
[scheduler shared deadline and Redis chaos 2026-08-05](../evidence/scheduler-shared-deadline-and-load-chaos-20260805.md)。

Redis usage writer 与调度联合故障矩阵此前出现一次 `latency-75-round-1` 边界失败。
增强断言记录实际耗时、Redis breaker 统计、usage 往返数和本地路由状态后，单轮诊断及
三轮完整矩阵均通过（`24/24` exact）。75ms 延迟下调度实际为约 `77–101ms`，没有
证据表明生产热路径固定进行了多次 Redis 往返；未放宽 250ms 共享期限。

外部池 usage 原始成本修复已完成本地 focused 验证；证据见
[外部池 usage 原始成本与 Dashboard 计费验证（2026-08-04）](../evidence/external-pool-billing-verification-20260804.md)。
后续调度、模型映射或重试改动仍必须先按页面中文字段说明问题，并至少核对：

1. 路由配置解析、内置入口与实际策略之间的关系；
2. 外部池选择、同池账号重试、跨池重试、本地凭证 fallback/local rescue 的完整矩阵；
3. “模型（请求）”“模型（上游）”“模型（本地解析）”“模型解析说明”的数据来源与发送时序；
4. Raw 透传、优先映射、模型回写开关和映射未命中时的透传语义；
5. 502、429、超时、网络错误、模型不可用、400 请求错误、上下文超限和协议错误的重试/冷却边界；
6. 首字节前、已提交响应后和流式终止阶段是否仍允许重试；
7. 三台机器对应版本、运行配置、账号状态和脱敏证据之间是否一致。

## 当前状态

159/170 的历史样本和 142 的当前样本已经完成同一字段矩阵的对照。修复前的
实现层根因已确认：选池阶段耦合了“请求正文模式”，高优先级池持续失败时只靠
短冷却和可选自动禁用，运行时外部池请求头也缺少 Anthropic API Key 兼容默认值。

当前工作树已完成 P0 修复以及外部池重试/冷却恢复的稳定性阶段实现，并通过
focused 验证：

- `模型` 参与外部池候选资格、路由和重选；`请求正文模式` 不再决定候选池集合。
- `请求正文模式` 仍保留为选中外部池之后的 body 处理配置：Raw 透传池按 Raw
  发送，标准处理池按 `MessagesRequest` 标准处理发送。
- Raw 入口在重选到标准处理池时，会基于已保留的原始 body 延迟解析
  `MessagesRequest`，因此 Raw 路由可以从 Raw 池 502 继续重选到模型匹配的
  标准处理池。
- 外部池运行时请求在客户端未提供时默认补
  `anthropic-version: 2023-06-01`，并保留客户端已经传入的 header。

- “外部池最多尝试”“跨池重试状态码”“网络错误跨池重试”“协议错误跨池重试”
  “同池重试次数”“同池重试状态码”“同池重试间隔”已经进入运行配置、默认值、
  校验和两套 UI；同池重试不消耗跨池预算，状态码未配置时不在同一外部池重复发送。
- 认证、配额、渠道禁用、端点配置错误和模型不可用会跳过同池重试，优先放弃当前
  外部池并按“跨池重试”配置尝试后续池；普通不可重试 400 仍不会被强行重试。
- 连续瞬态失败会在 5 分钟窗口内拉长池级冷却，默认最高 300 秒并带 20% 抖动；
  上游合法 `Retry-After` 可覆盖为更长冷却，成功请求会清除连续失败计数。
- 外部池 Admin 已增加“清除冷却”；该操作同时清除池级/模型级冷却，并立即失效
  当前进程的运行态快照。
- 外部失败后的本地 rescue 也已经收窄：只有“本地优先 fallback 到外部”这一路径，
  且当前本地池 fresh 路由状态仍是 `Ready`、还有可调度容量时，才允许回本地救援。
  直连外部、本地无凭证、全禁用、模型不兼容、Redis 调度降级、风险熔断和其他终态
  本地不可调度原因都不会被隐式回写成本地凭证重试。

仍未关闭的是生产升级后的复发观察、候选池筛选原因可观测性，以及更清晰展示
“模型（请求）”“模型（本地解析）”和实际外部发送模型。多实例同时写入/清除
冷却的竞态仍属于更大范围调度与混沌验证项。

## 根因

根因分为三层：

1. **修复前已证实配置/调度语义**：外部池候选先按“请求正文模式”筛选；严格
   优先级 `1/10/20` 不是权重分流；“外部池自动禁用”关闭且服务错误冷却只有
   10 秒。
2. **已证实配置不一致**：三个外部池的模型映射规则并不相同，后两个池缺少
   部分带日期模型规则；“必须命中映射”关闭导致未命中时保留原值。
3. **已修复的实现语义冲突**：用户确认不同正文模式的池也应能按配置参与同一
   外部池容灾；当前工作树已把“选池”和“body 处理”解耦，外部池候选选择改为
   基于路由配置、启用状态、优先级/容量/冷却和模型支持。
4. **已修复的外部池协议兼容缺口**：运行时转发外部池请求时没有默认补
   `anthropic-version: 2023-06-01`。这与用户提供的外部上游错误
   `anthropic-version header is required` 匹配，也与 `../sub2api` 的 API Key
   Anthropic 兼容路径不同；当前工作树已按默认兼容 header 修复。

## 最小复现与复现边界

本地可用三池配置复现：

```text
Raw 透传池 yuenan（优先级 1）返回 502
标准处理池 kkkkyue/jinnyapi（优先级 10/20）
同一 Raw 路由重选时只保留 Raw 候选
=> 外部池不可用
```

142 现场在 `07:55-08:05 +08` 已真实出现该序列。当前工作树的 focused 回归
已经覆盖“Raw 池 502 后重选标准处理池并成功”的修复后路径。

真实 Messages 低流量调用尚未完成，因为没有明确授权的请求 API 密钥；从生产配置
提取并使用密钥不属于只读证据审计的安全范围。

## 方案

候选方案按风险排序：

1. P0 已实施：外部池运行时请求缺省补 `anthropic-version: 2023-06-01`，保留
   客户端已传入的 header；Bearer 与 `x-api-key` 两种鉴权路径均有测试。
2. P0 已实施：外部池候选选择与“请求正文模式”解耦。选池先按路由配置、启用
   状态、优先级/容量/冷却和模型支持；选中池后再按该池配置执行 Raw 透传或标准
   处理。Raw 入口若选中标准处理池，会基于已保留的原始 body 延迟解析
   `MessagesRequest`。
3. P1 已实施第一阶段：参考 `../sub2api` 的池模式，增加“同池重试次数”“同池
   重试状态码”“同池重试间隔”，并保留独立的“外部池最多尝试”预算；默认只对
   已被错误分类为可重试且命中状态码配置的错误执行同池重试。普通 400 不会因为
   状态码配置存在就被强行重试。
4. P1 已实施跨池可配置重试：“跨池重试状态码”“网络错误跨池重试”“协议错误
   跨池重试”进入运行配置和两套 UI。跨池重试只在外部池集合内部换池，不改变
   “外部直连”是否能回退本地凭证的路由合同。
5. P1 已实施冷却稳定性：上游 `Retry-After` 会进入外部池池级冷却，并在 usage
   诊断里记录为 `cooldownMs`；连续瞬态失败会把短冷却上浮到更长的临时不可调度
   窗口，成功后清零。
6. P1 已实施手动恢复：Admin 与两套 UI 增加“清除冷却”，同时删除池级/模型级
   冷却并立即失效运行态快照。
7. P1 暂不强做自动硬禁用：泛化 `5xx` 自动禁用外部池可能误伤短时过载的上游。
   当前选择是更长临时冷却、跨池重试和手动恢复；如果要自动禁用，应另加
   “连续失败阈值/时间窗口/禁用原因/手动解除”配置后再实现。
8. P1 待实施：请求明细增加候选池筛选原因，并明确展示“模型（请求）”
   “模型（本地解析）”“模型（上游/外部发送）”三个阶段，避免把本地别名误读
   成外部池实际 body model。
9. P2 待决策：如果目标是三池共同分摊，需要明确权重/同优先级语义；不要把
   当前严格优先级 `1/10/20` 隐式改成权重。

usage 计费修复的 focused gate 已通过，P0 已按以上语义落地。后续 P1/P2 每项
实现仍需要同步更新本问题、当前问题索引和 plantree 状态。

## 验证与证据

- 142 只读证据：`tmp/prod-evidence/20260804-082500-142-external-priority/`
- 持久化摘要：[142 外部池直连、模型映射与重试证据](../evidence/external-pool-direct-model-retry-142-20260804.md)
- P0 修复证据：[外部池正文模式/模型路由 focused 验证](../evidence/external-pool-body-mode-model-routing-fix-20260804.md)
- 外部池重试/冷却证据：[外部池同池重试与冷却恢复 focused 验证](../evidence/external-pool-retry-cooldown-fix-20260804.md)
- 本次本地 rescue 收窄证据：`external_local_rescue_` 与
  `local_rescue_requires_remaining_shared_attempt_budget_for_five_rounds`、
  `preflight_external_error_can_rescue_once_then_attempt_budget_blocks_cycle_five_rounds`
  已通过 scoped Cargo focused 验证。
- `usage` 字段代码证据：
  - `src/anthropic/usage.rs` 的 `UsageRecord` 只定义请求级字段，例如 `routeSubtype`、`fallbackReason`、`directPolicyReason`、`localPreflight`、`externalAttempts`、`externalOutboundModel`、`externalPoolBilling`，没有完整 `runtime_config` 字段；
  - `src/storage/postgres.rs` 入库时把整个 `UsageRecord` 序列化到 `usage_records.data`，同时只把标准 usage/费用/错误列拆成索引列；
  - `runtime_config` 是独立表，测试 fixture 中出现过 `runtimeConfig` 导出对象，但那不是普通 usage 明细的持久化合同。
- 本地 P0 focused 复现/验证（2026-08-04，scoped Cargo）：
  - `cargo fmt --all -- --check`：通过；
  - `raw_route_`：`2 passed / 0 failed`；
  - `eligibility`：`7 passed / 0 failed`；
  - `external_pool::tests`：`218 passed / 0 failed`；
  - `all_parsed_external_fallback_entrypoints_share_model_only_eligibility`：`1 passed / 0 failed`。
- usage gate 复核（2026-08-04）：
  - `external_pool::tests`：`214 passed / 0 failed`；
  - `postgres_persists_runtime_config_credentials_stats_usage_and_pricing`：`1 passed / 0 failed`；
  - `postgres_rolls_up_external_pool_billing_for_large_samples_and_removes_after_cleanup`：`1 passed / 0 failed`；
  - `redis_usage_summary_and_dashboard_are_materialized`：`1 passed / 0 failed`；
  - `pnpm --dir admin-ui build`、`cargo fmt --check`、`git diff --check`、`node feature/tests/check-feature-docs.mjs` 均通过。
- 外部池重试/冷却 focused gate（2026-08-04，本地隔离 PostgreSQL/Redis）：
  - `external_pool_`：`146 passed / 0 failed`，覆盖跨池重试状态码、网络/协议跨池
    开关、终态错误跳过同池重试、连续瞬态失败冷却上浮和成功清理；
  - `external_pool_same_pool_retry`：`3 passed / 0 failed`；
  - `external_pool_atomic_acquire_honors_pool_cooldown_and_fails_closed_on_bad_state`：
    `1 passed / 0 failed`；
  - `external_pool_retry_after`：`3 passed / 0 failed`，覆盖 `Retry-After: 4` 秒和
    未来 HTTP date 的 7 天上限；
  - fake upstream 请求计数证明同池重试先于跨池切换，状态码未配置时不会同池重试；
  - 清除“冷却”后池级/模型级冷却均从运行态快照消失。
- 生产业务接口认证 smoke：阻塞于缺少明确授权的请求 API 密钥；仅完成健康检查
  和未认证 401 边界验证
