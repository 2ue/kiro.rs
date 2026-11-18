import { useRuntimeConfig } from "@/hooks/use-runtime-config";
import { useAccounts } from "@/hooks/use-accounts";
import {
  EmptyState,
  ErrorState,
  Field,
  LoadingState,
  ModalShell,
  PageContainer,
  PageHeader,
  SectionCard,
  StatCard,
  StatGrid,
  useConfirm,
} from "@/components/patterns";
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  Input,
  Switch,
} from "@/components/ui";
import { CACHE_STRATEGIES } from "@/features/cache-policies/route-cache-editor";
import { useRoutes, useUpdateRoutes } from "@/hooks/use-routes";
import { extractErrorMessage } from "@/lib/utils";
import type { RouteConfig } from "@/types/routes";
import type { RuntimeConfig } from "@/types/api";
import { Copy, CopyPlus, Edit3, Link2, Plus, Route as RouteIcon, Trash2 } from "lucide-react";
import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { toast } from "sonner";
import {
  CUSTOM_ROUTE_PREFIX,
  displayRoutePrefix,
  isBuiltinRoute,
  normalizeCustomRoutePrefix,
  normalizeRoutePrefix,
  routeBaseUrl,
  routeClaudeCodeEnv,
  routeEditorPath,
  routePolicySummary,
  sortRoutes,
} from "./route-utils";

function strategyLabel(type?: string): string {
  return CACHE_STRATEGIES.find(([key]) => key === type)?.[1] ?? "模拟高缓存";
}

function enabledInjectionCount(route: RouteConfig, runtime?: RuntimeConfig): number {
  return Object.entries(runtime?.promptInjection.defaults ?? {}).filter(
    ([id, value]) => route.promptInjection[id]?.enabled ?? value.enabled,
  ).length;
}

function enabledFeatureCount(route: RouteConfig, runtime?: RuntimeConfig): number {
  return Object.values({ ...runtime?.routeFeatures.defaults, ...route.features }).filter(
    (v) =>
      v !== undefined &&
      v !== null &&
      v !== false &&
      v !== "off" &&
      !(typeof v === "object" && "enabled" in v && !v.enabled),
  ).length;
}

async function copyText(label: string, value: string) {
  try {
    await navigator.clipboard.writeText(value);
    toast.success(`${label}已复制`);
  } catch (error) {
    toast.error(`复制失败: ${extractErrorMessage(error)}`);
  }
}

function RouteCard({
  route,
  runtime,
  accountCount,
  busy,
  onToggle,
  onEdit,
  onCopy,
  onDelete,
}: {
  route: RouteConfig;
  runtime?: RuntimeConfig;
  accountCount: number | "全部";
  busy: boolean;
  onToggle: (enabled: boolean) => void;
  onEdit: () => void;
  onCopy: () => void;
  onDelete: () => void;
}) {
  const builtIn = isBuiltinRoute(route.prefix);
  const prefix = displayRoutePrefix(route.prefix);
  const baseUrl = routeBaseUrl(route.prefix);
  const injections = enabledInjectionCount(route, runtime);
  const features = enabledFeatureCount(route, runtime);
  return (
    <Card className={`flex h-full flex-col border border-border/70 transition hover:shadow-md ${route.enabled ? "" : "opacity-70"}`}>
      <CardHeader className="items-start gap-3 pb-3">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <CardTitle className="font-mono text-sm">{prefix}</CardTitle>
            {builtIn && <Badge size="xs" tone="secondary">内置</Badge>}
            {!route.enabled && <Badge size="xs" tone="warning">已停用</Badge>}
          </div>
          <CardDescription className="mt-1 flex flex-wrap items-center gap-x-2">
            <span>{route.name || "未命名"}</span>
            <span className="text-muted-foreground/50">·</span>
            <span>{strategyLabel(route.cachePolicy.cacheType)}</span>
          </CardDescription>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          <Switch checked={route.enabled} disabled={busy} onCheckedChange={onToggle} aria-label={`启用 ${prefix}`} />
          <Button variant="ghost" size="icon-xs" title="编辑" aria-label={`编辑 ${prefix}`} onClick={onEdit}><Edit3 /></Button>
          <Button variant="ghost" size="icon-xs" title="复制为新路由" aria-label={`以 ${prefix} 的配置新建路由`} onClick={onCopy}><CopyPlus /></Button>
          {!builtIn && (
            <Button variant="ghost" size="icon-xs" className="text-destructive hover:text-destructive" title="删除" aria-label={`删除 ${prefix}`} disabled={busy} onClick={onDelete}><Trash2 /></Button>
          )}
        </div>
      </CardHeader>
      <CardContent className="mt-auto space-y-3 pt-1">
        <div className="flex min-w-0 items-center gap-1 rounded-md border border-border/70 bg-muted/40 py-1 pl-2.5 pr-1">
          <Link2 className="size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" />
          <span className="min-w-0 flex-1 truncate font-mono text-xs" title={baseUrl}>{baseUrl}</span>
          <Button variant="ghost" size="icon-xs" title="复制完整路由地址" aria-label={`复制 ${prefix} 的完整路由地址`} onClick={() => void copyText("路由地址", baseUrl)}>
            <Copy />
          </Button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="ghost" size="xs" title="更多复制方式" aria-label={`${prefix} 的更多复制方式`}>更多</Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuLabel>复制接入信息</DropdownMenuLabel>
              <DropdownMenuSeparator />
              <DropdownMenuItem onSelect={() => void copyText("路由地址", baseUrl)}>Base URL</DropdownMenuItem>
              <DropdownMenuItem onSelect={() => void copyText("Messages 接口地址", `${baseUrl}/v1/messages`)}>Messages 接口地址</DropdownMenuItem>
              <DropdownMenuItem onSelect={() => void copyText("Claude Code 环境变量", routeClaudeCodeEnv(route.prefix))}>Claude Code 环境变量</DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
        <div className="flex flex-wrap gap-2">
          {routePolicySummary(route.cachePolicy).map((item) => <Badge key={item} size="xs" tone="neutral">{item}</Badge>)}
          <Badge size="xs" tone="info">注入 {injections}</Badge>
          <Badge size="xs" tone="info">功能 {features}</Badge>
          <Badge size="xs" tone="info">账号 {accountCount}</Badge>
        </div>
      </CardContent>
    </Card>
  );
}

