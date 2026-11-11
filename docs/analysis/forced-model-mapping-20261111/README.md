# 强制模型收敛（Forced Model Mapping）分析与方案

日期：2026-11-11　状态：已实现并验收（分支 `feat/forced-model-mapping`），验收记录见 [10-implementation-checklist.md](10-implementation-checklist.md) 第 15 节

## 需求

1. 新增一个独立开关，打开后把所有 Claude 模型请求（如 `claude-opus-5-5`、`claude-fable-5-1`、`claude-sonnet-5`）强制改发到可配置的目标模型（默认 Sonnet 4.5 / Haiku 4.5）。
2. 它和现有 `modelMapping` 的跨族解析、跨族兜底、同族就近都没有关系。是一层独立的强制映射，现有解析、调度、转换的核心逻辑不改。
3. 对外不暴露真实模型。范围包括接口字段、响应头、错误文案、模型列表、公开页，以及模型自己说出来的内容。
4. 模型转换后能补齐的协议差异尽量补齐。补不齐的（例如上下文窗口）要退避，不能报错。
5. 用知识库补全 Haiku 4.5 / Sonnet 4.5 的自我认知和新知识，不能露馅。
6. 每个外显模型映射到 4.5 时都有专项处理，保证功能看起来正常、不报错、尽可能完整执行。

## 结论

- **能做，而且可以不动核心链路。** 做法是在入口新增一层"外显模型 → 目标模型"的改写。改写后的目标模型是目录里真实存在的模型，现有解析器会精确命中，后面的调度、冷却、转换、重试都沿用现状。
- **外显模型保持客户端原样。** 现在响应 `model`、usage 的 `model` 本来就用客户端模型，强制映射只要守住这一点，再补上错误文案、公开页、`/v1/models` 和模型自述这几处漏点。
- **协议差异大部分能在转换层补齐**，包括 max_tokens、thinking/effort、display、采样参数、中途 system、context_management、工具类型、图片尺寸。
- **上下文窗口补不齐，只能退避。** 客户端以为是 1M，实际只有 200K。服务端按 L0–L5 分级退避，强制模式下不再对外返回 `prompt is too long`。
- **知识可以部分补上，模型水平补不了。** 用知识库补自我认知（型号、窗口、知识截止）、Claude 生态知识，以及收录范围内的新事实。没收录的内容按"不确定"回答或去搜索，不编造。推理和编码水平补不了，这属于产品层风险，需要如实告知。
- **每个外显模型都有专项处理。** 包括 thinking 默认值、display 默认值（含 Fable 的 updates 进度说明）、Opus 5.5 与 Fable 工具调用之间的文字、逐条消息 effort 与 clear_at、computer toolset、截图坐标换算等。

## 文档索引

| 文件 | 内容 |
|---|---|
| [01-current-state.md](01-current-state.md) | 现状：模型解析链路、账号模型过滤、响应与计费口径、已有能力补全点、已知风险 |
| [02-design.md](02-design.md) | 总体设计：三种模型名语义、插入点、配置结构、规则语义、与现有逻辑的边界 |
| [03-capability-completion.md](03-capability-completion.md) | 能力补全矩阵：输出上限、thinking/effort、协议字段、工具、图片/PDF、tokenizer |
| [04-context-window-backoff.md](04-context-window-backoff.md) | 上下文窗口：客户端行为、退避分级、缓存稳定性、usage 口径 |
| [05-concealment.md](05-concealment.md) | 不暴露真实模型：暴露面清单、身份与自述、无法消除的残留指纹 |
| [06-rollout-and-acceptance.md](06-rollout-and-acceptance.md) | 实施分期、改动清单、验收矩阵、待决策项与风险 |
| [07-knowledge-completion.md](07-knowledge-completion.md) | 知识库补全：K0 自我认知卡、K1 生态速查、K2 时事事实、K3 实时兜底；检索、注入、usage 口径、维护与验证 |
| [08-per-model-handling.md](08-per-model-handling.md) | 逐模型专项处理：每个外显模型映射到 4.5 时的 thinking、display、工具、视觉、自述处理，以及输出侧的改写顺序 |
| [09-gaps-and-mitigations.md](09-gaps-and-mitigations.md) | 遗漏点与对策：请求链路、对外暴露、usage、运维、行为一致性、测试 |
| [10-implementation-checklist.md](10-implementation-checklist.md) | 实现清单（实施基准）：决策定稿、逐任务的文件、做法与验收，验收记录 |

## 依据

- 代码基线：`main` @ `acfdd10c`。
- 模型能力参考 Anthropic 官方模型表：Opus 5.5 / Fable 5.1 / Sonnet 5 为 1M 上下文、128K 输出、adaptive thinking；Sonnet 4.5 / Haiku 4.5 为 200K 上下文、64K 输出，只支持 `budget_tokens` 形式的 thinking，不支持 `effort`。
- Claude Code 行为参考官方文档 `code.claude.com/docs/en/env-vars`、`model-config`、`errors`。
