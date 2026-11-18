import type {
  InferenceAttemptSnapshot,
  AuxiliaryAttemptSnapshot,
  RawUpstreamError,
  AccountBilling,
  AccountAttempt,
} from "./api";
export type UsageCleanupMode = "soft_delete" | "hard_delete";

export type UsageCleanupJobStatus =
  | "idle"
  | "queued"
  | "running"
  | "paused"
  | "completed"
  | "cancelled"
  | "failed";

export type UsageCleanupPhase =
  | "idle"
  | "postgres"
  | "redis_admin_cache"
  | "redis_snapshots"
  | "complete";

export interface UsageCleanupRequest {
  mode?: UsageCleanupMode;
  includeSummary?: boolean;
  olderThanDays?: number;
  cutoffBefore?: string;
  batchSize?: number;
  maxBatches?: number;
  maxRows?: number;
  pauseMsBetweenBatches?: number;
}

export interface UsageCleanupPreviewResponse {
  mode: UsageCleanupMode;
  includeSummary: boolean;
  cutoffAt: string;
  matchedRows: number;
  oldestCreatedAt?: string;
  newestCreatedAt?: string;
}

export interface UsageCleanupStatusResponse {
  jobId?: string;
  status: UsageCleanupJobStatus;
  phase: UsageCleanupPhase;
  mode?: UsageCleanupMode;
  includeSummary: boolean;
  cutoffAt?: string;
  batchSize: number;
  maxBatches: number;
  maxRows?: number;
  pauseMsBetweenBatches: number;
  matchedRows?: number;
  remainingRows?: number;
  processedRows: number;
  lastBatchRows: number;
  batches: number;
  redisDeletedKeys: number;
  redisDeleteCommands: number;
  redisMaxCommandKeys: number;
  redisScanPasses: number;
  redisUsedDelFallback: boolean;
  redisPassLimitReached: boolean;
  cancelRequested: boolean;
  stopReason?: string;
  startedAt?: string;
  updatedAt?: string;
  finishedAt?: string;
  lastError?: string;
}

export type UsageRecordStatus =
  | "success"
  | "error"
  | "stream_error"
  | "upstream_timeout"
  | "client_dropped";

export type UsageSource =
  | "upstream_metadata"
  | "local_prompt_cache"
  | "context_estimate"
  | "request_estimate"
  | "none";

export type UsageRouteKindFilter = "account";

export interface UsageLatencyTrace {
  inferenceAttempts?: InferenceAttemptSnapshot;
  auxiliaryAttempts?: AuxiliaryAttemptSnapshot;
  capacityWeightUnits?: number;
  estimatedInputTokens?: number;
  payloadGuardMs?: number;
  upstreamHeaderMs?: number;
  downstreamFirstByteMs?: number;
  firstUpstreamChunkMs?: number;
  firstOutputDeltaMs?: number;
  firstThinkingDeltaMs?: number;
  firstVisibleTextDeltaMs?: number;
  streamGapToFirstOutputMs?: number;
  chunksBeforeFirstOutput?: number;
  eventsBeforeFirstOutput?: number;
  upstreamBytesBeforeFirstOutput?: number;
  upstreamFramesBeforeFirstOutput?: number;
  upstreamEventsBeforeFirstOutput?: number;
  upstreamFramesWithoutDownstreamEventsBeforeFirstOutput?: number;
  upstreamPendingChunksBeforeFirstOutput?: number;
  upstreamFrameDecodeErrorsBeforeFirstOutput?: number;
  upstreamEventParseErrorsBeforeFirstOutput?: number;
  upstreamEventTypesBeforeFirstOutput?: Record<string, number>;
  streamRetryAttempts?: number;
  streamRetryDispatchFailures?: number;
  streamRetryReasons?: string[];
  clientDroppedMs?: number;
  terminalReason?:
    | "completed"
    | "upstream_status_error"
    | "upstream_json_exception"
    | "upstream_idle_timeout"
    | "first_output_timeout"
    | "malformed_sse"
    | "client_dropped"
    | "internal_error";
  upstreamMessageStatus?: string;
  sawUpstreamCompleted?: boolean;
  stopReasonSource?: string;
  suspectedIntentPreambleEndTurn?: boolean;
  intentPreambleRisk?: "none" | "low" | "medium" | "high" | string;
  suspectedToolContextLeakEndTurn?: boolean;
  toolContextLeakMarkers?: string[];
  assistantTailIntentHint?: boolean;
  endTurnAnomalyReason?: string;
  endTurnAnomalyRisk?: "none" | "low" | "medium" | "high" | string;
  upstreamEofWithoutCompleted?: boolean;
  lastUpstreamEventType?: string;
  lastUpstreamEvents?: string[];
  sawUpstreamAssistantResponse?: boolean;
  sawUpstreamToolUse?: boolean;
  sawUpstreamMetadata?: boolean;
  lastAssistantContentChars?: number;
  filteredTrivialTextBlocks?: number;
  filteredTrivialTextChars?: number;
  websearchMode?: "template" | "model_driven" | string;
  webSearchRequests?: number;
}

