# 流式 end_turn 无法区分「模型主动收尾」与「上游静默截断」

Status: `observability-gap-fixed / silent-truncation-not-proven / final-fault-gate-pending`

Severity: P1

> 本文可脱离原始排查会话独立阅读。目标：让读者仅凭本文即可理解问题现象、
> 为什么现有数据无法自证、代码层面的根因线索、以及如何自行搭环境验证。

- 状态：观测盲区**已由代码与真实调用证实**；“静默截断”本身仍未被证实。2026-07-14
  真实 `/cc`/Claude Code CLI 验证显示：成功流可落为 `sawUpstreamCompleted=false`、
  `stopReasonSource=local_inferred_*`，说明仅凭 `success/end_turn/completed` 仍不能证明上游真完成。
- 严重级别：中（潜在观测盲区 / 可能的误判，非已证实的线上故障）
- 影响端点：全部 `/v1`、`/cc/v1`、`/ha/v1`、`/na/v1`、`/dfcache/*`（共用同一套流式处理逻辑）
- 相关代码：`src/anthropic/stream.rs`、`src/anthropic/handlers.rs`、`src/kiro/model/events/assistant.rs`、`src/kiro/model/events/base.rs`

---

## 0. 一句话结论（TL;DR）

Claude Code CLI 在与本服务的多轮对话中，偶发「助手输出一句『我来做 X』式的开场白后就停住、
无后续、无报错」的现象。usage 记录显示这些轮次是 `status=success` / `stopReason=end_turn` /
`terminalReason=completed`。

**但经代码审查发现：Kiro 上游从不发送显式的 `end_turn`；代理的 `end_turn` 是一个「流结束且
无其它信号时」的兜底默认值；修复前上游真正的完成标志 `messageStatus:"COMPLETED"` 被代理丢弃、
从不落库。**

2026-07-13 已补齐观测：代理会解析 `messageStatus`，并在 usage `latencyTrace` 写入
`upstreamMessageStatus`、`sawUpstreamCompleted`、`stopReasonSource`。这不改变下游 SSE，仅用于后续判定。

2026-07-14 用本地临时 release 服务 `127.0.0.1:19138` 跑真实 `/cc/v1/messages` 与真实
Claude Code CLI 后，得到两个关键结论：

- 文档核心问题成立：真实成功轮次存在 `status=success` / `downstreamStopReason=end_turn`
  / `terminalReason=completed`，但 `sawUpstreamCompleted=false`、
  `stopReasonSource=local_inferred_end_turn`。因此，`success/end_turn/completed` 仍然只是代理
  自身推断的干净结束，不能自证上游发送过完成信号。
- 用户观察到的日文/葡语等“串台”是另一个独立问题。已在本地 Claude Code JSONL 中找到
  `続けて本体を追記する。`：该条有 `requestId`，`stop_reason=tool_use`，下一条就是
  `Edit` 工具调用。它不是“end_turn 后无工具/无后续”的静默截断，而是模型在工具调用前输出了
  非中文说明，CLI 如实展示。

修复前 usage 记录中的 `end_turn` **无法区分**两种本质不同的情况：

- **(A) 模型主动收尾**：上游正常发完内容并标记完成，然后关闭流。
- **(B) 上游静默截断**：上游只发了一段开场白就异常 EOF（没有 error 事件、没有完成标记），
  代理把它当成功、并兜底成 `end_turn`，还向客户端补发一个「干净的」`message_stop`。

在(A)和(B)下，usage 记录和下游客户端看到的字节流**完全一致**，所以「记录显示 success」
不能证明「没有发生截断」。这是一个循环论证盲区，也是本文要验证的核心。

---

## 1. 现象（完整描述）

### 1.1 表现

- 使用 Claude Code CLI（agent 模式，模型为 Opus 系列，如 `claude-opus-4-8`）连续多轮对话。
- 某一轮，助手输出一句**声明意图的短开场白**，例如：
  - “先摸清楚当前进程和端口状态。”
  - “我现在去读本机的会话历史。”
  - “先定位历史文件。”
