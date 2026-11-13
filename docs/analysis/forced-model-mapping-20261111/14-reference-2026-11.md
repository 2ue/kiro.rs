# 14 参考资料（2026-11-12 核对）

本篇只记官方原文能核实的事实，供规格表、知识条目和验收使用。

来源前缀：`P` = https://platform.claude.com/docs/en/ ，`C` = https://code.claude.com/docs/en/

## 1. 模型规格（P `models/overview` 及各模型 overview 页）

| 模型 | ID | 窗口 / 输出 | 知识截止（可靠 / 训练） | 发布 | 价格 输入/输出（$/MTok） | 缓存读 | 思考：不传 / 可关闭 | API effort 默认 | display 默认 |
|---|---|---|---|---|---|---|---|---|---|
| Fable 5.2 | claude-fable-5-2 | 1M / 128K | 2026-06 / 2026-06 | 2026-11-09 | 8 / 40 | 0.20 | adaptive / 不可 | high | omitted |
| Mythos 5.2（需申请） | claude-mythos-5-2 | 1M / 128K | 2026-06 | 2026-11-09 | 8 / 40 | 0.20 | adaptive / 不可 | high | omitted |
| Opus 5.5 | claude-opus-5-5 | 1M / 128K | 2026-06 | 2026-09-22 | 4 / 20 | 0.20 | adaptive / 不可 | medium | omitted |
| Sonnet 5.5 | claude-sonnet-5-5 | 1M / 128K | 2026-06 | 2026-09-28 | 2 / 10 | 0.10 | adaptive / 只能 `between_tools`（effort ≤ high） | high | omitted |
| Haiku 5.5 | claude-haiku-5-5 | 1M / 128K | 2026-06 | 2026-10-07 | ≤100K：0.10 / 0.50；>100K：0.50 / 2.50 | 0.01 / 0.05 | adaptive / effort ≤ high 时可 | medium | omitted |
| Fable 5.1（legacy） | claude-fable-5-1 | 1M / 128K | 2026-06 | 2026-09-01 | 10 / 50 | 0.25 | adaptive / 不可 | high | omitted |
| Mythos 5.1 | claude-mythos-5-1 | 1M / 128K | 2026-06 | 2026-09-01 | 10 / 50 | 0.25 | adaptive / 不可 | high | omitted |
| Fable 5（legacy） | claude-fable-5 | 1M / 128K | 2026-01 | 2026-06-09 | 10 / 50 | 1.00 | adaptive / 不可 | high | omitted |
| Mythos 5 | claude-mythos-5 | 1M / 128K | 2026-01 | 2026-06-09 | 10 / 50 | 1.00 | adaptive / 不可 | high | omitted |
| Opus 5（legacy） | claude-opus-5 | 1M / 128K | 2026-05 | 2026-07-24 | 5 / 25 | 0.50 | adaptive / effort ≤ high 时可 | high | omitted |
| Sonnet 5（legacy） | claude-sonnet-5 | 1M / 128K | 2026-01 | 2026-06-30 | 2 / 10 | 0.20 | adaptive / 可 | high | omitted |
| Opus 4.8（legacy） | claude-opus-4-8 | 1M / 128K | 2026-01 | 2026-05-28 | 5 / 25 | 0.50 | 不思考 / — | high | omitted |
| Opus 4.7（legacy） | claude-opus-4-7 | 1M / 128K | 2026-01 | 2026-04-16 | 5 / 25 | 0.50 | 不思考 / — | high | omitted |
| Sonnet 4.6（legacy） | claude-sonnet-4-6 | 1M / 128K | 2025-08 / 2026-01 | 2026-02-17 | 3 / 15 | 0.30 | 不思考 / — | high | summarized |
| Opus 4.6（legacy） | claude-opus-4-6 | 1M / 128K | 2025-05 / 2025-08 | 2026-02-05 | 5 / 25 | 0.50 | 不思考 / — | high | summarized |
| Opus 4.5（legacy） | claude-opus-4-5 | 200K / 64K | 2025-05 / 2025-08 | 2025-11-24 | 5 / 25 | 0.50 | 不思考 / — | high（只有 low/medium/high） | summarized |
| Haiku 4.5（legacy） | claude-haiku-4-5 | 200K / 64K | 2025-02 / 2025-07 | 2025-10-15 | 1 / 5 | 0.10 | 不思考 / — | 不支持 | summarized |
| Sonnet 4.5（deprecated，2026-11-30 退役） | claude-sonnet-4-5 | 200K / 64K | 2025-01 / 2025-07 | 2025-09-29 | 3 / 15 | 0.30 | 不思考 / — | 不支持 | summarized |

补充说明：

- 缓存读一般按输入价的 10% 计价；Fable 5.2 / 5.1 为 2.5%，Opus 5.5 / Sonnet 5.5 为 5%。
- Fable 5.2 fast mode 价格为 $16 / $80。
- Batch 一律五折。

## 2. 协议限制（P `build-with-claude/thinking` "Limits and feature compatibility"）