export interface UsageRecord {
  accountCostUsd: number;
  id: string;
  createdAt: string;
  endpoint: string;
  stream: boolean;
  model: string;
  requestedMaxTokens?: number;
  upstreamModel?: string;
  externalOutboundModel?: string;
  modelResolutionSource?: string;
  modelResolutionNote?: string;
  conversationId?: string;
  requestApiKeyId?: string;
  accountId?: number;
  accountName?: string;
  status: UsageRecordStatus;
  usageSource: UsageSource;
  totalInputTokens: number;
  compatInputTokens: number;
  billableInputTokens: number;
  outputTokens: number;
  cacheReadInputTokens: number;
  cacheCreationInputTokens: number;
  cacheCreation5mInputTokens: number;
  cacheCreation1hInputTokens: number;
  estimatedCostUsd: number;
  originalCostUsd: number;
  pricingAvailable: boolean;
  pricingModel?: string;
  durationMs: number;
  firstTokenLatencyMs?: number;
  responseLatencyMs?: number;
  latencyTrace?: UsageLatencyTrace;
  simulated: boolean;
  routeKind?: "account";
  routeSubtype?: "account";
  fallbackReason?: string;
  directPolicyReason?: string;
  localAttempted?: boolean;
  localPreflight?: unknown;
  externalAttempts?: AccountAttempt[];
  usageProjectionApplied?: boolean;
  accountBilling?: AccountBilling;
  errorType?: string;
  errorMessage?: string;
  errorDetail?: string;
  errorStatusCode?: number;
  errorSource?: string;
  errorId?: string;
  errorMetadata?: unknown;
  rawUpstreamError?: RawUpstreamError;
  publicErrorStatusCode?: number;
  publicErrorType?: string;
  publicErrorMessage?: string;
  payloadBreakdown?: unknown;
  payloadGuardReport?: unknown;
}

export interface UsageRecorderStats {
  inMemoryLimit: number;
  inMemoryRecords: number;
  redisEnabled: boolean;
  redisQueueEnabled: boolean;
  redisQueueCapacity: number;
  redisQueueAvailable: number;
  droppedRedisRecords: number;
  postgresEnabled: boolean;
  writerQueueEnabled: boolean;
  writerQueueCapacity: number;
  writerQueueAvailable: number;
  droppedPersistRecords: number;
}

export interface UsageRecordsResult {
  total: number;
  records: UsageRecord[];
}

export interface UsageRecordsPageResult {
  page: number;
  limit: number;
  hasNext: boolean;
  records: UsageRecord[];
}

export interface UsageAggregate {
  key: string;
  label?: string;
  requests: number;
  cacheReadInputTokens: number;
  cacheCreationInputTokens: number;
  estimatedCostUsd: number;
  originalCostUsd: number;
}

