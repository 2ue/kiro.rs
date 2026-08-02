# 2026-08-01 生产外部池两类错误根因补充

Status: `root-cause-confirmed / implementation-focused-pass / frontend-contract-gate-pass / integration-dispatch-focused-pass / released-v0.0.130`

Severity: P0

生产主机：`152.53.194.170`<br>
部署：`ghcr.io/2ue/kiro-rs:0.0.123`<br>
证据归档：[redacted production evidence](../../tmp/prod-evidence/20260801-224454-152.53.194.170/20260801-224454-152.53.194.170-redacted.tar.gz)

本轮只读审计没有执行重启、Compose 写操作、数据库写入、Redis 写入、配置修改或远程文件删除。

## 现象与影响

生产 usage 明细中出现两类外部池 P0 行为偏差：

- P001：页面“路由”为“预检 fallback”，但客户端收到的是本系统按“输入上限预检”
  构造的 400；请求没有进入外部账号，也没有机会触发“请求大小保护”。
- P002：页面“模型（请求）”是 `claude-opus-4-6-thinking`，“模型（本地解析）”是
  `claude-opus-4.6`，但“模型（上游）”仍使用原始请求模型，外部账号返回 404。

影响是：本地账号不可用时，外部池并没有按当前配置提供兜底；部分请求被本地提前
拒绝，部分请求按错误的外发模型到达外部上游。

## 根因总览

根因分两层：

- P001：外部池在“请求体处理”之前执行了内容长度“输入上限预检”，这个全局估算
  拒绝早于“Body 模式”和“请求大小保护”，与本地凭证“失败后再处理并重试”的语义不一致。
- P002：显式直连外部账号路径在绕过本地凭证前没有把“模型（本地解析）”传给外部池
  route，导致“映射后内部处理”和“内部处理后映射”没有可用的本地解析模型。

## P001：外部池 prompt 超长预检绕过 payload guard

请求：`req_01APu85fPB6XDXrgYYkyp6gC`

- `/ha/v1/messages`
- `routeSubtype=external_fallback_preflight`
- `fallbackReason=local_no_credentials`
- 本地预检状态：`total=0`
- 估算输入：`1,511,372`
- 外部池预检上限：`1,000,000`
- external attempts：空
- `payloadGuardReport` / `payloadBreakdown`：空

根因不是外部上游返回 400，而是页面“路由”显示为“预检 fallback”时，kiro.rs
在外部池选择和“请求体处理”之前直接用“输入上限预检”拒绝。这个拒绝发生在
“Body 模式”进入“标准处理”或“Raw 透传”之前，因此也绕过了“请求大小保护”。

生产相关配置按页面字段对应为：

- “请求大小保护”：启用
- “过大请求处理方式”：失败后再处理并重试
- “外部账号请求大小保护”：启用
- “请求大小上限”：`460800`
- “安全余量”：`32768`
- “请求体整形”：启用
- “适配当前请求预算”：启用

“失败后再处理并重试”的本地凭证语义是：首发不做大小裁剪；只有真实上游返回
too-long/context-window 400 后，才按“请求大小保护”裁剪并重试。本请求没有发送到
上游，因此不会触发该重试。`compression.enabled=true` 只是本地 Kiro provider 的
JSON whitespace 压缩，不是上下文语义压缩，也不减少估算 token。

另外，日志中的 `local_no_credentials` 是真实状态：`credentials` 表 680 行
全部为软删除，未删除凭证为 0；代码中的模型不兼容状态会是
`no_model_compatible`，不是本次的 `no_credentials`。

选定修复方向：

- 取消外部池发送前的内容长度“输入上限预检”硬拒绝；历史配置
  `externalPoolMaxInputTokens` 保留为兼容字段，不再作为本地 400 条件。
- “Body 模式 = 标准处理”按“请求大小保护 / 过大请求处理方式”执行：
  “发送前先处理”先裁剪再发送；“失败后再处理并重试”首发给上游，收到真实
  too-long/context-window 400 后再裁剪重试。
- “Body 模式 = Raw 透传”不做内容长度预检、不做裁剪，直接发送，由外部上游的
  真实上下文窗口返回成功或 400。
- 仍保留必要的调度/安全预检：是否启用外部账号、路径是否允许、账号是否可调度、
  并发/排队、模型支持列表、URL/header/auth 可构造性。这些不是内容长度预检。

## P002：#11 `passthrough_mapping` 原始模型透传导致 404

请求：`req_01okoATxaKRJdsTVxWeemC2q`

- `/cc/v1/messages`
- 请求模型：`claude-opus-4-6-thinking`
- 本地解析：`claude-opus-4.6`
- 解析说明：`claude-opus-4-6-thinking -> claude-opus-4.6`
- 外部池：`#11 jinnyapi`
- 外发模型：`claude-opus-4-6-thinking`
- 外部状态：404
- 客户端状态：502

#11 相关“模型处理”配置：

- “映射模式”：请求模型优先映射
- “必须命中映射”：关闭
- “未命中时点号转横杠”：关闭
- “支持模型”：空列表
- 7 条规则只覆盖旧版模型，没有 requested/resolved model 的规则

“请求模型优先映射”的真实语义是：

```text
原始模型 -> 命中 mapping 则使用 target
         -> 未命中且 require_match=false 则回退原始模型
```

它不是“未命中映射就使用模型（本地解析）”。如果产品策略是“先按模型（请求）
匹配映射，映射不到再用模型（本地解析）”，应使用“映射后内部处理”，或添加明确
的映射规则。

