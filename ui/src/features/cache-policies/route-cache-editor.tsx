import { pathPolicy } from "@/lib/runtime-config-defaults";
import { Button } from "@/components/ui";
import {
  EditorSection,
  ParameterGroup,
  PathPolicyEditor,
  StrategyChoice,
  StrategyTemplateEditor,
  TogField,
} from "@/features/runtime/runtime-sections";
import type {
  CachePolicyConfig,
  CacheRoutePolicyPatch,
  PromptCacheStrategyType,
} from "@/types/api";
import { RotateCcw } from "lucide-react";

export const CACHE_STRATEGIES: Array<[PromptCacheStrategyType, string]> = [
  ["no_cache", "无缓存"],
  ["current_high_cache", "模拟高缓存"],
  ["prefix_tool", "前缀追踪"],
  ["stable_segment_cache", "稳定片段"],
  ["prefix_tool_plus", "前缀追踪 Plus"],
  ["bounded_high_cache", "上下限高缓存"],
];

const TEMPLATE_KEY = {
  no_cache: "default",
  current_high_cache: "currentHighCache",
  prefix_tool: "prefixTool",
  stable_segment_cache: "stableSegmentCache",
  prefix_tool_plus: "prefixToolPlus",
  bounded_high_cache: "boundedHighCache",
} as const;

/**
 * 路由的缓存策略编辑：与 main 的“编辑路径策略”同一套布局——
 * 先选策略（唯一决策区），再按策略只展示它真正用到的参数；未覆盖的参数继承全局模板。
 */
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
  const inherited = templates?.[TEMPLATE_KEY[type]] ?? {};
  const policy: CacheRoutePolicyPatch = { ...inherited, ...value };
  for (const key of Object.keys(inherited) as Array<keyof CacheRoutePolicyPatch>) {
    if (inherited[key] && typeof inherited[key] === "object" && value[key] && typeof value[key] === "object") {
      Object.assign(policy, { [key]: { ...(inherited[key] as object), ...(value[key] as object) } });
    }
  }
  const overridden = Object.keys(value).filter((key) => key !== "cacheType").length;
  const usesNamespace = type !== "no_cache";

  return (
    <div className="space-y-9">
      <EditorSection
        id="route-strategy"
        title="缓存策略"
        desc="这里是唯一的决策区。切换策略会载入对应模板的默认参数，下面只展示当前策略真正需要的设置。"
      >
        <StrategyChoice value={type} onChange={(next) => onChange({ cacheType: next })} />
      </EditorSection>

      <EditorSection
        id="route-parameters"
        className="border-t border-border/70 pt-8"
        title="策略参数"
        desc={templates ? "未覆盖的参数继承全局策略模板；改动过的参数只对这条路由生效。" : "按当前策略显示它真正需要的参数。"}
      >
        {templates && (
          <div className="mb-6 flex flex-wrap items-center justify-between gap-3 rounded-lg border bg-muted/30 px-3.5 py-2.5 text-xs text-muted-foreground">
            <span>
              {overridden > 0 ? `本路由覆盖了 ${overridden} 组参数，其余沿用“${CACHE_STRATEGIES.find(([k]) => k === type)?.[1]}”模板。` : `当前完全沿用“${CACHE_STRATEGIES.find(([k]) => k === type)?.[1]}”模板，没有路由级覆盖。`}
            </span>
            <Button size="xs" variant="outline" disabled={overridden === 0} onClick={() => onChange({ cacheType: type })}>
              <RotateCcw />
              恢复继承模板
            </Button>
          </div>
        )}
        {type === "no_cache" ? (
          <div className="max-w-2xl border-l-2 border-muted-foreground/40 pl-4 text-sm leading-6 text-muted-foreground">
            这条路由直接走无缓存逻辑，不进入缓存计算，也不展示缓存参数；usage 原样透传上游。
          </div>
        ) : (
          <div className="space-y-7">
            {usesNamespace && (
              <ParameterGroup title="缓存空间" desc="决定这条路由是否拥有独立的缓存状态。">
                <TogField
                  label="独立路由缓存空间"
                  desc="开启后，这条路由的缓存读取和写入按路由独立统计；关闭后与同一会话、模型的默认缓存空间共享。"
                  checked={Boolean(value.routeNamespace ?? inherited.routeNamespace)}
                  onChange={(routeNamespace) => onChange({ ...value, routeNamespace })}
                />
              </ParameterGroup>
            )}
            <StrategyTemplateEditor
              title="策略参数"
              desc=""
              showHeader={false}
              cacheType={type}
              policy={policy}
              onChange={(next) => onChange({ ...next, cacheType: type, routeNamespace: value.routeNamespace })}
            />
            {!["current_high_cache", "bounded_high_cache"].includes(type) && (
              <ParameterGroup title="usage 整形" desc="这条路由返回给客户端的 usage 按下面的规则整理；不设置则沿用全局 usage 默认。">
                <PathPolicyEditor
                  policy={policy.reportedUsage ?? pathPolicy(false)}
                  onChange={(reportedUsage) => onChange({ ...value, reportedUsage })}
                />
              </ParameterGroup>
            )}
          </div>
        )}
      </EditorSection>
    </div>
  );
}
