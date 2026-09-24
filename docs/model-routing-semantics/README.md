# 模型路由语义与改造说明

本文档定义当前系统中所有涉及模型名的位置应该如何处理，作为后续改造、修复和测试的依据。目标是在修复外部池模型出站 bug 的同时，保证现有本地 Kiro 账号、外部池、fallback、usage、计费、缓存和调度能力不退化。

## 目标

1. 调用方传入的模型、系统 usage 记录的请求模型、外部池默认出站模型都使用完整 Claude Code 横杠模型 ID，例如 `claude-opus-4-8`。
2. 本地 Kiro 账号实际调用仍使用 Kiro 需要的点号模型，例如 `claude-opus-4.8`。
3. 外部池默认不使用 Kiro 点号模型；外部池只在显式映射、自动映射或用户明确选择特殊模型处理模式时改变出站 model。
4. 保留当前已经实现的能力：本地 Kiro 账号模型过滤、账号支持模型列表、同族/版本兼容解析、fallback、冷却、usage、计费、缓存等逻辑不能被无关重构破坏。
5. 文档化入站模型、最终实际请求模型、计费模型、usage 字段之间的关系，避免后续改造再次混淆。

## 模型语义

### 调用方模型

调用方模型是入口请求中的 `model`，也是系统对外暴露的模型 ID。

示例：

- `claude-opus-4-8`
- `claude-sonnet-4-5-20250929`
- `claude-opus-4-8-thinking`

调用方模型应该用于：

- API 入站解析。
- 外部池默认出站。
- 外部池模型过滤的主候选。
- 路由策略中的模型匹配。
- usage 记录的 `model` 字段。
- 前端外部池测试请求模型。

调用方模型不应该被本地 Kiro 点号模型覆盖。

### 本地 Kiro 模型

本地 Kiro 模型是实际调用 Kiro 账号时使用的上游模型名。

示例：

- `claude-opus-4.8`
- `claude-sonnet-4.5`

本地 Kiro 模型应该用于：

- Kiro 请求体里的 `modelId`。
- 本地 prompt cache / thinking / payload 处理需要真实 Kiro 模型能力时。
- usage 记录的 `upstream_model` 字段。
- 本地账号模型级 cooldown 的 Kiro 侧兼容候选。

本地 Kiro 模型不应该作为外部池默认出站 model。

### 外部池出站模型

外部池出站模型是最终发给外部 baseUrl 的请求体 `model`。

默认情况下：

```text
调用方模型 = 外部池出站模型
claude-opus-4-8 -> claude-opus-4-8
```

只有以下情况可以改变外部池出站模型：

- 命中外部池模型映射规则。
- 外部池启用“未命中时点号转横杠”后，映射未命中的 Claude 数字点号版本会被转换为横杠版本，例如 `claude-opus-4.8 -> claude-opus-4-8`。
- 用户显式选择 `direct_mapping` 或 `processed_mapping` 等模式。
- 未来引入的明确外部协议适配器。

映射 target 是最终出站模型，不能在命中后再被隐式改成 Kiro 点号模型。

### 计费模型

计费模型是 pricing catalog 用来估价的模型名。当前系统已经在 usage 中记录 `pricing_model`，并通过定价候选支持点号、横杠、dated、thinking 等变体。

改造原则：

- usage 的 `model` 保持调用方模型。
- usage 的 `upstream_model` 记录本地解析后的上游模型，通常是 Kiro 点号模型。
- 外部池 billing 的 `pricing_model` 可以继续使用当前 pricing catalog 能识别的模型，但必须在 usage 中清晰记录，不能覆盖 `model`。
- 如后续需要更精确，应优先使用实际外部池出站模型估价；如果不可识别，再回退到 `upstream_model` 或调用方模型的 pricing 候选。

## 当前模型流转

### 入站解析

相关位置：

- `src/anthropic/handlers/request_entry.rs`
- `src/anthropic/request_facts.rs`
- `src/anthropic/types.rs`

