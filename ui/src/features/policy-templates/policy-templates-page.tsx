import {
  ErrorState,
  LoadingState,
  ModalShell,
  PageContainer,
  PageHeader,
  SectionCard,
} from "@/components/patterns";
import { PathPolicyEditor, StrategyTemplateEditor } from "@/features/runtime/runtime-sections";
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  Switch,
  Textarea,
} from "@/components/ui";
import { templateHighlights } from "@/features/routes/route-utils";
import {
  usePromptInjectionCatalog,
  useRuntimeConfig,
  useUpdateRuntimeConfig,
} from "@/hooks/use-runtime-config";
import { extractErrorMessage } from "@/lib/utils";
import type { CachePolicyConfig, CacheRoutePolicyPatch, PromptCacheStrategyType, RuntimeConfig } from "@/types/api";
import { Edit3, FileText, Gauge, Save, WandSparkles, X, Zap } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";

const TEMPLATE_KEYS: Record<PromptCacheStrategyType, keyof CachePolicyConfig> = {
  no_cache: "default",
  current_high_cache: "currentHighCache",
  prefix_tool: "prefixTool",
  stable_segment_cache: "stableSegmentCache",
  prefix_tool_plus: "prefixToolPlus",
  bounded_high_cache: "boundedHighCache",
};

/** 与 main 路由策略页的模板卡一致：标题、一句说明、色调。 */
const TEMPLATE_META: Array<{
  type: PromptCacheStrategyType;
  title: string;
  description: string;
  tone: "primary" | "success" | "warning" | "info" | "secondary";
}> = [
  { type: "current_high_cache", title: "本地模拟缓存策略", description: "通过本地计算模拟缓存命中和缓存创建的展示用量。", tone: "primary" },
  { type: "prefix_tool", title: "前缀追踪", description: "按会话和请求路径追踪稳定前缀，再生成缓存用量。", tone: "success" },
  { type: "stable_segment_cache", title: "稳定片段缓存", description: "从 tools、system 和历史消息中提取稳定片段作为缓存边界。", tone: "warning" },
  { type: "prefix_tool_plus", title: "前缀追踪 Plus", description: "独立控制四类 usage 的整形、非流式策略、小请求和写入频次。", tone: "info" },
  { type: "bounded_high_cache", title: "上下限优先缓存策略", description: "参数同本地模拟缓存；各字段上下限是最终约束，计费只在范围内补，差额跨请求记账。", tone: "secondary" },
];

const TONE_CLASS: Record<(typeof TEMPLATE_META)[number]["tone"], string> = {
  primary: "bg-primary/10 text-primary",
  success: "bg-success/10 text-success",
  warning: "bg-warning/10 text-warning",
  info: "bg-info/10 text-info",
  secondary: "bg-secondary/10 text-secondary",
};

function TemplateCard({ type, policy, onEdit }: { type: PromptCacheStrategyType; policy: CacheRoutePolicyPatch; onEdit: () => void }) {
  const meta = TEMPLATE_META.find((item) => item.type === type) ?? TEMPLATE_META[0];
  return (
    <Card className="flex h-full flex-col border border-border/70 transition hover:-translate-y-0.5 hover:shadow-md">
      <CardHeader className="items-start gap-3 pb-3">
        <div className="flex min-w-0 items-start gap-3">
          <span className={`flex size-9 shrink-0 items-center justify-center rounded-lg ${TONE_CLASS[meta.tone]}`}>
            <Zap className="size-4" />
          </span>
          <div className="min-w-0">
            <CardTitle>{meta.title}</CardTitle>
            <CardDescription>{meta.description}</CardDescription>
          </div>
        </div>
        <Button variant="outline" size="xs" onClick={onEdit}>
          <Edit3 />
          编辑
        </Button>
      </CardHeader>
      <CardContent className="mt-auto pt-1">
        <div className="flex flex-wrap gap-2">
          {templateHighlights(type, policy).map((item) => (
            <Badge key={item} tone={meta.tone} size="xs">{item}</Badge>
          ))}
        </div>
      </CardContent>
    </Card>
  );
}

