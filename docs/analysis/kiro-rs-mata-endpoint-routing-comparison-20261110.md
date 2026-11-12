# 当前项目与 kiro-rs-mata 多端点调度对比（2026-11-10）

## 结论

当前项目已经具备两类容易被混称为“端点轮换”的能力：

1. `ide` / `cli` 是协议端点，决定请求 URL、请求头、请求体和上游协议。
2. `us-east-1` / `eu-central-1` 是 API Region，决定同一协议端点实际访问的区域地址。

当前项目已经实现第二类 Region 轮换，但还没有实现 `kiro-rs-mata` 那种“同一个账号内按候选协议端点依次 fallback”的完整调度。当前项目通过凭据的 `endpoint` 字段选择一个协议端点，选定后整次请求使用该端点；瞬态失败时可以切 Region，也可以按已有账号调度器切换凭据。

因此，`kiro-rs-mata` 的端点回退逻辑可以移植，但应作为同一凭据内的候选端点链加入现有重试管线，不能直接替换当前的 Region 轮换，也不能原样复制其错误码判断和端点集合。

## 对比范围

本次分析对照了：

- 当前项目 `src/kiro/endpoint`、`src/kiro/provider.rs`、`src/kiro/retry_pipeline.rs`、`src/model/config.rs`、Admin 类型与两个管理前端；
- 本地参考目录 `/Users/yuanfeijie/Desktop/procode/kiro-rs-mata` 的 `src/kiro/provider.rs`、`src/model/config.rs`、Admin API 和设置页面；
- 当前项目已有的账号身份与上游调度分析文档 `docs/kiro-account-identity-and-upstream-scheduling-v161-v167-20260920.md`。

这份文档记录代码结构和行为判断，没有引入实现改动。

## 当前项目的端点模型

### 协议端点由凭据选择

当前项目的 `KiroEndpoint` 注册表目前包含 `ide` 和 `cli` 两个实现。每个实现负责自己的 API URL、MCP URL、请求头和请求体转换。`KiroProvider::endpoint_for` 按以下顺序解析端点：

```text
凭据.endpoint > config.defaultEndpoint
```

对应代码：

- `src/kiro/endpoint/mod.rs:14-18`：注册 `cli` / `ide`；
- `src/kiro/provider.rs:10394-10403`：根据凭据选择端点实现；
- `src/model/config.rs:5805-5807`：全局默认端点；
- `src/kiro/model/credentials.rs:216-221`：凭据级 `endpoint` 字段。

API Key 凭据在导入和规范化时默认使用 `cli`，见 `src/kiro/model/credentials.rs:385-395` 和 `src/kiro/model/credentials.rs:777-785`。这意味着不能把所有协议端点都视为同质候选：CLI 是专门的 runtime 协议，不能无条件回退到 IDE。

### 当前已有的是 Region 轮换

`kiroUpstreamRegionRotationEnabled` 是独立的 Region 轮换开关，默认关闭。开启后，程序内置两个候选区域：

```text
us-east-1
eu-central-1
```

轮换只覆盖 API Region，不改变 Token 刷新的 Auth Region。顺序按账号的有效 Region 决定：`eu-*` 账号优先 `eu-central-1`，其他账号优先 `us-east-1`。

相关实现位于：

- `src/kiro/retry_pipeline.rs:39-150`：候选 Region、轮换顺序和策略快照；
- `src/kiro/endpoint/mod.rs:289-309`：`RequestContext.region_override` 及 Region 解析优先级；
- `src/kiro/provider.rs:13590` 附近：把本次轮换得到的 Region 注入请求上下文；
- `src/kiro/provider.rs:15728-15780`：狂暴模式和普通模式的 Region 推进；
- `src/model/config.rs:5443-5455`：Region 轮换配置字段。

普通模式下，Region 轮换只影响下一次瞬态重试，不改变账号故障转移语义。狂暴模式则遍历“账号 × Region × 轮次”，且与 Region 轮换开关分开控制。

### 当前请求的实际调度层次

当前本地请求可以抽象为：