- 然后**这一轮就结束了**：没有执行任何工具调用（没有 tool_use），没有后续文本，也**没有任何报错**。
- CLI 表现为「转了几秒就停住」（如 `✻ ... 5s`），像是任务被无声吞掉。

### 1.2 关键旁证修正（来自 Claude Code 本地会话历史）

Claude Code 会把会话落到本地 JSONL：
`~/.claude/projects/<项目路径转义>/<sessionId>.jsonl`

2026-07-14 复核本次会话 JSONL：

- 会话文件：`~/.claude/projects/-Users-yuanfeijie-Desktop-procode-kiro-rs/4633d467-317c-4620-9545-a26f2d81eb66.jsonl`
- assistant 记录数：327。
- `requestId` 缺失数：0。
- 含日文假名的 assistant 文本：4 条，其中用户贴出的 `続けて本体を追記する。` 在第 875 行。

因此，先前“assistant 消息没有 `requestId`”这条旁证在当前复核中**不成立**，应视为早期排查
脚本/字段理解误判，不能再作为证据使用。

仍然成立的旁证是：

- 会话中存在多条 `stop_reason=end_turn` 的纯文本轮次，其中一些是“我先去做 X / 先定位 Y”式
  意图开场白。
- 这些轮次是否是“模型主动把开场白作为一轮正常收尾”，还是“上游未显式完成就 EOF 被代理兜底
  成 end_turn”，单靠旧 usage 与 Claude Code JSONL 仍无法区分。

对比正常工具轮：

- assistant 先输出一段说明；
- 同一 `requestId` 下下一条 assistant 记录包含 `[tool_use:Bash]` / `[tool_use:Edit]` 等；
- 随后 user 记录包含对应 `tool_result`。

也就是说：断流轮里模型「说了要做」，但工具调用从未进入这一轮。

这句话需要严格限定为“某些 `end_turn` 纯文本轮的表现”，不能套用到所有短开场白。用户贴出的
日文例子就不是这种情况：它后面确实跟了 `Edit` 工具调用。

### 1.3 触发关联（未证实但高度相关）

多次复现都伴随「用户在 agent 正在输出/执行时插话追加输入」。怀疑与 CLI 的
**turn 续接时序**有关（当前轮被判定正常 `end_turn` 结束后，下一轮没有被自动发起）。
但这只是诱因层面的观察，不是根因结论。

---

## 2. 现网证据（已采集，只读查询）

现网机器 postgres 表 `usage_records`（关键字段：`conversation_id`、`status`、`duration_ms`、
`output_tokens`、`data` jsonb）。用本次对话的 sessionId 作为 `conversation_id` 查询：

- `conversation_id = 4633d467-317c-4620-9545-a26f2d81eb66`（Claude Code 一个 CLI 会话全程共用同一个）
- 该会话共 145+ 条记录，`status` **全部 `success`**，无 error / stream_error / upstream_timeout / client_dropped。

断流轮次（如 15:36:55 那条）的 `data` 内关键字段：

```
status                  = success
terminalReason          = completed
downstreamStopReason    = end_turn      ← 核心存疑点
outputTokens            = 77            ← 确实吐了一段短文本
firstOutputDeltaMs      = 1477          ← 首字 1.5s，不存在卡顿/静默
firstVisibleTextDeltaMs = 1477
duration_ms             = 3499          ← 3.5s 就结束
```

最近轮次按时间排列，`end_turn` 与 `tool_use` 交替出现，且**全部 completed**：

```
15:56:27  tool_use   completed   ← 正常发了工具调用
15:56:11  tool_use   completed
15:53:31  end_turn   completed   ← 纯文本收尾（疑似「断流」轮）
15:52:19  end_turn   completed
15:49:47  tool_use   completed
...
```

**注意**：这些数据本身**不能作为「没有截断」的证据**（见第 4 节循环论证）。它们只能说明：
在服务的自我记录口径里，这些轮次都被判定为成功且正常收尾。

---

