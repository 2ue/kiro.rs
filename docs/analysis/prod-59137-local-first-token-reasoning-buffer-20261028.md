# 现网 59137 本地账号首字慢根因分析（2026-10-28）

范围：只看本地账号路由（usage `routeKind=local_credential`）。外部池不在本次范围内。
现网操作：只读查询 Postgres usage 记录和容器日志，未改配置、未重启。镜像 0.0.223（`19d05c3e`）。
时间口径：UTC。

## 结论

1. 首字慢的主因在我们自己：Kiro 的原生推理（`reasoningContentEvent`）几秒内就开始返回，但 `src/anthropic/stream.rs` 把整段推理缓存在内存里，等到正文、工具调用或流结束才一次性下发。首字因此被推迟到推理结束。
2. 这个缓冲是为了"签名推理被污染时整块丢弃"。现网 81952 条本地流式记录里，签名推理被污染导致的重试是 0 次，污染报错是 0 次，超过 1 MiB 缓冲的报错也是 0 次。它防的情况没有出现，代价却落在每个带推理的请求上。
3. 修复：原生推理改为边收边发，新增部分过增量清洗器后立即下发，签名在块结束时发送。污染照样被清洗器扣住，并以协议污染错误结束本轮。修复后用现网额度快用完的账号真实测试，同一题目首字从约 28s 降到 4.7～7.7s，基本等于上游响应头时间。
4. 去掉缓冲也解决不了的部分：超长输入导致的上游响应头慢，以及首个账号返回空流后的换号。

## 现网数据

### 最近 20 分钟（10-28 08:40～09:00）

本地账号流式成功 922 条：

| 指标 | 数值 |
| --- | ---: |
| 首字 p50 | 7.2s |
| 首字 p90 | 34.7s |
| 首字 ≥10s | 339 条 |
| 首字 ≥20s | 175 条 |
| 首字 ≥60s | 64 条 |

按 latencyTrace 拆这 175 条 ≥20s 的请求：

| 类型 | 条数 | 平均首字 | 平均响应头 | 平均"收到流后到出字" | 平均输入 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 上游早就开始返回，我们迟迟不出字 | 139（79%） | 98.8s | 4.3s | 93.3s | 20.3 万 |
| 上游响应头本身就慢 | 34（19%） | 30.6s | 28.8s | 1.7s | 39.6 万 |
| 两者都有 | 2 | 27.2s | 12.4s | 14.2s | 18.2 万 |

第一类里 137 条是 opus-5-5。出第一个字之前，上游平均已经发来 1244 帧，`upstreamEventTypesBeforeFirstOutput` 全部是 `reasoning_content`。发了 thinking 的那部分，第一个字出现在整个请求耗时的 93% 处。

### 按天趋势

| 日期 | 首字前有原生推理的占比 | 这组首字 p50 | 这组首字 p90 | 无推理组首字 p50 |
| --- | ---: | ---: | ---: | ---: |
| 10-23 | 12% | 10.6s | 33.8s | 1.2s |
| 10-24 | 37% | 8.3s | 52.9s | 1.4s |
| 10-25 | 41% | 9.6s | 44.0s | 1.2s |
| 10-26 | 51% | 10.3s | 51.7s | 1.8s |
| 10-27 | 58% | 11.1s | 94.0s | 2.5s |
| 10-28 | 58% | 11.9s | 96.8s | 3.1s |

最近几天变差，是因为带原生推理的请求占比在涨，基本都是 opus-5-5（10-27 有 9650 条，10-28 截至 09:00 有 7468 条）。

### 其他发现

- 最近 20 分钟有 98 条（约 14%）的第一个账号返回空流（日志 `upstream eventstream ended without any frames`），换号后才成功。这批首字 p50 是 31.6s，没换号的是 7.8s。容器日志里首字前换号原因：空流 636 次，只返回 usage 547 次。
- 最近 20 分钟有 164 条上游在推理、但我们没下发 thinking。对 opus-5.5 来说，Claude Code 默认请求（`thinking.type=adaptive`、不带 `ultrathink`）在现网路由配置下走 XML thinking 兼容路径。上游不发 `reasoningContentEvent`，所以不受本问题影响。现网的原生推理请求来自显式 `effort` 或 `ultrathink`。

## 根因

修复前的代码（0.0.223）：

- `process_reasoning_content`（`src/anthropic/stream.rs:2924`）只把推理文本合并进 `native_reasoning_content`，不产生任何下游事件。
- `close_native_reasoning_block`（`src/anthropic/stream.rs:3661`）只在正文、工具调用或流结束时被调用。它把整段推理一次性过清洗器，签名块被污染就整块丢弃，否则一次性发出 start、单个 `thinking_delta`、`signature_delta`、stop。
- `MAX_BUFFERED_ATOMIC_THINKING_BYTES = 1 MiB`，推理超过上限整轮报错。

这样设计的原因：Kiro 的推理带签名，签名对应原文，客户端下一轮会原样回传给 Kiro 校验。推理里如果混进内部工具协议文本，带签名的块不能删改，只能整块丢弃，所以要拿到完整的块再决定。

不合理之处：

- 现网从未触发过整块丢弃或超限（见结论 2）。
- 清洗器本身就是增量的：可疑片段会被扣住，直到确认是普通文本才放出，私有协议文本不会被放出去。XML thinking 路径早就是边收边发（`push_xml_thinking_delta`）。
- 整块丢弃也不是真正的兜底：被污染时整轮本来就以协议污染错误结束，客户端不会把这段 thinking 写进历史。

## 修复

分支 `fix/stream-native-reasoning-early-output`：

