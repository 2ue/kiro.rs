# 语言约束提示词首语言锁定

Status: `analysis-confirmed / compressed-summary-and-concurrent-matrix-not-reproduced / implementation-not-authorized`

Severity: `P1`

Last reviewed: 2026-08-02 Asia/Shanghai

## 范围与现象

用户观察到“语言约束”提示词似乎永远以第一次语言为准，可能导致后续对话即使用户改用中文，模型仍持续使用英文。当前尚未确认“第一次”指：

- 第一次用户消息语言；
- 第一次请求的语言；
- 服务启动或会话创建时的语言；
- 固定的 Claude Code 系统提示词语言；
- 某个会话缓存或压缩后的 system prompt。

因此本问题先记录为待复现，不把“永远英文”直接归因到语言检测算法。

## 用户可见影响

- 多语言会话中回答语言与当前用户消息不一致。
- Claude Code/SDK 长会话或压缩后语言约束可能继续沿用旧语言。
- 语言约束提示词如果被重复追加，可能与用户当前语言产生冲突。

## 当前源码链

2026-08-02 源码核对结果：

- `src/model/config.rs` 的默认“语言约束提示词”是静态文本，要求模型使用“用户最新消息的主要语言”；源码没有首语言变量、语言检测结果缓存或按会话保存的语言状态。
- `src/anthropic/prompt_steering.rs::apply_to_system` 在每个请求处理时检查提示词标记；未发现标记时把同一段代理提示插入 `system` 开头。若 Claude Code 客户端把上一轮的 `system` 原样带回，则标记会阻止重复追加，但不会把语言值写入服务端状态。
- `messages`、流式入口和 `count_tokens` 共用运行配置和提示词规则；当前代码没有“第一次语言优先”的分支。
- 路径是否注入提示词由运行配置的 `scope/routeMode/routeRules` 决定，不能把 `/cc` 当成语言状态作用域。
- 本轮未发现进程级或全局语言缓存；仍需在真实失败样本中确认 Claude Code 自身的会话系统提示、压缩恢复或上游模型行为是否造成表象锁定。

## 2026-08-02 首轮复现结果

本轮使用本地 `127.0.0.1:9022`、本地凭证、模型 `claude-sonnet-4.5`，未触碰现网：

| 场景 | 结果 | 证据 |
| --- | --- | --- |
| 直接 HTTP：英文首轮 -> 中文第二轮 | 通过，返回“测试” | request id `msg_01brGJWtB8Z81L4uYrGRuHgZ` |
| 直接 HTTP：中文首轮 -> 英文第二轮 | 通过，返回“beta” | request id `msg_01brcWpJqSCvb7fAaFTuZH5H` |
| 直接 HTTP：中文 `system` + 英文用户消息 | 按用户明确要求返回“gamma”，未观察到首语言强制覆盖 | request id `msg_012xtHtuU6e3c65Mr7wBtT7w` |
| 直接 HTTP：40 轮中英文交替长历史，最后要求中文 | 通过，返回“长会话” | request id `msg_01pkofTozKs7CJdYJ5H4pBiV` |
| 真实 Claude Code CLI 同一会话：英文首轮 -> 中文第二轮 | 通过，返回 `alpha` 后 `测试` | CLI session `fa1dad3b-25e6-4d8f-898a-83d3d081cdd7` |
| 真实 Claude Code CLI 同一会话：中文首轮 -> 英文第二轮 -> 中文第三轮 | 通过，返回 `甲`、`beta`、`丙` | CLI session `1a224f60-3c6c-42a8-a66a-caee4dfab9c7` |
| `count_tokens` 中文 `system` + 英文用户消息 | 接口正常返回输入 token 计数 | `POST /cc/v1/messages/count_tokens`，`input_tokens=963` |

结论：本轮没有复现“语言约束永远以第一次语言为准”。当前证据不支持直接修改语言提示词或增加无条件的服务端语言覆盖。

