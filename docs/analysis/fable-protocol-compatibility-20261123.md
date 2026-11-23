# Fable 协议兼容分析

Role: Fable 请求协议及同类项目的只读对照

Status: 校对完成；当前不需要 Fable 专项改造

Related: [当前运行时计划](../plantree/plans/rust-runtime-scheduler-stabilization/README.md)

本文本机路径以仓库根目录为基准，Markdown 文件链接以当前文档目录为基准。

日期：2026-11-23。范围：本地 `.` 的 main，发布源码 `22568577a6fd40ca3607db5fbaae88e2b4a24cef`、当前文档提交 `a20647e1d912263ac948248a883f76a80bcee016`。k1–k3 已运行相同业务代码的 0.0.263。

方式：只读源码审计、固定提交对照、读取已有 CI 证据及线上管理 API；不修改项目代码、账号配置或服务，不发送 Fable 推理请求。参考仓库版本见[参考源码版本](../../feature/evidence/fable-protocol-audit-20261123/source-revisions.json)。

## 结论

当前项目已经具备已知 Fable 5 / 5.1 的名称解析和通用 Kiro 请求、流式响应适配。按“官方后续向账号开放 Fable”的目标，目前不需要新增 Fable 专用端点、请求体或事件解码器，也不需要照搬上游的静态名称分支。

条件是官方继续使用目前这些实现所依据的 Kiro conversationState / GenerateAssistantResponse 系列合同，以及已支持的 reasoning 字段与事件结构。代码兼容不等于已用有权限账号完成 Fable 真实推理；未来官方若新增必填字段、不同 schema 结构或新事件，应以实际目录和响应再核验。

当前线上目录没有 Fable 不能用来否定未来兼容性，也不能反推所有账号均无权限。应区分代理能力、目录同步、账号权限、实际调用四个层次。

协议兼容不代表混合账号池已经自动掌握每个账号的 Fable 权限。当前调度的模型限制顺序是：账号自身非空 `supportedModels` → 所属订阅等级的非空模型列表 → 不限制。全局模型目录及名称映射不能代替账号级筛选。

若部分账号支持 Fable、部分不支持，必须让不支持的账号受到不含 Fable 的有效模型列表限制。仅给支持账号填写 Fable、让其他账号完全不限制，不能确保隔离：选择器在支持账号可调度时优先选择它们，但这些账号满载或不可用后，空列表账号仍能进入候选。

不必逐个限制所有账号：可以在订阅等级配置不含 Fable 的完整基础模型列表，普通账号继续留空继承；少数支持 Fable 的账号设置“基础模型 + Fable”的完整自身列表覆盖等级限制。账号自身列表是替换，不是与等级列表追加合并；只填 Fable 会让该账号只参与 Fable 调度。同一订阅等级内部权限不一致时，不能仅靠等级标签推断支持能力。

## 请求和响应的差异

| 层次 | 已检查到的 Fable 差异 | 当前实现 |
| --- | --- | --- |
| 客户端入口 | 仍使用 Anthropic Messages 的 model/messages/tools/thinking/output_config；未发现 Fable 新的 API 入口 | 复用现有 Messages 入口 |
| 模型 ID | 客户端常用 `claude-fable-5-1`，Kiro 上游通常用 `claude-fable-5.1`；具体版本以账号目录为准 | 支持 Fable 家族、同版本横线/点号、thinking/[1m] 后缀、`fable` 别名 |
| Kiro 主请求 | 仍为 conversationState，modelId 位于 currentMessage.userInputMessage，配合既有 profileArn/认证 | 复用现有请求 DTO 和端点 |
| Thinking 控制 | 新家族常用 adaptive；effort 经 additionalModelRequestFields 承载 | 已支持 adaptive；由真实 schema 选择 output_config.effort 或 reasoning.effort，并按枚举选择支持档位 |
| Thinking 显示 | 与推理开关不同：display omitted/summarized/updates 决定客户端是否看到推理文本 | 已有 Fable 5 家族显示策略，显式 display 优先；它是现有代码策略，不作为未来官方默认值的独立证明 |
| 流式响应 | 在本次检查的实现中仍复用文本、toolUse、reasoningContent、metadata/metering 等事件 | 已解码原生 reasoning、签名、工具参数和用量事件；未发现 Fable 专属事件 |
| 长度限制 | 不同版本/账号的上下文与输出上限可能不同 | 目录提供 maxInputTokens/maxOutputTokens；上下文计算优先目录，模型展示及 thinking budget 修复可读取输出上限 |
| 工具图片 | 上游 Fable 支持提交同时修复了 tool_result 图片丢失 | 本项目已有独立的工具结果图片转换及回归，不是缺失的 Fable 分支 |

不能将“模型名包含 Fable”“响应 model 回显 Fable”或者“把 Fable 改路由至 Opus”当作真实 Fable 推理成功的证据。当前项目本来就能按客户端请求名回显响应 model。

## 当前项目如何承接官方后续开放