```text
请求
  -> 选择一个凭据
  -> 根据凭据.endpoint 选择一个协议端点（ide 或 cli）
  -> 根据 Region 轮换策略确定 API Region
  -> 发送请求
  -> 瞬态失败时按现有重试/换号/换 Region 逻辑继续
```

其中“协议端点”在一次账号尝试内是单值；“Region”才是当前已经存在的同账号切换维度。

## kiro-rs-mata 的端点回退模型

`kiro-rs-mata` 在 `KiroProvider` 中额外维护运行时端点路由配置：

- `preferred_endpoint`：全局首选端点；
- `endpoint_fallback`：是否在同一个凭据上尝试其他候选端点。

其候选顺序是：

```text
ide -> codewhisperer -> amazonq -> runtime
```

当首选端点不是 `auto` 时，首选端点排在第一位，其余候选端点按固定顺序补齐。CLI 被明确排除在自动 fallback 链之外。

对应参考代码：

- `/Users/yuanfeijie/Desktop/procode/kiro-rs-mata/src/kiro/provider.rs:717-790`：构造端点顺序；
- `/Users/yuanfeijie/Desktop/procode/kiro-rs-mata/src/kiro/provider.rs:793-801`：网络错误和 HTTP 状态的 fallback 判定；
- `/Users/yuanfeijie/Desktop/procode/kiro-rs-mata/src/kiro/provider.rs:1227-1405`：同一凭据内逐个发送并记录每一跳；
- `/Users/yuanfeijie/Desktop/procode/kiro-rs-mata/src/model/config.rs:259-279`：配置字段和默认值；
- `/Users/yuanfeijie/Desktop/procode/kiro-rs-mata/admin-ui/src/components/settings-page.tsx:82-160`：管理页面的首选端点和 fallback 开关。

mata 的一次请求大致是：

```text
请求
  -> 选择一个凭据
  -> 构造该凭据的端点候选链
  -> 端点 A 失败
  -> 端点 B 失败
  -> 端点 C 成功或候选耗尽
  -> 候选耗尽后再进入账号级重试/换号
```

这是“同一账号内换协议端点”，与当前项目已有的“同一账号换 API Region”不同。

## 两种轮换的区别

| 维度 | 当前项目已有 Region 轮换 | mata 的协议端点 fallback |
| --- | --- | --- |
| 切换对象 | API Region | `ide`、`codewhisperer`、`amazonq`、`runtime` 等协议端点 |
| 凭据是否改变 | 不改变 | 不改变，同一账号继续使用原 token/profile/machine ID |
| URL 变化 | 主要改变 `q.<region>.amazonaws.com` 等区域地址 | 可能改变域名、协议、请求头和请求体格式 |
| Auth Region | 不改变 | 通常不改变，但各端点可能有自己的认证要求 |
| 当前项目状态 | 已有独立开关和测试 | 当前项目未实现同账号多协议端点链 |
| CLI 处理 | 凭据可显式选择 CLI | mata 明确不把 CLI 放进自动 fallback 链 |
| 失败后下一层 | 账号调度器或 Region 轮换 | 端点候选耗尽后才进入账号级调度 |

不能把两者合并成一个名为“端点轮换”的开关。用户看到的“端点”如果指 Region，和 `endpoint` 字段指的协议端点不是同一个配置维度。

## 是否需要绑定账号

需要区分“请求运行时绑定”和“配置持久化绑定”。

### 请求运行时需要绑定

端点 fallback 必须绑定到当前已经选中的凭据。一个请求先取得账号上下文，再在这个账号上尝试候选端点。每一跳都应继续使用该账号的：

- access token 或 API Key；
- profile ARN；
- machine ID；
- 凭据级代理；
- 账号级 Region 和认证信息。

端点候选全部失败后，才能释放当前账号并由账号调度器选择下一个账号。这样可以保证：

1. 不会把账号 A 的 token 发送到账号 B 的端点上下文；
2. 账号级失败、冷却、并发 lease 和 usage 归因仍然对应原账号；
3. 同一请求的端点跳转不会绕过现有请求发送预算；
4. 端点 fallback 不会和账号 fallback 形成无界来回循环。

### 不一定需要新增账号与端点关系表

