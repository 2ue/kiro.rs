import type { ExternalPoolBilling } from './usage'

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

export type UsageRecordStatus = 'success' | 'error' | 'stream_error' | 'upstream_timeout' | 'client_dropped'

export type UsageSource = 'upstream_metadata' | 'local_prompt_cache' | 'context_estimate' | 'request_estimate' | 'none'

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
  terminalReason?:
    | 'completed'
    | 'upstream_status_error'
    | 'upstream_json_exception'
    | 'upstream_idle_timeout'
    | 'first_output_timeout'
    | 'malformed_sse'
    | 'client_dropped'
    | 'internal_error'
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
  routeSubtype?:
    | 'local_success'
    | 'local_error_no_fallback'
    | 'local_rescue_after_external'
    | 'external_fallback_preflight'
    | 'external_fallback_after_local_attempts'
    | 'external_direct_policy'
    | 'external_route_policy'
    | 'external_error'
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
