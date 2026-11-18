import {
  ErrorState,
  LoadingState,
  PageContainer,
  PageHeader,
} from "@/components/patterns";
import {
  Button,
  Checkbox,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Switch,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
  Textarea,
} from "@/components/ui";
import { RouteCacheEditor } from "@/features/cache-policies/route-cache-editor";
import { RouteInjectionEditor, SettingGroup, SettingRow } from "@/features/prompt-injection/route-injection-editor";
import { useAccounts } from "@/hooks/use-accounts";
import {
  useRouteEffective,
  useRoutes,
  useUpdateRoutes,
} from "@/hooks/use-routes";
import {
  usePromptInjectionCatalog,
  useRuntimeConfig,
} from "@/hooks/use-runtime-config";
import { extractErrorMessage } from "@/lib/utils";
import type { RouteConfig, RouteRule } from "@/types/routes";
import { ArrowLeft, Copy, Eye, Link2, Save } from "lucide-react";
import { useEffect, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { toast } from "sonner";
import { EffectiveSheet } from "./effective-sheet";
import { RouteFeaturesEditor } from "./route-features-editor";
import {
  accountAllowsRoute,
  displayRoutePrefix,
  isBuiltinRoute,
  routeBaseUrl,
  ruleSummary,
} from "./route-utils";
import { Badge } from "@/components/ui";
import { Field } from "@/components/patterns";
import { RULE_MODE_OPTIONS } from "@/lib/enum-labels";
export function RouteEditPage() {
  const params = useParams(),
    prefix = params.prefix === "_root" ? "" : (params.prefix ?? ""),
    navigate = useNavigate();
  const routes = useRoutes(),
    runtime = useRuntimeConfig(),
    catalog = usePromptInjectionCatalog(),
    accounts = useAccounts(),
    update = useUpdateRoutes();
  const [draft, setDraft] = useState<RouteConfig | null>(null),
    [showEffective, setShowEffective] = useState(false);
  const effective = useRouteEffective(prefix, showEffective);
  useEffect(() => {
    const route = routes.data?.find((r) => r.prefix === prefix);
    if (route) setDraft(structuredClone(route));
  }, [routes.data, prefix]);
  if (routes.isLoading || runtime.isLoading || catalog.isLoading)
    return <LoadingState />;
  if (routes.error || runtime.error || catalog.error || !draft || !runtime.data)
    return (
      <ErrorState
        title="路由或模板读取失败"
        action={
          <Button
            onClick={() => {
              routes.refetch();
              runtime.refetch();
              catalog.refetch();
            }}
          >
            重试
          </Button>
        }
      />
    );
  const set = <K extends keyof RouteConfig>(key: K, value: RouteConfig[K]) =>
    setDraft({ ...draft, [key]: value });
  const availableAccounts = (accounts.data?.accounts ?? []).filter(
    (a) =>
      a.enabled &&
      !a.autoDisabled &&
      accountAllowsRoute(a, prefix) &&
      (draft.accountRule.mode === "allow_all" ||
        (draft.accountRule.mode === "allow_list"
          ? draft.accountRule.rules.includes(String(a.id)) ||
            draft.accountRule.rules.includes("*")
          : !draft.accountRule.rules.includes(String(a.id)) &&
            !draft.accountRule.rules.includes("*"))),
  );
  const models = [
    ...new Set(availableAccounts.flatMap((a) => a.supportedModels)),
  ].sort();
  const toggleAccount = (id: string, checked: boolean) =>
    set("accountRule", {
      ...draft.accountRule,
      rules: checked
        ? [...draft.accountRule.rules, id]
        : draft.accountRule.rules.filter((v) => v !== id),
    });
  return (
    <PageContainer>
      <PageHeader
        actions={
          <>
            <Button variant="outline" onClick={() => navigate("/routes")}>
              <ArrowLeft />
              返回路由
            </Button>
            <Button variant="outline" onClick={() => setShowEffective(true)}>
              <Eye />
              查看生效配置
            </Button>
            <Button
              disabled={update.isPending}
              onClick={async () => {
                try {
                  await update.mutateAsync(
                    (routes.data ?? []).map((r) =>
                      r.prefix === prefix ? draft : r,
                    ),
                  );
                  toast.success("路由已保存");
                } catch (error) {
                  toast.error(extractErrorMessage(error));
                }
              }}
            >
              <Save />
              保存
            </Button>
          </>
        }
      />
      <div className="rounded-xl border bg-card p-4 shadow-sm">
        <div className="flex flex-wrap items-start justify-between gap-4">
          <div className="min-w-0 space-y-2">
            <div className="flex flex-wrap items-center gap-2">
              <h2 className="font-mono text-lg font-semibold">{displayRoutePrefix(prefix)}</h2>
              {isBuiltinRoute(prefix) ? <Badge size="xs" tone="secondary">内置</Badge> : <Badge size="xs" tone="info">自定义</Badge>}
              {!draft.enabled && <Badge size="xs" tone="warning">已停用</Badge>}
            </div>
            <div className="flex min-w-0 items-center gap-1 rounded-md border border-border/70 bg-muted/40 py-1 pl-2.5 pr-1 text-xs">
              <Link2 className="size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" />
              <span className="min-w-0 flex-1 truncate font-mono" title={routeBaseUrl(prefix)}>{routeBaseUrl(prefix)}</span>
              <Button
                variant="ghost"
                size="icon-xs"
                title="复制路由地址"
                onClick={async () => {
                  try {
                    await navigator.clipboard.writeText(routeBaseUrl(prefix));
                    toast.success("路由地址已复制");
                  } catch (error) {
                    toast.error(extractErrorMessage(error));
                  }
                }}
              >
                <Copy />
              </Button>
            </div>
          </div>
          <div className="flex flex-wrap items-end gap-4">
            <Field label="名称" htmlFor="route-name" className="w-64">
              <Input id="route-name" value={draft.name} onChange={(e) => set("name", e.target.value)} />
            </Field>
            <Field label="启用" htmlFor="route-enabled" inline className="py-0">
              <Switch id="route-enabled" checked={draft.enabled} onCheckedChange={(v) => set("enabled", v)} />
            </Field>
          </div>
        </div>
      </div>
      <Tabs defaultValue="cache">
        <TabsList>
          {[
            ["cache", "缓存与 usage"],
            ["injection", "提示词注入"],
            ["features", "路由功能"],
            ["accounts", "账号规则"],
            ["models", "模型规则"],
          ].map(([value, label]) => (
            <TabsTrigger key={value} value={value}>
              {label}
            </TabsTrigger>
          ))}
        </TabsList>
        <TabsContent value="cache">
          <RouteCacheEditor
            templates={runtime.data.cachePolicy}
            value={{
              ...draft.cachePolicy,
              ...(draft.reportedUsage && !draft.cachePolicy.reportedUsage
                ? { reportedUsage: draft.reportedUsage }
                : {}),
            }}
            onChange={(v) => set("cachePolicy", v)}
          />
        </TabsContent>
        <TabsContent value="injection">
          <RouteInjectionEditor
            catalog={catalog.data?.items ?? []}
            config={runtime.data}
            injection={draft.promptInjection}
            onChange={(v) => set("promptInjection", v)}
          />
        </TabsContent>
        <TabsContent value="features">
          <RouteFeaturesEditor
            value={draft.features}
            defaults={runtime.data.routeFeatures.defaults}
            onChange={(v) => set("features", v)}
          />
        </TabsContent>
        <TabsContent value="accounts">
          <SettingGroup title="账号规则" desc="决定哪些账号可以承接这条路由。账号自身也有“接受的路由”规则，两边都放行才会被调度到。">
            <SettingRow
              title="规则模式"
              control={
                <Select value={draft.accountRule.mode} onValueChange={(mode) => set("accountRule", { mode: mode as RouteRule["mode"], rules: mode === "allow_all" ? [] : draft.accountRule.rules })}>
                  <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                  <SelectContent>
                    {RULE_MODE_OPTIONS.map((option) => <SelectItem key={option.value} value={option.value}>{option.label.replace("入口", "账号")}</SelectItem>)}
                  </SelectContent>
                </Select>
              }
            >
              <p className="text-xs leading-5 text-muted-foreground">{ruleSummary(draft.accountRule.mode, draft.accountRule.rules)}；可用账号 {availableAccounts.length} 个。</p>
            </SettingRow>
            {draft.accountRule.mode !== "allow_all" && (
              <div className="py-3">
                <div className="mb-2 flex flex-wrap items-center justify-between gap-2 text-xs text-muted-foreground">
                  <span>{draft.accountRule.mode === "allow_list" ? "勾选允许承接这条路由的账号" : "勾选禁止承接这条路由的账号"}，已选 {draft.accountRule.rules.length} 个</span>
                  <div className="flex gap-1">
                    <Button size="xs" variant="ghost" onClick={() => set("accountRule", { ...draft.accountRule, rules: (accounts.data?.accounts ?? []).map((a) => String(a.id)) })}>全选</Button>
                    <Button size="xs" variant="ghost" disabled={draft.accountRule.rules.length === 0} onClick={() => set("accountRule", { ...draft.accountRule, rules: [] })}>清空</Button>
                  </div>
                </div>
                {(accounts.data?.accounts ?? []).length === 0 ? (
                  <p className="py-6 text-center text-sm text-muted-foreground">还没有账号，先到账号页添加。</p>
                ) : (
                  <div className="max-h-[28rem] divide-y overflow-y-auto rounded-md border">
                    {(accounts.data?.accounts ?? []).map((a) => {
                      const checked = draft.accountRule.rules.includes(String(a.id));
                      const acceptsRoute = accountAllowsRoute(a, prefix);
                      return (
                        <label key={a.id} className="flex cursor-pointer items-center gap-3 px-3 py-2 hover:bg-muted/30">
                          <Checkbox checked={checked} onCheckedChange={(v) => toggleAccount(String(a.id), v === true)} aria-label={`选择账号 #${a.id}`} />
                          <span className="shrink-0 font-mono text-xs text-muted-foreground">#{a.id}</span>
                          <span className="min-w-0 flex-1 truncate text-sm">{a.name}</span>
                          <span className="flex shrink-0 gap-1">
                            {!a.enabled && <Badge size="xs" tone="neutral">停用</Badge>}
                            {a.autoDisabled && <Badge size="xs" tone="error">自动禁用</Badge>}
                            {!acceptsRoute && <Badge size="xs" tone="warning" title="该账号自身的路由规则不接受这条路由">账号侧不接受</Badge>}
                            {a.supportedModels.length > 0 && <Badge size="xs" tone="info">{a.supportedModels.length} 个模型</Badge>}
                          </span>
                        </label>
                      );
                    })}
                  </div>
                )}
              </div>
            )}
          </SettingGroup>
        </TabsContent>
        <TabsContent value="models">
          <SettingGroup title="模型规则" desc="决定这条路由允许请求哪些模型。模型还要被某个可用账号接受才会真正可调度。">
            <SettingRow
              title="规则模式"
              control={
                <Select value={draft.modelRule.mode} onValueChange={(mode) => set("modelRule", { mode: mode as RouteRule["mode"], rules: mode === "allow_all" ? [] : draft.modelRule.rules })}>
                  <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                  <SelectContent>
                    {RULE_MODE_OPTIONS.map((option) => <SelectItem key={option.value} value={option.value}>{option.label.replace("入口", "模型")}</SelectItem>)}
                  </SelectContent>
                </Select>
              }
            >
              <p className="text-xs leading-5 text-muted-foreground">{ruleSummary(draft.modelRule.mode, draft.modelRule.rules.filter(Boolean))}。</p>
            </SettingRow>
            {draft.modelRule.mode !== "allow_all" && (
              <div className="grid gap-5 py-3 md:grid-cols-2">
                <div className="space-y-1.5">
                  <label className="text-sm font-medium" htmlFor="route-model-rules">模型规则</label>
                  <p className="text-xs text-muted-foreground">每行一条，支持尾部 *，例如 claude-sonnet-4-5 或 claude-opus-*。</p>
                  <Textarea
                    id="route-model-rules"
                    rows={12}
                    className="font-mono text-xs"
                    value={draft.modelRule.rules.join("\n")}
                    onChange={(e) => set("modelRule", { ...draft.modelRule, rules: e.target.value.split("\n") })}
                  />
                </div>
                <div className="space-y-1.5">
                  <div className="text-sm font-medium">当前可用账号支持的模型</div>
                  <p className="text-xs text-muted-foreground">点一下加入规则。支持模型为空的账号不限模型。</p>
                  {models.length === 0 ? (
                    <p className="py-6 text-center text-xs text-muted-foreground">可用账号没有声明支持的模型。</p>
                  ) : (
                    <div className="flex max-h-[20rem] flex-wrap gap-1.5 overflow-y-auto rounded-md border p-2">
                      {models.map((model) => {
                        const included = draft.modelRule.rules.includes(model);
                        return (
                          <Button
                            key={model}
                            size="xs"
                            variant={included ? "secondary" : "outline"}
                            className="font-mono"
                            onClick={() => set("modelRule", { ...draft.modelRule, rules: included ? draft.modelRule.rules.filter((r) => r !== model) : [...new Set([...draft.modelRule.rules.filter(Boolean), model])] })}
                          >
                            {model}
                          </Button>
                        );
                      })}
                    </div>
                  )}
                </div>
              </div>
            )}
          </SettingGroup>
        </TabsContent>
      </Tabs>
      <EffectiveSheet
        open={showEffective}
        onOpenChange={setShowEffective}
        title="已保存路由的生效配置"
        value={
          effective.data && {
            cachePolicy: effective.data.cachePolicy,
            features: effective.data.features,
            injections: effective.data.injections,
            accountRule: effective.data.accountRule,
            modelRule: effective.data.modelRule,
          }
        }
        sources={effective.data?.sources}
        loading={effective.isLoading}
        error={effective.error}
      />
    </PageContainer>
  );
}