- `process_reasoning_content`：每个事件用 `merge_native_reasoning_text` 算出新增部分（累计快照只取后缀，增量片段直接追加），过清洗器后立即下发 `thinking_delta`。第一段安全文本到达时才开 thinking 块。
- `close_native_reasoning_block`：放出清洗器暂存的安全文本，然后发 `signature_delta` 和 stop。本段被拦截过内容（包括签名里带协议文本）时不发签名，拦截计数会让本轮以协议污染错误结束。
- 签名有效性：清洗器没拦截时，输出与输入逐字一致，原生签名照常有效。测试用逐字符、跨行、代码围栏、CRLF、看起来像协议行的普通文本固定了这一点。
- 去掉 1 MiB 整段缓冲上限，只保留当前段累计文本用于快照去重。
- 流结束时先关闭仍打开的推理块，再处理暂存文本和工具块，保证内容块不重叠。
- 非流式路径不变（本来就是整体返回）。

代价：第一个推理字节一发出，就不能再透明换号重试。首字前的重试几乎都是"上游一帧都没发"和"只返回 usage"，都发生在任何推理字节之前，不受影响。

## 验证

### 单元和集成测试

- 新增 `handler_native_reasoning_reaches_client_before_reasoning_ends`：上游先发两段推理，停 3 秒再发正文。要求 1.5 秒内收到 thinking、不带签名，签名在正文之前，usage 的首字小于 1.5 秒、正文晚于 3 秒。修复前的代码上这个测试失败（"thinking was held until reasoning ended"），修复后通过。
- 新增或改写的 stream 单测：每个事件立即下发且不重复、超过 1 MiB 的推理逐段下发、干净签名推理逐字一致、污染时扣住协议文本和签名并报错、签名字段污染时不发签名。
- 全量 `cargo test --bin kiro-rs`：2701 通过，0 失败，16 忽略。`cargo fmt --check` 通过。Clippy 基线通过（745，基线 849）。

### 现网账号真实测试

账号 #1972（KIRO PRO MAX，已用 98.1%，并发 5，RPM 20，代理 #172）。现网配置表导入本地隔离库。代理经 152.53.243.159 转发，出口 IP 与现网一致（64.32.179.40）。本地副本的 refreshToken 是假值，后台刷新关闭。路由 `/dfcache/mine-haa`，模型 `claude-opus-5-5`，`effort=high`，提示词带 `ultrathink`，同一道 12 枚硬币题：

| 版本 | 响应头 | 首个 thinking | thinking_delta 数 | 首个正文 | 总耗时 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 修复前（`51a9deb1`） | 7.8s | 28.0s | 1 | 28.0s | 39.5s |
| 修复前（`51a9deb1`） | 5.2s | 28.0s | 1 | 28.6s | 45.1s |
| 修复后 | 7.7s | 7.7s | 192 | 26.1s | 38.3s |
| 修复后 | 4.7s | 4.7s | 139 | 22.0s | 37.6s |

修复前的 usage `upstreamEventTypesBeforeFirstOutput` 是 `{"reasoning_content": 128}` 和 `179`，与现网慢请求形态一致。修复后为空，首字等于响应头时间。

其他现网账号场景（修复后）：

- `/dfcache/mine-haa` 和 `/dfcache/minea` 上的 opus-5.5、sonnet-5.5、thinking enabled、thinking 关闭：全部 200，`end_turn`，有 thinking 时签名 1 个，内容块无重叠。
- 账号并发上限内 5 并发：全部 200，各 1 个签名，无报错。
- 真实 Claude Code 2.1.283（`claude -p --model claude-opus-5-5 --effort high`，带工具，多轮）：修 bug 和实现函数两个任务都成功，3 个 thinking 块都带签名，签名回传 Kiro 后没有签名失效重试。本地 33 条请求里 `thinking_signature_retry` 为 0。

### 本地 JSON 账号回归（不经代理）

`kiro-credentials-2026-09-16T12-11-07-416Z.json` 里 30 个 social 账号，导入后自动刷新 token 并探测余额：30 个有效，25 个有余额（KIRO FREE，共 418.6 额度）。可用模型：claude-haiku-4-5、claude-sonnet-4-5、claude-sonnet-4-6、sonnet-thinking、deepseek-3.2、glm-5、minimax-m2.1/m2.5、qwen3-coder-next。这些账号的 thinking 走 XML 兼容路径，用来确认其他场景没有回归：

| 场景 | 结果 |
| --- | --- |
| sonnet-4.5 / haiku-4.5 / sonnet-4.6 流式，thinking 开和关 | 全部 200，`end_turn`，开 thinking 时有签名，无块重叠 |
| sonnet-thinking 模型名 | 200，有 thinking 和签名 |
| 非流式 thinking enabled | thinking 块带签名，正文正常 |
| 12 并发（sonnet/haiku 混合、thinking 开关混合） | 12/12 成功，用到 14 个账号，无重试 |
| 真实 Claude Code 工具多轮（sonnet-4.5、haiku-4.5） | 都成功，thinking 都带签名 |
| Claude Code `--resume` 续接 | 9 轮成功，8 个 thinking 块都带签名 |
| `thinking.display=omitted` | 有 thinking 块和签名，无 thinking_delta，正文正常 |
| `tool_choice: none` | 无 tool_use，正文没有提到策略 |

本地两个实例共 55 条 usage，全部 success。

## 遗留问题（与本次修复无关）

- `tool_choice` 注入文案在 thinking 里被复述（`tool_choice_policy`），修复前后都有，与首字修复无关。已单独修复，见 [tool_choice 注入策略在 thinking 中被复述](../../feature/issues/tool-choice-policy-leak-in-xml-thinking-20261028.md)。
- 首个账号空流后换号的请求，首字 p50 是 31.6s，需要单独查是哪些账号、什么原因返回空流。
- 超长输入（40 万 token 左右）的上游响应头慢，是 Kiro 侧的问题。
