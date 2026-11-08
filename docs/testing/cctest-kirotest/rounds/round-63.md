# Round 63：提示词注入审计与抓包复测

## 结果

- 时间：2026-11-08 00:55–00:58（北京时间；抓包时间 UTC 16:55–16:58）
- 路由：`/dfcache/tune-ss6-99`
- 模型：`claude-opus-5-5`
- 代码：无源码改动，59139 debug 热更新实例
- 结果文件：`results/20261107T165556-round-63-capture.json`
- 总分：`82/110`
- 失败项：`stream_structure=5`、`websearch=0`、`signature_proto=5`
- `output_config=10`，其余保护项全部通过

## 抓包

- 目录：`logs/llm-capture/capture-20261107T-round-63-capture-20261107T165556Z-f866fbae/`
- 原始服务端会话：`logs/llm-capture/capture-20261107T165556Z-f866fbae/`
- 事件行：`9501`
- JSON 错误：`0`
- 丢弃事件：`0`
- 上游：`25/25 HTTP 200`
- 客户端请求：`22`
- 客户端完整流：`22/22`
- `message_start/message_delta/message_stop`：`22/22/22`
- `content_block_start/content_block_stop`：`55/55`
- WebSearch 路由事件：`38`

## 提示词注入证据

59139 Admin preview 显示 tune 路由关闭了身份、任务质量、语言、custom、WebSearch guidance、工具选择、空轮恢复等注入；`thinkingControls` 仅显示协议项 `not_triggered`。上游 marker 统计：

- `<system-reminder>`：客户端 17 个，上游 17 个，属于 CCTest/Claude Code 自带内容；
- `<thinking_mode>`：上游 16 个，属于客户端 thinking 请求/兼容控制；
- `<assistant_identity>`、`<prompt_steering>`、`<web_search_guidance>`、`<language_reminder>`、`<thinking_output_policy>`、工具选择提示：均为 0。

结论：代理提示词注入没有在本轮额外改变测试路由；客户端自己的 system/history 仍被保留并进入 Kiro。提示词注入是评分敏感面，但当前不能把它当作三项失败的直接根因。

## 请求矩阵结论

- 8 个 thinking + tool；
- 3 个 thinking + text；
- 3 个 tool continuation；
- 6 个 plain stream（其中 1 个最终 tool use）；
- 1 个 server tool/WebSearch；
- 1 个 non-stream。

除 non-stream 外，所有请求都记录 `upstreamEofWithoutCompleted=true`，本地按内容推断 `local_inferred_end_turn` 或 `local_inferred_tool_use`；客户端生命周期全部闭合。该事实继续支持 EOF/COMPLETED 是 `stream_structure` 候选，但没有失败样本 ID，暂不改状态机。

## 参考资料

- 本项目 `src/model/prompt_injection/catalog.rs`、`src/anthropic/prompt_injection.rs`、`src/anthropic/converter.rs`：本地注入定义和实际注入点，2026-11-08 北京时间核对。
- 本项目 `src/anthropic/converter/thinking.rs`：Opus 5.x 对可见 thinking policy 的过滤与空响应降级，2026-11-08 北京时间核对。
- 本项目 `docs/testing/cctest-kirotest/analysis/issues.md` I-25/I-26/I-42/I-43：既有 A/B 和请求级证据。

## 结论

Round 63 复核最高分仍为 `82/110`。本轮不改源码、不改 59138、不改 Caddy、不改生产账号状态；保留原始抓包，继续按提示词单变量实验与评分差异证据推进。
