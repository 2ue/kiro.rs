import type { ExternalPoolStreamResponseMode } from './config'

export type UsageCleanupMode = 'soft_delete' | 'hard_delete'
export type UsageCleanupJobStatus = 'idle' | 'queued' | 'running' | 'paused' | 'completed' | 'cancelled' | 'failed'
export type UsageCleanupPhase = 'idle' | 'postgres' | 'redis_admin_cache' | 'redis_snapshots' | 'complete'

export interface UsageCleanupRequest {
  mode?: UsageCleanupMode
  olderThanDays?: number
  cutoffBefore?: string
  batchSize?: number
  maxBatches?: number
  pauseMsBetweenBatches?: number
}

export interface UsageCleanupPreviewResponse {
  mode: UsageCleanupMode
  cutoffAt: string
  matchedRows: number
  oldestCreatedAt?: string
  newestCreatedAt?: string
}

export interface UsageCleanupStatusResponse {
  jobId?: string
  status: UsageCleanupJobStatus
  phase: UsageCleanupPhase
  mode?: UsageCleanupMode
  cutoffAt?: string
  batchSize: number
  maxBatches: number
  pauseMsBetweenBatches: number
  matchedRows?: number
  remainingRows?: number
  processedRows: number
  lastBatchRows: number
  batches: number
  redisDeletedKeys: number
  redisDeleteCommands: number
  redisMaxCommandKeys: number
  redisScanPasses: number
  redisUsedDelFallback: boolean
  redisPassLimitReached: boolean
  cancelRequested: boolean
  stopReason?: string
  startedAt?: string
  updatedAt?: string
  finishedAt?: string
  lastError?: string
}

export type UsageRecordStatus =
  | 'success'
  | 'error'
  | 'stream_error'
  | 'upstream_timeout'
  | 'client_dropped'

export type UsageSource =
  | 'upstream_metadata'
  | 'local_prompt_cache'
  | 'context_estimate'
  | 'request_estimate'
  | 'none'

export type UsageRouteKindFilter = 'local_credential' | 'external_pool'

export interface InferenceAttemptSnapshot {
  maxAttempts: number
  consumed: number
  localAttempts: number
  externalAttempts: number
  mcpAttempts: number
  exhausted: boolean
  downstreamCommitted: boolean
}

export interface AuxiliaryAttemptSnapshot {
  maxAttempts: number
  consumed: number
  tokenRefreshAttempts: number
  profileDiscoveryAttempts: number
  exhausted: boolean
}

export interface UsageLatencyTrace {
  inferenceAttempts?: InferenceAttemptSnapshot
  auxiliaryAttempts?: AuxiliaryAttemptSnapshot
  capacityWeightUnits?: number
  estimatedInputTokens?: number
  payloadGuardMs?: number
  upstreamHeaderMs?: number
  firstUpstreamChunkMs?: number
  firstOutputDeltaMs?: number
  firstThinkingDeltaMs?: number
  firstVisibleTextDeltaMs?: number
  streamGapToFirstOutputMs?: number
  chunksBeforeFirstOutput?: number
  eventsBeforeFirstOutput?: number
  upstreamBytesBeforeFirstOutput?: number
  upstreamFramesBeforeFirstOutput?: number
  upstreamEventsBeforeFirstOutput?: number
  upstreamFramesWithoutDownstreamEventsBeforeFirstOutput?: number
  upstreamPendingChunksBeforeFirstOutput?: number
  upstreamFrameDecodeErrorsBeforeFirstOutput?: number
  upstreamEventParseErrorsBeforeFirstOutput?: number
  upstreamEventTypesBeforeFirstOutput?: Record<string, number>
  streamRetryAttempts?: number
  streamRetryDispatchFailures?: number
  streamRetryReasons?: string[]
  clientDroppedMs?: number
  terminalReason?: 'completed' | 'upstream_status_error' | 'upstream_json_exception' | 'upstream_idle_timeout' | 'first_output_timeout' | 'malformed_sse' | 'client_dropped' | 'internal_error'
  upstreamMessageStatus?: string
  sawUpstreamCompleted?: boolean
  stopReasonSource?: string
  suspectedIntentPreambleEndTurn?: boolean
  intentPreambleRisk?: 'none' | 'low' | 'medium' | 'high' | string
  suspectedToolContextLeakEndTurn?: boolean
  toolContextLeakMarkers?: string[]
  assistantTailIntentHint?: boolean
  endTurnAnomalyReason?: string
  endTurnAnomalyRisk?: 'none' | 'low' | 'medium' | 'high' | string
  upstreamEofWithoutCompleted?: boolean
  lastUpstreamEventType?: string
  lastUpstreamEvents?: string[]
  sawUpstreamAssistantResponse?: boolean
  sawUpstreamToolUse?: boolean
  sawUpstreamMetadata?: boolean
  lastAssistantContentChars?: number
  filteredTrivialTextBlocks?: number
  filteredTrivialTextChars?: number
}