export interface UsageRealtimeStats {
  windowSeconds: number;
  requests: number;
  successRequests?: number;
  errorRequests?: number;
  rpm: number;
  successRpm?: number;
  errorRpm?: number;
  inputTpm: number;
  outputTpm: number;
  totalTpm: number;
  billableTpm: number;
}

export interface UsageSummary {
  totalRequests: number;
  successRequests: number;
  errorRequests: number;
  highCacheRequests: number;
  totalInputTokens: number;
  totalOutputTokens: number;
  totalCacheReadInputTokens: number;
  totalCacheCreationInputTokens: number;
  totalEstimatedCostUsd: number;
  totalOriginalCostUsd: number;
  pricedRequests: number;
  unpricedRequests: number;
  localPromptCacheRequests: number;
  localPromptCacheInputTokens: number;
  localPromptCacheReadInputTokens: number;
  localPromptCacheCreationInputTokens: number;
  simulatedRequests: number;
  upstreamMetadataRequests: number;
  accountBilling?: UsageAccountBillingSummary;
  realtime: UsageRealtimeStats;
  topAccounts: UsageAggregate[];
  topConversations: UsageAggregate[];
}

export interface UsageAccountBillingSummary {
  requests: number;
  pricedRequests: number;
  unpricedRequests: number;
  costFloorAppliedRequests: number;
  rawCostUsd: number;
  shapedCostUsd?: number;
  upliftedCostUsd?: number;
  profitUsd?: number;
  reportedCostUsd: number;
  billableCostUsd: number;
  costFloorDeltaUsd: number;
}

export interface UsageAccountBillingByPool extends UsageAccountBillingSummary {
  poolId: number;
  poolName: string;
}

export interface UsageDashboardResponse {
  generatedAt: string;
  timezone: string;
  windows: UsageDashboardWindow[];
  series: UsageDashboardSeries;
  top: UsageDashboardTop;
}

export interface UsageOverviewMetrics {
  requests: number;
  successRequests: number;
  errorRequests: number;
  errorRate: number;
  inputTokens: number;
  /** 未缓存输入；缓存命中率 = 缓存读 / (未缓存输入 + 缓存读 + 缓存写)，与 sub2api 口径一致 */
  uncachedInputTokens: number;
  outputTokens: number;
  cacheReadInputTokens: number;
  cacheCreationInputTokens: number;
  estimatedCostUsd: number;
  originalCostUsd: number;
  pricedRequests: number;
  unpricedRequests: number;
  averageDurationMs: number;
}

export interface UsageOverviewSeriesMetrics {
  requests: number;
  errorRequests: number;
  inputTokens: number;
  uncachedInputTokens: number;
  outputTokens: number;
  cacheReadInputTokens: number;
  cacheCreationInputTokens: number;
  estimatedCostUsd: number;
  originalCostUsd: number;
  /** 账号上游原始成本（只有账号记录有值） */
  rawCostUsd: number;
  /** 账号最终可计费金额 */
  billableCostUsd: number;
  /** 账号补偿后成本 - 上游原始成本 */
  profitUsd: number;
}

export interface UsageOverviewSeriesBreakdown {
  key: string;
  metrics: UsageOverviewSeriesMetrics;
}

export interface UsageOverviewSeriesPoint {
  bucketStart: string;
  label: string;
  /** 全部请求 */
  all: UsageOverviewSeriesMetrics;
  /** 走账号池的请求（当前与 all 相同，字段名沿用后端） */
  external: UsageOverviewSeriesMetrics;
  /** 按账号拆分，成员与 `UsageOverviewSeriesGroups.accounts` 对应 */
  accounts: UsageOverviewSeriesBreakdown[];
}

export interface UsageOverviewSeriesGroup {
  key: string;
  label: string;
}

export interface UsageOverviewSeriesGroups {
  /** 按上游原始成本降序 */
  accounts: UsageOverviewSeriesGroup[];
}

export interface UsageOverviewPoolRow {
  poolId: number;
  poolName: string;
  metrics: UsageOverviewMetrics;
  rawCostUsd: number;
  shapedCostUsd: number;
  upliftedCostUsd: number;
  reportedCostUsd: number;
  billableCostUsd: number;
  profitUsd: number;
  costFloorDeltaUsd: number;
  costFloorAppliedRequests: number;
}

