# 生产 `req_01hJ2YhjMEyjrrVEQFTZpqri` Thinking Signature 根因跟进

日期：2026-09-28（Asia/Shanghai）

目标服务：`152.53.243.159:59137`

目标请求：`req_01hJ2YhjMEyjrrVEQFTZpqri`

证据目录：`tmp/prod-evidence/20260928-162208-152.53.243.159-signature-followup/`

本文只做分析和设计建议，不修改生产代码、配置、数据库、Redis、容器、账号状态或本地业务代码。

## 1. 任务计划与进度

本次补充任务的执行计划和完成情况：

| 项 | 状态 | 说明 |
| --- | --- | --- |
| 固定任务边界 | 已完成 | 只分析当前签名/重试/体积问题；不切换到其它任务；不做 v161 历史对比，因为用户已说明旧数据已清掉 |
| 只读采集新请求 | 已完成 | 通过 `usage_records.id` 精确定位 `req_01hJ2YhjMEyjrrVEQFTZpqri` |
| 采集当前窗口同类记录 | 已完成 | 在最近 5000 条 usage 中聚类 signature retry 后续结果，并导出 28 条同类 “signature -> too long” 记录 |
| 核对生产版本与配置 | 已完成 | 生产运行 `0.0.173`，runtime config version `459`，payload guard mode 为 `on_too_long` |
| 对照生产版本源码 | 已完成 | 核对 `v0.0.173` 中 signature retry、payload guard 初始模式、too-long retry 的状态机顺序 |
| 输出中文根因文档 | 已完成 | 本文给出根因、可疑机制、改造建议、未修改项和后续验证建议 |

## 2. 结论先行

这条请求的根因不是单纯的“上游报错”，也不是网络问题。它是一个组合问题：

```text
原始请求携带无效历史 thinking signature
  -> Attempt 1: Kiro 返回 400 THINKING_SIGNATURE_INVALID
  -> 本地兼容逻辑剥离所有历史 reasoningContent 后同账号重试
  -> Attempt 2: 原来的无效 signature 已经不在请求里，所以不再报 signature
  -> 但请求仍然约 5.76 MB / 1996 条历史 / 851129 input tokens
  -> Kiro 返回 400 CONTENT_LENGTH_EXCEEDS_THRESHOLD
  -> provider 把最终错误归类为 thinking_signature_retry 场景下的 invalid_request
```

所以，“第一次是签名问题，第二次就不是签名问题”的直接原因是：第二次请求不是同一份 body，而是经过 `build_thinking_signature_retry_body` 派生出来的 body；它会移除历史 `reasoningContent`。Kiro 在第二次看不到那个无效签名，自然不会再返回 `THINKING_SIGNATURE_INVALID`，而是继续校验后暴露了下一层问题：请求太大。

这说明两件事同时成立：

1. **签名处理确实存在稳定性问题。** 系统会把某些 Kiro 不再接受的历史 signed thinking 重新发给 Kiro，导致第一次 400。
2. **最终失败的直接根因是 outbound body admission 和上游错误分类没有统一。** 这条 5.76 MB 请求在 `on_too_long` 初始 pass 中被允许先发上游；第一次 400 是 signature，不是 too-long。之后生成的去历史 reasoning 派生 body 仍然过大，但第二次 `CONTENT_LENGTH_EXCEEDS_THRESHOLD` 被包在 provider 内部分支里，没有被统一 too-long classifier 接住，也没有进入通用 payload guard/retry 流程。

更准确的根因表述：

> 当前服务把 “签名兼容性重试” 和 “请求体过大重试/裁剪” 分散在不同特殊分支里，而不是抽象成统一的 outbound body admission 和 upstream error classifier。正确设计不是让 signature retry 自己触发裁剪；正确设计是任何即将发往 Kiro 的 body，无论原始还是派生，都先经过同一套 size/weight admission，任何 attempt 返回的 too-long 都进入同一套错误分类和可恢复处理。

## 3. 目标请求事实

生产状态：

