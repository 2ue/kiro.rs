# 功能补全清单(对比老系统 admin-ui-daisy)— 已完成

> 审计基准:`admin-ui-daisy/src/components/*.tsx`。全部缺失项已补全并经 tsc/build/CDP 验证。

## P0 — 运行配置(老 ConfigPanel 13 tab)✅
- [x] 用量展示规则(reportedUsage:default policy + pathOverrides 可变长)
- [x] 缓存创建频次(promptCacheCreationControl 9 字段)
- [x] 旧内容清理(payloadShaping 历史相关 9 字段)
- [x] 当前内容兜底(payloadShaping 当前相关 9 字段)
- [x] 模型映射规则编辑器 + 填充默认规则按钮 + modelResolutionMode
- [x] kiroAgentModeStrategy 字段
- [x] schedulerTotalSelectionWeight 字段
- [x] payloadGuard 两条数值校验
- [x] normalizeConfig 调用 normalizeReportedUsage + normalizePromptCacheCreationControl
- [x] 后台登录密码 = Admin Key(安全页已实现)

## P0 — 账号页 ✅
- [x] 积分统计面板(更新按钮 + 汇总 + 最近查询 + 明细弹窗)
- [x] 清除全部已禁用按钮(主工具栏)
- [x] 批量查询积分
- [x] 排序补 refresh_failure_count(13 维)
- [x] 展开态字段:总调度/近期调度三窗口/调度压力/Lease/观察期/计价覆盖/per-model cooldowns

## P1 — 总览页 ✅
- [x] 外部池计费拆分面板(raw/shaped/uplifted/profit + byPool)
- [x] 状态分布 + 用量来源分解面板

## P1 — 用量页 ✅
- [x] 主表列:账号/缓存/调用链路/endpoint/stream/sticky/usageSource badge
- [x] 筛选:endpoint/conversationId/routeTarget/stream/minCacheRead/重置
- [x] 详情:调用链路表格/errorDetail/payload 诊断/外部池计费快照/modelResolutionNote/用量口径
- [x] 清理:batchSize/pauseMs/maxBatches/jobId/stopReason/newestCreatedAt
- [x] 明细列表自动刷新
- [x] 实时 RPM/TPM、highCacheRequests 统计卡

## P1 — 成本页 ✅
- [x] 能力目录独立表(displayName/supportsPromptCaching/supportedInputTypes/maxOutputTokens)
- [x] 能力/价格状态卡 + 价格来源 URL
- [x] 手动模型 Modal:supportedInputTypes/description/数值校验

## P2 — 外部池/代理 ✅
- [x] 外部池用量补偿配置段(经核实原已存在)
- [x] 代理卡片展开态 CredentialBindingPicker(绑定账号批量管理)
