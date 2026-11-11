# 02 总体设计

## 1. 三种模型名

强制映射开启后，一个请求里同时有三种模型名，用途严格分开。

| 名称 | 例子 | 用于 | 绝不用于 |
|---|---|---|---|
| **外显模型**（display model） | `claude-opus-5-5` | 响应 `model`、usage 的 `model`、下游用量与账单、错误文案、`/v1/models`、count_tokens 的口径、公开页、身份提示、日志中面向用户的字段 | 调度、能力判断 |
| **目标模型**（target model） | `claude-sonnet-4-5` | 内部请求模型：账号筛选、冷却、劣化计分、能力查询、上下文预算、内部成本、prompt cache 路由键 | 任何对外字段 |
| **上游模型**（upstream model） | `claude-sonnet-4.5` | Kiro `modelId`。由现有解析器根据目标模型得出 | 任何对外字段 |

外显模型就是客户端传入的 `model`，原样保留，不做规范化。

## 2. 插入点

只在入口加一层，下游链路不改：

```
客户端 model ──► [ForcedModelMapping] ──► 目标模型 ──► 现有 resolve_model_with_mapping ──► 上游模型
         │
  └─► 外显模型（请求上下文里另存一份，只供对外字段使用）
```

- 位置：`src/anthropic/handlers/request_entry.rs`，在入口能力预查（`:125`）和 `resolve_request_model`（`handlers.rs:7476`）之前。
- 做法：开关命中后，把 `payload.model` 换成目标模型；原始值放进请求上下文的 `display_model`。
- 这样一来，第 01 篇第 2 节的风险自动消除：调度、冷却、劣化计分、`remap_model_for_usable_catalog` 拿到的都是目标模型。
- 目标模型在上游目录里真实存在，现有解析器精确命中，不会进入跨族解析或同族就近逻辑。
- 对外字段原本取 `payload.model` 的地方，改为取 `display_model`（未开启时它就等于 `payload.model`）。涉及的位置有：
  - 流式模板：`handlers.rs:10395`
  - 非流式响应：`handlers.rs:15038`
  - usage 记录：`handlers.rs:6277`
  - 错误文案中的 `model_not_found_message`

**顺序约束**：下面这些逻辑都按 `payload.model` 判断，必须在替换模型**之前**、按外显模型执行一次，把结果固化到请求体里。替换之后不能再按目标模型重新计算。

| 逻辑 | 位置 | 原因 |
|---|---|---|
| `-thinking` 后缀补 thinking 默认值 | `handlers/parsed_body_pipeline.rs:33` → `override_thinking_from_model_name`（`handlers.rs:15118`） | 目标模型名里没有后缀，替换后会丢掉"开启思考"这一语义 |
| 外显模型的 thinking 默认语义（Fable、Opus 5.5 始终思考，Opus 5.5 默认 effort 为 medium） | 新增，见 03 第 2 节 | 只有外显模型带有这个语义 |
| display 默认值 | `handlers.rs:10784` `request_hides_reasoning` | 现有实现同时检查请求模型和上游模型。替换后两者都是 4.5，会从 omitted 变成 summarized，需要改为读取 `display_model` |

做法：入口处先算出 `display_model` 的 thinking 和 display 默认值，写进 `payload.thinking`，之后再替换 `payload.model`。`request_hides_reasoning` 的"请求模型"参数改为传入 `display_model`。

不按账号做映射。原因有两点：

1. 映射发生在账号选定之前，`modelId`、推理字段、上下文预算、cache 路由键都要先确定。若按账号改写，这些都得在每次尝试时重算，改动面大。
2. 同一个会话会因为换账号而在两个模型之间来回切，prompt cache 和 thinking 历史都会失效。

需要"某一批账号专门承接"时，做法是：路由开强制映射，再把这条路由绑定到账号池（复用现有路由和账号策略）。效果相同，结果也稳定。

## 3. 配置结构

放在运行时配置里（`runtime_config.config` JSONB），不需要建表或迁移。

```jsonc
"forcedModelMapping": {
  "enabled": false,   // 默认关
  "rules": [          // 自上而下匹配外显模型的模型族，首条命中生效
    { "families": ["opus", "fable", "sonnet"], "target": "claude-sonnet-4-5" },
    { "families": ["haiku"],      "target": "claude-haiku-4-5" }
  ],
  "defaultTarget": "claude-sonnet-4-5",  // 其它可识别的 claude-* 模型
  "applyToExternalPool": false,   // 外部号池默认不受影响
  "contextBackoff": { ... },      // 见 04
  "identity": { ... },            // 见 05
  "pathOverrides": {       // 按路由覆盖，最长前缀匹配，与身份配置一致
    "/cc": { "enabled": true }
  }
}
```

## 4. 规则语义

- **匹配对象**：外显模型去掉 `[1m]`、`-thinking`、日期后缀之后的模型族（opus / sonnet / haiku / fable / 以及 auto、best、default、opusplan 等短别名）。短别名一律按 `defaultTarget` 处理。
- **只处理 Claude 模型**：非 Claude 模型（deepseek、glm 等）以及空模型不改写，按现状处理。
- **目标模型已等于外显模型**时不改写（例如请求本身就是 `claude-sonnet-4-5`）。
- **静态规则，不做动态路由**：不按 effort、工具、输入长度切换目标模型。会话内目标模型必须稳定，否则每次切换都会让 prompt cache 全部失效，thinking 历史也不兼容。
- **目标不可用时**：目标模型不在任何可用账号的能力范围内，就照常排队等待调度（参见现有调度语义），不回退到外显模型，也不报"模型不存在"。配置保存时校验目标模型在目录中存在，不存在就拒绝保存。
- **thinking 变体**：外显模型带 `-thinking` 时，视为客户端开启了 thinking，交给第 03 篇的 thinking 补全处理；目标模型本身不加 `-thinking` 后缀。

## 5. 与现有逻辑的边界

| 现有逻辑 | 强制映射关闭 | 强制映射开启 |
|---|---|---|
| `modelMapping` 显式规则、自动规则、同族就近、跨族兜底 | 不变 | 不变；它们的输入是目标模型，一般直接精确命中 |
| 账号 `supported_models`、订阅等级列表 | 不变 | 按目标模型匹配 |
| 冷却、劣化计分 | 不变 | 按目标模型计算 |
| 外部号池 | 不变 | 默认不改写；`applyToExternalPool=true` 时外部池出站也用目标模型 |
| 计费 `pricing_model` | 不变 | 内部成本按上游模型计算；对外计费口径见 06 待决策项 |
| usage 整形策略 | 不变 | 按路由挂载策略照常生效，叠加 04 的 usage 口径 |

开关关闭时，代码路径与现在逐字节相同。实现时所有新逻辑都放在 `if forced.is_active()` 分支里。