| 项 | 值 |
| --- | --- |
| `/healthz` / `/readyz` | 均为 200 / ready |
| app 版本 | `0.0.173` |
| app revision | `9022fc7f0a868c83dc2550b7b88ed23a2237ce90` |
| runtime config version | `459` |
| payload guard mode | `on_too_long` |
| payloadGuardMaxBytes | `460800` |
| payloadGuardSafetyMarginBytes | `32768` |
| payloadGuardTrimHistory | `true` |
| payloadShaping.enabled | `true` |
| payloadShaping.discardHistoricalThinking | `true` |

目标 usage row：

| 字段 | 值 |
| --- | --- |
| `usage_records.id` | `req_01hJ2YhjMEyjrrVEQFTZpqri` |
| `data.errorId` | `req_01fLM4ouQziUTVrrCWoA9yPp` |
| 时间（UTC） | `2026-09-28 08:16:56.464526+00` |
| endpoint | `/ha/v1/messages` |
| stream | `true` |
| requested model | `claude-opus-4-8` |
| upstream model | `claude-opus-4.8` |
| credential | `#1428` |
| status | `error` |
| error type | `api_error` |
| final internal error | `upstream_failure ... reason=thinking_signature_retry` |
| public error status | `502` |
| total input tokens | `851129` |
| requested max tokens | `64000` |
| output tokens | `0` |

payload guard 记录：

| 字段 | 值 |
| --- | ---: |
| enabled | `true` |
| maxBytes | `0` |
| originalBytes | `5765288` |
| finalBytes | `5765288` |
| originalHistoryEntries | `1996` |
| finalHistoryEntries | `1996` |
| historyTrimPasses | `0` |
| removedHistoryThinkingBlocks | `0` |
| removedHistoryThinkingChars | `0` |
| stillOversized | `false` |

这里的 `maxBytes=0` 不是 UI 展示错误。`v0.0.173` 的 `on_too_long` 模式下，初始请求会构造一个 `PayloadGuardConfig { max_bytes: 0, trim_history: false }`，意思是先不裁剪，只在上游返回 too-long 类错误后再用完整 guard 配置重试。

这也解释了为什么 5.76 MB 请求在初始阶段没有被本地裁掉：第一次上游返回的是 `THINKING_SIGNATURE_INVALID`，不是 too-long，所以先进入了 signature retry。

## 4. Attempt 链路

先明确一个容易混淆的点：`usage_records` 记录的是一次客户端请求在服务端内部的完整处理结果，它可以包含多次上游 attempt。下面的 signature invalid 和 input too long 不是同一个上游 HTTP response 里同时出现的两个错误，而是同一个客户端请求内的两次上游调用结果。

Attempt 1：

| 字段 | 值 |
| --- | --- |
| credential | `#1428` |
| upstream status | `400 Bad Request` |
| action | `thinking_signature_retry_same_credential` |
| error type | `thinking_signature_invalid` |
| raw upstream reason | `THINKING_SIGNATURE_INVALID` |
| raw upstream message | `messages.1727.content.0: Invalid signature in thinking block` |

Attempt 2：

| 字段 | 值 |
| --- | --- |
| credential | 仍为 `#1428` |
| upstream status | `400 Bad Request` |
| action | `fail` |
| error type | `invalid_request` |
| raw upstream reason | `CONTENT_LENGTH_EXCEEDS_THRESHOLD` |
| raw upstream message | `Input is too long.` |

关键点：

- Attempt 2 没有再报 `THINKING_SIGNATURE_INVALID`，是因为本地 retry body 已经去掉历史 `reasoningContent`。
- Attempt 2 没有成功进入流式输出，也没有 `downstreamCommitted=true`；因此理论上它还处于可以安全改造/重试的阶段。
- 当前 `v0.0.173` 没有用统一分类器识别 Attempt 2 的 too-long terminal reason，也没有把它交给通用 payload guard/retry 流程处理。

## 5. 当前窗口同类记录

在最近 5000 条 usage 记录里，signature 相关后续结果如下：