export interface KiroCredentialAttempt {
  attempt: number
  credentialId: number
  credentialLabel?: string
  status?: number
  statusText?: string
  action: string
  model?: string
  errorType?: string
  errorMessage?: string
  rawUpstreamError?: RawUpstreamError
  durationMs: number
}

export interface RawUpstreamError {
  source: string
  statusCode?: number
  contentType?: string
  body: string
  bodyBytes: number
  truncated: boolean
}

export interface UsageRecord {
  id: string
  createdAt: string
  endpoint: string
  stream: boolean
  model: string
  requestedMaxTokens?: number
  upstreamModel?: string
  externalOutboundModel?: string
  modelResolutionSource?: string
  modelResolutionNote?: string
  conversationId?: string
  requestApiKeyId?: string
  credentialId?: number
  credentialLabel?: string
  status: UsageRecordStatus
  usageSource: UsageSource
  totalInputTokens: number
  compatInputTokens: number
  billableInputTokens: number
  outputTokens: number
  cacheReadInputTokens: number
  cacheCreationInputTokens: number
  cacheCreation5mInputTokens: number
  cacheCreation1hInputTokens: number
  estimatedCostUsd: number
  originalCostUsd: number
  kiroMeteringUsage: number
  pricingAvailable: boolean
  pricingModel?: string
  durationMs: number
  firstTokenLatencyMs?: number
  responseLatencyMs?: number
  latencyTrace?: UsageLatencyTrace
  simulated: boolean
  stickyBound: boolean
  fallbackFromSticky: boolean
  credentialAttempts?: KiroCredentialAttempt[]
  routeKind?: 'local_credential' | 'external_pool'
  routeSubtype?: 'local_success' | 'local_error_no_fallback' | 'local_rescue_after_external' | 'external_fallback_preflight' | 'external_fallback_after_local_attempts' | 'external_direct_policy' | 'external_route_policy' | 'external_error'
  fallbackReason?: string
  directPolicyReason?: string
  localAttempted?: boolean
  localPreflight?: unknown
  externalPoolId?: number
  externalPoolName?: string
  externalAttempts?: ExternalPoolAttempt[]
  usageProjectionApplied?: boolean
  externalPoolBilling?: ExternalPoolBilling
  errorType?: string
  errorMessage?: string
  errorDetail?: string
  errorStatusCode?: number
  errorSource?: string
  errorId?: string
  errorMetadata?: unknown
  rawUpstreamError?: RawUpstreamError
  publicErrorStatusCode?: number
  publicErrorType?: string
  publicErrorMessage?: string
  payloadBreakdown?: unknown
  payloadGuardReport?: unknown
}

export interface UsageRecorderStats {
  accepting?: boolean
  inMemoryLimit: number
  inMemoryRecords: number
  redisEnabled: boolean
  redisQueueEnabled: boolean
  redisQueueCapacity: number
  redisQueueAvailable: number
  redisWriterAccepted?: number
  redisWriterFinished?: number
  backpressuredRedisRecords?: number
  droppedRedisRecords: number
  postgresEnabled: boolean
  writerQueueEnabled: boolean
  writerQueueCapacity: number
  writerQueueAvailable: number
  writerAccepted?: number
  writerFinished?: number
  backpressuredPersistRecords?: number
  droppedPersistRecords: number
  rejectedAfterShutdown?: number
  rejectedByCleanupWatermark?: number
}

