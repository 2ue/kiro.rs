import type { RequestAdmissionConfig } from "./api";
export interface AccessKeysResponse {
  requestApiKey: string;
  maskedRequestApiKey: string;
  requestApiKeys: RequestApiKeyItem[];
  defaultRequestAdmission: RequestAdmissionConfig;
  adminApiKey: string;
  maskedAdminApiKey: string;
}

export type KeyLimitMetric = "cost_usd" | "credits" | "tokens";

export type KeyLimitPeriod = "daily" | "monthly" | "total";

export interface KeyUsageLimit {
  metric: KeyLimitMetric;
  period: KeyLimitPeriod;
  limit: number;
}

export interface RequestApiKeyItem {
  allowedRoutes: string[];
  id: string;
  apiKey: string;
  maskedApiKey: string;
  primary: boolean;
  name: string;
  enabled: boolean;
  requestAdmission?: RequestAdmissionConfig;
  usageLimits: KeyUsageLimit[];
}

export interface KeyLimitStatus {
  metric: KeyLimitMetric;
  period: KeyLimitPeriod;
  limit: number;
  used: number;
  exceeded: boolean;
  /** RFC3339; absent for `total`. */
  resetsAt?: string | null;
}

export interface RequestKeyUsageLimitItem {
  keyId: string;
  enabled: boolean;
  statuses: KeyLimitStatus[];
  /** Counters could not be read; requests with this key are rejected. */
  error?: string;
}

export interface RequestKeyUsageLimitsResponse {
  timezone: string;
  items: RequestKeyUsageLimitItem[];
}

export interface CreateRequestApiKeyRequest {
  allowedRoutes?: string[];
  apiKey?: string;
  name?: string;
  enabled?: boolean;
  requestAdmission?: RequestAdmissionConfig;
  usageLimits?: KeyUsageLimit[];
}

export interface UpdateRequestApiKeyRequest {
  allowedRoutes?: string[];
  apiKey?: string;
  name?: string;
  enabled?: boolean;
  requestAdmission?: RequestAdmissionConfig;
  usageLimits?: KeyUsageLimit[];
}

export interface UpdateAdminApiKeyRequest {
  adminApiKey: string;
}