当前凭据已经有 `endpoint: Option<String>`，它足以表达账号级首选协议端点。新增功能可以先采用：

```text
凭据.endpoint：该账号的首选端点
全局 endpoint fallback：是否允许同账号尝试其他兼容端点
```

只有在需要“每个账号自定义候选端点列表”时，才需要扩展凭据模型，例如增加 `endpointFallbacks` 或结构化路由策略。第一阶段不需要额外的账号绑定表。

## 可以直接移植的部分

以下设计可以借鉴 mata：

1. 在 Provider 内增加端点路由快照，避免一次请求进行到一半时热配置改变候选顺序。
2. 把“构造端点候选链”和“发送单个端点请求”分开。
3. 每个端点跳转都记录独立 attempt/trace，不能只记录最终账号结果。
4. 同一凭据的端点候选耗尽后，再回到账号级重试。
5. 提供运行时首选端点和 fallback 开关，并通过 Admin API 热更新。
6. 保留 CLI 的独立路由语义，默认不让 CLI 参与 IDE 类端点的自动 fallback。

## 不能直接照搬的部分

### 端点集合不能照搬

当前项目只有 `ide` 和 `cli` 实现，尚未具备 mata 中的 `codewhisperer`、`amazonq`、`runtime` 端点实现。先把这些名称加入候选列表会产生“配置可选但运行时不可用”的假能力。

如果要支持这些端点，必须分别实现并验证：

- API/MCP/模型列表 URL；
- Content-Type 和目标 Header；
- profile ARN 注入规则；
- stream/event-stream 兼容性；
- tool、thinking、usage 和错误体语义；
- external IdP、API Key、OAuth 等账号类型的适配范围。

### HTTP 错误 fallback 规则不能照搬

mata 当前把除 401、402、403 外的大多数非 2xx 状态都视为可尝试下一个端点，包含 400 和 429。当前项目已经在 Provider 和 endpoint 层区分：

- 请求格式/客户端校验错误；
- 账号认证错误；
- 风控型 429；
- 带 `Retry-After` 的限流；
- 普通瞬态错误；
- 网关超时和协议错误。

移植时应按错误语义决定是否换端点。尤其是：

- 确定性请求错误不应在多个端点重复发送；
- 带 `Retry-After` 的 429 不应被无条件放大为端点风暴；
- 账号认证失败通常不能靠换同账号协议端点修复；
- endpoint 层错误分类不能绕过现有的账号冷却和请求预算。

### 请求预算必须统一

当前项目已有 `InferenceAttemptBudget`、本地账号重试上限、外部池预留和狂暴模式预算。端点 fallback 的每次真实发送都必须消耗同一套发送预算，否则“账号数 × 端点数”会隐式突破原有上限。

狂暴模式的预算还需要明确乘积关系：

```text
账号 × Region × 狂暴轮次 × 同账号协议端点
```

如果端点 fallback 只作为普通模式的单次账号内部候选链，则应按每个账号候选链的真实发送次数计入预算，不能让端点跳转绕过狂暴上限。

## 建议的移植边界

### 第一阶段：只增加可见性和配置语义

先把已有两种概念在文档和管理界面中拆开：

- 将 `kiroUpstreamRegionRotationEnabled` 明确标为“API Region 轮换”；
- 将凭据 `endpoint` 明确标为“协议端点”；
- 展示当前账号的有效端点、有效 API Region 和回退行为；
- 在没有实现多协议端点链之前，不提供 `codewhisperer` 等不可用选项。

### 第二阶段：实现 `ide` 同账号内的有限 fallback

当前项目可以先只实现：

```text
ide -> （未来已验证的兼容 IDE 类端点）
```

该阶段不应自动把 OAuth/IDE 账号切换为 CLI，也不应改变 API Key 默认 CLI 行为。候选端点应由注册表实际存在的 endpoint 实现生成。

### 第三阶段：再评估其他协议端点

只有每个新端点完成真实 CLI/上游验证、流式和工具调用验证、错误分类验证后，才加入自动候选链。管理界面中的端点选项应从运行时注册表动态生成，避免前端硬编码出尚未实现的端点。

## 推荐的目标调度顺序

