# 强制映射"输入过长"被改成 529，客户端死循环重试（2026-11-18）

时间均为北京时间。

## 一、现象

59142 下游反复收到：

```
The model service did not respond in time. Please try again. (request-id: req_01vXF55UsU8tJTHy56ydzJiC)
错误类型 api_error / 内部状态码 400 / 错误阶段 local_account
内部错误：upstream_failure class=invalid_request upstream_status=400 ... reason=CONTENT_LENGTH_EXCEEDS_THRESHOLD
上游：{"message":"Input content length exceeds threshold.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}
```

这不是超时，而是上游判定输入过长。强制映射分支（`map_provider_error_inner`）为了不暴露真实窗口，把它改成了 529 `overloaded_error`。客户端把 529 当成临时错误原样重试，同样的输入每次都失败。

样本请求（11-18 15:03:05）：客户端请求 `claude-opus-4-8`，实际走 `claude-haiku-4.5`；请求体 534KB，本地估算 182214 token（退避预算 182217，刚好裁进去）；历史 4 条占 530KB，几乎全是中文正文；发了两次（第二次是裁剪后重试），都被拒。

## 二、影响（11-18 前 24 小时）

| 实例 | 次数 | 占该实例全部错误 | 会话数 |
|---|---|---|---|
| 59140 | 6004 | 66% | 83 |
| 59142 | 2986 | 55% | 74 |
| 59143 | 2939 | 89% | 72 |

真正卡住的会话不多，但每个会话平均重试约 40 次。例如会话 `86b258d0-…` 14:50–14:54 四分钟内同一请求发了 33 次，没有一次成功。每次失败还多打一次上游。

## 三、根因：两类请求，各占一半左右

按会话去重后：

| 实例 | 历史里有大段文本 | 工具定义过大 | 其他 |
|---|---|---|---|
| 59140 | 38 | 35 | 10 |
| 59142 | 38 | 34 | 2 |
| 59143 | 37 | 30 | 5 |

1. **历史里有大段文本**（多为中文，约 17 万汉字）。强制退避按本地估算裁到预算，但本地估算（每个汉字算 1 token）比上游实际计数偏少，发出去仍超。上游拒绝后的重试只跑 payload guard：它只会整轮删历史，删不动受保护的开头和与当前工具结果配对的那一轮，也不截断历史里的文本块，结果什么都没删。
2. **工具定义过大**：平均 388 个工具、640KB，单个工具最大 148KB，而用户正文只有几十字节。裁历史没用（还会把历史删光），现有的工具压缩只省约 7%。客户端压缩也没用，因为压缩不会减少工具定义。

另外，上游的判定标准不只看 token 数：本地估算 19 万的请求有成功的，14.6 万的反而失败（工具 schema、代码、中文的切分比例差很多）。所以单靠调一个固定系数不可靠，要靠"被拒后缩小再试"来兜底。

## 四、Claude Code 怎么识别"输入过长"（2.1.283 源码）

- `rC(e)`：错误文本（不区分大小写）含 `prompt is too long` 或 `input is too long for requested model` 就算，不看状态码。命中后统一变成内容为 "Prompt is too long" 的 API 错误消息。
- 主循环看到这条消息，在自动压缩开启时走被动压缩（`reactive-compact`）。
- `N tokens > M maximum` 只被 `getPromptTooLongTokenGap` 用来估算"压缩请求本身也超长时每次多裁多少"；没有数字时退回每次多裁一组（`gap_unparseable`），压缩照常进行。
- 会话只有一轮、没东西可压时，提示"单轮会话无法压缩，体积主要来自系统提示、工具定义或附件"。
- 413 `request_too_large` 被归为"媒体过大"，走去图片的流程，不会压缩上下文，不适合用来表示输入过长。

结论：**触发自动压缩和不暴露 token 数字可以兼得**，返回不带数字的 `prompt is too long` 即可。

## 五、修复

1. **发送前**（`forced_backoff::fit_to_budget`）：
   - 新增第 0 步：工具定义超过预算的 `toolBudgetRatio`（默认 0.3）时分级压缩（描述 2000→800→300→120 字；参数说明截短→去掉；长枚举截到 64/32 项；最后一级把第二层以下的嵌套 schema 收成只留 `type`）。工具名和参数名不变，只看工具本身，同一会话每轮结果相同。
   - 截断最大内容块时也考虑系统提示块。
2. **上游判定过长后**：服务端按 `本次预算 × retryBudgetRatio`（默认 0.65）重新跑一遍退避、重新转换再发一次（`ForcedRefit`，只为接近预算的请求保留一份副本），替代原来只会整轮删历史的重试。同时按会话记住缩小后的比例（6 小时有效，连乘不低于 0.25），这个会话后续请求第一次就按它裁剪，不再先失败一次。
3. **仍然超长时**按 `tooLongResponse` 答复客户端，两种都不带 token 数字，不暴露服务窗口：
   - `prompt_too_long`（默认）：400 `invalid_request_error`，`prompt is too long: the request exceeds the maximum input length. ...`，请求 ID 放在 `x-error-id` 头里；
   - `overloaded`：旧的 529。
   usage 记录里的对外错误和实际返回一致。

配置位置：`forcedModelMapping.contextBackoff.{tooLongResponse, retryBudgetRatio, toolBudgetRatio}`，管理界面"强制模型收敛 → 上下文退避"里可改。

## 六、上线后怎么验证

- `usage_records` 里 `error_message like '%CONTENT_LENGTH_EXCEEDS_THRESHOLD%'` 的次数和会话数应明显下降；同一会话不再出现几十次连续失败。
- 日志里出现 `re-fitting it to a smaller budget` 后应紧跟成功；出现 `fitted a conversation the upstream rejected as too long earlier` 说明记住的预算在起作用。
- 仍然失败的请求，对外应是 400 `prompt is too long`（`publicErrorStatusCode=400`），之后同一会话应出现 Claude Code 的压缩请求（"create a detailed summary of the conversation"）。
- 工具定义过大的会话，日志里 `compressed_tools` 大于 0，且请求成功。
