import type {
  AccountRoutePolicy,
  CacheRoutePolicyPatch,
  InjectionItemOverride,
  ReportedUsagePathPolicy,
  RouteFeatureOverride,
} from "./api";
export type RouteRuleMode = "allow_all" | "allow_list" | "deny_list";

export interface RoutePolicy {
  routeMode: RouteRuleMode;
  routeRules: string[];
}

export interface RouteScenarioContent {
  routes: import("./routes").RouteConfig[];
  externalAccounts: AccountRoutePolicy[];
}

export interface RouteScenario {
  id: number;
  name: string;
  description?: string | null;
  content: RouteScenarioContent;
  revision: number;
  createdAt: string;
  updatedAt: string;
}

export interface RouteScenarioListResponse {
  scenarios: RouteScenario[];
  activeScenarioId?: number | null;
  activeInSync: boolean;
}

export interface RouteScenarioOptions {
  routes: string[];
  externalAccounts: RouteScenarioAccountOption[];
}

export interface SaveRouteScenarioRequest {
  name: string;
  description?: string;
  content: RouteScenarioContent;
}

export type RouteScenarioItemKind = "account" | "external_account";

export interface RouteScenarioItemKey {
  kind: RouteScenarioItemKind;
  id: number;
}

export interface RouteScenarioChange extends RouteScenarioItemKey {
  label: string;
  before: string;
  after: string;
}

export interface RouteScenarioPreview {
  changes: RouteScenarioChange[];
  unchanged: number;
  missingAccounts: RouteScenarioItemKey[];
  unknownRoutes: string[];
  uncoveredExternalRoutes: string[];
}

export interface RouteScenarioApplyItem extends RouteScenarioItemKey {
  label: string;
  success: boolean;
  error?: string;
}

export interface RouteScenarioApplyResponse {
  total: number;
  success: number;
  failed: number;
  skipped: number;
  items: RouteScenarioApplyItem[];
}

export interface RouteScenarioAccountOption extends RoutePolicy {
  id: number;
  label: string;
}
export interface RouteRule {
  mode: RouteRuleMode;
  rules: string[];
}
export interface RouteConfig {
  /** 稳定标识；新建时为 0，由后端分配。 */
  id: number;
  prefix: string;
  name: string;
  enabled: boolean;
  cachePolicy: CacheRoutePolicyPatch;
  reportedUsage?: ReportedUsagePathPolicy | null;
  features: RouteFeatureOverride;
  promptInjection: Partial<Record<string, InjectionItemOverride>>;
  accountRule: RouteRule;
  modelRule: RouteRule;
}
export interface RouteEffective {
  prefix: string;
  cachePolicy: Record<string, unknown>;
  features: Record<string, unknown>;
  injections: Record<string, unknown>;
  accountRule: RouteRule;
  modelRule: RouteRule;
  sources: Record<string, string>;
}
export interface AccountEffective {
  id: number;
  parameters: Record<string, unknown>;
  sources: Record<string, string>;
}