| 类型 | 数量 | 时间范围（UTC） |
| --- | ---: | --- |
| signature retry 后 200 并最终 success | 31 | `2026-09-28 06:33:04` 到 `08:34:56` |
| signature retry 后第二次 400 `CONTENT_LENGTH_EXCEEDS_THRESHOLD` | 28 | `2026-09-28 07:20:11` 到 `08:20:51` |
| signature retry 后 200，但后续 stream read error | 6 | `2026-09-28 06:37:05` 到 `08:32:51` |
| signature retry 后 client dropped | 1 | `2026-09-28 08:32:22` |

这说明 signature retry 本身不是必然失败：31 条在剥离历史 reasoning 后成功了。目标请求所属的失败簇有更明确的共同特征：

- endpoint 都是 `/ha/v1/messages`；
- model 都是 `claude-opus-4-8`，upstream model 都是 `claude-opus-4.8`；
- credential 都是 `#1428`；
- history entries 都是 `1996`；
- final/original body 都约 `5.76 MB`；
- requested max tokens 都是 `64000`；
- attempt 1 都是 `THINKING_SIGNATURE_INVALID`；
- attempt 2 都是 `CONTENT_LENGTH_EXCEEDS_THRESHOLD`；
- body/hash/体积高度一致，说明很可能是同一条巨大会话状态被客户端或调用方反复重试。

### 5.1 28 条同类记录明细

以下 28 条均为 `THINKING_SIGNATURE_INVALID -> CONTENT_LENGTH_EXCEEDS_THRESHOLD`，没有省略同类记录：

| usage id | UTC 时间 | endpoint | model | credential | input tokens | final bytes | history entries |
| --- | --- | --- | --- | ---: | ---: | ---: | ---: |
| `req_01DcRARGDz919mt38Fy7wYvy` | `2026-09-28 08:20:51.099449` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01pFhHxvRjqD71XdmvzUgjyS` | `2026-09-28 08:20:30.96454` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01hJ2YhjMEyjrrVEQFTZpqri` | `2026-09-28 08:16:56.464526` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01kpLBhPYNdXystXswsAogZc` | `2026-09-28 08:16:51.006755` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01154h6qdm16o9orvGt8mGU4` | `2026-09-28 08:14:05.333522` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01kA89suRefRVa8b9dV1YApg` | `2026-09-28 08:13:53.052961` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01XvkoL3yCCY2G8iHL9E52uU` | `2026-09-28 08:10:38.75401` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01AgKH5wegjZP5VW7Pt7LPP5` | `2026-09-28 08:10:25.848244` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01bT9eDZpg3pNXCNkGnxY8xX` | `2026-09-28 08:09:17.38625` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01mi9YfXXLKfxF2iZpvtzcmz` | `2026-09-28 08:03:47.804371` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01o5pBbSd4Wsw2i1mDZCffsz` | `2026-09-28 08:03:37.918062` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_013x2rdwPLmkoRS59Wrxft5f` | `2026-09-28 07:58:47.672996` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01dvs4Gm1uxSoF43kMsMvzrB` | `2026-09-28 07:58:33.496719` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01BZYhpPQs7gBZm53NdfZPjV` | `2026-09-28 07:50:09.56236` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01pGCPEAegkPSgbYQ7foZBzY` | `2026-09-28 07:45:16.087586` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01cqEFv2snZUFJ1uCus6GEvU` | `2026-09-28 07:44:36.656855` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_014HMxVmWXJ4qoHKtnW8HYpT` | `2026-09-28 07:43:48.920058` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01o8KHGi4LdtbvVjSGqVRGDZ` | `2026-09-28 07:43:40.616937` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01iDfFPrzr7cp9TbZD76Bg3W` | `2026-09-28 07:37:30.034205` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_016qrz9xCa38ZDVRTb1Pjird` | `2026-09-28 07:37:20.261435` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01ScJfCPDUGZ238KZt8zsRyR` | `2026-09-28 07:34:23.846642` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01kJoGX5gSoQaXYd6EdWPidC` | `2026-09-28 07:33:35.228761` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01EYa2Ci2ztPf2rYwoje8teW` | `2026-09-28 07:31:57.033146` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01ubQ4bKiTWTHnhBHBsxrJyY` | `2026-09-28 07:31:45.285424` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01uNM7DVWDcfE5beGm4ZbNKq` | `2026-09-28 07:26:12.732412` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_0197C6yJvpZoS2sfn5hw6ddY` | `2026-09-28 07:26:01.21704` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01DnTd3y412ZzUXqr1qnW8eF` | `2026-09-28 07:20:15.620106` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 851129 | 5765288 | 1996 |
| `req_01fMEnMCLLJh3tUUJ9jmSdiQ` | `2026-09-28 07:20:11.434331` | `/ha/v1/messages` | `claude-opus-4-8` | 1428 | 850685 | 5763925 | 1996 |