## 3. 代码链路（根因线索，逐条可复查）

### 3.1 流式主循环的终止路径

`src/anthropic/handlers.rs` 中 `/cc/v1/messages`（及其它端点）的流式循环是一个
`tokio::select!`（约 5584 行起），有以下几种终止分支，**每一种都会给客户端发终止帧**：

| 终止路径 | 位置（约） | 客户端收到 | usage 记录 |
|---|---|---|---|
| 上游正常 EOF | 5738 `None` 分支 | `message_delta` + `message_stop` | success |
| 读流错误 | 5722 `Some(Err)` | SSE `error` 事件 | stream_error |
| 空闲超时(默认180s) | 5819 `idle_sleep` | SSE `error` 事件 | upstream_timeout |
| 上游 JSON 错误体 | 5739 `json_sniffer.finish()` | SSE `error` 事件 | stream_error |
| 客户端断开 | `Drop for StreamUsageGuard`(3180) | （连接已断） | client_dropped |

**重点**：代理层不存在「不发任何东西就静默掐流」的路径——异常都会发 SSE error 或触发 client_dropped。
这也是为什么「无报错、干净停住」看起来不像链路故障。

### 3.2 `None`（上游 EOF）分支的判定逻辑

`handlers.rs:5738` 起：

```
None => {                                    // 上游流 EOF
    if let Some(error) = json_sniffer.finish() { ... 记 stream_error ... }   // 有残留错误体
    if ctx.has_stream_error() {
        completion.report_upstream_stream_failure(...)                        // 流内出现过 error 事件
    } else {
        completion.report_success()          // ← 只要没显式错误，就算成功
    }
    ... generate_final_events() 补发 message_delta + message_stop ...
}
```

即：**上游连接关闭 + 期间没有显式 error → 一律记成 success**。无论上游是「说完才关」还是「没说完就关」。

### 3.3 `stop_reason` 的来源：`end_turn` 是兜底默认值，不是上游给的

`src/anthropic/stream.rs`：

```rust
// 1054：唯一的赋值入口
pub fn set_stop_reason(&mut self, reason: impl Into<String>) { self.stop_reason = Some(reason.into()); }

// 1099-1107：最终取值
pub fn get_stop_reason(&self) -> String {
    if let Some(ref reason) = self.stop_reason {   // 显式设置过
        reason.clone()
    } else if self.has_tool_use {
        "tool_use".to_string()
    } else {
        "end_turn".to_string()                     // ← 兜底默认！
    }
}
```

全项目 `set_stop_reason` 的调用**只有三处**，且只会设成：
- `max_tokens`（1067、1768、2750）
- `model_context_window_exceeded`（1704）

**没有任何一处把 `stop_reason` 设为 `end_turn`。** 结论：
- Kiro 上游**从不发送显式 `end_turn`**。
- 代理的 `end_turn` = 「流结束了 + 没 tool_use + 没 max_tokens/context 信号」时的默认兜底。

### 3.4 上游真正的完成信号 `messageStatus`（修复前被丢弃，现已记录）

修复前 `src/kiro/model/events/assistant.rs` 只保留 `content`，其它字段都被吞进 `extra`：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantResponseEvent {
    #[serde(default)]
    pub content: String,          // 只保留 content

    #[serde(flatten)]
    #[serde(skip_serializing)]
    #[allow(dead_code)]
    extra: serde_json::Value,     // messageStatus / followupPrompt 等全被吞进这里
}
```

上游 assistant 事件实际带有 `messageStatus`（值如 `"COMPLETED"`，见该文件 83 行测试用例）以及
`followupPrompt` 等字段。修复前它们被 `#[serde(flatten)]` 收进 `extra`，而 `extra` 标了
`#[allow(dead_code)]`——全项目除测试外**无任何代码读取它**。

2026-07-13 已修复为：

- `AssistantResponseEvent` 正式反序列化 `message_status`。
- `StreamContext` 记录最近一次上游 `messageStatus`，并识别 `COMPLETED`。
- 成功流结束时，usage `latencyTrace` 写入 `upstreamMessageStatus`、`sawUpstreamCompleted`、`stopReasonSource`。