export interface ExternalPoolUsageSnapshot {
  totalInputTokens: number
  inputTokens: number
  billableInputTokens: number
  outputTokens: number
  cacheReadInputTokens: number
  cacheCreationInputTokens: number
  cacheCreation5mInputTokens: number
  cacheCreation1hInputTokens: number
}

export interface ExternalPoolBilling {
  requestInputTokens?: number
  rawUsage: ExternalPoolUsageSnapshot
  shapedUsage?: ExternalPoolUsageSnapshot
  reportedUsage: ExternalPoolUsageSnapshot
  usageProjectionApplied: boolean
  rawCostUsd: number
  shapedCostUsd?: number
  upliftedCostUsd?: number
  profitUsd?: number
  reportedCostUsd: number
  billableCostUsd: number
  costFloorDeltaUsd: number
  costFloorApplied: boolean
  pricingAvailable: boolean
  pricingModel?: string
  usageProjectionMode: string
  streamResponseMode?: ExternalPoolStreamResponseMode
  usageEstimated?: boolean
  usageEstimateReason?: string
  usageCandidatePath?: string
  bodyUsageProjectionApplied?: boolean
}

export interface ExternalPoolAttempt {
  attempt: number
  poolId: number
  poolName: string
  outboundModel?: string
  status?: number
  action: string
  durationMs: number
  errorType?: string
  errorMessage?: string
  rawUpstreamError?: RawUpstreamError
}

export interface UsageRecordsResult {
  total: number
  records: UsageRecord[]
}

export interface UsageRecordsPageResult {
  page: number
  limit: number
  hasNext: boolean
  records: UsageRecord[]
}

export interface UsageAggregate {
  key: string
  label?: string
  requests: number
  cacheReadInputTokens: number
  cacheCreationInputTokens: number
  estimatedCostUsd: number
  originalCostUsd: number
}

export interface UsageRealtimeStats {
  windowSeconds: number
  requests: number
  successRequests?: number
  errorRequests?: number
  rpm: number
  successRpm?: number
  errorRpm?: number
  inputTpm: number
  outputTpm: number
  totalTpm: number
  billableTpm: number
}

export interface UsageSummary {
  totalRequests: number
  successRequests: number
  errorRequests: number
  highCacheRequests: number
  totalInputTokens: number
  totalOutputTokens: number
  totalCacheReadInputTokens: number
  totalCacheCreationInputTokens: number
  totalEstimatedCostUsd: number
  totalOriginalCostUsd: number
  totalKiroMeteringUsage: number
  pricedRequests: number
  unpricedRequests: number
  localPromptCacheRequests: number
  localPromptCacheInputTokens: number
  localPromptCacheReadInputTokens: number
  localPromptCacheCreationInputTokens: number
  simulatedRequests: number
  upstreamMetadataRequests: number
  externalPoolBilling?: UsageExternalPoolBillingSummary
  realtime: UsageRealtimeStats
  topCredentials: UsageAggregate[]
  topConversations: UsageAggregate[]
}

export interface UsageExternalPoolBillingSummary {
  requests: number
  pricedRequests: number
  unpricedRequests: number
  costFloorAppliedRequests: number
  rawCostUsd: number
  shapedCostUsd?: number
  upliftedCostUsd?: number
  profitUsd?: number
  reportedCostUsd: number
  billableCostUsd: number
  costFloorDeltaUsd: number
}

export interface UsageExternalPoolBillingByPool extends UsageExternalPoolBillingSummary {
  poolId: number
  poolName: string
}

export interface UsageDashboardResponse {
  generatedAt: string
  timezone: string
  windows: UsageDashboardWindow[]
  series: UsageDashboardSeries
  top: UsageDashboardTop
}