当前入口会从 raw body 探测 `model`，必要时解析成 `MessagesRequest`。

要求：

- raw body 和 parsed payload 都要保留调用方原始模型。
- 入口阶段可以用模型解析结果查询 max output tokens、thinking 限制等能力，但不能把 payload 的 `model` 改成本地 Kiro 点号模型。

### 模型解析

相关位置：

- `src/anthropic/model_capabilities.rs`
- `src/anthropic/converter.rs`
- `src/model/config.rs`

模型解析负责把调用方模型解析为当前可用上游模型。

现有能力必须保留：

- 显式 alias 规则。
- 版本等价规则，例如横杠和点号的等价。
- 同族可用模型选择。
- 请求低版本但 Kiro 已不支持时，按支持列表找到同族支持模型。
- 支持列表来源可以是全局模型目录，也可以是单个账号的 supported_models。

重要边界：

- 这些解析结果服务于本地 Kiro 调用和能力判断。
- 外部池默认出站不能因为 `upstream_model` 是点号就改成点号。

### 本地 Kiro 调度

相关位置：

- `src/kiro/token_manager/manager.rs`
- `src/kiro/token_manager/capacity.rs`
- `src/kiro/model/credentials.rs`
- `src/model/model_support.rs`

本地账号选择需要回答：某个账号是否能承接调用方请求模型。

要求：

- 调用方传 `claude-opus-4-8` 时，支持 `claude-opus-4.8` 的账号也应该可匹配。
- 如果账号 supported_models 为空，仍按现有逻辑表示不限制。
- Opus 等订阅等级限制继续有效。
- 账号 region / proxy / Hong Kong 账号等属性不能被模型改造影响。
- 账号模型级 cooldown 继续有效，并且等价模型不应造成明显绕过。

测试要求：

- `claude-opus-4-8` 能让支持 `claude-opus-4.8` 的 Pro / HK Kiro 账号进入 Ready。
- Free 账号不支持 opus 时仍被过滤。
- 本地调用前实际 Kiro 模型仍是 `claude-opus-4.8`。

### 本地 Kiro 调用

相关位置：

- `src/anthropic/converter.rs`
- `src/anthropic/handlers/local_body_pipeline.rs`
- `src/kiro/provider.rs`

要求：

- 本地 Kiro 调用使用 `ModelResolution.upstream_model`。
- 请求 `claude-opus-4-8`，若解析到 `claude-opus-4.8`，Kiro 请求必须使用 `claude-opus-4.8`。
- 本地路径不能被外部池默认改造影响。

### 外部池调度

相关位置：

- `src/external_pool.rs`
- `src/external_pool/model_pipeline.rs`
- `src/external_pool/body_pipeline.rs`

外部池选择需要回答：某个外部池是否能承接调用方请求模型。

要求：

- 模型过滤主语义是调用方模型，例如 `claude-opus-4-8`。
- 旧配置兼容可以保留点号、dated、thinking 等价扩展，但仅用于匹配，不影响出站模型。
- 单池 supported_models 为空表示不限制。
- 单池 routeMode 和全局 routeMode 继续同时生效。
- 外部池模型级 cooldown 应至少覆盖调用方模型和实际出站模型。

当前 bug：

- 外部池默认 `model_mapping_mode` 为 `processed_mapping` 时，未配置映射也可能使用 `upstream_model`，从而把 Kiro 点号模型发给外部池。

修复方向：

- 外部池默认模式应为 `passthrough_mapping`：先用调用方模型匹配规则，命中则用 target，未命中则原样透传调用方模型。
- 保留 `processed_mapping`，但它必须是用户显式选择的高级模式，用于以 Kiro 点号模型作为映射 source 的场景。

### 外部池出站 body

相关位置：

- `src/external_pool/body_pipeline.rs`
- `src/external_pool/model_pipeline.rs`

模式语义：