export interface UsageOverviewRankRow {
  key: string;
  label: string;
  requests: number;
  errorRequests: number;
  estimatedCostUsd: number;
}

export interface UsageOverviewResponse {
  generatedAt: string;
  timezone: string;
  range: {
    key?: string | null;
    from: string;
    to: string;
    requestedFrom: string;
    requestedTo: string;
    dataThrough: string;
    granularity: string;
  };
  totals: UsageOverviewMetrics;
  billing: {
    rawCostUsd: number;
    shapedCostUsd: number;
    upliftedCostUsd: number;
    reportedCostUsd: number;
    billableCostUsd: number;
    profitUsd: number;
    costFloorDeltaUsd: number;
    pools: UsageOverviewPoolRow[];
  };
  accountCostUsd: number;
  rawTokens: number;
  series: Array<{
    bucketStart: string;
    label: string;
    metrics: UsageOverviewSeriesMetrics;
  }>;
  topModels: UsageOverviewRankRow[];
  topAccounts: UsageOverviewRankRow[];
  topKeys: UsageOverviewRankRow[];
  topPaths: UsageOverviewRankRow[];
  topErrors: UsageOverviewRankRow[];
}

export interface UsageOverviewSummaryResponse {
  generatedAt: string;
  timezone: string;
  range: UsageOverviewResponse["range"];
  totals: { all: UsageOverviewMetrics; external: UsageOverviewMetrics };
  external: {
    rawCostUsd: number;
    shapedCostUsd: number;
    upliftedCostUsd: number;
    reportedCostUsd: number;
    billableCostUsd: number;
    profitUsd: number;
    costFloorDeltaUsd: number;
    costFloorAppliedRequests: number;
  };
}

export interface UsageOverviewSeriesResponse {
  generatedAt: string;
  timezone: string;
  range: UsageOverviewResponse["range"];
  series: UsageOverviewSeriesPoint[];
  seriesGroups: UsageOverviewSeriesGroups;
}

export interface UsageOverviewRankingsResponse {
  generatedAt: string;
  timezone: string;
  range: UsageOverviewResponse["range"];
  totalRequests: number;
  totalErrors: number;
  topModels: UsageOverviewRankRow[];
  topAccounts: UsageOverviewRankRow[];
  topKeys: UsageOverviewRankRow[];
  topPaths: UsageOverviewRankRow[];
  topErrors: UsageOverviewRankRow[];
}

export interface UsageDashboardWindowsResponse {
  generatedAt: string;
  timezone: string;
  windows: UsageDashboardWindow[];
}

export interface UsageDashboardSeriesResponse {
  generatedAt: string;
  timezone: string;
  series: UsageDashboardSeries;
}

export interface UsageDashboardTopResponse {
  generatedAt: string;
  top: UsageDashboardTop;
}

export interface UsageDashboardBreakdownResponse {
  generatedAt: string;
  timezone: string;
  windowKey: string;
  statusBreakdown: UsageBreakdownItem[];
  usageSourceBreakdown: UsageBreakdownItem[];
}

export interface UsageDashboardAccountBillingResponse {
  generatedAt: string;
  timezone: string;
  windowKey: string;
  accountBillingByPool: UsageAccountBillingByPool[];
}

export interface UsageAccountRiskQuery {
  timezone?: string;
  windowKey?: string;
  since?: string;
  until?: string;
  warningThresholdTokens?: number;
  criticalThresholdTokens?: number;
  accountId?: number;
  endpoint?: string;
  model?: string;
  stream?: boolean;
  limit?: number;
}

export interface UsageAccountRiskWindow {
  key: string;
  label: string;
  from: string;
  to: string;
}

export interface UsageAccountRiskThresholds {
  warningTokens: number;
  criticalTokens: number;
  costFloorEnabled: boolean;
  costFloorMarginPercent: number;
  costTargetMultiplier: number;
}