1. 现有模型同步取得参与账号的 `ListAvailableModels` 结果，保留模型 ID、输入类型、token limits、缓存能力及 additionalModelRequestFieldsSchema。
2. Fable 家族解析已存在，`claude-fable-5-1` 可以根据真实目录解析到 `claude-fable-5.1`。`fable` 别名选择目录中的对应家族，不需要写死某个版本。显式映射或关闭自动映射仍按现有配置生效。
3. `/v1/models` 根据真实目录展示 Fable，已有测试要求内置 seed 不能凭空宣告 Fable 可用。
4. reasoning 由目录 schema 解析出字段路径、effort 枚举与默认值；模型名称不是无条件发送扩展字段的依据。未发现完整能力时保留既有兼容路径，不凭 Fable 这个名字捏造 schema。
5. 沿用现有请求发送、工具转换、AWS event-stream 解码、Anthropic SSE/JSON 输出。若实际目录及事件维持已知合同，则无需为官方开放再次发版。

默认跨模型家族兜底关闭；不会仅因 Fable 不在目录就默认替换成 Opus。自定义映射和显式开启跨族兜底可以改变这一行为，应按实际配置判断。

相关本地代码：

- `src/anthropic/model_capabilities.rs`：Fable 家族、版本解析、目录同步、动态 reasoning schema、模型展示；`resolver_matches_synced_fable_models_from_official_request_forms` 与 `fable_models_are_listed_only_after_real_upstream_catalog_sync` 已覆盖。
- `src/anthropic/converter/model.rs`：名称兼容入口、schema 驱动的原生 reasoning 构造。
- `src/anthropic/handlers.rs`：adaptive thinking、thinking display、真实目录上下文优先。
- `src/anthropic/types.rs`：Thinking、OutputConfig、MessagesRequest。
- `src/kiro/model/requests/kiro.rs`：Kiro 主请求和 additionalModelRequestFields。
- `src/kiro/model/events/base.rs`、`additional.rs`：流式事件。
- `src/anthropic/converter/content.rs`：工具结果图片。

## 上游具体做了什么

上游为 `hank9999/kiro.rs`，不是其他同名仓库。检查 master `8fb3dd691d1cedf437bd6a4e9ef9f60b3f217fdd`。

