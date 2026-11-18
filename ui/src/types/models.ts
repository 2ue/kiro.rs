export interface ModelPricing {
  inputCostPerToken: number;
  outputCostPerToken: number;
  /** 5 分钟缓存写入单价 */
  cacheCreationInputTokenCost: number;
  /** 1 小时缓存写入单价；来源没有时按 input 的 2 倍 */
  cacheCreation1hInputTokenCost: number;
  cacheReadInputTokenCost: number;
}

export interface ModelPriceItem {
  model: string;
  pricing: ModelPricing;
  source?: string;
}

export interface ModelPricingStatus {
  available: boolean;
  source: string;
  sourceUrl: string;
  modelCount: number;
  lastSyncedAt?: string;
  lastError?: string;
  models: ModelPriceItem[];
}

export interface ModelCapabilityItem {
  listed: boolean;
  model: string;
  displayName: string;
  description?: string;
  maxInputTokens?: number;
  maxOutputTokens?: number;
  supportsPromptCaching?: boolean;
  supportedInputTypes: string[];
  source?: string;
}

export interface ModelCapabilitiesStatus {
  available: boolean;
  source: string;
  modelCount: number;
  lastSyncedAt?: string;
  lastError?: string;
  models: ModelCapabilityItem[];
  reasoningFields?: Record<
    string,
    {
      path: "output_config" | "reasoning";
      efforts: string[];
      defaultEffort?: string;
    }
  >;
}

export interface ManualModelPricingRequest {
  inputCostPerMillion: number;
  outputCostPerMillion: number;
  cacheCreationInputCostPerMillion?: number;
  cacheCreation1hInputCostPerMillion?: number;
  cacheReadInputCostPerMillion?: number;
}

export interface UpsertManualModelRequest {
  listed?: boolean;
  model: string;
  displayName?: string;
  description?: string;
  maxInputTokens?: number;
  maxOutputTokens?: number;
  supportsPromptCaching?: boolean;
  supportedInputTypes: string[];
  pricing?: ManualModelPricingRequest;
  clearPricing?: boolean;
}

export interface ManualModelResponse {
  success: boolean;
  message: string;
  model: string;
}

export interface ModelCatalogCandidate {
  model: string;
  displayName: string;
  maxInputTokens?: number;
  maxOutputTokens?: number;
  supportsPromptCaching?: boolean;
  supportedInputTypes: string[];
  inCatalog: boolean;
  hasPricing: boolean;
}
export interface ModelCatalogDiscoverResponse {
  source: string;
  sourceUrl: string;
  fetchedAt: string;
  candidates: ModelCatalogCandidate[];
}
export interface ModelCatalogAdoptResponse {
  added: string[];
  skipped: string[];
  status: ModelCapabilitiesStatus;
}