## 6. 为什么第二次不再是 signature

生产 `v0.0.173` 的签名重试逻辑是：

```text
如果上游 400 且 JSON reason 精确等于 THINKING_SIGNATURE_INVALID
  -> 不换账号
  -> 用 same credential / same region
  -> 调用 retry body builder
  -> retry body builder clone KiroRequest
  -> clear_history_reasoning_content_for_compatibility_retry()
  -> 重新序列化后发送
```

`clear_history_reasoning_content_for_compatibility_retry()` 的语义是移除每个 assistant history entry 的 native `reasoningContent`。因此第二次请求里已经没有第一次报错的历史 signed thinking block。

这不是“签名被修好了”。更准确地说：

- 签名没有被重算、修复或重新签发；
- 本地只是把历史 signed thinking 从 retry 请求里拿掉；
- 如果上游允许省略这些历史 thinking，第二次可能成功；
- 如果省略后仍有其它问题，例如请求太大、tool 格式不合法、模型不支持，则第二次会暴露新的错误。

目标请求就是第四种情况：签名不再挡住校验后，Kiro 继续发现 input too long。

## 7. 是否说明签名处理存在问题

是，但要分两层看。

### 7.1 已证实的问题

目标请求第一次发给 Kiro 的 body 中有一个 Kiro 不接受的 signed thinking block，位置为 `messages.1727.content.0`。这已经足以证明当前系统在某些长会话/历史回放场景下会把无效 signed thinking 发回 Kiro。

同时，目标请求的 `payloadGuardReport.removedHistoryThinkingBlocks=0`，所以不能把这条请求的第一次 signature invalid 归因成“本次 payload guard 删除了 thinking 后造成签名坏掉”。它更像是入站历史本来就携带了 Kiro 不再接受的 signature，或者更早的转换/模型切换/历史拼装已经让 signature 和内容、模型或上下文不匹配。

### 7.2 仍需证据确认的产生机制

以下机制都可能导致第一次 `THINKING_SIGNATURE_INVALID`，但仅凭当前 usage row 不能唯一确定是哪一个：

| 可能机制 | 为什么可能 | 当前证据状态 |
| --- | --- | --- |
| 历史 signed thinking 来自不同真实模型 | signature 通常与生成它的模型强相关；当前请求是 `claude-opus-4.8`，历史中可能混入其它模型产生的 signed thinking | usage 只显示当前模型和错误位置，不含每个 reasoning block 的模型溯源 |
| 长会话历史被客户端或中间层压缩/重排 | signed thinking 是不透明块，内容、位置、关联 tool_use 改变后可能失效 | 没有目标请求的脱敏完整 Kiro body，所以不能证明 |
| 多块/交错 thinking 与 tool_use 的表示丢失顺序 | 旧分析已经指出单值 `reasoningContent` 和多块 thinking 存在结构风险 | 当前记录只显示一个报错 path，不足以判断是否多块/交错 |
| payload guard 或 sanitizer 在更早请求中改动过历史 | 如果早先某轮改动过 signed thinking，后续重放会失效 | 目标请求本轮 guard 没删除 thinking，但不排除更早轮次 |
| 客户端重复提交同一坏历史 | 28 条几乎相同的记录说明同一巨大历史状态被重复发送 | 能证明重复发生，不能证明这段历史最初由谁写坏 |