[Fable 5.1 提交 fcb6e7ad](https://github.com/hank9999/kiro.rs/commit/fcb6e7ad9d9dc44a4615313ebd27cddc628710ab) 在 2026-10-08 合入，Fable 部分为：

- `map_model` 增加 contains("fable") 分支，统一写为 `claude-fable-5.1`。提交注释称上游实测拒绝横线形式和 `claude-fable-5`；这是该项目当时的观察，不能推广为所有账号和未来版本的固定规则。
- 上下文函数加入 Fable 5.1 的 1M 返回值。
- `/v1/models` 静态加入 `claude-fable-5-1` 和 thinking 变体，max_tokens=128000。
- thinking 后缀处理将 Fable 归入 adaptive。
- 同一提交修复 tool_result 中图片被丢弃：提取图片后放入 UserInputMessage.images。这部分是通用多模态修复。

没有在该提交增加 Fable 专用 HTTP 路径、认证方式、conversationState 结构、AWS event-stream 帧或解码器。所以上游“增加支持”的主要原因是旧版本映射函数会拒绝或无法识别 Fable 名称，不是它出现了一套全新传输协议。

另查 `refactor/v2`：`932a89fb13d66ec18794e64f459326365af09b12`，2026-08-10；该分支是更旧的精确映射/静态列表实现，未包含上述 Fable 5.1 支持。不能把 master 和 refactor/v2 的支持状态混为一谈。

## 同类项目与 fork 对照

| 项目与固定提交 | 实际改动/状态 | 是否证明需要新的 Fable 协议 |
| --- | --- | --- |
| `Quorinex/Kiro-Go` main `f8f6071c9298a4266ad3e0c7e483d4a2510cbcaf` | 未见 Fable 专项代码或提交。未知 `claude-*` 可透传；横线转点号的正则和 1M 上下文正则仅包含 opus/sonnet/haiku，没有 fable。因此点号原名透传不等于完整的 Fable 名称及上下文适配 | 否，也不能将该版本说成已完整适配 Fable |
| 同项目 dev `d42f42e6e7a86b311f2c3c1315a31e626a7e5971` | 2026-05 的更旧别名表，未发现 Fable 专项实现 | 否 |
| `hj01857655/Kiro-Go` 默认 feature/react-admin `d035fe81bf3c59ad34ce20831bb6204adc7294ae` | 未找到 Fable/Mythos 处理，不作为完整支持证据 | 否 |
| `ZyphrZero/kiro.rs` master `c37fe5afe7ee3adba836abfcee9a97aba5ea54f9` | 最初加入 Fable 5 映射、静态列表、1M、xhigh；当前通用 Claude 家族规范化含 fable，原生 effort 判定含 fable-5；同批 Enterprise SSO 是账号认证能力 | 否，主要是名称、参数及认证能力；当前上下文硬编码仍仅列 fable-5，不能全量照搬 |
| `liuran001/kiro.rs-admin` master `f9d8401a094a44c50a6305fa01db06fd75ce1d38` | 延续 Fable 5 名称/上下文/原生 effort 改造，另有版本格式归一；仍有 contains(fable) 的固定旧版本回退 | 否，固定版本假设会随官方上线变化 |
| `TsinHzl/kiro2cc-proxy` master `4dd9b67884f79669efbb3a8aa66b9e94b876e56d` | [8e75f638](https://github.com/TsinHzl/kiro2cc-proxy/commit/8e75f6386e1743d6c1683de12223c32b9408d5b7) 增加 Fable 5.1 名称/静态模型表、输出上限 128K、缓存门槛 512；仍使用既有 Kiro 主请求和流事件 | 否，额外区别是参数边界和缓存策略 |
| `ndycode/kiro-rs` main `529fc2f852d4a5895457652721b75157948f1981` | `claude-fable-*` 直接改为 `claude-opus-4.8`，测试明确要求该结果 | 这是替代路由，不能算原生 Fable 支持 |
| `freebattle/kiro.rs`、`ykn1002/kiro.rs` 本次默认分支快照 | 未找到 Fable/Mythos 专项代码 | 不作支持证据 |

ZyphrZero 与部分 fork 注释称 Fable/Mythos 同底座；本次没有以官方模型身份材料证实该关系，不用这些注释推导两者必然共享版本、价格、权限或全部协议。

## 有差异，但不应强行改造的地方

### 1. 原生 max_tokens 控制

TsinHzl 的本地转换会在部分模型的 additionalModelRequestFields 中带 `max_tokens`，对 Fable 采用 128K 上限，并按其 Kiro 观察将最小值抬到 1024。当前项目的 AdditionalModelRequestFields 强类型结构承载 thinking/output_config/reasoning，没有该 max_tokens 字段；当前本地 Kiro 路径的长度控制与该项目不同。

这是现有本地协议适配的通用差异，不能从“Fable 支持”推导它是 Fable 成功请求的必填字段。上游的 Fable 5.1 支持提交也没有要求加该字段。当前目录可以展示真实输出上限，并在相关入口修复逻辑中读取它，但不应据此声称已把 max_tokens 原样发送至 Kiro。

当前建议：保留现状；官方 schema 或可用账号证明需要原生上游长度控制时，再单独按模型能力设计，而不是给所有 Fable 请求强塞 max_tokens/128K/1024。

### 2. 512 与 1024 的缓存门槛

TsinHzl 为 Fable 5/5.1 使用 512；当前 `min_cacheable_tokens_for_model` 对 Fable 走默认 1024。上游 DTO 能读 minimumTokensPerCacheCheckpoint，但这个值没有贯穿到当前该缓存估算函数。

这会影响小前缀的本地缓存估算/模拟资格，不是 HTTP 传输或模型推理能否成功的协议障碍。不能因另一个 fork 用 512 就调整当前 usage、缓存或成本行为，尤其不应混入刚完成的 rawUsage 改动。

当前建议：等真实 Fable 目录给出缓存能力，再决定是否需要通用的目录驱动缓存门槛改进。这里保留为差异记录，不立刻修改。

### 3. 未来未知字段与事件

当前 schema 解析器覆盖已知 output_config.effort / reasoning.effort 路径，并非任意 JSON Schema 解释器；MessagesRequest 与 Kiro 扩展字段也不是对所有未来字段的无损透传器。因此“官方以后开放已知 Fable 模型”预计可自动承接，“官方以后引入全新协议”仍需具体核验。没有已知新增必填字段时，不为假设增加代码分支。

## 线上只读观察

2026-11-23 读取 k1–k3 的 `/api/admin/model-capabilities`：source 都为 `kiro-list-available-models`，各 23 个模型，Fable 条目为空；目录最近同步分别在 02:52:26、02:51:44、02:51:27 UTC 左右。三个实例 bodyConversion.nativeReasoningFields 均启用。

外部池中明确声明 `claude-fable-5` 的 yuenan / chaozhiyuanai 当前停用。这不证明其它未限制 supportedModels 的外部池不能服务 Fable，也不代表未来官方本地 Kiro 能力。外部池是 Anthropic 兼容 HTTP 转发，须按对方暴露的 model ID / 映射配置判断，不能套用本地 Kiro 点号命名。

没有发送 Fable 真实推理，没有切换/启用账号，没有启动抓包。官方开放后可在目录同步完成时用有权限账号做少量流式、非流式和工具回合验证，不需要先做强制代码改造。

## 验证证据与建议

已发布 main 源码的 [完整 CI 45096986363](https://github.com/2ue/kiro.rs/actions/runs/45096986363) 中，Fable 名称、动态列表、thinking display、schema reasoning 和工具图片相关回归在默认/无默认特性均通过；精简记录见[现有 CI 回归记录](../../feature/evidence/fable-protocol-audit-20261123/existing-ci-regressions.txt)。这是现有回归证据，不是本次新增测试，也不是有权限账号的真实 Fable 推理证明。

建议现在保持代码不变。官方开放后优先核验真实目录里的精确 ID、token limits、reasoning schema、账号可调度性，再验证普通文字、thinking 显示及工具多轮是否按既有链路完成。只有实际出现 schema/事件/参数要求差异时，才做最小的通用能力修复。