- **采样参数**：Fable/Mythos 全系、Opus 5.5 / 5 / 4.8 / 4.7、Sonnet 5.5 / 5、Haiku 5.5 收到非默认的 temperature / top_p / top_k 时返回 400。API 版本 `2026-10-21` 起，只要带上这些参数，所有模型都返回 400。
- **prefill**：4.6 及之后的模型拒绝以 assistant 结尾的请求。
- **forced tool_choice**（any / tool）：Opus 5.5、Sonnet 5.5、Fable 5.2 / 5.1、Mythos 5.2 / 5.1 一律 400；其它模型在 adaptive 思考下可用。
- **`budget_tokens`**：4.7 及之后的模型和 5.x 返回 400；4.6 已弃用但仍可用；4.5 及更早只支持这种形式。
- **effort 档位**：
  - xhigh：Fable/Mythos 5.x、Opus 5.5 / 5 / 4.8 / 4.7、Sonnet 5.5 / 5、Haiku 5.5；
  - max：再加上 Opus 4.6、Sonnet 4.6；
  - Opus 4.5 只有 low / medium / high。
- **display**：取值 `summarized` / `omitted` / `updates`。`updates` 是 beta，需要 `thinking-display-updates-2026-08-18`。
- **工具调用之间的进度说明以 thinking 块返回**：Fable 5.2 / 5.1 / 5、Mythos 5.2 / 5.1、Opus 5.5、Sonnet 5.5。
- **对话中途的 system 消息**：Fable / Mythos 全系、Opus 5.5 / 5 / 4.8、Sonnet 5.5、Haiku 5.5 支持；Sonnet 5 不支持。
- **视觉**：高分辨率（最长边 2576px）适用于 Claude 4.7 及之后的模型；其余模型为 1568px。
- **tokenizer**：Opus 4.7 起使用新 tokenizer，同样的文本多出约 30% 的 token。
- **最小可缓存长度**：Opus 5.5 / 5、Sonnet 5.5、Haiku 5.5 为 512。

## 3. Claude Code（C `model-config`、`llm-gateway-protocol`）

- **别名**：

| 别名 | 指向 |
|---|---|
| `opus` | Opus 5.5 |
| `sonnet` | Sonnet 5.5（2.1.284 起） |
| `haiku` | Haiku 5.5（2.1.293 起） |
| `fable` | Fable 5.2（2.1.321 起） |
| `best` | 与 opus 相同 |
| `default` | Opus 5.5 |
| `opusplan` | 计划阶段用 opus，执行阶段用 sonnet |

  官方没有 `auto` 这个模型别名。

- **effort 默认值**：Fable 5.2、Opus 5.5、Sonnet 5.5、Haiku 5.5 为 medium；Opus 4.7 为 xhigh；其余为 high。
- **网关下的上下文**：识别出的模型按官方窗口处理（Fable、Sonnet 5 及之后、Haiku 5.5、Opus 4.7 及之后为 1M），客户端检测不到网关实际的限制。自动压缩阈值约为窗口 − min(max_output, 20K) − 13K（1M 时为 967K）。
- **超长恢复**：只认 `prompt is too long: N tokens > M` 等官方措辞，识别到后会被动压缩并重试；遇到 `max_tokens` 会自动续写，最多 3 次。
- **tool search**：2.1.304 起在网关下默认开启，会发出 `defer_loading` 和 `tool_reference`；网关拒绝时客户端会去掉这些字段重发。
- **WebSearch**：客户端会另发一个辅助请求，携带 `web_search_20250305` 服务端工具。
- **子代理与后台请求**：发送的是别名解析后的新 ID（sonnet-5-5、haiku-5-5、fable-5-2）。后台请求不带完整的系统提示。

## 4. 退役时间表（P `about-claude/model-deprecations`）

| 模型 | 状态 |
|---|---|
| Sonnet 4.5 | 2026-09-30 宣布弃用，2026-11-30 退役，建议迁移到 claude-sonnet-5-5 |
| Opus 4.5 | 不早于 2026-11-24 |
| Opus 4.1 | 2026-08-05 已退役 |
| Opus 4、Sonnet 4 | 2026-06-15 已退役 |

Sonnet 4.5 在 Anthropic 官方 API 上 11-30 退役，不影响本服务的上游（Kiro）。但上游目录如果随之下线这个模型，强制收敛的默认目标需要切换，而目标模型在配置里可以改。

## 5. 2026-06 之后的重要发布（P `release-notes/overview`）

| 日期 | 事件 |
|---|---|
| 06-09 | Fable 5、Mythos 5 |
| 06-30 | Sonnet 5 |
| 07-24 | Opus 5 |
| 09-01 | Fable 5.1 / Mythos 5.1；display `updates`；单条消息 effort |
| 09-22 | Opus 5.5 |
| 09-28 | Sonnet 5.5（`between_tools`） |
| 10-07 | Haiku 5.5 |
| 10-21 | API 版本 2026-10-21（移除采样参数） |
| 11-02 | Message threads（beta） |
| 11-09 | Fable 5.2 / Mythos 5.2、Fable 5.2 fast mode |
