export interface SystemVersionResponse {
  version: string;
}

export interface SuccessResponse {
  success: boolean;
  message: string;
}

export interface SetDisabledRequest {
  disabled: boolean;
}

export interface SetPriorityRequest {
  priority: number;
}

export interface SetWarmupRequest {
  warmupRemaining: number;
}

export interface InferenceAttemptSnapshot {
  maxAttempts: number;
  consumed: number;
  localAttempts: number;
  externalAttempts: number;
  mcpAttempts: number;
  exhausted: boolean;
  downstreamCommitted: boolean;
}

export interface AuxiliaryAttemptSnapshot {
  maxAttempts: number;
  consumed: number;
  tokenRefreshAttempts: number;
  profileDiscoveryAttempts: number;
  exhausted: boolean;
}

export interface RawUpstreamError {
  source: string;
  statusCode?: number;
  contentType?: string;
  body: string;
  bodyBytes: number;
  truncated: boolean;
}

export type LoadBalancingMode =
  | "priority"
  | "balanced"
  | "health_balanced"
  | "weighted_least_inflight";