**含义**：代理判断「一轮是否正常说完」的唯一依据是**上游流的 EOF**，它**根本不看上游有没有
发出 `messageStatus:COMPLETED`**。修复后下游 SSE 行为仍不改变，但 usage 记录已经能区分“有 completed 的 EOF”和“无 completed 的 EOF”。

### 3.5 三条串起来 → 核心盲区

1. Kiro 从不发显式 `end_turn`（3.3）。
2. 修复前上游真正的完成标志 `messageStatus:COMPLETED` 被丢弃、从不校验（3.4）；2026-07-13 起已记录为 usage 观测字段。
3. 上游 EOF 且无显式 error → 一律 success + 兜底 `end_turn`（3.2 + 3.3）。

所以无论上游是：
- **(A)** 发完内容 + `messageStatus:COMPLETED` 后正常关闭，还是
- **(B)** 只发一段开场白就异常 EOF（无 error、无完成标记），

代理都走同一条 `None` 分支，都记成 `success` + `end_turn` + `completed`，并向下游补发
一个「干净的」`message_delta + message_stop`。**两种情况在 usage 记录和下游字节流里完全不可区分。**

---

## 4. 为什么「usage 记录全 success」不能证明没有截断（循环论证）

usage 记录是**由同一套流处理代码写入的**。若终止判定本身有盲区（3.5），那么：

- 「上游静默截断」这一异常，会被这套代码**自己**判定并记录为 `success + end_turn`。
- 再用「记录显示 success」去反推「没有发生截断」，等于用「被质疑的代码的输出」证明
  「被质疑的代码没问题」——是循环论证。

因此，要判定是否真的发生截断，**必须引入独立于该判定逻辑的观测**（见第 6 节）。

---

## 5. 假设清单（待验证，不预设结论）

- **H1（模型行为）**：上游确实发了 `messageStatus:COMPLETED`，模型只是把「意图开场白」
  单独作为一轮正常收尾。→ 此时 `end_turn` 是**真结束**，代理无责，属模型 + CLI 续轮时序问题。
- **H2（静默截断）**：上游未发 `COMPLETED` 就 EOF（无 error 帧），代理兜底成 `end_turn`。
  → 此时是**上游/网络截断被伪装成成功**，是真实的可靠性问题，且被现有记录口径掩盖。
- **H3（混合）**：两种都存在，不同轮次成因不同。当前数据无法排除。

判定 H1 / H2 的唯一区别信号：**那一轮上游到底有没有发 `messageStatus:COMPLETED`**。

---

## 6. 复现与验证方法（关键：需要独立观测）

### 6.1 为什么单纯抓包不够

- **下游（Claude Code ↔ 代理）**：本地是 **HTTP 明文**（`127.0.0.1:9022`），tcpdump 可抓。
  但下游看到的是代理**重新合成**的输出，截断也被补成「干净结束」→ **抓下游无法区分 H1/H2**。
- **上游（代理 ↔ Kiro）**：**HTTPS**，tcpdump 抓到的是密文 → 看不了。而区分 H1/H2 恰恰
  要看上游有没有发 `messageStatus:COMPLETED`、是否发完就 EOF。

结论：**tcpdump 抓包解决不了本问题**，必须在代理内部对上游事件做观测插桩。

### 6.2 方案：本地 debug 构建加只读插桩（不改流行为、不碰现网）

在以下位置加临时 `tracing` 日志（仅本地分支，不影响线上）：

1. **上游 assistant 事件解析处**（`src/kiro/model/events/assistant.rs` 或 `base.rs` 的
   `EventType::AssistantResponse` 分支）：把 `extra` 里的 `messageStatus` 解析出来并记录。
   建议临时给 `AssistantResponseEvent` 增加 `pub message_status: Option<String>` 字段
   （从 flatten 提出来），只用于日志。
