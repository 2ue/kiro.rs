import type { CacheRoutePolicyPatch, PromptCacheStrategyType } from "@/types/api";

/** 内置入口，不可删除；根入口 `/v1` 用空串表示。 */
export const BUILTIN_ROUTE_PREFIXES = ["", "/cc", "/ha", "/na"] as const;
/** 自定义路由的专有前缀：和内置入口、管理端 `/api`、控制台 `/ui` 隔离，不能修改。 */
export const CUSTOM_ROUTE_PREFIX = "/dfcache/";

export function normalizeRoutePrefix(prefix: string): string {
  const value = prefix.trim().replace(/^\/+|\/+$/g, "");
  return !value || value === "v1" ? "" : `/${value}`;
}
export function displayRoutePrefix(prefix: string): string {
  return normalizeRoutePrefix(prefix) || "/";
}
/** 注入预览接口的路由写法：默认路由是 `/v1`（`/` 会被拒绝），其余为归一化前缀。 */
export function previewRoutePrefix(prefix: string): string {
  return normalizeRoutePrefix(prefix) || "/v1";
}

/** 用量里无法从入口解析出路由前缀时的键。 */
export const UNKNOWN_USAGE_ROUTE = "unknown";
/**
 * 从入口路径推路由前缀，与后端 `usage_route_prefix_for_endpoint` 同一口径：入口固定是
 * `{前缀}/v1/...`，取最后一个 `/v1/` 之前的部分（自定义路由名本身含 `v1` 也不会截错）；
 * 前缀为空是默认路由 `/`；空或 `unknown` 返回 `unknown`；不含 `/v1` 的路径原样返回。
 */
export function routePrefixForEndpoint(endpoint: string | null | undefined): string {
  const value = (endpoint ?? "").trim();
  if (!value || value === UNKNOWN_USAGE_ROUTE) return UNKNOWN_USAGE_ROUTE;
  let index = value.lastIndexOf("/v1/");
  if (index < 0 && value.endsWith("/v1")) index = value.length - 3;
  if (index < 0) return value;
  const prefix = value.slice(0, index).replace(/\/+$/, "");
  return prefix || "/";
}
/** 路由编辑页的页签。 */
export const ROUTE_EDIT_TABS = ["cache", "injection", "features", "accounts", "models"] as const;
export type RouteEditTab = (typeof ROUTE_EDIT_TABS)[number];
/** main 的旧页签名：behavior 是“提示词与功能”，按注入页签打开。 */
const ROUTE_EDIT_TAB_ALIASES: Record<string, RouteEditTab> = { behavior: "injection" };

/** `?tab=` 的取值归一化；不认识的值回到缓存页签。 */
export function resolveRouteEditTab(value: string | null | undefined): RouteEditTab {
  const key = (value ?? "").trim().toLowerCase();
  if ((ROUTE_EDIT_TABS as readonly string[]).includes(key)) return key as RouteEditTab;
  return ROUTE_EDIT_TAB_ALIASES[key] ?? "cache";
}

/** 编辑页地址只放 id，前缀不进 URL；可带页签直达。 */
export function routeEditorPath(id: number, tab?: RouteEditTab): string {
  return tab && tab !== "cache" ? `/routes/${id}?tab=${tab}` : `/routes/${id}`;
}
export function isBuiltinRoute(prefix: string): boolean {
  return (BUILTIN_ROUTE_PREFIXES as readonly string[]).includes(normalizeRoutePrefix(prefix));
}

/** 自定义路由名归一化：只允许 1–64 个小写字母、数字、`.`、`_`、`-`；非法返回 null。 */
export function normalizeCustomRouteName(name: string): string | null {
  const value = name.trim().replace(/^\/+|\/+$/g, "").toLowerCase();
  if (!value || value.length > 64 || !/^[a-z0-9._-]+$/.test(value)) return null;
  return value;
}
/** 把用户输入（名字或完整前缀）归一化成 `/dfcache/{name}`；非法返回 null。 */
export function normalizeCustomRoutePrefix(input: string): string | null {
  const trimmed = input.trim().replace(/\/+$/, "");
  const lower = (trimmed.startsWith("/") ? trimmed : `/${trimmed}`).toLowerCase();
  // 输入可以是完整前缀 /dfcache/team，也可以只是名字 team；单独一个 "dfcache" 不是名字。
  const fixed = CUSTOM_ROUTE_PREFIX.replace(/\/$/, "");
  if (lower === fixed) return null;
  const name = lower.startsWith(CUSTOM_ROUTE_PREFIX) ? lower.slice(CUSTOM_ROUTE_PREFIX.length) : lower.replace(/^\//, "");
  const normalized = normalizeCustomRouteName(name);
  return normalized ? `${CUSTOM_ROUTE_PREFIX}${normalized}` : null;
}
/** 自定义前缀去掉固定部分后的名字；内置或非法返回 null。 */
export function customRouteName(prefix: string): string | null {
  const normalized = normalizeRoutePrefix(prefix);
  return normalized.startsWith(CUSTOM_ROUTE_PREFIX) ? normalized.slice(CUSTOM_ROUTE_PREFIX.length) : null;
}

export function sortRoutes<T extends { prefix: string }>(routes: T[]): T[] {
  const order: readonly string[] = BUILTIN_ROUTE_PREFIXES;
  return [...routes].sort((a, b) => {
    const ap = normalizeRoutePrefix(a.prefix),
      bp = normalizeRoutePrefix(b.prefix);
    const ai = order.indexOf(ap),
      bi = order.indexOf(bp);
    return ai >= 0 || bi >= 0
      ? (ai < 0 ? 99 : ai) - (bi < 0 ? 99 : bi)
      : ap.localeCompare(bp);
  });
}
export function ruleSummary(
  mode: "allow_all" | "allow_list" | "deny_list",
  rules: string[],
): string {
  if (mode === "allow_all") return "全部";
  if (mode === "allow_list")
    return rules.length ? `仅允许 ${rules.length} 项` : "不允许任何项";
  return rules.length ? `排除 ${rules.length} 项` : "全部";
}

export function accountAllowsRoute(
  account: {
    routeMode: "allow_all" | "allow_list" | "deny_list";
    routeRules: string[];
  },
  prefix: string,
): boolean {
  const rules = account.routeRules ?? [];
  const normalized = normalizeRoutePrefix(prefix);
  const hit = rules.some((rule) => normalizeRoutePrefix(rule) === normalized || rule === "*");
  if (account.routeMode === "allow_list") return hit;
  if (account.routeMode === "deny_list") return !hit;
  return true;
}

// ─── 接入地址（与 main 一致：根入口的 base 是服务根，其余入口 base 是前缀本身）────

export function routeBasePath(prefix: string): string {
  return normalizeRoutePrefix(prefix);
}
export function currentOrigin(): string {
  return typeof window === "undefined" ? "" : window.location.origin;
}
export function routeBaseUrl(prefix: string, origin = currentOrigin()): string {
  return `${origin.replace(/\/+$/, "")}${routeBasePath(prefix)}`;
}
export function routeClaudeCodeEnv(prefix: string, origin = currentOrigin()): string {
  return `ANTHROPIC_BASE_URL=${routeBaseUrl(prefix, origin)}\nANTHROPIC_API_KEY=<请求 Key>`;
}

// ─── 策略摘要（卡片徽章，文案沿用 main）──────────────────────────────────────────

function formatRatio(value?: number): string {
  return value == null ? "—" : `${Math.round(value * 100)}%`;
}
function formatTokens(value?: number): string {
  if (value == null || value === 0) return "不限制";
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 1000) return `${Math.round(value / 1000)}K`;
  return String(value);
}

