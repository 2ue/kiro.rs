import {
  CONFIG_FIELD_GRID_CLASS,
  CONFIG_FIELD_GRID_STYLE,
  ConfigFieldCell,
  ConfigNumberControl,
  ConfigSwitchControl,
} from "@/components/patterns/config-fields";
import type { AccountsConfig } from "@/types/api";
import {
  DEFAULT_CACHE_UPLIFT_PERCENT,
  DEFAULT_OUTPUT_UPLIFT_MIN_TOKENS,
  DEFAULT_OUTPUT_UPLIFT_PERCENT,
  cacheUpliftEnabled,
  outputUpliftEnabled,
  withCacheUplift,
  withOutputUplift,
} from "./usage-compensation";

function Group({ title, desc, children }: { title: string; desc: string; children: React.ReactNode }) {
  return (
    <section className="space-y-4 border-t pt-5 first:border-t-0 first:pt-0">
      <header>
        <h4 className="text-sm font-semibold">{title}</h4>
        <p className="mt-0.5 text-xs leading-5 text-muted-foreground">{desc}</p>
      </header>
      <div className={CONFIG_FIELD_GRID_CLASS} style={CONFIG_FIELD_GRID_STYLE}>
        {children}
      </div>
    </section>
  );
}

/**
 * 账号池“用量补偿”：与 main 外部池页同一套交互——补偿用开关表达，打开时没有值就填默认值
 * （缓存 25%，输出 1000 Token + 25%），关闭即把百分比归 0；排版沿用参数格。
 */
export function UsageCompensationFields({
  value,
  onChange,
}: {
  value: AccountsConfig;
  onChange: (next: AccountsConfig) => void;
}) {
  const cacheOn = cacheUpliftEnabled(value);
  const outputOn = outputUpliftEnabled(value);
  const floorOn = value.usageProjectionCostFloorEnabled;
  const set = (patch: Partial<AccountsConfig>) => onChange({ ...value, ...patch });
  return (
    <div className="max-w-5xl space-y-6">
      <Group title="成本底线" desc="用最终返回给调用方的 usage 覆盖原始上游成本；有上游缓存证据时优先补缓存字段，并遵守当前路由上限。">
        <ConfigFieldCell id="uplift-cost-floor" label="启用成本底线" description="最终 usage 折算的成本不低于上游原始成本。">
          <ConfigSwitchControl id="uplift-cost-floor" checked={floorOn} onChange={(usageProjectionCostFloorEnabled) => set({ usageProjectionCostFloorEnabled })} />
        </ConfigFieldCell>
        <ConfigFieldCell id="uplift-cost-floor-margin" label="成本补齐余量" description="成本底线之上再补的余量百分比。">
          <ConfigNumberControl
            id="uplift-cost-floor-margin"
            value={value.usageProjectionCostFloorMarginPercent}
            unit="%"
            min={0}
            integer
            disabled={!floorOn}
            onChange={(usageProjectionCostFloorMarginPercent) => set({ usageProjectionCostFloorMarginPercent })}
          />
        </ConfigFieldCell>
      </Group>
      <Group title="缓存读写补偿" desc="有上游缓存证据时按百分比补缓存字段，并遵守当前路由上限。">
        <ConfigFieldCell id="uplift-cache" label="启用缓存补偿" description={`打开时没有百分比会自动填 ${DEFAULT_CACHE_UPLIFT_PERCENT}%；关闭即百分比归 0。`}>
          <ConfigSwitchControl id="uplift-cache" checked={cacheOn} onChange={(on) => onChange(withCacheUplift(value, on))} />
        </ConfigFieldCell>
        <ConfigFieldCell id="uplift-cache-percent" label="放大百分比" description="缓存读写在上游原始值之上放大的比例。">
          <ConfigNumberControl
            id="uplift-cache-percent"
            value={value.usageProjectionUpliftPercent}
            unit="%"
            min={0}
            integer
            disabled={!cacheOn}
            onChange={(usageProjectionUpliftPercent) => set({ usageProjectionUpliftPercent })}
          />
        </ConfigFieldCell>
      </Group>
      <Group title="输出用量补偿" desc="输出 token 超过阈值后按百分比放大；阈值和百分比都大于 0 才生效。">
        <ConfigFieldCell
          id="uplift-output"
          label="启用输出补偿"
          description={`打开时没有值会自动填 ${DEFAULT_OUTPUT_UPLIFT_MIN_TOKENS} Token + ${DEFAULT_OUTPUT_UPLIFT_PERCENT}%；关闭即两项归 0。`}
        >
          <ConfigSwitchControl id="uplift-output" checked={outputOn} onChange={(on) => onChange(withOutputUplift(value, on))} />
        </ConfigFieldCell>
        <ConfigFieldCell id="uplift-output-min" label="输出阈值" description="输出 token 超过这个值才放大。">
          <ConfigNumberControl
            id="uplift-output-min"
            value={value.usageProjectionOutputUpliftMinTokens}
            unit="Token"
            min={0}
            integer
            disabled={!outputOn}
            onChange={(usageProjectionOutputUpliftMinTokens) => set({ usageProjectionOutputUpliftMinTokens })}
          />
        </ConfigFieldCell>
        <ConfigFieldCell id="uplift-output-percent" label="放大百分比" description="输出 token 放大的比例。">
          <ConfigNumberControl
            id="uplift-output-percent"
            value={value.usageProjectionOutputUpliftPercent}
            unit="%"
            min={0}
            integer
            disabled={!outputOn}
            onChange={(usageProjectionOutputUpliftPercent) => set({ usageProjectionOutputUpliftPercent })}
          />
        </ConfigFieldCell>
      </Group>
    </div>
  );
}
