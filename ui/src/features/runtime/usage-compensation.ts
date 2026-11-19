// 运行配置“用量补偿”的开关语义（与 main 外部池页一致）：百分比为 0 即关闭，
// 打开时没有值就填默认值。只有纯逻辑，`node --test` 可以直接加载。
import type { AccountsConfig } from "@/types/api";

export const DEFAULT_CACHE_UPLIFT_PERCENT = 25;
export const DEFAULT_OUTPUT_UPLIFT_MIN_TOKENS = 1000;
export const DEFAULT_OUTPUT_UPLIFT_PERCENT = 25;

export type UsageCompensation = Pick<
  AccountsConfig,
  | "usageProjectionUpliftPercent"
  | "usageProjectionCostFloorEnabled"
  | "usageProjectionCostFloorMarginPercent"
  | "usageProjectionOutputUpliftMinTokens"
  | "usageProjectionOutputUpliftPercent"
>;

export function cacheUpliftEnabled(value: UsageCompensation): boolean {
  return value.usageProjectionUpliftPercent > 0;
}

/** 输出补偿要阈值和百分比都大于 0 才生效。 */
export function outputUpliftEnabled(value: UsageCompensation): boolean {
  return value.usageProjectionOutputUpliftMinTokens > 0 && value.usageProjectionOutputUpliftPercent > 0;
}

/** 打开缓存补偿：没有百分比时填 25%；关闭：百分比归 0。 */
export function withCacheUplift<T extends UsageCompensation>(value: T, enabled: boolean): T {
  return {
    ...value,
    usageProjectionUpliftPercent: enabled ? value.usageProjectionUpliftPercent || DEFAULT_CACHE_UPLIFT_PERCENT : 0,
  };
}

/** 打开输出补偿：没有值时填“1000 Token + 25%”；关闭：两项都归 0。 */
export function withOutputUplift<T extends UsageCompensation>(value: T, enabled: boolean): T {
  return {
    ...value,
    usageProjectionOutputUpliftMinTokens: enabled ? value.usageProjectionOutputUpliftMinTokens || DEFAULT_OUTPUT_UPLIFT_MIN_TOKENS : 0,
    usageProjectionOutputUpliftPercent: enabled ? value.usageProjectionOutputUpliftPercent || DEFAULT_OUTPUT_UPLIFT_PERCENT : 0,
  };
}
