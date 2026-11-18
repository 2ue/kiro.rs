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
import { RouteInjectionEditor } from "@/features/prompt-injection/route-injection-editor";
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
function RuleMode({
  value,
  onChange,
}: {
  value: RouteRule;
  onChange: (v: RouteRule) => void;
}) {
  return (
    <div className="space-y-3">
      <Select
        value={value.mode}
        onValueChange={(mode) =>
          onChange({ ...value, mode: mode as RouteRule["mode"] })
        }
      >
        <SelectTrigger className="max-w-sm">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {RULE_MODE_OPTIONS.map((option) => (
            <SelectItem key={option.value} value={option.value}>{option.label}</SelectItem>
          ))}
        </SelectContent>
      </Select>
      <p className="text-sm text-muted-foreground">
        {ruleSummary(value.mode, value.rules)}
      </p>
    </div>
  );
}
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
        <TabsContent value="accounts" className="space-y-5">
          <RuleMode
            value={draft.accountRule}
            onChange={(v) => set("accountRule", v)}
          />
          {draft.accountRule.mode !== "allow_all" && (
            <div className="grid gap-3 md:grid-cols-2">
              {(accounts.data?.accounts ?? []).map((a) => (
                <label
                  className="flex items-center gap-3 rounded-lg border p-3"
                  key={a.id}
                >
                  <Checkbox
                    checked={draft.accountRule.rules.includes(String(a.id))}
                    onCheckedChange={(v) =>
                      toggleAccount(String(a.id), v === true)
                    }
                  />
                  {a.name}{" "}
                  <span className="text-xs text-muted-foreground">#{a.id}</span>
                </label>
              ))}
            </div>
          )}
        </TabsContent>
        <TabsContent value="models" className="space-y-5">
          <RuleMode
            value={draft.modelRule}
            onChange={(v) => set("modelRule", v)}
          />
          <div className="grid gap-5 md:grid-cols-2">
            <div>
              <p className="mb-2 text-sm">每行一条模型规则，支持尾部 *</p>
              <Textarea
                rows={12}
                value={draft.modelRule.rules.join("\n")}
                disabled={draft.modelRule.mode === "allow_all"}
                onChange={(e) =>
                  set("modelRule", {
                    ...draft.modelRule,
                    rules: e.target.value.split("\n"),
                  })
                }
              />
            </div>
            <div>
              <h3 className="mb-2 font-medium">当前可用账号支持的模型</h3>
              <div className="flex flex-wrap gap-2">
                {models.map((model) => (
                  <Button
                    key={model}
                    size="sm"
                    variant="outline"
                    onClick={() =>
                      set("modelRule", {
                        ...draft.modelRule,
                        mode:
                          draft.modelRule.mode === "allow_all"
                            ? "allow_list"
                            : draft.modelRule.mode,
                        rules: [...new Set([...draft.modelRule.rules, model])],
                      })
                    }
                  >
                    {model}
                  </Button>
                ))}
              </div>
              <p className="mt-3 text-xs text-muted-foreground">
                支持模型为空的账号不限模型。模型是否可用仍受账号自己的路由规则约束。
              </p>
            </div>
          </div>
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
