# Dashboard 费用、积分与外部池计费口径验证

验证日期：2026-08-30  
验证范围：当前工作区源码与本机隔离服务；未连接生产机器、未读取生产数据库。

## 结论摘要

Dashboard 现在把两种费用明确分开：

| 页面字段 | 后端字段 | 含义 |
| --- | --- | --- |
| 窗口实际费用（原始计费） | `totalOriginalCostUsd` / `windowOriginalCostUsd` | 优先按上游原始 usage 计算；上游没有原始 usage 时回退到估算成本。 |
| 窗口估算成本 | `totalEstimatedCostUsd` / `windowEstimatedCostUsd` | 按最终记录 usage 和当前价格表计算，不代表上游实际扣费。 |
| 累计实际费用（原始计费） | `lifetimeOriginalCostUsd` | 累计原始计费口径，同样遵守原始 usage 缺失时的 fallback。 |
| 累计估算成本 | `lifetimeEstimatedCostUsd` | 累计最终 usage 估算值。 |

因此，“窗口实际费用”和“累计实际费用”当前展示的是**原始计费口径**，不是估算成本。表头和 tooltip 已明确这一点。

## 积分口径

Dashboard 中的“窗口积分消耗”和账号表中的“窗口积分消耗”来自请求级 `meteringEvent`，对应后端字段 `kiroMeteringUsage` / `totalKiroMeteringUsage`。

这不是账号订阅接口返回的“剩余积分”：

- 请求级积分：统计窗口内实际请求的计量消耗，可按模型、Endpoint、账号聚合。
- 余额快照：账号最近一次订阅/额度查询保存的 `creditUsed`、`creditLimit`、`creditRemaining`。
- Dashboard 读取账号余额时只读数据库快照，不主动发起订阅查询，不刷新 Token，也不影响调度。

## 外部池为什么看起来没有数据

外部池计费明细不嵌在窗口摘要的单个字段中，而是通过独立接口获取：

```text
GET /api/admin/usage-dashboard/external-pool-billing?windowKey=today
```

窗口摘要接口只返回窗口级汇总；UI 会在加载窗口后再调用上面的独立接口，按池显示：

- 上游原始成本；
- 展示/整形后成本；
- 补偿后成本；
- 差额及差额占原始成本百分比；
- 原始/整形倍率；
- 未计价请求数与成本 floor 兜底次数。

初始隔离配置使用 `pass_through`，所以原始成本、展示成本和补偿后成本相同，看不到差额和倍率变化。将池切换为 `current_path_policy` 并通过真实 HTTP 请求生成样本后，差异可以被观察到。

## 隔离环境与数据重建

本次只使用本机隔离实例：

- Dashboard API：`127.0.0.1:19022`
- 普通 UI：`127.0.0.1:19023`
- Admin UI：`127.0.0.1:19025`
- PostgreSQL：独立临时数据库
- 外部池 fixture：`127.0.0.1:39091`
- 外部池 ID：`1`
- 外部池名称：`Dashboard fixture pool`
- 测试 key：`sk-test-admin`、`sk-test-client`（仅隔离环境）

外部池切换请求：

```bash
curl -X PUT \
  -H 'x-api-key: sk-test-admin' \
  -H 'content-type: application/json' \
  http://127.0.0.1:19022/api/admin/external-pools/1 \
  -d '{"usageProjectionMode":"current_path_policy"}'
```

随后通过 `/cc/v1/messages` 发送成功请求，fixture 返回合法 Anthropic JSON usage。请求使用 `claude-sonnet-4-5`，该模型存在于本地价格表，因此可以计价。

## 重建后的接口证据

窗口摘要：

```text
总请求：147
成功：84
错误：63
窗口估算成本：0.1335729 USD
窗口原始计费：0.126837 USD
窗口 Kiro 积分：16.5
外部池请求：43
外部池可计价：17
外部池未计价：26
```

外部池明细：

```json
{
  "poolId": 1,
  "poolName": "Dashboard fixture pool",
  "requests": 43,
  "pricedRequests": 17,
  "unpricedRequests": 26,
  "rawCostUsd": 0.126837,
  "shapedCostUsd": 0.0922083,
  "upliftedCostUsd": 0.1335729,
  "profitUsd": 0.0067359,
  "reportedCostUsd": 0.1335729,
  "billableCostUsd": 0.1335729,
  "costFloorDeltaUsd": 0
}
```

由此可计算：

- 外部池总体差额占原始成本：`0.0067359 / 0.126837 = 5.31%`；
- 原始/整形倍率：`0.126837 / 0.0922083 = 1.376`；
- 若原始成本为 100、整形后为 200，原始/整形倍率为 `0.500`；
- 若原始成本为 100、整形后为 150，原始/整形倍率为 `0.667`。

## 全量账号与积分验证

账号接口：

```text
GET /api/admin/usage-dashboard/accounts?windowKey=today&page=1&pageSize=100
```

隔离数据结果：

- 已配置本地账号：5,000；
- 窗口活跃账号：67；
- 窗口空闲账号：4,933；
- `complete=true`；
- 服务端按契约将过小 `pageSize` 收敛为 20；
- 返回字段同时包含窗口/累计积分、窗口/累计原始计费、窗口/累计估算成本和余额快照。

账号表现在保留零请求账号并分页展示，不再只显示部分活跃账号。窗口积分列展示请求级积分，余额快照列展示已保存额度，两者不会混为一个指标。

## 代码与验证

相关实现：

- 普通 UI 费用与外部池面板：[ui/src/features/usage/usage-billing.tsx](../../ui/src/features/usage/usage-billing.tsx)
- 普通 UI Dashboard：[ui/src/features/overview/overview-page.tsx](../../ui/src/features/overview/overview-page.tsx)
- Admin UI Dashboard：[admin-ui/src/components/usage-dashboard-panel.tsx](../../admin-ui/src/components/usage-dashboard-panel.tsx)
- usage 解析和费用口径：[src/anthropic/usage.rs](../../src/anthropic/usage.rs)
- Dashboard 聚合与分页：[src/admin/service.rs](../../src/admin/service.rs)、[src/storage/postgres.rs](../../src/storage/postgres.rs)

已通过：

```text
cd ui && pnpm check
cd admin-ui && pnpm exec tsc -b --pretty false
cd admin-ui && pnpm build
cargo check --locked
git diff --check
```