- `passthrough`：直接发送调用方模型，不应用映射。
- `passthrough_mapping`：用调用方模型匹配映射；未开启点号归一化时，未命中原样透传调用方模型；开启“未命中时点号转横杠”后，未命中的 Claude 数字点号版本会转为横杠版本。应作为默认。
- `direct_mapping`：用调用方模型匹配映射；未命中后使用本系统解析模型和兜底转换。属于显式高级模式。
- `processed_mapping`：先使用本系统解析后的 Kiro 模型匹配规则；未命中后使用 Kiro 模型或兜底转换。属于显式高级模式。

要求：

- 默认外部池出站不使用 Kiro 点号模型。
- “未命中时点号转横杠”必须由外部池开关控制，不能无条件默认转换；否则该开关失去配置意义。
- raw passthrough 如果开启顶层 model 重写，也遵循同一模型处理语义。
- normalized body 的 payload guard、图片修正、thinking 修正不能改变调用方模型语义。

### fallback

相关位置：

- `src/anthropic/handlers/request_entry.rs`
- `src/external_pool.rs`
- `src/kiro/provider.rs`

要求：

- 本地账号 fallback 到外部池时，外部池应拿到原始入站 body / 调用方模型，再按外部池自己的 body/model 配置处理。
- 外部池失败后的本地救援或本地 retry，不应拿外部池映射后的模型去调 Kiro；本地仍应使用原始请求模型重新解析为 Kiro 模型。
- fallback 应保留 `effective_raw_body`，避免一次路径处理污染另一条路径。
- fallback route 的 eligibility、model filtering、route filtering 要统一按调用方模型判断。

当前已有测试覆盖 WebSearch external fallback、local pool preflight、local transient fallback 等逻辑。改造时需要增加模型特化断言。

### usage、计费、缓存

相关位置：

- `src/anthropic/usage.rs`
- `src/external_pool/usage_projection.rs`
- `src/external_pool.rs`
- `src/anthropic/pricing.rs`
- `src/anthropic/prompt_cache.rs`

要求：

- usage `model`：调用方模型。
- usage `upstream_model`：本地解析后的上游模型，常见为 Kiro 点号模型。
- usage `externalPoolBilling.pricingModel`：用于计费估算的模型，必须明确记录。
- 外部池 usage projection 当前会优先使用 `upstream_model` 判断 prompt cache 能力和高缓存 profile；改造外部池出站模型时不能顺手改变该逻辑，除非单独评估和测试。
- prompt cache 的模型阈值需要保持横杠/点号等价模型的一致性。

## 改造原则

1. 不删除现有兼容能力。短别名、点号兼容、dated 模型兼容如果当前已支持，不在本次改造中删除。
2. 不把兼容能力当作推荐路径。前端外部池测试和提示只展示完整 Claude Code 横杠 ID。
3. 不改变本地 Kiro 点号调用要求。
4. 不让外部池默认走 Kiro 点号模型。
5. 不让某一路径处理后的 body/model 污染 fallback 到另一条路径。
6. 每一个模型字段都必须能回答：它是调用方模型、Kiro 模型、外部池出站模型，还是计费模型。

## 需要改的地方

### 后端

- `src/external_pool.rs`
  - 外部池默认 `ExternalPoolModelMappingMode` 改为 `PassthroughMapping`。
  - 审核 `model_candidates_for_support`，确保调用方模型优先。
  - 审核 `model_cooldown_candidates`，确保模型级冷却覆盖调用方模型和实际出站模型。

- `src/external_pool/model_pipeline.rs`
  - 保持 `PassthroughMapping` 的语义：命中映射用 target，未命中保留 original。
  - 保持 `ProcessedMapping` 作为显式高级模式。

- `src/external_pool/body_pipeline.rs`
  - normalized / raw rewrite 共用同一模型语义。

- `src/storage/postgres.rs`
  - 新建列/新建表默认值改为 `passthrough_mapping`。
  - 添加一次性迁移：仅迁移“旧默认、无规则、无 require match、无点号转换”的外部池到新默认，避免覆盖显式配置。