因此，根因不是“签名处理某一行代码一定错了”，而是当前系统缺少 signed thinking 的模型溯源、保护边界和可观测性，导致它只能在上游 400 后用 strip-all 兜底。

## 8. 为什么 payload guard 没挡住

这是目标请求最终失败的关键。

生产配置：

```text
payloadGuardEnabled=true
payloadGuardMode=on_too_long
payloadGuardMaxBytes=460800
payloadGuardSafetyMarginBytes=32768
payloadGuardTrimHistory=true
```

但 `v0.0.173` 的 `on_too_long` 初始策略是：

```text
initial_payload_guard_config:
  enabled = true
  max_bytes = 0
  trim_history = false
```

也就是说，第一次发送前只做轻量 repair/shaping，不做 size-limit trim。只有当上游错误被 handler 层识别成 “too long” 后，才会用完整 `payload_guard_config()` 进行一次 payload guard retry。

目标请求的顺序是：

```text
第一次上游错误 = THINKING_SIGNATURE_INVALID
  -> 进入 provider 内部 signature retry 分支

第二次上游错误 = CONTENT_LENGTH_EXCEEDS_THRESHOLD
  -> 发生在 signature retry 分支内部
  -> 被 provider 直接包装为 thinking_signature_retry 场景下的 invalid_request
  -> 没有经过统一 too-long classifier / outbound admission 流程
```

所以 `maxBytes=0` 和 `stillOversized=false` 是初始 no-limit pass 的报告，不代表 5.76 MB body 符合 Kiro 限制。它只代表本地在这次初始 pass 中没有启用 size limit。

换成一句话：**输入过长没有触发裁剪，不是因为裁剪配置没开，也不是因为 signature retry 应该自己负责裁剪；根因是 size/weight admission 和 too-long classifier 分散在不同层，派生 body 的第二次 attempt 绕过了通用处理入口。**

## 9. 为什么页面/usage 的错误容易误导

当前最终错误摘要类似：

```text
upstream_failure class=invalid_request ... reason=thinking_signature_retry
```

但真正的第二次上游 body 是：

```text
$.message: Input is too long.
$.reason: CONTENT_LENGTH_EXCEEDS_THRESHOLD
```

`reason=thinking_signature_retry` 在这里表示“错误发生在签名重试分支”，不是最终上游拒绝原因。这会让排障者误以为最终还是签名问题。

建议拆成：

```text
priorAttemptFailures:
  attempt=1
  upstreamStatus=400
  upstreamReason=THINKING_SIGNATURE_INVALID
  action=thinking_signature_retry_same_credential

terminalFailure:
  attempt=2
  upstreamStatus=400
  upstreamReason=CONTENT_LENGTH_EXCEEDS_THRESHOLD
  localPhase=thinking_signature_retry
```

这样 UI 能同时说明“为什么进入重试”和“为什么最终失败”。

## 10. 是否应该让 Claude Code CLI 继续

这个问题要按阶段判断。

### 10.1 还没向下游提交输出时

目标请求属于这种情况。`downstreamCommitted=false`，第二次是 upstream 400，尚未开始向 Claude Code CLI 输出内容。

合理设计是：服务端应尽量在内部完成安全修复，然后继续当前请求，而不是把可修复错误直接暴露给 CLI。

对这条请求来说，可以尝试的安全修复是：

1. 第一次 signature invalid 后，派生去历史 reasoning 的 retry body。
2. 任何派生 body 在发送前都进入统一 Kiro size/weight admission。
3. 如果 admission 判断超限，按 configured guard 裁剪历史或压缩可压缩内容。
4. 如果裁剪后仍超限，再返回清晰的本地 `invalid_request_error` 或 `413` 风格错误，说明请求体过大。

这比直接中断 CLI 更稳定，因为错误还没越过下游提交边界，且 too-long 属于可由本地 guard 处理的确定性问题。

### 10.2 已经向下游提交输出后