2. **流 `None`（EOF）结束分支**（`handlers.rs:5738`）：记录本轮
   - 是否曾收到 `messageStatus == "COMPLETED"`；
   - 最终 `stop_reason` 是「显式设置」还是「兜底 end_turn」（可加 bool 标记）；
   - 最后一个上游帧类型、`output_tokens`、`firstOutputDeltaMs`。
3. 落一条结构化日志（如 `target=stream_terminal_probe`），字段：
   `request_id`、`conversation_id`、`saw_completed`、`stop_reason_source`（explicit/default）、
   `last_event_type`、`output_tokens`。

### 6.3 本地复现步骤

前置：本地这台可改代码、可重启本地服务（现网只读的约束**不适用**本地）。

1. 在本地分支实现 6.2 的插桩，`cargo build --release`。
2. 起本地服务（示例，沿用现有本地配置）：
   `./target/release/kiro-rs -c config.json --credentials credentials.json`（监听 `127.0.0.1:9022`）。
   注意：本地账号仅支持 sonnet，长会话用 `claude-sonnet-4-20250514`。
3. 让 Claude Code 指向本地服务，跑一段**长的、多工具轮次**的真实会话（模拟你遇到问题的场景：
   连续 bash/编辑/重启/校验，并在 agent 执行中途插话，以尽量复现「开场白后停住」）。
   - 环境变量示例：`ANTHROPIC_BASE_URL=http://127.0.0.1:9022/cc  ANTHROPIC_API_KEY=<config 里的 apiKey>`
     （`/cc` 前缀对应 Claude Code 端点，以实际路由为准）。
4. 采集本地服务日志里的 `stream_terminal_probe` 记录，按 `stop_reason=end_turn` 过滤，统计：
   - `saw_completed=true` 的比例 → 支持 H1（真结束）；
   - `saw_completed=false`（EOF 前无 COMPLETED）的比例 → 支持 H2（静默截断）。

### 6.4 辅助：下游字节流旁证（可选）

`tcpdump -i lo0 -A -s0 'tcp port 9022'`（或用 `nc` 反代中转）抓下游明文 SSE，确认：
- 断流轮下游是否收到了完整的 `message_delta{stop_reason:end_turn}` + `message_stop`；
- 用于验证「代理无论如何都补干净结束帧」这一行为（3.2），但**不能**区分 H1/H2。

### 6.5 2026-07-14 本地真实验证结果

验证环境：

- 服务：临时 release 服务 `127.0.0.1:19138`，不触碰本地/现网主端口。
- Claude Code CLI：`2.1.197 (Claude Code)`。
- 入口：`ANTHROPIC_BASE_URL=http://127.0.0.1:19138/cc`。
- 验证产物：`target/validation/direct-cc-lang-probe.json`、
  `target/validation/claude-cli-lang-simple-now.jsonl`、
  `target/validation/claude-cli-lang-tool-now.jsonl`、
  `target/validation/claude-cli-lang-tool-repeat-summary.json`、
  `target/validation/recent-usage-19138-after-cli.json`。

真实调用结论：

1. 直接 `/cc/v1/messages` 语言采样 6 次：
   - 4 次成功，2 次 400（命中不支持模型的账号/路由问题）。
   - 成功 4 次均为中文输出，未出现日文/韩文/俄文/阿语/泰文。
   - 成功轮均为 `stop_reason=end_turn`。

2. 真实 Claude Code CLI：
   - 简单回答场景成功，输出 `正常`。
   - 工具写文件场景成功，stream-json 中有真实 `tool_use`，输出中文。
   - 重复工具场景 4 次：3 次成功，1 次在工具后的续轮遇到 400
     `Invalid model ID`（命中不支持模型的账号）；成功 3 次均未出现日文/韩文/俄文/阿语/泰文。

3. usage 观测字段：
   - 多条真实成功 `end_turn` 记录为：
     `terminalReason=completed`、`sawUpstreamCompleted=false`、
     `stopReasonSource=local_inferred_end_turn`。
   - 多条真实成功 `tool_use` 记录为：
     `terminalReason=completed`、`sawUpstreamCompleted=false`、
     `stopReasonSource=local_inferred_tool_use`。

