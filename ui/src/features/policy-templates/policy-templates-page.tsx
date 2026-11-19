import {
  Callout,
  ErrorState,
  LoadingState,
  ModalShell,
  PageContainer,
  PageHeader,
  SectionCard,
  useConfirm,
} from "@/components/patterns";
import {
  CONFIG_FIELD_GRID_CLASS,
  CONFIG_FIELD_GRID_STYLE,
  ConfigFieldCell,
  ConfigSwitchControl,
} from "@/components/patterns/config-fields";
import { PathPolicyEditor, StrategyTemplateEditor } from "@/features/runtime/runtime-sections";
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from "@/components/ui";
import { InjectionItemCard } from "@/features/prompt-injection/injection-item-card";
import { InjectionRouteMatrix } from "@/features/prompt-injection/injection-route-matrix";
import {
  checkInjectionTexts,
  describePrefixChanges,
  injectionDefaultsAlready,
  injectionPrefixChanges,
  injectionTextError,
  withAllInjectionDefaults,
  type BulkDefaultMode,
} from "@/features/prompt-injection/injection-rules";
import { templateHighlights } from "@/features/routes/route-utils";
import { useRoutes } from "@/hooks/use-routes";
import {
  usePromptInjectionCatalog,
  useRuntimeConfig,
  useUpdateRuntimeConfig,
} from "@/hooks/use-runtime-config";
import { useUnsavedChangesGuard } from "@/hooks/use-unsaved-changes-guard";
import { extractErrorMessage } from "@/lib/utils";
import type { CachePolicyConfig, CacheRoutePolicyPatch, PromptCacheStrategyType, PromptInjectionConfig, RuntimeConfig } from "@/types/api";
import { Edit3, FileText, Gauge, PowerOff, RotateCcw, Save, WandSparkles, X, Zap } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { toast } from "sonner";

/** 注入部分的锚点；`?tab=injection` / `?tab=matrix` 定位到这里。 */
const INJECTION_SECTION_ID = "policy-templates-injection";
type InjectionTab = "items" | "matrix";