export interface UsageDashboardWindowsResponse {
  generatedAt: string
  timezone: string
  windows: UsageDashboardWindow[]
}

export interface UsageDashboardSeriesResponse {
  generatedAt: string
  timezone: string
  series: UsageDashboardSeries
}

export interface UsageDashboardTopResponse {
  generatedAt: string
  top: UsageDashboardTop
}

export interface UsageDashboardBreakdownResponse {
  generatedAt: string
  timezone: string
  windowKey: string
  statusBreakdown: UsageBreakdownItem[]
  usageSourceBreakdown: UsageBreakdownItem[]
}

export interface UsageDashboardExternalPoolBillingResponse {
  generatedAt: string
  timezone: string
  windowKey: string
  externalPoolBillingByPool: UsageExternalPoolBillingByPool[]
}

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

export interface UsageDashboardWindow {
  key: string
  label: string
  from: string
  to: string
  summary: UsageDashboardSummary
}

export interface UsageDashboardSummary {
  totalRequests: number
  successRequests: number
  errorRequests: number
  errorRate: number
  streamRequests: number
  nonStreamRequests: number
  highCacheRequests: number
  totalInputTokens: number
  billableInputTokens: number
  totalOutputTokens: number
  totalCacheReadInputTokens: number
  totalCacheCreationInputTokens: number
  cacheReadRatio: number
  totalEstimatedCostUsd: number
  totalOriginalCostUsd: number
  totalKiroMeteringUsage: number
  pricedRequests: number
  unpricedRequests: number
  averageDurationMs: number
  p95DurationMs: number
  stickyBoundRequests: number
  fallbackFromStickyRequests: number
  simulatedRequests: number
  upstreamMetadataRequests: number
  externalPoolBilling?: UsageExternalPoolBillingSummary
  externalPoolBillingByPool?: UsageExternalPoolBillingByPool[]
  statusBreakdown: UsageBreakdownItem[]
  usageSourceBreakdown: UsageBreakdownItem[]
}

export interface UsageBreakdownItem {
  key: string
  label: string
  requests: number
  ratio: number
}

export interface UsageDashboardSeries {
  hourly24h: UsageSeriesPoint[]
  daily7d: UsageSeriesPoint[]
}

export interface UsageSeriesPoint {
  key: string
  label: string
  from: string
  to: string
  requests: number
  successRequests: number
  errorRequests: number
  totalInputTokens: number
  billableInputTokens: number
  totalOutputTokens: number
  totalEstimatedCostUsd: number
  totalOriginalCostUsd: number
  totalKiroMeteringUsage: number
}

export interface UsageDashboardTop {
  windowKey: string
  models: UsageTopAggregate[]
  credentials: UsageTopAggregate[]
  endpoints: UsageTopAggregate[]
  errors: UsageTopAggregate[]
  modelsTotal: number
  credentialsTotal: number
  endpointsTotal: number
  errorsTotal: number
  modelsTruncated: boolean
  credentialsTruncated: boolean
  endpointsTruncated: boolean
  errorsTruncated: boolean
  orderBy: string
  errorsOrderBy: string
}

export interface UsageTopAggregate {
  key: string
  label?: string
  requests: number
  errorRequests: number
  totalInputTokens: number
  billableInputTokens: number
  totalOutputTokens: number
  totalCacheReadInputTokens: number
  totalCacheCreationInputTokens: number
  totalEstimatedCostUsd: number
  totalOriginalCostUsd: number
  totalKiroMeteringUsage: number
}

export interface UsageRecordsQuery {
  limit?: number
  requestId?: string
  requestApiKeyId?: string
  q?: string
  endpoint?: string
  conversationId?: string
  credentialId?: number
  externalPoolId?: number
  routeKind?: UsageRouteKindFilter
  model?: string
  status?: UsageRecordStatus
  source?: UsageSource
  stream?: boolean
  minCacheRead?: number
  minFirstTokenLatencyMs?: number
  since?: string
  until?: string
}

export interface UsageRecordsPageQuery extends UsageRecordsQuery {
  page: number
  limit: number
}