解释：

- 这证明“代理仍可能在未观察到上游 `COMPLETED` 的情况下记录 success/completed”是真实存在的。
- 这不能进一步证明“发生了静默截断”。如果当前 Kiro 上游本来就经常不发送 `messageStatus`，
  那 `sawUpstreamCompleted=false` 只能说明 `messageStatus` 不是可依赖的完成信号，不能单独判 H2。
- 需要补强的不是下游抓包，而是上游事件观测：记录最后若干上游事件类型、是否见到明确完成/收尾事件、
  以及 EOF 前最后一次内容事件。

---

## 7. 修复 / 改进方向（按风险从低到高，均待验证后再定）

1. **解析并记录 `messageStatus`（零风险，先做）** ✅ 2026-07-13 已实施
   把 `messageStatus` 从 `extra` 提为正式字段，在流结束时落入 usage `data`
   （如 `upstreamMessageStatus`、`sawUpstreamCompleted`）。这样**不改任何行为**，但让
   「真结束 vs 截断」在记录层面**可区分**，直接破掉第 4 节的循环论证。

2. **区分 stop_reason 来源（零风险）**
   ✅ 2026-07-13 已实施。在 usage `latencyTrace` 增加 `stopReasonSource`，标明本轮结束依据，例如
   `upstream_message_status_completed`、`local_inferred_end_turn`、`local_inferred_tool_use`、`local_inferred_max_tokens`。

3. **截断检测告警（低风险，观测类）**
   当 `None` EOF 分支满足「无 error + 无 COMPLETED + 有工具定义 + 无 tool_use + output 很小」时，
   记一个 `suspectedSilentTruncation` 标记/计数（沿用问题 09 的诊断框架），不改流行为。

4. **工具上下文泄漏型 end_turn 观测（低风险，观测类）** ✅ 2026-07-14 已实施
   针对真实样本里出现的长文本异常（例如正文包含 `Tool results provided`、`Tool results:`、
   `<function_results>`、`</function_results>`、`readHash/editHash/writeHash/bashHash`，但下游仍是
   `text-only + end_turn + no tool_use`），新增独立 usage latencyTrace 字段：
   `suspectedToolContextLeakEndTurn`、`toolContextLeakMarkers`、`assistantTailIntentHint`、
   `endTurnAnomalyReason`、`endTurnAnomalyRisk`。

   该分类不保存完整正文，只保存固定 marker 名和布尔/风险值；检测过程只扫描当前 assistant/code
   内容和 4K 字符尾部窗口，不改变下游 SSE，不自动重试。

5. **Claude Code 默认 prompt steering 补强（低风险，可配置）** ✅ 2026-07-14 已实施
   在已有 `/cc` prompt steering 的任务质量提示中补充：需要读/搜/执行/编辑/调用工具时必须输出结构化
   `tool_use`，不要把 “Let me look/我先检查” 作为最终回答结束；不要把内部工具结果包装、函数结果标签、
   历史工具结果标记作为可见正文。该提示仍走原有 `promptSteering` 配置，用户自定义提示词不会被迁移覆盖。

6. **是否对下游改变行为（高风险，需极谨慎，验证后再议）**
   若确认 H2 存在且影响大，才考虑：对「疑似截断」是否应发 SSE error 而非补 `message_stop`、
   或触发换号重试（仅在首字前安全）。此方向牵涉重试安全边界与 CLI 兼容，**不在本阶段实施**。

> 明确不做：代理层自动「续轮」（破坏 Claude Code 的 turn 语义，易致重复/乱序/自续）；
> 全局无差别 system 注入（污染所有请求、破坏 prompt cache）。

---

## 8. 回归 / 验收清单

- [x] 本地真实 `/cc`/Claude Code CLI 验证成功流会出现
      `sawUpstreamCompleted=false` + `local_inferred_end_turn/tool_use`。