在引入协议端点 fallback 后，一次本地请求建议采用以下层次：

```text
请求
  -> 按路由规则和模型过滤选择账号
  -> 固定当前账号的端点候选链
  -> 对每个端点：构造 endpoint-specific 请求并发送
  -> 端点候选耗尽
  -> 按账号级错误语义决定冷却、释放或换号
  -> 新账号重新从它自己的首选端点开始
  -> Region 轮换只作用于当前 endpoint 的 API Region
```

这里的关键约束是：换账号时端点候选链必须重新从新账号的首选端点开始；不能把账号 A 已经推进到的 endpoint/Region 游标直接带给账号 B。当前项目的 Region 轮换代码已经针对普通模式增加了按账号归属的 Region 游标保护，这个原则也应延伸到协议端点游标。

## 待确认问题

在真正编码前，需要通过真实上游或 mock 矩阵确认：

1. 当前项目是否只需要 Region 轮换，还是确实要增加 IDE 类协议端点 fallback。
2. `kiro-rs-mata` 中的 `codewhisperer`、`amazonq`、`runtime` 实现是否计划同步迁移，还是只借鉴路由框架。
3. 端点 fallback 对 400、429、5xx、网络错误、流式首输出前错误分别采用什么策略。
4. 同一账号的多个端点请求是否共享相同的 profile ARN、machine ID 和代理；对于 external IdP 是否存在端点特定限制。
5. 端点跳转是否纳入现有 `InferenceAttemptBudget` 的每次发送计数，以及狂暴模式预算如何计算。
6. MCP、模型列表、额度查询是否跟随 inference 的端点 fallback，还是继续保持各自固定路径。

## 最终判断

当前项目的“多端点”不是完全黑盒：账号级协议端点选择、API Region 轮换、账号级调度和狂暴模式都已有清晰代码路径，只是它们分布在不同模块，名称上容易混淆。

`kiro-rs-mata` 的主要可移植价值是同账号端点候选链、运行时首选端点配置、端点跳转的独立 trace，以及“端点耗尽后再换账号”的层次。协议端点必须绑定当前账号运行时上下文，但第一阶段不需要新增账号与端点关系表。实现前应先限定端点集合和错误矩阵，再把候选链接入现有重试预算与账号状态机。

## 2026-11-10 远端同步后复核

### 复核基线

本节基于当前项目 `origin/main` 的提交 `e8fddbaf0e9726a3e19b45880a06f0b346c6224e`（`2026-11-09`）复核，并与同步前的 `2e625e52` 对比。当前本地 `main` 已快进到该提交。

### 核心结论没有变化

最新代码仍只注册 `ide` 和 `cli` 两个 `KiroEndpoint` 实现；`KiroProvider::endpoint_for` 仍按 `凭据.endpoint > config.defaultEndpoint` 选出单个实现。没有新增 `endpoint_order_for`、`preferredEndpoint`、`endpointFallback` 或同账号多协议端点候选链。

因此，最初的问题答案仍是：可以借鉴 mata 的端点候选链，但当前项目还没有这个能力；现有 Region 轮换不能等同于协议端点 fallback。

### 最新提交中与调度有关的变化

