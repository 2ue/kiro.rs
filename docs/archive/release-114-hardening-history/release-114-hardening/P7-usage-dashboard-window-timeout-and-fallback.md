# P7 - 159 机器 usage-dashboard/windows 超时与 fallback

日期：2026-07-26

## 现象

在 159 机器上，用户看到的 Dashboard 现象是：

- `/api/admin/usage-dashboard`
- `/api/admin/usage-dashboard/windows`

会出现：

- 页面“总览加载失败”
- `usage dashboard 查询繁忙，请稍后重试`
- `error returned from database`

而同一批数据下，`/series`、`/top`、部分 breakdown 路径又可能是 200。

这说明问题不是简单的“整库坏了”，而是 dashboard 的某个高成本窗口聚合在大表上超时。

## 生产证据

现网证据指向的是 PostgreSQL statement timeout，而不是账号池或 Redis 本身：

- `dashboard_windows` / `dashboard` 500
- 后端报错接近：`canceling statement due to statement timeout`
- `usage_records` 表很大，达到 GB 级
- 现网缺少当前代码期望的某些索引
- 这类超时发生在 dashboard 聚合阶段，不是在核心请求路由阶段

这类错误会让人误以为“数据没了”或者“服务整体挂了”，但实际上更像是某个重查询把总览拖死。

## 根因判断

当前最合理的根因链条是：

1. `usage_records` 体积很大。
2. 精确 `dashboard_windows` 需要扫描/聚合大量历史行。
3. 某些索引缺失或未及时维护，导致 SQL 在 statement_timeout 前跑不完。
4. 总览页把精确窗口聚合放在主路径上，于是出现 500。

这不是主业务 dispatch 链路的问题，也不应该因此让账号调度或请求接入一并失败。

## 已经落地的修复

我已经把 dashboard 窗口聚合改成了“精确优先、失败降级”：

- `dashboard_windows_with_basic_fallback()` 先跑精确窗口；
- 一旦精确窗口失败，就降级到 `dashboard_windows_basic_from_series()`；
- `dashboard()` 在 `populate_dashboard_window_details()` 失败时只记录 warning，不再把整个页面打成 500；
- basic fallback 保留核心 series 口径：
  - `total_requests`
  - `success_requests`
  - `error_requests`
  - `total_input_tokens`
  - `billable_input_tokens`
  - `total_output_tokens`
  - `total_estimated_cost_usd`
  - `total_original_cost_usd`

也就是说，页面现在优先保证“能打开、能看核心趋势”，而不是被一个超时的精确 SQL 卡死。

## 为什么这是对的

dashboard 是管理/观测面，不是核心请求转发面。

在高流量、历史数据很大、索引又没完全跟上的情况下，继续强迫 dashboard 主路径等精确聚合，只会把“看板查询慢”放大成“整个页面 500”。

降级策略的目标是：

- 主业务不被 dashboard 拖死；
- dashboard 先可用；
- 精确值后续通过索引维护恢复。

## 还没自动修掉的部分

这个 fallback 只能保证可用性，不能替代索引维护。

如果你希望 exact window 也恢复到稳定快速，仍然要做下面这类维护：

- 补齐当前代码期望的 usage index；
- 按维护命令创建缺失索引；
- 在低峰复核 exact `/usage-dashboard/windows`；
- 确认 statement timeout 不再触发。

所以这个问题的正确结论是：

- “页面 500” 已经有保护；
- “精确窗口查询慢” 还需要索引层持续治理。

## 复现方式

如果要在隔离环境里复现，最简单的方法是：

1. 构造一个较大的 `usage_records` 表；
2. 移除精确窗口查询依赖的索引；
3. 触发 `/api/admin/usage-dashboard/windows`；
4. 观察 statement timeout；
5. 再确认 fallback 后页面不再 500。

## 验证

我已经补了对应单测：

- `dashboard_window_basic_fallback_preserves_core_series_metrics`

这个测试通过，说明 fallback 至少保住了核心 series 口径，不会把页面直接打空。

## 结论

159 机器的问题不是“总览数据真的消失”，而是“精确 dashboard 窗口聚合在大表上超时”。

现在代码层的修复已经把它从“页面级 500”降级为“精确值可继续优化、页面先可用”。