- [ ] 继续采集足量长会话 `end_turn` 样本（≥ 数十条），但不要只看 `saw_completed`，还要记录
      EOF 前最后若干上游事件类型，否则无法判断“上游不发 messageStatus”与“异常 EOF”的区别。
- [ ] 统计 `saw_completed` 与 EOF 前事件分布，明确 H1 / H2 / H3 哪个成立。
- [ ] 若 H1 为主：确认 `end_turn` 为真结束，问题归入 09（模型行为 + CLI 续轮），代理仅加记录。
- [x] 记录 `messageStatus` + `stopReasonSource` 到 usage latencyTrace，使截断在记录层可辨识。
- [ ] 若 H2 存在：再评估是否需要把“无 completed 的 EOF”从 success 改为可观测异常；当前不改行为。
- [x] 单测：构造「有 COMPLETED 的 EOF」与「无 COMPLETED 的 EOF」两种上游流，断言
      `sawUpstreamCompleted` / `stopReasonSource` 取值正确。
- [x] 单测确认观测插桩不改变现有下游 SSE stop_reason/message_stop。
- [x] 单测：构造「工具上下文 marker 泄漏 + 长文本 + end_turn + no tool_use」样本，断言新增
      `tool_context_leak_text_only_end_turn` 诊断命中。
- [x] 单测：构造「同样 marker + 正常 tool_use」样本，断言不误报为 end_turn 异常。
- [x] 真实服务/Claude CLI 确认当前观测字段不改变现有下游 SSE 输出语义。

---

## 9. 关联

- 问题 09（`09-intent-preamble-end-turn-no-tool-use.md`）：本问题在「H1 成立」时的表现形态与诊断方案，
  两者共用「end_turn + 无 tool_use + 有工具定义」的识别特征。
- 流式其它终止类：`02-stream-upstream-idle-timeout.md`、`06-stream-upstream-status-error.md`、
  `07-stream-internal-read-error.md`（这些是**有显式信号**的终止，与本问题「无信号静默 EOF」相对）。
- 代码位置索引：
  - `src/anthropic/handlers.rs` 流循环 `tokio::select!`（约 5584）、`None` EOF 分支（约 5738）。
  - `src/anthropic/stream.rs` `get_stop_reason`（1099-1107）、`set_stop_reason`（1054）。
  - `src/kiro/model/events/assistant.rs` `AssistantResponseEvent`（已解析 `messageStatus`）。
  - `src/kiro/model/events/base.rs` 事件分发（`parse_event`）。

## 10. 结论（当前阶段，诚实陈述）

- **已确证（代码事实 + 真实调用）**：代理仍会在未观察到上游 `COMPLETED` 的情况下，把流记录为
  `success + completed`，并根据本地状态推断 `end_turn` / `tool_use`。旧 usage 记录的
  `success + end_turn` **不能自证**没有截断。
- **已修复（观测层）**：2026-07-13 起，usage `latencyTrace` 会记录 `messageStatus` 与 stop reason 来源；这仍不改变下游 SSE 行为。
- **已补强（观测层）**：2026-07-14 起，工具上下文/函数结果标签泄漏型长文本 `end_turn` 会单独记录
  anomaly reason、risk 和 marker 分类；该补强不保存完整文本、不改变协议输出。
- **未确证（需进一步上游事件观测）**：本次会话那些 `end_turn` 轮**究竟**是 H1（模型主动真结束）
  还是 H2（静默截断）。`sawUpstreamCompleted=false` 是必要证据之一，但如果 Kiro 当前协议经常不发送
  `messageStatus`，它不是充分证据。
- **已拆分（语言串台）**：日文/葡语等偶发外语输出是真实存在的模型输出问题，但用户贴出的
  `続けて本体を追記する。` 有 `requestId` 且后续正常 `tool_use=Edit`，不属于本问题的
  “end_turn 后无工具/无后续”类别。
- **此前的错误**：先前「现网全 success，已排除代理/上游断流」的说法**过强、不成立**，属循环论证，已在本文档纠正。
