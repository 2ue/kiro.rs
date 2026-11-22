# 账号级 rawUsage 记录依据

Role: 原始用量统计的兼容性设计与验收契约

Status: `In Progress / execute-ready`，已授权实施和三个分支发布

Date: 2026-11-23

Authority: 当前会话要求：账号级开关，默认保持现状；仅改变原始用量保存及其成本统计，不影响 usage 整形、客户端回包和补偿后计费。

Related: [Roadmap](../roadmap.md)、[存储与状态边界](../../../baseline/storage-and-state.md)、[验证与发版门禁](validation-and-release-gates.md)

## 已确认的问题

`main` 与 `forced-main` 的 `src/external_pool.rs` 通过 `merge_external_usage` 对流式各字段取最大值；`claude-main` 的对应实现在 `src/account/mod.rs`，同样存在该行为。

2026-11-22 对 k1 的 171 条请求进行了脱敏上游 usage 与数据库对照：jinnyapi-kiro 82 条、zslnh-kiro4 71 条一致；ddpapi-kiro 18 条中的 16 条流式请求不一致，2 条非流式一致。典型流式请求先给出 `input_tokens=262393`，随后明确给出 `input_tokens=1`，现有 rawUsage 仍保存 262393。该样本按相同价格计算的原始成本分别为 `$1.2370642` 与 `$0.1874962`。

脱敏审计报告位于本机 `/Users/yuanfeijie/Downloads/k1-external-pool-usage-audit-20261122/核验报告.md`。该路径是外部证据位置，不是仓库内已提交的测试 fixture；实施时只抽取必要 usage 字段，不携带正文和凭据。

## 账号配置与默认值

- 在外部池/外部账号的新建、编辑表单增加开关：**原始用量按上游最后返回值记录**，默认关闭。
- 关闭：保持现有 rawUsage 采集、修复与逐字段最大值合并行为。
- 开启：原始统计使用上游原始响应中各字段最后一次明确给出的有效数值。
- API 拟使用 `rawUsageSource: "legacy_max" | "upstream_last"`，缺省为 `legacy_max`；界面用开关展示这两个状态。该字段独立于 `usageProjectionMode`。
- 配置归属于每一个已保存的外部池/账号条目；不按站点、域名、账号类型或分组隐式共享。`main`/`forced-main` 使用 `ExternalPool`；`claude-main` 使用 `Account`。
- 新建遗漏字段使用旧行为；已有账号迁移为旧行为；编辑遗漏字段保留当前值；复制账号继承源配置；已有导入导出路径中保留该字段，旧格式缺省旧行为。未知枚举值应明确校验失败。
- 每次上游 attempt 在发出请求前使用权威账号快照固定策略，处理中编辑账号不改变该 attempt；重试/切池重新取对应账号策略，各 attempt 的状态互相隔离。
- 通过现有数据库迁移机制增加账号策略列与默认值，并更新 schema 兼容性检查、CRUD、复制和已有缓存失效路径。这是账号持久开关所需的 schema 扩展，不修改 usage 记录表结构、不迁移历史用量。
- 用量详情的外部池计费元数据拟追加本次生效的 `rawUsageSource`；历史记录缺省按旧行为解释，后续改开关不重解释历史记录。

## upstream_last 的合并语义

| 上游后续事件 | 原始统计行为 |
| --- | --- |
| 明确给出较小数值，如输入 262393 → 1 | 覆盖为 1 |
| 明确给出 0 | 覆盖为 0，不能把显式零当作缺失而退回旧最大值 |
| 未给出该字段，如最终事件只有 output_tokens | 保留该字段此前明确给出的值 |
| 字段为 null、非数值或无效 token 数 | 不冒充有效零，不覆盖已有有效值 |
| 标准 message_start/message_delta 用量 | 按响应顺序合并直接 usage 字段 |
| OpenAI 兼容 prompt/completion/cache 用量 | 沿用支持的字段语义，归一化前保留字段存在性 |
| usage.iterations 等嵌套子用量 | 不递归累计，不将子对象当最终总用量 |

