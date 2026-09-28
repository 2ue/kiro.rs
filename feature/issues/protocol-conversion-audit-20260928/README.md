# Claude Code 协议与 Kiro 协议互转审计问题索引（2026-09-28）

Role: 2026-09-28 协议互转审计的单问题文档索引
Status: documented / 各问题独立跟踪，状态以各文档 `Status:` 为准

## 项目定位与方向约定

kiro.rs 以 Kiro 为后端，对外提供 Anthropic 协议（Claude Code 协议）接口，任何兼容该协议的客户端都可以接入，例如 Claude Code CLI。

- 请求方向：客户端发出 Claude Code 协议请求，由 kiro.rs 转换成 Kiro 协议（`conversationState`），再调度到最终上游 Kiro。
- 响应方向：Kiro 返回的 event-stream 被转换回 Claude Code 协议（SSE 或 JSON），再返回给客户端。
- "官方协议对照"只是规范参照：一边是 Claude Code 协议要求接口做到什么，另一边是 Kiro 协议对上游请求的要求。Anthropic 官方 API 不是本项目的上游。
- thinking 签名是 Kiro 原生签名。kiro.rs 在双向转换中原样透传签名，保证 Claude Code CLI 回放历史时 Kiro 能校验通过。

usage 与缓存模拟（token 放大、cache 命中统计）属于产品策略，按要求不纳入本目录。

## 文档结构

每份问题文档包含：问题与影响、官方协议对照、源码链与根因、复现（单测与端到端）、修复方案（候选与推荐）、测试与验收、兼容性与风险。

## 修复计划

- [00 当前协议问题修复计划与进度](00-current-protocol-fix-plan.md)：由并行实现会话维护，记录 P22 等问题的实际修复与真实 CLI 验证进度。

## 问题列表

| 编号 | 问题 | 严重度 | 方向 |
| --- | --- | --- | --- |
| [P01](01-context-window-error-message-not-official.md) | 上下文超限错误文案不是 `prompt is too long` 格式，Claude Code 无法触发自动 compact | High | response |
| [P02](02-thinking-end-tag-strict-double-newline.md) | `</thinking>` 后必须紧跟 `\n\n` 才会被识别为结束，否则正文被整段吞进 thinking | High | stream |
| [P03](03-payload-guard-silent-context-trimming.md) | payload guard 静默裁剪上下文（system prompt 最先被删），客户端无感知 | High | request |
| [P04](04-tool-result-document-and-media-blocks-stringified.md) | tool_result 内的 document/search_result 被整块字符串化，嵌套 image url/file 返回 400 | High | request |
| [P05](05-pascalcase-tool-names-rewritten-with-hash.md) | 合法工具名被改写成 `xxxHash<8hex>`；已补充 Kiro/Mantle 真实校验报文和设计 | Medium | request |
| [P06](06-image-only-history-user-empty-content.md) | 只含图片的历史 user 消息 content 为空 | Medium | request |
| [P07](07-non-stream-duplicate-tool-use-dedup.md) | 非流式按 name+input 去重，合法的重复工具调用被丢弃 | Medium | response |
| [P08](08-stream-content-length-exceeded-becomes-error-event.md) | 流式 ContentLengthExceeded 变成 error 事件，与非流式 max_tokens 不一致 | Medium | stream |
| [P09](09-xml-thinking-whitespace-loss-and-false-detection.md) | XML thinking 模式吞空白，正文中的 `<thinking>` 被误判 | Medium | stream |
| [P10](10-request-fields-and-beta-headers-silently-dropped.md) | stop_sequences、temperature、context_management 等字段以及 anthropic-beta 被静默忽略 | Medium | request |
| [P11](11-models-endpoint-openai-shape.md) | `/v1/models` 返回 OpenAI 风格结构 | Medium | aux |
| [P12](12-count-tokens-estimate-inconsistent.md) | count_tokens 为估算值，与 messages 口径不一致 | Medium | aux |
| [P13](13-websearch-emulation-incomplete.md) | web_search 模拟不完整（query 来源、citations、encrypted_content） | Medium | aux |
| [P14](14-prompt-steering-alters-tool-behavior.md) | 提示注入与 chunked_write 改变 Claude Code 工具行为 | Medium | request |
| [P15](15-tool-choice-prompt-only-enforcement.md) | tool_choice `any` 只靠提示词约束，`disable_parallel_tool_use` 被忽略 | Medium | request |
| [P16](16-xml-thinking-block-missing-signature.md) | XML 提取出的 thinking block 没有 signature | Low | stream |
| [P17](17-error-status-and-envelope-deviations.md) | 错误状态码和错误信封与协议有偏差（404/529/空 body 等） | Low | aux |
| [P18](18-stream-event-ordering-details.md) | 流式 block 关闭顺序、tool_use 块并行打开、空 text 块 | Low | stream |
| [P19](19-model-id-silent-upgrade-mapping.md) | 模型 ID 被静默升级，显式 4.1 被映射成 4.5 | Low | request |
| [P20](20-history-role-alternation-edge-cases.md) | history 角色交替的边界（前导 assistant、伪造的 "OK"） | Low | request |
| [P21](21-tool-schema-and-result-field-fidelity.md) | 工具 schema 与 tool_result 字段保真度 | Low | request |
| [P22](22-consecutive-assistant-native-reasoning-merge-rejected.md) | 连续 assistant 各带原生 reasoning 时本地前置 400（usage 页面样本） | High | request |
| [P23](23-native-reasoning-multi-block-conversion-gaps.md) | P22 之外的原生 reasoning 多 block 互转缺口 | Medium | request + stream |
| [P24](24-history-server-tool-and-search-result-blocks-dropped.md) | 历史中的 server_tool_use、web_search_tool_result、search_result 被丢弃 | Low | request |
| [P25](25-thinking-parameter-validation-gaps.md) | thinking 参数校验缺口 | Low | request |

## 维护规则

- 修复落地后，需要同步更新对应文档的 `Status:`、"测试与验收"结果和本索引。
- 发现新问题时，使用下一个可用编号新建文档，不要把新问题并入已有文档。