export function RoutesPage() {
  const runtime = useRuntimeConfig();
  const accounts = useAccounts();
  const query = useRoutes();
  const update = useUpdateRoutes();
  const navigate = useNavigate();
  const confirm = useConfirm();
  const [creator, setCreator] = useState<{ source: RouteConfig | null; name: string; routeName: string } | null>(null);

  const save = async (routes: RouteConfig[], message = "路由已保存") => {
    try {
      await update.mutateAsync(routes);
      toast.success(message);
    } catch (error) {
      toast.error(extractErrorMessage(error));
    }
  };

  if (query.isLoading) return <LoadingState text="加载路由..." />;
  if (query.error)
    return <ErrorState title="加载路由失败" message={extractErrorMessage(query.error)} action={<Button variant="outline" onClick={() => query.refetch()}>重试</Button>} />;

  const routes = query.data ?? [];
  const sorted = sortRoutes(routes);
  const builtinCount = routes.filter((r) => isBuiltinRoute(r.prefix)).length;
  const customCount = routes.length - builtinCount;
  const disabledCount = routes.filter((r) => !r.enabled).length;
  const accountCountFor = (route: RouteConfig): number | "全部" => {
    if (route.accountRule.mode === "allow_all") return "全部";
    const list = accounts.data?.accounts ?? [];
    return list.filter((a) =>
      route.accountRule.mode === "allow_list"
        ? route.accountRule.rules.includes("*") || route.accountRule.rules.includes(String(a.id))
        : !route.accountRule.rules.includes("*") && !route.accountRule.rules.includes(String(a.id)),
    ).length;
  };

  const emptyRoute = (): RouteConfig => ({
    prefix: "",
    name: "",
    enabled: true,
    cachePolicy: {},
    features: {},
    promptInjection: {},
    accountRule: { mode: "allow_all", rules: [] },
    modelRule: { mode: "allow_all", rules: [] },
  });
  const openCreator = (source: RouteConfig | null) =>
    setCreator({
      source,
      name: "",
      routeName: source ? `${source.name || displayRoutePrefix(source.prefix)} 副本` : "",
    });
  const normalizedNewPrefix = creator ? normalizeCustomRoutePrefix(creator.name) : null;
  const duplicate = normalizedNewPrefix != null && routes.some((r) => normalizeRoutePrefix(r.prefix) === normalizedNewPrefix);
  const create = async () => {
    if (!creator || !normalizedNewPrefix || duplicate) return;
    const base = creator.source ? structuredClone(creator.source) : emptyRoute();
    const next: RouteConfig = { ...base, prefix: normalizedNewPrefix, name: creator.routeName.trim() || normalizedNewPrefix, enabled: true };
    try {
      await update.mutateAsync([...routes, next]);
      toast.success(`路由 ${normalizedNewPrefix} 已创建`);
      setCreator(null);
      navigate(routeEditorPath(normalizedNewPrefix));
    } catch (error) {
      toast.error(extractErrorMessage(error));
    }
  };

  return (
    <PageContainer>
      <PageHeader
        actions={
          <Button onClick={() => openCreator(null)}>
            <Plus />
            新增路由
          </Button>
        }
      />

      <StatGrid min="12rem">
        <StatCard title="路由" value={routes.length} desc={`内置 ${builtinCount} · 自定义 ${customCount}`} icon={<RouteIcon />} tone="primary" />
        <StatCard title="已停用" value={disabledCount} desc={disabledCount ? "停用的入口返回 404" : "全部入口可用"} icon={<RouteIcon />} tone={disabledCount ? "warning" : "success"} />
        <StatCard title="自定义前缀" value={CUSTOM_ROUTE_PREFIX.replace(/\/$/, "")} desc="自定义路由统一挂在这个前缀下，与内置入口隔离" icon={<Link2 />} tone="info" />
      </StatGrid>

      <SectionCard
        title="路由"
        description="每个入口独立配置缓存策略、提示词注入、路由功能与账号/模型规则。内置路由不能删除；自定义路由通过 /dfcache/{name} 管理。"
        icon={<RouteIcon />}
      >
        {sorted.length === 0 ? (
          <EmptyState title="暂无路由" />
        ) : (
          <div className="grid gap-4 xl:grid-cols-2">
            {sorted.map((route) => (
              <RouteCard
                key={route.prefix}
                route={route}
                runtime={runtime.data}
                accountCount={accountCountFor(route)}
                busy={update.isPending}
                onToggle={(enabled) =>
                  save(
                    routes.map((item) => (item.prefix === route.prefix ? { ...item, enabled } : item)),
                    enabled ? `${displayRoutePrefix(route.prefix)} 已启用` : `${displayRoutePrefix(route.prefix)} 已停用`,
                  )
                }
                onEdit={() => navigate(routeEditorPath(route.prefix))}
                onCopy={() => openCreator(route)}
                onDelete={async () => {
                  if (
                    await confirm({
                      title: "删除路由",
                      message: `删除 ${displayRoutePrefix(route.prefix)} 后该入口立即返回 404，使用它的客户端会失败。`,
                      confirmText: "删除",
                      tone: "danger",
                    })
                  )
                    save(routes.filter((item) => item.prefix !== route.prefix), `路由 ${displayRoutePrefix(route.prefix)} 已删除`);
                }}
              />
            ))}
          </div>
        )}
      </SectionCard>

      <ModalShell
        open={creator !== null}
        onClose={() => setCreator(null)}
        title={creator?.source ? `复制路由 · 来自 ${displayRoutePrefix(creator.source.prefix)}` : "新增路由"}
        description={creator?.source ? "缓存、注入、功能与规则都会复制；路由前缀需要重新填写。" : "自定义路由固定挂在 /dfcache/ 下，只填后缀名。创建后进入编辑页配置策略。"}
        footer={
          <>
            <Button variant="outline" onClick={() => setCreator(null)} disabled={update.isPending}>取消</Button>
            <Button disabled={update.isPending || !normalizedNewPrefix || duplicate} onClick={create}>
              <Plus />
              创建并编辑
            </Button>
          </>
        }
      >
        {creator && (
          <div className="space-y-5">
            <Field
              label="路由前缀"
              htmlFor="route-create-name"
              required
              description={
                duplicate
                  ? "这个前缀已存在"
                  : creator.name.trim() && !normalizedNewPrefix
                    ? "名字只允许小写字母、数字、点、下划线或短横线，不超过 64 个字符"
                    : normalizedNewPrefix
                      ? `完整前缀 ${normalizedNewPrefix}，请求地址 ${normalizedNewPrefix}/v1/messages`
                      : "固定前缀用于避开内置路由，只能填后缀名，例如 team-a"
              }
              error={duplicate ? "前缀重复" : undefined}
            >
              <div className="flex items-stretch overflow-hidden rounded-md border border-input bg-card focus-within:ring-2 focus-within:ring-ring">
                <div className="flex items-center border-r border-input bg-muted/30 px-3 font-mono text-sm text-muted-foreground">{CUSTOM_ROUTE_PREFIX}</div>
                <Input
                  id="route-create-name"
                  className="min-w-0 border-0 shadow-none focus-visible:ring-0"
                  placeholder="team-a"
                  autoFocus
                  value={creator.name}
                  onChange={(e) => setCreator({ ...creator, name: e.target.value })}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void create();
                  }}
                />
              </div>
            </Field>
            <Field label="名称" htmlFor="route-create-title" description="显示在列表与用量统计里；留空用前缀">
              <Input id="route-create-title" value={creator.routeName} onChange={(e) => setCreator({ ...creator, routeName: e.target.value })} />
            </Field>
          </div>
        )}
      </ModalShell>
    </PageContainer>
  );
}
