export interface UsageExternalPoolRiskQuery {
  timezone?: string
  windowKey?: string
  since?: string
  until?: string
  warningThresholdTokens?: number
  criticalThresholdTokens?: number
  externalPoolId?: number
  endpoint?: string
  model?: string
  stream?: boolean
  limit?: number
}

export interface UsageExternalPoolRiskWindow {
  key: string
  label: string
  from: string
  to: string
}

export interface UsageExternalPoolRiskThresholds {
  warningTokens: number
  criticalTokens: number
  costFloorEnabled: boolean
  costFloorMarginPercent: number
  costTargetMultiplier: number
}

export interface UsageExternalPoolRiskFilters {
  poolId?: number
  endpoint?: string
  model?: string
  stream?: boolean
}

export interface UsageExternalPoolRiskTotals {
  records: number
  successRecords: number
  errorRecords: number
  streamRecords: number
  nonStreamRecords: number
  pricedRecords: number
  unpricedRecords: number
  rawUsageRecords: number
  reportedUsageRecords: number
  missingExternalPoolBillingRecords: number
  outputZeroRecords: number
}

export interface UsageExternalPoolRiskCacheStats {
  minReadTokens: number
  maxReadTokens: number
  avgReadTokens: number
  totalReadTokens: number
  minWriteTokens: number
  maxWriteTokens: number
  avgWriteTokens: number
  totalWriteTokens: number
  readWarningCount: number
  writeWarningCount: number
  eitherWarningCount: number
  readCriticalCount: number
  writeCriticalCount: number
  eitherCriticalCount: number
}

export interface UsageExternalPoolRiskCostStats {
  rawCostUsd: number
  reportedCostUsd: number
  targetCostUsd: number
  profitUsd: number
  totalLossUsd: number
  totalTargetGapUsd: number
  maxLossUsd: number
  maxTargetGapUsd: number
  maxRawCostUsd: number
  maxReportedCostUsd: number
  belowRawCount: number
  belowTargetCount: number
  costFloorAppliedRecords: number
  minCostRatio?: number | null
  avgCostRatio?: number | null
  maxCostRatio?: number | null
}

export interface UsageExternalPoolRiskBucket {
  key: string
  label: string
  minTokens?: number | null
  maxTokens?: number | null
  rawReadCount: number
  rawWriteCount: number
  reportedReadCount: number
  reportedWriteCount: number
}

export interface UsageExternalPoolRiskGroup {
  key: string
  label: string
  records: number
  successRecords: number
  warningRecords: number
  criticalRecords: number
  outputZeroRecords: number
  rawReadMax: number
  rawWriteMax: number
  reportedReadMax: number
  reportedWriteMax: number
  rawCostUsd: number
  reportedCostUsd: number
  targetCostUsd: number
  profitUsd: number
  totalLossUsd: number
  totalTargetGapUsd: number
  belowRawCount: number
  belowTargetCount: number
}

export interface UsageExternalPoolRiskSample {
  id: string
  createdAt: string
  endpoint: string
  stream: boolean
  model: string
  status: string
  externalPoolId?: number
  externalPoolName?: string
  pricingModel?: string
  usageProjectionMode?: string
  externalPoolBillingPresent: boolean
  costFloorApplied: boolean
  rawInputTokens: number
  rawOutputTokens: number
  rawCacheReadInputTokens: number
  rawCacheCreationInputTokens: number
  reportedInputTokens: number
  reportedOutputTokens: number
  reportedCacheReadInputTokens: number
  reportedCacheCreationInputTokens: number
  rawCostUsd: number
  reportedCostUsd: number
  targetCostUsd: number
  lossUsd: number
  targetGapUsd: number
  costRatio?: number | null
  riskReasons: string[]
}

export interface UsageExternalPoolRiskResponse {
  generatedAt: string
  timezone: string
  window: UsageExternalPoolRiskWindow
  thresholds: UsageExternalPoolRiskThresholds
  filters: UsageExternalPoolRiskFilters
  totals: UsageExternalPoolRiskTotals
  rawCache: UsageExternalPoolRiskCacheStats
  reportedCache: UsageExternalPoolRiskCacheStats
  cost: UsageExternalPoolRiskCostStats
  buckets: UsageExternalPoolRiskBucket[]
  byPool: UsageExternalPoolRiskGroup[]
  byPath: UsageExternalPoolRiskGroup[]
  byModel: UsageExternalPoolRiskGroup[]
  samples: UsageExternalPoolRiskSample[]
}