export interface UsageAccountRiskFilters {
  poolId?: number;
  endpoint?: string;
  model?: string;
  stream?: boolean;
}

export interface UsageAccountRiskTotals {
  records: number;
  successRecords: number;
  errorRecords: number;
  streamRecords: number;
  nonStreamRecords: number;
  pricedRecords: number;
  unpricedRecords: number;
  rawUsageRecords: number;
  reportedUsageRecords: number;
  missingAccountBillingRecords: number;
  outputZeroRecords: number;
}

export interface UsageAccountRiskCacheStats {
  minReadTokens: number;
  maxReadTokens: number;
  avgReadTokens: number;
  totalReadTokens: number;
  minWriteTokens: number;
  maxWriteTokens: number;
  avgWriteTokens: number;
  totalWriteTokens: number;
  readWarningCount: number;
  writeWarningCount: number;
  eitherWarningCount: number;
  readCriticalCount: number;
  writeCriticalCount: number;
  eitherCriticalCount: number;
}

export interface UsageAccountRiskCostStats {
  rawCostUsd: number;
  reportedCostUsd: number;
  targetCostUsd: number;
  profitUsd: number;
  totalLossUsd: number;
  totalTargetGapUsd: number;
  maxLossUsd: number;
  maxTargetGapUsd: number;
  maxRawCostUsd: number;
  maxReportedCostUsd: number;
  belowRawCount: number;
  belowTargetCount: number;
  costFloorAppliedRecords: number;
  minCostRatio?: number | null;
  avgCostRatio?: number | null;
  maxCostRatio?: number | null;
}

export interface UsageAccountRiskBucket {
  key: string;
  label: string;
  minTokens?: number | null;
  maxTokens?: number | null;
  rawReadCount: number;
  rawWriteCount: number;
  reportedReadCount: number;
  reportedWriteCount: number;
}

export interface UsageAccountRiskGroup {
  key: string;
  label: string;
  records: number;
  successRecords: number;
  warningRecords: number;
  criticalRecords: number;
  outputZeroRecords: number;
  rawReadMax: number;
  rawWriteMax: number;
  reportedReadMax: number;
  reportedWriteMax: number;
  rawCostUsd: number;
  reportedCostUsd: number;
  targetCostUsd: number;
  profitUsd: number;
  totalLossUsd: number;
  totalTargetGapUsd: number;
  belowRawCount: number;
  belowTargetCount: number;
}

export interface UsageAccountRiskSample {
  id: string;
  createdAt: string;
  endpoint: string;
  stream: boolean;
  model: string;
  status: string;
  accountId?: number;
  accountName?: string;
  pricingModel?: string;
  usageProjectionMode?: string;
  accountBillingPresent: boolean;
  costFloorApplied: boolean;
  rawInputTokens: number;
  rawOutputTokens: number;
  rawCacheReadInputTokens: number;
  rawCacheCreationInputTokens: number;
  reportedInputTokens: number;
  reportedOutputTokens: number;
  reportedCacheReadInputTokens: number;
  reportedCacheCreationInputTokens: number;
  rawCostUsd: number;
  reportedCostUsd: number;
  targetCostUsd: number;
  lossUsd: number;
  targetGapUsd: number;
  costRatio?: number | null;
  riskReasons: string[];
}

export interface UsageAccountRiskResponse {
  generatedAt: string;
  timezone: string;
  window: UsageAccountRiskWindow;
  thresholds: UsageAccountRiskThresholds;
  filters: UsageAccountRiskFilters;
  totals: UsageAccountRiskTotals;
  rawCache: UsageAccountRiskCacheStats;
  reportedCache: UsageAccountRiskCacheStats;
  cost: UsageAccountRiskCostStats;
  buckets: UsageAccountRiskBucket[];
  byPool: UsageAccountRiskGroup[];
  byPath: UsageAccountRiskGroup[];
  byModel: UsageAccountRiskGroup[];
  samples: UsageAccountRiskSample[];
}