/** 模板卡上的三条要点。 */
export function templateHighlights(type: PromptCacheStrategyType, policy: CacheRoutePolicyPatch): string[] {
  if (type === "no_cache") return ["请求不进入缓存计算"];
  if (type === "bounded_high_cache") {
    return [
      `命中读取 ${formatRatio(policy.simulation?.targetReadRatio)}`,
      `输入上限 ${formatTokens(policy.reportedUsage?.input?.maxTokens)}`,
      `计费下限 ${policy.hitShaping?.enabled ? `${policy.hitShaping.minCostRatio ?? 1}×` : "关闭"}`,
    ];
  }
  if (type === "current_high_cache") {
    return [
      `命中读取 ${formatRatio(policy.simulation?.targetReadRatio)}`,
      `输入放大 ${policy.simulation?.tokenScale ?? 1}×`,
      `写入间隔 ${policy.creationControl?.minCreationIntervalSecs ?? 0}s`,
    ];
  }
  if (type === "prefix_tool") {
    return [
      `覆盖率 ${formatRatio(policy.prefixTool?.coverageRatio)}`,
      `增量写入 ${policy.prefixTool?.incrementalCreateEnabled === false ? "关闭" : "开启"}`,
      `稳定前缀 ${policy.prefixTool?.cacheCurrentUserStablePrefix ? "开启" : "关闭"}`,
    ];
  }
  if (type === "stable_segment_cache") {
    return [
      `覆盖率 ${formatRatio(policy.stableSegment?.coverageRatio)}`,
      `最小片段 ${formatTokens(policy.stableSegment?.minSegmentTokens)}`,
      `工具 / 系统 / 历史 ${[
        policy.stableSegment?.includeTools,
        policy.stableSegment?.includeSystem,
        policy.stableSegment?.includeHistory,
      ].filter(Boolean).length}/3`,
    ];
  }
  const plus = policy.prefixToolPlus;
  return [
    `覆盖率 ${formatRatio(plus?.coverageRatio)}`,
    `输入 ${plus?.shaping?.input?.multiplier ?? 1}× · 输出 ${plus?.shaping?.output?.multiplier ?? 1}×`,
    `小请求 ${plus?.smallRequest?.enabled ? `${formatTokens(plus.smallRequest.maxInputTokens)} 内` : "关闭"}`,
  ];
}

/** 路由卡上的两条要点。 */
export function routePolicySummary(policy: CacheRoutePolicyPatch): string[] {
  const type = policy.cacheType ?? "current_high_cache";
  const ns = `路径空间 ${policy.routeNamespace ? "独立" : "共享"}`;
  if (type === "no_cache") return ["请求不进入缓存计算"];
  if (type === "bounded_high_cache") return [`输入上限 ${formatTokens(policy.reportedUsage?.input?.maxTokens)}`, ns];
  if (type === "current_high_cache") return [`命中读取 ${formatRatio(policy.simulation?.targetReadRatio)}`, ns];
  if (type === "prefix_tool") return [`覆盖率 ${formatRatio(policy.prefixTool?.coverageRatio)}`, `增量写入 ${policy.prefixTool?.incrementalCreateEnabled === false ? "关闭" : "开启"}`];
  if (type === "stable_segment_cache") return [`覆盖率 ${formatRatio(policy.stableSegment?.coverageRatio)}`, ns];
  return [`覆盖率 ${formatRatio(policy.prefixToolPlus?.coverageRatio)}`, `输入 ${policy.prefixToolPlus?.shaping?.input?.multiplier ?? 1}×`];
}
