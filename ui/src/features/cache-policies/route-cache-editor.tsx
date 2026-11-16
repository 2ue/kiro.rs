import { pathPolicy } from "@/lib/runtime-config-defaults";
import {
  Button,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Switch,
} from "@/components/ui";
import {
  PathPolicyEditor,
  StrategyTemplateEditor,
} from "@/features/runtime/runtime-sections";
import type {
  CachePolicyConfig,
  CacheRoutePolicyPatch,
  PromptCacheStrategyType,
} from "@/types/api";
export const CACHE_STRATEGIES: Array<[PromptCacheStrategyType, string]> = [
  ["no_cache", "无缓存"],
  ["current_high_cache", "模拟高缓存"],
  ["prefix_tool", "前缀追踪"],
  ["stable_segment_cache", "稳定片段"],
  ["prefix_tool_plus", "前缀追踪 Plus"],
  ["bounded_high_cache", "上下限高缓存"],
];
export function RouteCacheEditor({
  value,
  templates,
  onChange,
}: {
  value: CacheRoutePolicyPatch;
  templates?: CachePolicyConfig;
  onChange: (next: CacheRoutePolicyPatch) => void;
}) {
  const type = value.cacheType ?? "current_high_cache";
  const templateKey = {
    no_cache: "default",
    current_high_cache: "currentHighCache",
    prefix_tool: "prefixTool",
    stable_segment_cache: "stableSegmentCache",
    prefix_tool_plus: "prefixToolPlus",
    bounded_high_cache: "boundedHighCache",
  } as const;
  const inherited = templates?.[templateKey[type]] ?? {};
  const policy = { ...inherited, ...value };
  for (const key of Object.keys(inherited) as Array<
    keyof CacheRoutePolicyPatch
  >) {
    if (
      inherited[key] &&
      typeof inherited[key] === "object" &&
      value[key] &&
      typeof value[key] === "object"
    )
      Object.assign(policy, {
        [key]: { ...(inherited[key] as object), ...(value[key] as object) },
      });
  }
  return (
    <div className="space-y-6">
      <Select
        value={type}
        onValueChange={(v) =>
          onChange({ ...value, cacheType: v as PromptCacheStrategyType })
        }
      >
        <SelectTrigger className="max-w-sm">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {CACHE_STRATEGIES.map(([key, label]) => (
            <SelectItem key={key} value={key}>
              {label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      {templates && (
        <Button variant="outline" onClick={() => onChange({ cacheType: type })}>
          恢复继承模板
        </Button>
      )}
      <label className="flex items-center gap-3 text-sm">
        <Switch
          checked={value.routeNamespace ?? false}
          onCheckedChange={(v) => onChange({ ...value, routeNamespace: v })}
        />
        按路由隔离缓存
      </label>
      {type === "no_cache" ? (
        <p className="text-sm text-muted-foreground">此路由不模拟缓存。</p>
      ) : (
        <StrategyTemplateEditor
          title="缓存与 usage"
          desc="未覆盖的参数继承全局策略模板。"
          cacheType={type}
          policy={policy}
          onChange={onChange}
        />
      )}
      {!["current_high_cache", "bounded_high_cache"].includes(type) && (
        <PathPolicyEditor
          policy={policy.reportedUsage ?? pathPolicy(false)}
          onChange={(reportedUsage) => onChange({ ...value, reportedUsage })}
        />
      )}
    </div>
  );
}
