# Dashboard 全量账号接口大数据验证

验证日期：2026-08-29  
验证范围：当前工作区源码，隔离本机服务，不连接生产。

## 隔离数据集

- PostgreSQL 独立临时数据库。
- 5,000 个本地 API-key 账号。
- 5,000 条 `usage_rollup_totals` credential 维度记录。
- 3,500 条完整小时 `usage_rollup_time_buckets` 记录。
- 250 条当前小时边界 `usage_records`。
- 额外插入同名 `model` 维度 key，用于验证账号聚合不会发生维度串数据。

## HTTP 契约结果

接口：

`GET /api/admin/usage-dashboard/accounts`

结果：

| 场景 | 状态 | 关键结果 |
| --- | ---: | --- |
| 第 1 页，`pageSize=50` | 200 | `total=5000`、`totalPages=100`、返回 50 条、ID 1-50 |
| 最后一页，`page=25&pageSize=200` | 200 | 返回 200 条、ID 4801-5000 |
| `pageSize=1` | 200 | 服务端按合同收敛为 `pageSize=20`，返回 20 条 |
| `status=disabled` | 200 | `filteredTotal=294`，返回项全部禁用 |
| `status=idle` | 200 | `filteredTotal=1500`，返回项窗口用量均为 0 |
| `q=dashboard-4999` | 200 | `filteredTotal=1`，命中账号 4999 |
| 未知 `windowKey` | 200 | 回退到 `windowKey=today` |
| 缺少 Admin key | 401 | 未授权请求被拒绝 |

账号 1 的聚合断言：

- lifetime requests = 1001；
- 当前窗口 requests = 12（完整小时 rollup + 当前小时边界记录）；
- 当前窗口 error requests = 1；
- 当前窗口 input tokens = 1202。

这些断言同时验证了 partial-hour 边界合并和 credential 维度过滤。

## 并发大数据结果

- 请求数：60。
- 客户端并发：12。
- 每次请求 `pageSize=200`，混合 UTC/Asia-Shanghai、ID/用量排序。
- 结果：60/60 返回 HTTP 200，60/60 `complete=true`，60/60 `total=5000` 且每次返回 200 条。
- 总耗时：约 6.02 秒。
- 延迟：p50 约 1.10 秒，p95 约 1.35 秒，p99 约 1.46 秒，最大约 1.49 秒。
- 并发期间健康探针：59/59 返回 HTTP 200。

## 资源观察

服务进程在压测前约 49 MB RSS、38-40 个 FD；压测结束后短时峰值约 277 MB RSS、60 个 FD。等待请求完成后：

- 5 秒：约 51 MB RSS、47 个 FD；
- 15 秒：约 47 MB RSS、46 个 FD；
- 30 秒：约 79 MB RSS、46 个 FD。

FD 数量回落，未观察到请求结束后的持续增长。30 秒采样期间的 RSS 波动来自假 API-key 触发的后台模型能力探测，不是 dashboard 查询路径；`/healthz` 与 `/readyz` 仍保持 200。

## 结论

当前 `/api/admin/usage-dashboard/accounts` 在 5,000 账号规模下能够正常完成聚合和分页，未复现 Top 10 截断、N+1 请求、零用量账号丢失或维度串数据。此次验证使用的是隔离服务和临时数据库，测试完成后已删除原始响应、临时二进制、临时数据库和服务进程。
