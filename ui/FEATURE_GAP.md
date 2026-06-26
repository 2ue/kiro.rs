# 功能补全清单(对比老系统 admin-ui-daisy)

> 审计结论:重构丢失大量功能。本清单逐项追踪补全,完成一项勾一项。
> 核对基准:`admin-ui-daisy/src/components/*.tsx`

## P0 — 运行配置(老 ConfigPanel 13 tab,风险最高,含序列化)
- [ ] 用量展示规则(usage tab):reportedUsage.default 完整 path policy 编辑器(input/output/cacheRead/cacheCreation 各 mode/maxTokens/targetTokens/normalMaxMultiplier/moveDeltaToCacheRead)+ finalCacheRead* + pathOverrides 可变长列表
- [ ] 缓存创建频次(cacheCreate tab):promptCacheCreationControl 9 字段
- [ ] 旧内容清理(payloadHistory tab):payloadShaping 9 子字段
- [ ] 当前内容兜底(payloadFallback tab):payloadShaping 9 子字段
- [ ] 模型映射规则编辑器(compat):modelMapping.enabled/autoGenerateRules/rules JSON + "填充默认规则"按钮 + modelResolutionMode + 规则计数
- [ ] kiroAgentModeStrategy 字段(compat)
- [ ] schedulerTotalSelectionWeight 字段(scheduler)
- [ ] payloadGuard 两条数值校验
- [ ] **关键**:normalizeConfig 调用 normalizeReportedUsage + normalizePromptCacheCreationControl(否则配置存不进)
- [ ] 后台登录密码设置(access,确认是否=Admin Key 之外的独立项)

## P0 — 账号页
- [ ] 积分统计面板:更新积分统计按钮 + 剩余可用积分汇总 + 最近查询时间 + 明细弹窗
- [ ] 清除已禁用账号按钮(全量,工具栏)
- [ ] 批量查询积分操作
- [ ] 排序补 refresh_failure_count
- [ ] 展开态字段:schedulerSelectionCount、recentScheduler*(60s/10s/5m)、schedulerSelectionPressure、inFlightLeaseMaxSecs、oldest/newestInFlight*、inProbation+remaining、pricedCoverage、per-model cooldowns 列表

## P1 — 总览页
- [ ] 外部池计费拆分面板(ExternalPoolBillingPanel:raw/shaped/uplifted/profit + byPool)
- [ ] 状态分布 + 用量来源分解面板(BreakdownPanel)

## P1 — 用量页
- [ ] 主表列:账号列、缓存列(读写量+率)、调用链路列、endpoint/stream/sticky/usageSource badge
- [ ] 筛选:endpoint、conversationId、routeTarget(账号/外部池)、stream 模式、minCacheRead、重置按钮
- [ ] 详情:调用链路表格(模型/动作/错误列)、errorDetail pre、payloadBreakdown/guardReport、外部池计费 shaped/uplifted token 快照、modelResolutionNote、用量口径说明
- [ ] 独立计费弹窗 UsageBillingModal
- [ ] 清理:batchSize、pauseMs、maxBatches 说明、jobId/stopReason、预览 newestCreatedAt
- [ ] 明细列表自动刷新(refetchInterval 传入)
- [ ] 实时 RPM/TPM、highCacheRequests 统计卡

## P1 — 成本页
- [ ] 能力目录独立表:displayName、supportsPromptCaching、supportedInputTypes、maxOutputTokens 独立列
- [ ] 能力/价格状态卡(available/来源/count/lastSyncedAt)
- [ ] 手动模型 Modal:supportedInputTypes(TEXT/IMAGE)、description、数值校验
- [ ] 价格来源 URL 展示

## P2 — 外部池/代理
- [ ] 外部池全局策略:用量补偿配置段(uplift percent/min tokens)
- [ ] 代理卡片展开态渲染 CredentialBindingPicker(绑定账号批量管理)