## 2026-08-02 压缩摘要模拟与并发会话补充

在同一台本地服务上补充了两类边界：

- 模拟 Claude Code 压缩后的旧 system 摘要：英文旧摘要 + 最新中文请求返回 `中文`；中文旧摘要 + 最新英文请求返回 `english`。
- 两个并发会话首语言相反：英文首轮的会话切换到中文返回 `并发`；中文首轮的会话切换到英文返回 `parallel`，没有互相污染。

请求 ID：

- `msg_01UjbHgAhTLYibLJMSd6UaYM`
- `msg_01EhR1kPqeWhVTzLPPmsedG4`
- `msg_01MeP8MaBp8bFotMxvYUe25E`
- `msg_01kbjXHia73Brwap1SQYfSAu`

详细命令和 Usage UI 浏览器证据见 [Language And Usage Focused Validation](../evidence/language-and-usage-cleanup-focused-validation-20260802.md)。

这里的“压缩”是重复较长旧 system 摘要的协议模拟，不等同于真实 Claude Code 自动 `/compact` 已达到上下文阈值；真实自动压缩边界仍需受控长上下文或用户异常样本，不因本轮模拟通过而宣称关闭。

## 仍未关闭的验证

- 真实 Claude Code 自动压缩/自动裁剪阈值之后的 CLI 交互仍未触发；本轮已完成等价旧摘要协议模拟。
- 两个并发会话首语言相反的本地 HTTP 矩阵已通过；共享高缓存/重试的更高压力矩阵仍未运行。
- 尚未对照用户实际失败请求的完整“会话 ID”“请求 ID”“模型（请求）”“模型（本地解析）”“模型（上游）”和脱敏 system prompt。
- 若未来捕获异常样本，必须先区分：客户端发送的固定系统提示、服务端重复/遗漏合并、压缩恢复旧历史、上游模型偏离静态约束，不能仅凭最终回复语言归因于服务端首语言锁定。

## 复现矩阵

至少验证以下会话：

1. 英文首轮 -> 中文第二轮 -> 英文第三轮；
2. 中文首轮 -> 英文第二轮 -> 中文第三轮；
3. 首轮仅 system prompt 指定语言、用户消息不指定；
4. 长历史压缩前后切换语言；
5. 同一会话不同请求路径；
6. 并发两个会话，首语言不同；
7. `messages` 与 `count_tokens` 的 system prompt 是否一致。

每个样本记录页面上的“模型（请求）”“模型（本地解析）”“模型（上游）”、会话 ID、请求 ID 和最终 system prompt 摘要，不记录原始敏感内容。

## 根因候选与方案边界

候选根因包括全局缓存、会话级固定状态、提示词追加顺序、语言检测只执行一次、压缩后恢复旧 system prompt，以及客户端自身的语言约束。未完成证据前不选择“每轮强制切换”或删除语言提示词，因为这可能破坏稳定输出和 Claude Code 系统指令。

## 验收矩阵

- 语言状态的作用域明确为请求、会话或全局之一，并有代码/测试证据。
- 当前语言切换符合产品规则，长历史和压缩后不回退到第一语言。
- 不同会话互不污染。
- `messages`、流式、重试和 `count_tokens` 的语言约束行为一致。
- 真实 Claude Code CLI 和直接 HTTP 均有最小矩阵验证。

## 修复后结果

当前无代码修复：短会话、长历史、真实 Claude Code CLI 基础会话、旧摘要协议模拟和并发会话均未复现首语言锁定。保持现有“匹配最新用户消息主要语言”的静态提示词，等待真实自动 compact 边界或用户异常样本后再决定是否改动。

## 残余风险与回滚

- 语言检测本身可能不可靠；应保留显式用户语言指令优先级。
- 若客户端显式要求固定语言，服务端不能无条件覆盖。
- 改动必须可通过运行配置关闭或恢复历史行为。