export interface UsageDashboardWindow {
  key: string;
  label: string;
  from: string;
  to: string;
  summary: UsageDashboardSummary;
}

export interface UsageDashboardSummary {
  totalRequests: number;
  successRequests: number;
  errorRequests: number;
  errorRate: number;
  streamRequests: number;
  nonStreamRequests: number;
  highCacheRequests: number;
  totalInputTokens: number;
  billableInputTokens: number;
  totalOutputTokens: number;
  totalCacheReadInputTokens: number;
  totalCacheCreationInputTokens: number;
  cacheReadRatio: number;
  totalEstimatedCostUsd: number;
  totalOriginalCostUsd: number;
  pricedRequests: number;
  unpricedRequests: number;
  averageDurationMs: number;
  p95DurationMs: number;
  simulatedRequests: number;
  upstreamMetadataRequests: number;
  accountBilling?: UsageAccountBillingSummary;
  accountBillingByPool?: UsageAccountBillingByPool[];
  statusBreakdown: UsageBreakdownItem[];
  usageSourceBreakdown: UsageBreakdownItem[];
}

export interface UsageBreakdownItem {
  key: string;
  label: string;
  requests: number;
  ratio: number;
}

export interface UsageDashboardSeries {
  hourly24h: UsageSeriesPoint[];
  daily7d: UsageSeriesPoint[];
}

export interface UsageSeriesPoint {
  key: string;
  label: string;
  from: string;
  to: string;
  requests: number;
  successRequests: number;
  errorRequests: number;
  totalInputTokens: number;
  billableInputTokens: number;
  totalOutputTokens: number;
  totalEstimatedCostUsd: number;
  totalOriginalCostUsd: number;
}

export interface UsageDashboardTop {
  windowKey: string;
  models: UsageTopAggregate[];
  accounts: UsageTopAggregate[];
  endpoints: UsageTopAggregate[];
  errors: UsageTopAggregate[];
  modelsTotal: number;
  accountsTotal: number;
  endpointsTotal: number;
  errorsTotal: number;
  modelsTruncated: boolean;
  accountsTruncated: boolean;
  endpointsTruncated: boolean;
  errorsTruncated: boolean;
  orderBy: string;
  errorsOrderBy: string;
}

export interface UsageTopAggregate {
  key: string;
  label?: string;
  requests: number;
  errorRequests: number;
  totalInputTokens: number;
  billableInputTokens: number;
  totalOutputTokens: number;
  totalCacheReadInputTokens: number;
  totalCacheCreationInputTokens: number;
  totalEstimatedCostUsd: number;
  totalOriginalCostUsd: number;
}

export interface UsageRecordsQuery {
  limit?: number;
  requestId?: string;
  requestApiKeyId?: string;
  q?: string;
  endpoint?: string;
  conversationId?: string;
  accountId?: number;
  routeKind?: UsageRouteKindFilter;
  model?: string;
  status?: UsageRecordStatus;
  source?: UsageSource;
  stream?: boolean;
  minCacheRead?: number;
  minFirstTokenLatencyMs?: number;
  since?: string;
  until?: string;
}

export interface UsageRecordsPageQuery extends UsageRecordsQuery {
  page: number;
  limit: number;
}

export interface AdminAuditLogRow {
  id: number;
  createdAt: string;
  actor: string;
  action: string;
  objectType: string;
  objectId?: string;
  success: boolean;
  errorMessage?: string;
  detail: unknown;
}

export interface AdminAuditLogPage {
  page: number;
  limit: number;
  hasNext: boolean;
  records: AdminAuditLogRow[];
}

export interface AdminAuditLogPageQuery {
  page: number;
  limit: number;
  /** Only account change events. */
  accountEvents?: boolean;
  accountEvent?: string;
  accountSource?: string;
  objectType?: string;
  objectId?: string;
  /** Account label substring or id. */
  account?: string;
  action?: string;
  actor?: string;
  success?: boolean;
  /** RFC3339, inclusive. */
  since?: string;
  /** RFC3339, exclusive. */
  until?: string;
}