export function PolicyTemplatesPage() {
  const query = useRuntimeConfig();
  const catalog = usePromptInjectionCatalog();
  const update = useUpdateRuntimeConfig();
  const [draft, setDraft] = useState<RuntimeConfig | null>(null);
  const [editing, setEditing] = useState<{ type: PromptCacheStrategyType; policy: CacheRoutePolicyPatch } | null>(null);
  useEffect(() => {
    if (query.data) setDraft(structuredClone(query.data));
  }, [query.data]);

  if (query.isLoading || catalog.isLoading) return <LoadingState text="加载策略模板..." />;
  if (query.error || !draft)
    return <ErrorState title="读取策略模板失败" message={extractErrorMessage(query.error)} action={<Button variant="outline" onClick={() => query.refetch()}>重试</Button>} />;

  const dirty = JSON.stringify(draft) !== JSON.stringify(query.data);
  const saveAll = async (next: RuntimeConfig, message: string) => {
    try {
      await update.mutateAsync({
        cachePolicy: next.cachePolicy,
        reportedUsage: next.reportedUsage,
        promptInjection: next.promptInjection,
      });
      toast.success(message);
    } catch (error) {
      toast.error(extractErrorMessage(error));
    }
  };
  const templatePolicy = (type: PromptCacheStrategyType): CacheRoutePolicyPatch => ({
    ...(draft.cachePolicy[TEMPLATE_KEYS[type]] ?? {}),
    cacheType: type,
  });
  const saveTemplate = async () => {
    if (!editing) return;
    const next: RuntimeConfig = {
      ...draft,
      cachePolicy: { ...draft.cachePolicy, [TEMPLATE_KEYS[editing.type]]: editing.policy },
    };
    setDraft(next);
    await saveAll(next, `${TEMPLATE_META.find((m) => m.type === editing.type)?.title ?? "模板"}已保存`);
    setEditing(null);
  };

  return (
    <PageContainer className="pb-24">
      <PageHeader
        actions={
          <Button disabled={update.isPending || !dirty} onClick={() => void saveAll(draft, "策略模板已保存")}>
            <Save />
            保存全部
          </Button>
        }
      />

      <SectionCard
        title="缓存策略模板"
        description="路由选择某种缓存策略后，先读取这里的模板参数；路由编辑页里可以再做独立覆盖。每张卡单独编辑、单独保存。"
        icon={<WandSparkles />}
      >
        <div className="grid gap-4 xl:grid-cols-2">
          {TEMPLATE_META.map((meta) => (
            <TemplateCard key={meta.type} type={meta.type} policy={templatePolicy(meta.type)} onEdit={() => setEditing({ type: meta.type, policy: templatePolicy(meta.type) })} />
          ))}
        </div>
      </SectionCard>

      <SectionCard
        title="usage 默认整形"
        description="不选缓存策略模板、或策略模板没有覆盖的字段，按这里的默认规则整理返回给客户端的 usage。"
        icon={<Gauge />}
      >
        <PathPolicyEditor
          policy={draft.reportedUsage.default}
          onChange={(value) =>
            setDraft({
              ...draft,
              reportedUsage: { default: value as unknown as RuntimeConfig["reportedUsage"]["default"] },
            })
          }
        />
      </SectionCard>

      <SectionCard
        title="提示词注入默认值与文案"
        description="这里是全局默认开关与文案；路由编辑页可以按路由覆盖开关，文案全局共用。"
        icon={<FileText />}
      >
        <div className="divide-y">
          {catalog.data?.items.map((item) => {
            const enabled = draft.promptInjection.defaults[item.id]?.enabled ?? false;
            return (
              <div key={item.id} className="space-y-4 py-5 first:pt-0 last:pb-0">
                <div className="flex items-start justify-between gap-4">
                  <div className="min-w-0">
                    <div className="flex flex-wrap items-center gap-2">
                      <h3 className="font-medium">{item.name}</h3>
                      <Badge size="xs" tone="neutral">{item.position}</Badge>
                      {item.strictBlocks && <Badge size="xs" tone="warning">strict 下不生效</Badge>}
                    </div>
                    <p className="mt-1 text-sm text-muted-foreground">{item.impactWhenOff}</p>
                  </div>
                  <Switch
                    checked={enabled}
                    onCheckedChange={(value) =>
                      setDraft({
                        ...draft,
                        promptInjection: {
                          ...draft.promptInjection,
                          defaults: { ...draft.promptInjection.defaults, [item.id]: { enabled: value } },
                        },
                      })
                    }
                  />
                </div>
                {enabled && item.texts.length > 0 && (
                  <div className="grid gap-4 lg:grid-cols-2">
                    {item.texts.map((variant) => (
                      <label key={variant.key} className="block space-y-2 text-sm">
                        <span className="font-medium">{variant.label}</span>
                        <Textarea
                          rows={5}
                          value={draft.promptInjection.texts[variant.key] ?? variant.defaultText}
                          onChange={(e) =>
                            setDraft({
                              ...draft,
                              promptInjection: {
                                ...draft.promptInjection,
                                texts: { ...draft.promptInjection.texts, [variant.key]: e.target.value },
                              },
                            })
                          }
                        />
                      </label>
                    ))}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </SectionCard>

      {editing && (
        <ModalShell
          open
          title={`编辑${TEMPLATE_META.find((m) => m.type === editing.type)?.title ?? "缓存"}模板`}
          description="参数只会在点击保存后写入运行配置；取消不会改变当前服务配置。"
          width="max-w-5xl"
          noBodyPadding
          onClose={() => {
            if (!update.isPending) setEditing(null);
          }}
          footer={
            <>
              <Button variant="outline" onClick={() => setEditing(null)} disabled={update.isPending}><X />取消</Button>
              <Button onClick={() => void saveTemplate()} disabled={update.isPending}><Save />保存模板</Button>
            </>
          }
        >
          <div className="px-5 py-6 sm:px-7 sm:py-7">
            <StrategyTemplateEditor
              title="策略参数"
              desc={TEMPLATE_META.find((m) => m.type === editing.type)?.description ?? ""}
              cacheType={editing.type}
              policy={editing.policy}
              onChange={(policy) => setEditing((current) => (current ? { ...current, policy } : current))}
            />
          </div>
        </ModalShell>
      )}
    </PageContainer>
  );
}
