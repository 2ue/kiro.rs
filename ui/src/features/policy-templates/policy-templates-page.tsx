import {
  ErrorState,
  LoadingState,
  PageContainer,
  PageHeader,
} from "@/components/patterns";
import { PathPolicyEditor } from "@/features/runtime/runtime-sections";
import {
  Button,
  Switch,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
  Textarea,
} from "@/components/ui";
import {
  CACHE_STRATEGIES,
  RouteCacheEditor,
} from "@/features/cache-policies/route-cache-editor";
import {
  usePromptInjectionCatalog,
  useRuntimeConfig,
  useUpdateRuntimeConfig,
} from "@/hooks/use-runtime-config";
import { extractErrorMessage } from "@/lib/utils";
import type { CachePolicyConfig, RuntimeConfig } from "@/types/api";
import { Save } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";
const templateKeys: Record<string, keyof CachePolicyConfig> = {
  no_cache: "default",
  current_high_cache: "currentHighCache",
  prefix_tool: "prefixTool",
  stable_segment_cache: "stableSegmentCache",
  prefix_tool_plus: "prefixToolPlus",
  bounded_high_cache: "boundedHighCache",
};
export function PolicyTemplatesPage() {
  const query = useRuntimeConfig(),
    catalog = usePromptInjectionCatalog(),
    update = useUpdateRuntimeConfig(),
    [draft, setDraft] = useState<RuntimeConfig | null>(null);
  useEffect(() => {
    if (query.data) setDraft(structuredClone(query.data));
  }, [query.data]);
  if (query.isLoading || catalog.isLoading) return <LoadingState />;
  if (query.error || !draft)
    return (
      <ErrorState
        title="读取策略模板失败"
        action={
          <Button variant="outline" onClick={() => query.refetch()}>
            重试
          </Button>
        }
      />
    );
  return (
    <PageContainer>
      <PageHeader
        actions={
          <Button
            disabled={update.isPending}
            onClick={async () => {
              try {
                await update.mutateAsync({
                  cachePolicy: draft.cachePolicy,
                  reportedUsage: draft.reportedUsage,
                  promptInjection: draft.promptInjection,
                });
                toast.success("模板已保存");
              } catch (error) {
                toast.error(extractErrorMessage(error));
              }
            }}
          >
            <Save />
            保存模板
          </Button>
        }
      />
      <Tabs defaultValue="current_high_cache">
        <TabsList className="flex-wrap">
          {CACHE_STRATEGIES.map(([type, label]) => (
            <TabsTrigger key={type} value={type}>
              {label}
            </TabsTrigger>
          ))}
          <TabsTrigger value="usage">usage 默认</TabsTrigger>
          <TabsTrigger value="injection">注入默认与文案</TabsTrigger>
        </TabsList>
        {CACHE_STRATEGIES.map(([type]) => (
          <TabsContent key={type} value={type}>
            <RouteCacheEditor
              value={{
                ...draft.cachePolicy[templateKeys[type]],
                cacheType: type,
              }}
              onChange={(policy) =>
                setDraft({
                  ...draft,
                  cachePolicy: {
                    ...draft.cachePolicy,
                    [templateKeys[type]]: policy,
                  },
                })
              }
            />
          </TabsContent>
        ))}
        <TabsContent value="usage">
          <PathPolicyEditor
            policy={draft.reportedUsage.default}
            onChange={(value) =>
              setDraft({
                ...draft,
                reportedUsage: {
                  default:
                    value as unknown as RuntimeConfig["reportedUsage"]["default"],
                },
              })
            }
          />
        </TabsContent>
        <TabsContent value="injection" className="space-y-5">
          {catalog.data?.items.map((item) => (
            <section key={item.id} className="space-y-4 rounded-xl border p-5">
              <div className="flex justify-between gap-4">
                <div>
                  <h3 className="font-medium">{item.name}</h3>
                  <p className="text-sm text-muted-foreground">
                    {item.impactWhenOff}
                  </p>
                </div>
                <Switch
                  checked={
                    draft.promptInjection.defaults[item.id]?.enabled ?? false
                  }
                  onCheckedChange={(enabled) =>
                    setDraft({
                      ...draft,
                      promptInjection: {
                        ...draft.promptInjection,
                        defaults: {
                          ...draft.promptInjection.defaults,
                          [item.id]: { enabled },
                        },
                      },
                    })
                  }
                />
              </div>
              {item.texts.map((variant) => (
                <label key={variant.key} className="block space-y-2 text-sm">
                  <span>{variant.label}</span>
                  <Textarea
                    rows={5}
                    value={
                      draft.promptInjection.texts[variant.key] ??
                      variant.defaultText
                    }
                    onChange={(e) =>
                      setDraft({
                        ...draft,
                        promptInjection: {
                          ...draft.promptInjection,
                          texts: {
                            ...draft.promptInjection.texts,
                            [variant.key]: e.target.value,
                          },
                        },
                      })
                    }
                  />
                </label>
              ))}
            </section>
          ))}
        </TabsContent>
      </Tabs>
    </PageContainer>
  );
}