缓存写总量及 5m/1h 拆分须一致更新：后续明确的总量或拆分修订不能遗留旧 TTL 分量而多计费用。细化规则必须由回归 fixture 固定；原始统计不得复用会补零或逐字段取最大值的整形归一化状态。

非流式仅有一个 usage 时记录该对象的有效字段。开启模式下，采集点放在归一化、零用量修复和整形之前，确保明确返回的原始数值不被修复值覆盖。完全没有可用上游 usage 时沿用现有缺失用量估算策略，并保留 `usageEstimated` 标识；存在有效显式零的 usage 不算缺失。该兼容性回退不改变下游已有的零用量修复/缺失估算处理。

## 隔离实现边界

保留现有 `capture.raw` 和 `merge_external_usage`，继续供整形、零用量修复及回退判断使用；新增仅供原始统计使用的上游字段状态。不要直接修改共用 max 合并函数。

账号模式只在最终生成统计快照时选择原始统计来源。旧模式完整走原有统计路径；新模式仅替换统计中的 rawUsage 及对应价格计算输入，不反向输入投影、整形、成本兜底或客户端回包。`reported`/`shaped` 的回退仍以原处理链状态为依据。

允许随模式改变：

- `rawUsage` / 顶层 `raw_usage`。
- `rawCostUsd` / `originalCostUsd` 与上游原始成本汇总，仍按现有价格目录计算，不读取站点账单金额。
- 原始成本派生的利润、成本差额及成本对比标志。
- `claude-main` 的 `account_cost_usd = raw_cost_usd * cost_multiplier`；倍率及公式保持原样。

必须保持：

- 客户端收到的 SSE/JSON 正文、usage、事件顺序。
- `shapedUsage`、`reportedUsage`、`shapedCostUsd`、`reportedCostUsd`、`upliftedCostUsd`、`billableCostUsd`。
- usage 整形、零用量修复、缺失估算、补偿和成本兜底的实际执行结果。
- 路由、调度、重试、模型映射、账号启停、分组及现有价格策略。

成本差额/对比标志变化属于统计结果，不意味着重新执行或调整成本兜底。不同来源不得以修正后的 rawCost 改写既有整形结果。

## 验收与发布

1. 验证默认/关闭时与修复前完全一致；开启后 ddpapi 的初始大值→最终 1 得到正确原始统计。同一输入下切换开关，客户端完整回包、shaped/reported 及补偿后计费不变。
2. 覆盖字段缺失、显式零、null/非法值、只更新 output、缓存总量/TTL 修订、OpenAI 字段、多事件与分块 SSE、非流式、异常断开、重试切池、缺失 usage 估算。用确定性整形配置排除随机抖动。
3. 回放 171 条脱敏样本：开启后的 16 条 ddpapi 流式使用最后明确值；其余 155 条保持一致。关闭后保持全部现有结果。
4. 使用隔离 PostgreSQL/Redis 验证策略默认、更新遗漏、复制、重启持久性及使用记录/原始成本汇总；验证迁移关闭时能准确诊断缺列。前端验证新建默认关闭、编辑回显、保存、复制，并通过现有构建检查。
5. `main` 实现并验证后合入 `forced-main`，保留 forced 自有差异；将同等改动移植至硬分叉 `claude-main`，不直接合并。三个分支各自跑相关测试、默认/无默认特性及现有发布门禁。
6. 业务提交与版本提交分开；发布前刷新远端分支、tag 和真实发布产物，分别确定 `v*`、`forced-v*`、`claude-v*` 的下一 patch；等待质量检查、amd64/arm64 和 manifest 成功再认定发布完成。
7. 发版后已有账号仍全部保持旧模式，不自动开启 ddpapi 或其它账号，不自动升级生产实例。启用只影响后续采用新策略的 attempt，不回填历史数据；关闭开关可恢复后续请求的原统计方式。

## 当前进度

main 的账号配置、默认兼容迁移、CRUD/复制、表单、计费来源快照及隔离采集已实现。新增 8 项测试和 UI 88 项合同通过；外部池扩展验证 429 通过、7 忽略、1 项准备门禁 1 秒超时，待独立重跑。forced 合并、claude 独立移植验证及三分支发布仍在进行，未声称发布完成。