- `src/anthropic/handlers/tests.rs`
  - handler 级 mock fallback 测试应验证原始 model 保持为横杠模型。

### 前端

- `admin-ui/src/lib/test-models.ts`
  - 外部池测试模型列表使用完整横杠模型。
  - 兼容输入可以规范化到完整横杠模型，但发送值不能省略 `claude-`。

- `admin-ui/src/components/external-pools-panel.tsx`
  - 新建外部池默认模式为 `passthrough_mapping`。
  - supported_models 占位与提示使用完整横杠模型。
  - 映射规则描述区分调用方模型和 Kiro 点号模型。

## 测试矩阵

### 单元测试

1. 模型解析
   - 请求 `claude-opus-4-8`，本地 Kiro 可解析到 `claude-opus-4.8`。
   - 请求低版本模型，在 Kiro 支持列表不包含该版本时，保留现有同族可用模型选择逻辑。
   - 单账号 supported_models 与全局模型目录都能参与选择。

2. 本地 Kiro 调度
   - `local_pool_route_state(Some("claude-opus-4-8"))` 能匹配支持 `claude-opus-4.8` 的账号。
   - Free 账号不支持 opus，Pro/HK 账号支持 opus。
   - 模型级 cooldown 不误伤其他模型。

3. 外部池过滤
   - supported_models 配 `claude-opus-4-8` 能匹配请求 `claude-opus-4-8`。
   - supported_models 配其他模型不能匹配。
   - route allow/deny 仍生效。

4. 外部池模型出站
   - 默认模式：`claude-opus-4-8` 出站仍是 `claude-opus-4-8`，即使 route 的 `upstream_model` 是 `claude-opus-4.8`。
   - `passthrough_mapping` 命中规则使用 target，未命中默认原样透传。
   - `passthrough_mapping` 开启“未命中时点号转横杠”后，`claude-opus-4.8` 出站为 `claude-opus-4-8`。
   - `processed_mapping` 显式模式可用 `claude-opus-4.8 -> claude-opus-4-8`。
   - raw passthrough rewrite 和 normalized body rewrite 都一致。

5. 冷却
   - 外部池 `model_unavailable` 冷却覆盖调用方模型。
   - 如果实际出站模型由映射产生，也覆盖映射后的出站模型。

6. usage / 计费 / 缓存
   - usage `model` 是调用方模型。
   - usage `upstream_model` 保留 Kiro 解析模型。
   - pricing_model 字段明确，不覆盖 requested model。
   - high-cache 模型阈值不因点号/横杠变化退化。

### 本地 mock 调度测试

1. 本地 Kiro 可用
   - 调用方请求 `claude-opus-4-8`。
   - mock HK/Pro Kiro 账号支持 `claude-opus-4.8`。
   - 调度选中本地账号。
   - 实际 Kiro 请求 model 为 `claude-opus-4.8`。

2. 本地不可用，fallback 到外部池
   - 同一个入站 body 和 `claude-opus-4-8`。
   - 本地账号容量满 / disabled / no model compatible。
   - 外部池支持 `claude-opus-4-8`。
   - fallback 后外部池收到原始横杠模型。

3. 外部池不可用或模型不匹配
   - 外部池 supported_models 不包含请求模型。
   - 不应错误调度到该池。
   - 如果本地可用，应保持本地路径。

4. 映射场景
   - 外部池映射 `claude-opus-4-8 -> external-opus`。
   - 出站为 `external-opus`。
   - cooling / billing 能记录到实际出站或 pricing 模型。

## 验证命令

Rust 测试必须使用项目 scoped Cargo wrapper：

```bash
feature/tests/run-cargo-scoped.sh model-routing -- cargo test <targeted tests>
```

前端验证：

```bash
cd admin-ui
pnpm build
```

必要时增加 mock handler / external pool 测试，覆盖本地账号、外部池、fallback、模型过滤、映射、冷却和 usage 字段。