上一条请求 `req_01tA8uNUqeCYgg94MNMbNaX6` 是另一种情况：第二次已经收到 200 headers 和 reasoning frames，`downstreamCommitted=true` 后才发生 stream read error。

这时不能安全重放，因为重放可能导致 CLI 收到重复、不连续或语义不一致的输出。合理设计是发送 SSE error，明确当前 turn 失败，不伪造 `message_stop`。

### 10.3 不应该“为了不中断而伪造成功”

稳定性不是把所有错误都变成成功。适合转换成可恢复结果的，是 WebSearch 这类工具执行失败、且协议能表达 `tool_result_error` 的场景。

signature invalid / request too long 是请求协议层错误，不是模型可自然换一种搜索方式解决的工具结果。只有在服务端能安全转换 body 的前提下，才应该内部修复并继续；如果不能安全修复，就应 fail closed，并给出明确可操作错误。

## 11. 应该怎么改

以下是设计建议，不是本次已实施变更。

### P0：统一 outbound body admission 和 upstream error classifier

当前问题来自特殊分支各自发送 body、各自解释错误。建议把所有 outbound Kiro 请求统一成一个状态机：派生请求可以有来源和语义标记，但发送前 admission、上游错误分类、是否允许继续尝试，应由同一套入口处理。

```text
Original
  -> StripHistoricalReasoning
  -> GuardedAfterStripHistoricalReasoning
  -> CachePointRemoved
  -> GuardedAfterCachePointRemoved
```

每个状态都有：

- 派生原因；
- 是否改变了语义；
- 是否还允许重试；
- 是否已向下游提交；
- 使用的 credential/region；
- body bytes / Kiro weighted size；
- 进入和退出的 upstream reason。

当 `StripHistoricalReasoning` 后又遇到 `CONTENT_LENGTH_EXCEEDS_THRESHOLD`，应进入 `GuardedAfterStripHistoricalReasoning`，而不是直接终止。

### P0：每个 outbound Kiro body 发送前都跑 size/weight admission

最直接的修复路径：

```text
prepare_next_kiro_body()
  -> 得到原始或派生 KiroRequest
  -> 序列化后测 bytes / Kiro weight
  -> 如果超过限制，执行通用 Kiro payload guard
  -> 再发送
```

这里的重点不是“signature retry 触发裁剪”，而是所有准备发往 Kiro 的 body 都必须走同一套 admission。实现上应使用当前已经迁入的新 Kiro weighted guard 逻辑，而不是旧的纯 bytes 逻辑。当前本地后续版本已经有 `payloadGuardKiroMaxWeight=1,300,000`、`limitBasis=kiroWeighted`、`originalWeight/finalWeight/maxWeight` 等字段；但目标生产服务仍是 `0.0.173`，还没有这些字段。

### P0：修正最终错误归因

保留 `thinking_signature_retry` 作为 local phase，但 terminal upstream reason 必须是第二次真实原因：

```text
localPhase=thinking_signature_retry
terminalUpstreamReason=CONTENT_LENGTH_EXCEEDS_THRESHOLD
terminalPublicType=invalid_request_error
```

否则页面会继续把“为什么进了重试”和“最终为什么失败”混在一起。

### P0：对确定性重复失败做短窗口去重

28 条记录高度一致，说明同一坏 body 被反复发送。可以考虑按以下 key 做短窗口 deterministic failure cache：

```text
endpoint + model + credential/session + bodySha256 + terminalUpstreamReason
```

如果几秒/几十秒内重复命中同一个 `THINKING_SIGNATURE_INVALID -> CONTENT_LENGTH_EXCEEDS_THRESHOLD`，本地可直接返回清晰的 cached deterministic invalid_request，避免继续打上游。

这不能替代真正修复，只是降低上游压力和页面噪声。

### P1：为 thinking block 增加模型溯源和保护边界

长期修复不能只靠 strip-all。建议记录每个 native reasoning block 的：

- 真实 upstream model；
- 生成来源；
- 是否属于当前最新工具回合；
- 是否可安全省略；
- 是否是历史旧块。

然后 retry 分类：