因此本次 404 是配置语义与预期不一致，不是本地 alias 解析失败。`normalize_model_version_dots`
也不是根因：该 fallback transform 只用于 direct/processed mapping 模式。

当前 usage/UI 已区分：

- “模型（请求）”：客户端原始模型；
- “模型（本地解析）”：本地能力目录/alias 解析后的模型；
- “模型（上游）”：实际发给本地凭证或外部账号的模型。

实现补充：显式直连外部账号时，现在也会先计算“模型（本地解析）”并传入外部池
“模型处理”。如果本地能力目录判定模型不支持，则不会因此阻断显式直连；仍交给
外部账号配置和外部上游处理。直接外部池和本地失败 fallback route 还会补齐本地
Kiro 发送链路的兼容模型处理，避免只有内置 seed 或无本地账号时把
`claude-opus-4-6-thinking` 误当成最终“模型（上游）”。这保证“映射后内部处理”
和“内部处理后映射”在外部池路径上也有“模型（本地解析）”可用。

但 v0.0.123 的外部错误记录只持久化 404 和归一化错误文本，没有原始上游
response body；若要求 usage 弹层展示真实上游报错，还需要补 raw upstream
diagnostics 的脱敏持久化。

## 复现

P001 固定复现条件：

- 外部账号启用；
- 本地账号不可调度，页面“Fallback 原因”为 `local_no_credentials` 或等价本地不可用原因；
- 请求估算输入大于历史 `externalPoolMaxInputTokens`；
- “过大请求处理方式”为“失败后再处理并重试”。

旧行为：页面“路由”为“预检 fallback”，external attempts 为空，客户端直接收到本地 400。
修复后预期：不按估算输入直接本地 400；标准处理路径按“请求大小保护”配置发送和重试，
Raw 透传路径直接发送到外部上游。

P002 固定复现条件：

- 显式直连外部账号；
- “模型（请求）”是可被本地能力目录解析的 alias，如 `claude-opus-4-6-thinking`；
- 外部账号“映射模式”为“映射后内部处理”或“内部处理后映射”。

旧行为：直接外部池 route 缺少“模型（本地解析）”。修复后预期：route 携带
`claude-opus-4.6`，外部池“模型处理”可按配置使用。

## 选定修复方案

- 删除外部池主调度路径中的内容长度“输入上限预检”发送前拒绝；
- 将 `externalPoolMaxInputTokens` 降级为兼容保留字段，UI 不再说明它会本地拒绝请求；
- 保留调度/安全预检，包括启用状态、路径策略、账号可调度、并发/排队、模型支持列表、
  URL/header/auth 可构造性；
- 显式直连外部账号前计算“模型（本地解析）”，并传给外部池 route；
- 本地能力目录不支持的模型不因这一步被拦截，仍由外部账号配置和外部上游决定。

## 验证与证据

当前本地验证：

- focused Rust：外部池主源码不再包含 `external_prompt_too_long_preflight` 或
  `external_pool_max_input_tokens_for_route`；
- focused Rust：`external_pool_outbound_body` 模型处理组通过，覆盖“请求模型优先映射”、
  “映射后内部处理”、“内部处理后映射”；
- focused Rust：外部池 payload guard retry route 仍能裁剪并禁用第二次 retry；
- focused Rust：handler 直接外部池路径结构测试通过，确认先计算并传递“模型（本地解析）”；
- focused Rust：PG/Redis 集成 dispatch hit 通过，`normalized_external_direct_policy_skips_raw_preparse_without_raw_pool`
  确认直接外部池请求进入 fake external 一次，usage 的“模型（请求）”保留
  `claude-opus-4-6-thinking`，“模型（上游）”和外发 body `model` 为 `claude-opus-4.6`；
- frontend：`npm run check` 通过，外部账号策略页和运行时配置页的兼容字段文案类型检查通过；
- frontend contract：发布前检查发现 `admin-ui` 类型合同仍缺外部池路径策略字段，且旧后台仍显示
  “输入上限预检”；已补齐 `externalPoolRouteMode` / `externalPoolRouteRules` 并把旧后台文案
  同步为“估算输入上限（兼容）”；
- docs：`node feature/tests/check-feature-docs.mjs` 通过；
- focused Rust：`external_route_model_resolution_prefers_local_processed_model_for_cc_aliases`
  固化直接外部池专用模型处理边界。

## 残余风险与回滚

残余风险：

- 取消内容长度发送前拒绝后，超长请求会真实打到外部上游；这是按本地凭证语义对齐后的
  预期行为，但会消耗一次外部请求尝试。
- “请求模型优先映射”仍按页面语义使用“模型（请求）”优先，映射不到就使用原始请求模型；
  需要使用“模型（本地解析）”兜底时，应配置“映射后内部处理”。
- Raw 透传不解析、不裁剪、不预检内容长度；错误由外部上游真实返回。

回滚：

- 不应回滚到全局 `externalPoolMaxInputTokens` 发送前 400，因为这会重新绕过“请求大小保护”。
- 若未来需要上下文窗口策略，应做 per-pool / per-model 能力建模，并接入“请求大小保护”，
  不能恢复全局估算 token 硬拦截。

## 关联

- [04-external-pool-prompt-too-long](04-external-pool-prompt-too-long.md)
- [本轮 P001](../../tmp/prod-evidence/20260801-224454-152.53.194.170/problems/P001-external-preflight-bypasses-payload-guard/problem.md)
- [本轮 P002](../../tmp/prod-evidence/20260801-224454-152.53.194.170/problems/P002-external-model-original-passthrough-404/problem.md)