1. **CLI/API Key 的额度耗尽现在会触发账号级故障转移。** 旧路径主要按 HTTP 402 加额度错误体处理；新代码也识别 HTTP 400 且包含结构化额度 `reason` 的响应。命中后禁用当前账号并让调度器尝试其他账号，避免已耗尽额度的 Fast 账号持续独占流量。实现见 `src/kiro/endpoint/mod.rs:354-375`、`src/kiro/provider.rs:14392-14420` 和 `src/kiro/provider.rs:16264-16275`。这改变了**账号选择层**，没有改变协议端点选择层。
2. **批量凭据导入的默认 `endpoint` 入口在同步前后都已存在，本次没有变化。** UI 默认参数使用 `ide / cli`，并将默认值合并到没有显式端点的凭据。它便于批量设置账号级首选端点，不会生成同账号 fallback 链。见 `admin-ui/src/components/credential-parameter-defaults.tsx:473`、`ui/src/features/credentials/credential-dialogs.tsx:722`。
3. **IDE Runtime 域名是第三个需要区分的维度。** `kiroRuntimeDomainEnabled` 控制 IDE 对话和 MCP 是否改发到 `runtime.{region}.kiro.dev`；默认走 `q.{region}.amazonaws.com`。它只切换 IDE 实现内部的目标 host，保留 IDE 请求头和协议；模型列表继续走 q host。这个开关在同步前已存在，因此不是此次远端新增，但上一节没有把它单独列清。代码见 `src/model/config.rs:5096-5103`、`src/kiro/endpoint/ide.rs:38-68`，且有 `runtime_domain_switch_moves_chat_and_mcp_only` 测试。
4. **首输出前重试预算增加了 usage-only 独立上限，但没有增加端点链。** usage-only（只有 usage/metadata、没有 assistant/reasoning/tool 输出）最多尝试默认 2 次，次数包含首次调用；普通首输出前错误默认最多尝试 6 次。两者都受单请求 `inferenceUpstreamMaxAttempts` 硬上限约束，默认 4 次。预热慢启动改变账号流量分配；请求参数兼容在选账号前归一化请求；这些变化均未引入新的 Kiro 协议端点或 endpoint fallback。未来若加入同账号端点 fallback，每次实际发送都必须计入同一推理发送预算，避免独立重试层叠加后突破硬上限。配置定义见 `src/model/config.rs:5424-5435`、默认值见 `src/model/config.rs:6147-6187`，运行时管理界面见 `ui/src/features/runtime/runtime-page.tsx:864-869`。

### 修正后的现状模型

当前至少有三种容易被叫作“端点”的机制：

| 机制 | 控制对象 | 行为 | 是否自动按候选链故障转移 |
| --- | --- | --- | --- |
| 凭据 `endpoint` / 全局 `defaultEndpoint` | 协议实现：`ide` 或 `cli` | 每次账号尝试选一个 endpoint 实现 | 否 |
| `kiroRuntimeDomainEnabled` | IDE 对话/MCP 的目标 host | 在 q host 与 runtime host 间固定选路 | 否 |
| `kiroUpstreamRegionRotationEnabled` | API Region | 瞬态重试时在两个区域地址间推进 | 是，限 Region 重试策略 |

前文“Region 是当前已经存在的同账号切换维度”应理解为**Region 是当前存在的自动轮换维度**。IDE Runtime 域名开关也能改变同一账号的目标 host，但它是配置型固定选路，不会在 q host 失败后自动改试 runtime host，反向也一样。

管理界面把 `kiroUpstreamRegionRotationEnabled` 标作“上游端点轮换”，并把狂暴模式的一轮描述为“所有账号 × 所有端点”。这里的“端点”实际指 API Region 对应的 host：开启 Region 轮换时枚举 `us-east-1` 与 `eu-central-1`，关闭时只使用凭据自身的有效 Region；它不代表在 `ide` 与 `cli` 两种协议实现之间轮换。狂暴模式因此遍历的是账号 × Region × 轮次，而不是账号 × 协议端点。对应界面见 `ui/src/features/runtime/runtime-page.tsx:886-888` 和 `admin-ui/src/components/runtime-config-panel.tsx:4557-4575`，候选 Region 与轮换游标见 `src/kiro/retry_pipeline.rs:48-59`、`src/kiro/retry_pipeline.rs:69-118`、`src/kiro/retry_pipeline.rs:207-250`。

### 对移植建议的影响

移植 mata 的同账号 endpoint fallback 时，候选项应描述为 endpoint 实现，而不是简单的 host 列表。IDE Runtime host 开关和 Region 轮换属于 endpoint 实现内部的地址构造策略；若都开启，实际目标地址由两者组合决定。第一阶段仍建议只给 IDE/CLI 明确首选端点，不把 CLI 和 IDE 自动互相 fallback，也不把 Runtime host 开关当作 fallback 开关。

新引入的额度 400 分类也应纳入 fallback 判定矩阵：它表示账号额度耗尽，当前语义是禁用并换账号，不应在同一账号上尝试另一个协议端点。继续保留“端点失败候选耗尽后，再按账号级分类换号”的分层。