| block 类型 | 建议 |
| --- | --- |
| 历史旧块，模型不匹配 | 可省略 |
| 历史旧块，模型匹配 | 可保留或按 guard 省略 |
| 最新工具回合，模型匹配 | 应保留 |
| 最新工具回合，模型不匹配 | fail closed，除非真实抓包证明省略合法 |
| 来源未知 | 保守处理，至少不要静默伪造成成功 |

### P1：补 request body 级别的脱敏诊断

当前 usage 能看到 `messages.1727.content.0`，但看不到该 block 的模型来源、邻近 role、tool_use 关系、是否多块 thinking。

建议在 signature invalid 时保存脱敏结构诊断：

- offending path；
- role；
- assistant history index；
- 是否含 toolUses；
- reasoningContent 类型；
- signature 长度/hash；
- upstream model；
- 该 history entry 前后各 1-2 个 role/type 摘要；
- body bytes/weight；
- guard transform chain。

不要保存正文、signature 原文、token、邮箱或凭据。

### P2：UI 分开展示 bytes guard 和 Kiro weighted guard

目标生产还是 `0.0.173`，所以页面上仍能看到旧的 `payloadGuardMaxBytes=460800`。本地后续版本已经引入 Kiro weighted 字段后，UI 应确保：

- Kiro local path 展示 `payloadGuardKiroMaxWeight` 默认 `1,300,000`；
- external/raw byte path 仍展示 byte guard；
- usage report 中显示 `limitBasis`；
- `maxBytes=0` 时明确说明是 `on_too_long initial pass`，不要让人误解为“没有限制”或“通过限制”。

## 12. 这次没有改什么

本次未做：

- 未修改生产服务；
- 未修改本地业务代码；
- 未发布新版本；
- 未改 runtime config；
- 未重启容器；
- 未修改账号状态；
- 未执行生产压测；
- 未对 v161 做数量对比。

## 13. 最小修复验收建议

后续如果实施修复，建议至少覆盖这些测试：

1. 原始请求第一次 `THINKING_SIGNATURE_INVALID`，strip historical reasoning 后小于限制，第二次成功。
2. 原始请求第一次 `THINKING_SIGNATURE_INVALID`，strip 后仍超 Kiro weighted limit，guard 后成功。
3. 原始请求第一次 `THINKING_SIGNATURE_INVALID`，strip + guard 后仍超限，返回明确 local invalid_request，不再打第三次上游。
4. 原始请求第一次就是 `CONTENT_LENGTH_EXCEEDS_THRESHOLD`，现有 `on_too_long` retry 行为不回退。
5. signature retry 后第二次为非 too-long 400，仍 fail closed，不误裁剪。
6. signature retry 后第二次为 200 并开始 stream，后续 post-commit read error 仍不重放。
7. protected latest tool-turn thinking 不被无脑剥离，或在证据不足时 fail closed。
8. usage 中 `priorAttemptFailures` 和 `terminalFailure` 都正确记录。
9. UI 中最终错误显示 `CONTENT_LENGTH_EXCEEDS_THRESHOLD`，同时保留 first attempt `THINKING_SIGNATURE_INVALID`。
10. 重复同一 body/hash 的 deterministic failure 不造成上游放大。

## 14. 最终判断

目标请求的根因可以稳定归纳为：

```text
历史 signed thinking 失效
+ signature retry 通过 strip historical reasoning 绕过签名校验
+ retry 后 body 仍过大
+ 当前 size/weight admission 与 too-long classifier 没有覆盖所有派生 outbound body
+ 错误归因把 terminal reason 掩盖成 thinking_signature_retry
```

因此，最优先的修复不是“禁用账号”或“把所有签名都删掉”，而是：

1. 统一 outbound Kiro body 的 size/weight admission 和上游错误分类；
2. 确保原始 body 与所有派生 body 都使用 Kiro weighted payload guard；
3. 改善 usage/UI 的最终错误归因；
4. 长期补 signed thinking 的模型溯源和保护边界，逐步替代 strip-all 兜底。
