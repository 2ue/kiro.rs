# 05 已知问题

本次改造过程中发现的问题。第 1–3 条在本次一起修复，其余只记录，后续单独处理。

| # | 问题 | 证据 | 影响 | 处理 |
|---|---|---|---|---|
| 1 | 提示词总开关同时控制 `-thinking` 模型名解析、thinking 触发方式和强制可见 thinking | `parsed_body_pipeline.rs:30-47`、`handlers.rs:13659,13778` | 关掉总开关后，`claude-xxx-thinking` 不再进入 thinking 模式，发给 Kiro 的 `display` 也跟着变 | 本次修复：拆到"路由 → 功能"（02 第 2 节） |
| 2 | 分块写入策略、tool_choice 提示、thinking 提示各有两道开关（`bodyConversion.*` 和 `promptSteering.*`），两道都开才生效，页面上看不出来 | `converter.rs:162-194` | 运营只关一道以为已经关了，或者只开一道以为已经开了 | 本次修复：每项只保留一个开关 |
| 3 | 分块写入策略（A7）只看请求有没有 system，不看有没有 Write/Edit 工具 | `converter/history.rs:80` | 普通 API 请求也会被加上"静默遵守 Write/Edit 限制"这段话 | 本次修复：只在声明了 Write/Edit 时注入 |
| 4 | 渠道信息输出过滤（F2）的正则太宽 | `identity_guard.rs:26-72`，例如 `(我\|你).{0,20}kiro`、`\b(initial\|original\|earlier\|previous\|first)\s+(system\s+)?(instructions?\|prompt\|message\|preamble)\b`、`(提到\|声称\|说).{0,8}(kiro)` | 开启后，讨论 kiro.rs 项目、或者正常提到"previous message"的句子会被整句删掉，而且客户端不知道。流式下要等整句检查完才下发，首字也会变慢 | 记录。页面在开关旁写明误删风险。后续要用线上真实泄露样本和"讨论 kiro 代码"的样本做双向回归，再收窄正则 |
| 5 | F2 的兜底文本是写死的 "I'm Claude, an AI assistant made by Anthropic." | `handlers.rs:7370` | 整段回复都被删光时，客户端会收到一句和问题无关的话 | 记录。和第 4 条一起处理，考虑把兜底改成可配置，或者改为触发一次重试 |
| 6 | 分块写入策略（A7/A8）的阈值 150/50 行没有实测依据 | 仓库里没有 Kiro 截断 Write 输入的证据；`feature/issues/protocol-conversion-audit-20260928/14-…` 的分析也没有 | 300 行的文件会变成 1 次 Write 加 5 次 Edit，往返次数和 token 成本成倍增加；中途被打断会留下半截文件和占位符 | 记录。用真实 Kiro 账号分别写 200、500、1000、2000 行，确认上游是否截断，再决定默认值和阈值 |
| 7 | strict 模式对注入项的覆盖不一致 | `converter.rs:158-194`、`prompt_steering.rs:24` | A5/A6/D1/E1 在 strict 下照样注入，其余项不注入。"strict 不做兼容改写"的说法和实际不符 | 记录。本次保持现状，页面逐项标注。是否统一由用户决定 |
| 8 | 空轮续写提示（E1）伪装成 Claude Code 的 `<system-reminder>` 格式 | `empty_turn_nudge.rs:10` | 模型会把它当成客户端发来的提醒；如果模型复述出来，F1 清洗不了，因为 F1 不认识这个标签 | 记录。本次给它加开关和可编辑文本。F1 是否覆盖这个标签需要先看线上有没有复述样本 |
| 9 | Write/Edit 的描述在追加后缀之前就被截到 10000 字符，截断时没有标记，也没有告警 | `converter/tools.rs:709` | 客户端给的超长描述被静默截断 | 记录。属于工具描述保真度问题，不在本次范围 |
| 10 | `/v1/models` 的 handler 拿不到路由 | `handlers.rs:7348` | "只显示 Claude 模型"没法按路由设置 | 记录。按路由设置需要改 router 把入口前缀传进来，这次先做成全局开关 |