/** 写进 system 的部分（默认开关与文本）是否有改动；count_tokens 开关不影响请求内容。 */
function injectionContentChanged(before: PromptInjectionConfig, after: PromptInjectionConfig): boolean {
  return JSON.stringify(before.defaults) !== JSON.stringify(after.defaults) || JSON.stringify(before.texts) !== JSON.stringify(after.texts);
}

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
  { type: "current_high_cache", title: "本地模拟缓存", description: "通过本地计算模拟缓存命中和缓存创建的展示用量。", tone: "primary" },
  { type: "prefix_tool", title: "前缀追踪", description: "按会话和请求路径追踪稳定前缀，再生成缓存用量。", tone: "success" },
  { type: "stable_segment_cache", title: "稳定片段缓存", description: "从 tools、system 和历史消息中提取稳定片段作为缓存边界。", tone: "warning" },
  { type: "prefix_tool_plus", title: "前缀追踪 Plus", description: "独立控制四类 usage 的整形、非流式策略、小请求和写入频次。", tone: "info" },
  { type: "bounded_high_cache", title: "上下限优先缓存", description: "参数同本地模拟缓存；各字段上下限是最终约束，计费只在范围内补，差额跨请求记账。", tone: "secondary" },
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
  const routes = useRoutes();
  const update = useUpdateRuntimeConfig();
  const confirm = useConfirm();
  const [searchParams, setSearchParams] = useSearchParams();
  const [draft, setDraft] = useState<RuntimeConfig | null>(null);
  const [editing, setEditing] = useState<{ type: PromptCacheStrategyType; policy: CacheRoutePolicyPatch } | null>(null);
  const [injectionTab, setInjectionTab] = useState<InjectionTab>("items");
  // 服务端配置变化时（首次加载、本页或别处保存后），只有页面没有未保存改动才整体跟随；
  // 否则保留草稿，避免保存单个模板时把其它分区未保存的改动冲掉。
  const syncedRef = useRef<RuntimeConfig | null>(null);
  useEffect(() => {
    if (!query.data) return;
    const previous = syncedRef.current;
    const next = query.data;
    syncedRef.current = next;
    setDraft((current) =>
      current === null || previous === null || JSON.stringify(current) === JSON.stringify(previous)
        ? structuredClone(next)
        : current,
    );
  }, [query.data]);
  // 其它页面用 ?tab=injection / ?tab=matrix 直达注入部分；读一次后清掉参数。
  const linkedTab = searchParams.get("tab");
  const ready = draft !== null && !catalog.isLoading;
  useEffect(() => {
    if (!linkedTab || !ready) return;
    if (linkedTab === "injection" || linkedTab === "matrix") {
      setInjectionTab(linkedTab === "matrix" ? "matrix" : "items");
      requestAnimationFrame(() => document.getElementById(INJECTION_SECTION_ID)?.scrollIntoView({ block: "start", behavior: "smooth" }));
    }
    setSearchParams({}, { replace: true });
  }, [linkedTab, ready, setSearchParams]);
  // hooks 必须在任何条件 return 之前调用。
  useUnsavedChangesGuard(
    draft !== null && query.data !== undefined && JSON.stringify(draft) !== JSON.stringify(query.data) && !update.isPending,
  );

  if (query.isLoading || catalog.isLoading) return <LoadingState text="加载策略模板..." />;
  if (query.error || !draft)
    return <ErrorState title="读取策略模板失败" message={extractErrorMessage(query.error)} action={<Button variant="outline" onClick={() => query.refetch()}>重试</Button>} />;

  const dirty = JSON.stringify(draft) !== JSON.stringify(query.data);
  const items = catalog.data?.items ?? [];
  const limits = {
    maxTextChars: catalog.data?.maxTextChars ?? Number.POSITIVE_INFINITY,
    maxTotalTextChars: catalog.data?.maxTotalTextChars ?? Number.POSITIVE_INFINITY,
  };
  const strict = draft.compatProfile === "anthropic-strict";
  const textCheck = checkInjectionTexts(draft.promptInjection.texts, items, limits);
  const textError = injectionTextError(draft.promptInjection.texts, items, limits);
  /** 注入改动会让哪些路由的缓存前缀失效；路由读不到时退回“只要写进 system 的内容有改动就提醒”。 */
  const confirmInjectionChanges = async (next: RuntimeConfig): Promise<boolean> => {
    const before = query.data!.promptInjection;
    if (!injectionContentChanged(before, next.promptInjection)) return true;
    let message: string;
    if (routes.data) {
      const live = routes.data.filter((route) => route.enabled);
      const changes = injectionPrefixChanges(items, { config: before, routes: live }, { config: next.promptInjection, routes: live }, strict);
      if (changes.length === 0) return true;
      message = `${describePrefixChanges(changes)}写进 system 的内容会变化。这些内容计入缓存前缀，保存后这些路由上已有会话的缓存前缀会失效一次。`;
    } else {
      message = "注入的默认开关或文本有改动。写进 system 的内容计入缓存前缀，保存后跟随这些设置的路由上已有会话的缓存前缀会失效一次。";
    }
    return confirm({ title: "保存注入改动", message, confirmText: "保存" });
  };
  const saveAll = async (next: RuntimeConfig, message: string) => {
    if (textError) {
      toast.error(`无法保存：${textError}`);
      return;
    }
    if (!(await confirmInjectionChanges(next))) return;
    try {
      const saved = await update.mutateAsync({
        cachePolicy: next.cachePolicy,
        reportedUsage: next.reportedUsage,
        promptInjection: next.promptInjection,
      });
      // 全部已保存：以服务端规范化后的结果为准。
      syncedRef.current = saved;
      setDraft(structuredClone(saved));
      toast.success(message);
    } catch (error) {
      toast.error(extractErrorMessage(error));
    }
  };
  const templatePolicy = (type: PromptCacheStrategyType): CacheRoutePolicyPatch => ({
    ...(draft.cachePolicy[TEMPLATE_KEYS[type]] ?? {}),
    cacheType: type,
  });
  // 单个模板的保存只提交 cachePolicy 里这一个键，页面上其它分区（usage 整形、注入文案）
  // 未保存的改动不被连带写入；失败时弹窗保持打开，用户可以改完重试。
  const saveTemplate = async () => {
    if (!editing || !query.data) return;
    const key = TEMPLATE_KEYS[editing.type];
    const cachePolicy = { ...query.data.cachePolicy, [key]: editing.policy };
    try {
      const saved = await update.mutateAsync({ cachePolicy });
      const savedPolicy = saved.cachePolicy[key];
      setDraft((current) => current && { ...current, cachePolicy: { ...current.cachePolicy, [key]: savedPolicy } });
      toast.success(`${TEMPLATE_META.find((m) => m.type === editing.type)?.title ?? "模板"}已保存`);
      setEditing(null);
    } catch (error) {
      toast.error(extractErrorMessage(error));
    }
  };
  const templateDirty = editing
    ? JSON.stringify(editing.policy) !== JSON.stringify(templatePolicy(editing.type))
    : false;
  const closeEditor = async () => {
    if (update.isPending) return;
    if (templateDirty) {
      const ok = await confirm({
        title: "放弃未保存的修改？",
        message: "这个模板里改过的参数还没有保存，关闭后会丢失。",
        confirmText: "放弃修改",
        tone: "danger",
      });
      if (!ok) return;
    }
    setEditing(null);
  };
  const setInjection = (promptInjection: PromptInjectionConfig) => setDraft({ ...draft, promptInjection });
  // 新装默认全部是关时，"恢复新装默认"与"全部设为关"效果相同，只保留前者。
  const freshDefaultsAllOff = items.every((entry) => !entry.defaultEnabled);
  const setAllDefaults = async (mode: BulkDefaultMode) => {
    const turnsOffRecommended = items.some(
      (entry) =>
        entry.recommendation === "keep_on" &&
        (draft.promptInjection.defaults[entry.id]?.enabled ?? false) &&
        !(mode === "off" ? false : entry.defaultEnabled),
    );
    const warning = turnsOffRecommended ? "标“建议开”的项会被关闭，关闭后会有功能退化。" : "";
    const ok = await confirm({
      title: mode === "off" ? "全部默认值设为关" : "全部恢复新装默认",
      message:
        mode === "off"
          ? `所有注入项的全局默认改为关闭。路由上的显式设置不变；${warning}点“保存全部”后才生效。`
          : `所有注入项的全局默认恢复成新装值。路由上的显式设置不变；${warning}点“保存全部”后才生效。`,
      confirmText: "确认",
      tone: turnsOffRecommended ? "danger" : "default",
    });
    if (ok) setInjection(withAllInjectionDefaults(draft.promptInjection, items, mode));
  };

  return (
    <PageContainer className="pb-24">
      <PageHeader
        actions={
          <Button
            disabled={update.isPending || !dirty || textError !== null}
            title={textError ?? (dirty ? undefined : "没有未保存的修改")}
            onClick={() => void saveAll(draft, "策略模板已保存")}
          >
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

      <div id={INJECTION_SECTION_ID} className="scroll-mt-4">
        <SectionCard
          title="提示词注入默认值与文案"
          description="这里是全局默认开关与文案；路由编辑页可以按路由覆盖开关，文案全局共用。"
          icon={<FileText />}
          actions={
            items.length > 0 && (
              <>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={injectionDefaultsAlready(draft.promptInjection, items, "fresh")}
                  title={injectionDefaultsAlready(draft.promptInjection, items, "fresh") ? "已经是新装默认" : undefined}
                  onClick={() => void setAllDefaults("fresh")}
                >
                  <RotateCcw />
                  全部恢复新装默认
                </Button>
                {!freshDefaultsAllOff && <Button
                  variant="outline"
                  size="sm"
                  disabled={injectionDefaultsAlready(draft.promptInjection, items, "off")}
                  title={injectionDefaultsAlready(draft.promptInjection, items, "off") ? "默认值已经全部是关" : undefined}
                  onClick={() => void setAllDefaults("off")}
                >
                  <PowerOff />
                  全部默认值设为关
                </Button>}
              </>
            )
          }
        >
          {catalog.error || !catalog.data ? (
            <ErrorState
              title="读取注入目录失败"
              message={extractErrorMessage(catalog.error)}
              action={<Button variant="outline" onClick={() => catalog.refetch()}>重试</Button>}
            />
          ) : (
            <Tabs value={injectionTab} onValueChange={(value) => setInjectionTab(value as InjectionTab)}>
              <TabsList>
                <TabsTrigger value="items">注入项</TabsTrigger>
                <TabsTrigger value="matrix">路由矩阵</TabsTrigger>
              </TabsList>
              <TabsContent value="items" className="space-y-4 pt-2">
                {strict && <Callout tone="warning">当前兼容模式是 anthropic-strict，标“strict 下不生效”的项都不会注入。</Callout>}
                {textError && <Callout tone="error">{textError}，无法保存。</Callout>}
                <div className={CONFIG_FIELD_GRID_CLASS} style={CONFIG_FIELD_GRID_STYLE}>
                  <ConfigFieldCell
                    id="apply-count-tokens"
                    label="count_tokens 计入注入内容"
                    description="计数时同样加上写进 system 的项（任务质量、语言约束、自定义、身份声明），避免估算低于真实请求。"
                  >
                    <ConfigSwitchControl
                      id="apply-count-tokens"
                      checked={draft.promptInjection.applyToCountTokens}
                      onChange={(applyToCountTokens) => setInjection({ ...draft.promptInjection, applyToCountTokens })}
                    />
                  </ConfigFieldCell>
                </div>
                <p className={textCheck.overTotal ? "text-xs text-destructive" : "text-xs text-muted-foreground"}>
                  可编辑文本合计 {textCheck.total} / {catalog.data.maxTotalTextChars} 字符，单段不超过 {catalog.data.maxTextChars} 字符。
                </p>
                {items.map((entry) => (
                  <InjectionItemCard
                    key={entry.id}
                    entry={entry}
                    config={draft.promptInjection}
                    maxTextChars={catalog.data.maxTextChars}
                    onChange={setInjection}
                  />
                ))}
              </TabsContent>
              <TabsContent value="matrix" className="pt-2">
                {routes.isLoading ? (
                  <LoadingState text="加载路由..." />
                ) : routes.error || !routes.data ? (
                  <ErrorState
                    title="读取路由失败"
                    message={extractErrorMessage(routes.error)}
                    action={<Button variant="outline" onClick={() => routes.refetch()}>重试</Button>}
                  />
                ) : (
                  <InjectionRouteMatrix routes={routes.data} catalog={items} config={draft.promptInjection} strict={strict} />
                )}
              </TabsContent>
            </Tabs>
          )}
        </SectionCard>
      </div>

      {editing && (
        <ModalShell
          open
          title={`编辑${TEMPLATE_META.find((m) => m.type === editing.type)?.title ?? "缓存"}模板`}
          description="参数只会在点击保存后写入运行配置；取消不会改变当前服务配置。"
          width="max-w-5xl"
          noBodyPadding
          onClose={() => void closeEditor()}
          footer={
            <>
              <Button variant="outline" onClick={() => void closeEditor()} disabled={update.isPending}><X />取消</Button>
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
