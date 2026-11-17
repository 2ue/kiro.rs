import { useRuntimeConfig } from "@/hooks/use-runtime-config";
import { useAccounts } from "@/hooks/use-accounts";
import {
  EmptyState,
  ErrorState,
  LoadingState,
  ModalShell,
  PageContainer,
  PageHeader,
  useConfirm,
} from "@/components/patterns";
import {
  Button,
  Input,
  Switch,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui";
import { useRoutes, useUpdateRoutes } from "@/hooks/use-routes";
import { extractErrorMessage } from "@/lib/utils";
import type { RouteConfig } from "@/types/routes";
import { Copy, Pencil, Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { toast } from "sonner";
import {
  displayRoutePrefix,
  isBuiltinRoute,
  normalizeRoutePrefix,
  routeEditorPath,
  sortRoutes,
} from "./route-utils";
export function RoutesPage() {
  const runtime = useRuntimeConfig(),
    accounts = useAccounts();
  const query = useRoutes(),
    update = useUpdateRoutes(),
    navigate = useNavigate(),
    confirm = useConfirm();
  const [copy, setCopy] = useState<RouteConfig | null>(null),
    [prefix, setPrefix] = useState(""),
    [name, setName] = useState("");
  const save = async (routes: RouteConfig[]) => {
    try {
      await update.mutateAsync(routes);
      toast.success("路由已保存");
    } catch (error) {
      toast.error(extractErrorMessage(error));
    }
  };
  if (query.isLoading) return <LoadingState />;
  if (query.error)
    return (
      <ErrorState
        title="加载路由失败"
        action={
          <Button variant="outline" onClick={() => query.refetch()}>
            重试
          </Button>
        }
      />
    );
  const routes = query.data ?? [];
  const create = async () => {
    const normalized = normalizeRoutePrefix(prefix);
    if (
      !normalized ||
      routes.some((r) => normalizeRoutePrefix(r.prefix) === normalized)
    ) {
      toast.error("请输入不重复的路由前缀");
      return;
    }
    const next: RouteConfig = copy
      ? { ...structuredClone(copy), prefix: normalized, name }
      : {
          prefix: normalized,
          name,
          enabled: true,
          cachePolicy: {},
          features: {},
          promptInjection: {},
          accountRule: { mode: "allow_all", rules: [] },
          modelRule: { mode: "allow_all", rules: [] },
        };
    try {
      await update.mutateAsync([...routes, next]);
      setCopy(null);
      setPrefix("");
      navigate(routeEditorPath(normalized));
    } catch (error) {
      toast.error(extractErrorMessage(error));
    }
  };
  return (
    <PageContainer>
      <PageHeader
        actions={
          <Button
            onClick={() => {
              setCopy({
                prefix: "",
                name: "",
                enabled: true,
                cachePolicy: {},
                features: {},
                promptInjection: {},
                accountRule: { mode: "allow_all", rules: [] },
                modelRule: { mode: "allow_all", rules: [] },
              });
              setPrefix("");
              setName("");
            }}
          >
            <Plus />
            新增路由
          </Button>
        }
      />
      {routes.length === 0 ? (
        <EmptyState title="暂无路由" />
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              {[
                "前缀",
                "名称",
                "启用",
                "缓存策略",
                "注入项数",
                "开启功能数",
                "允许账号数",
                "操作",
              ].map((t) => (
                <TableHead key={t}>{t}</TableHead>
              ))}
            </TableRow>
          </TableHeader>
          <TableBody>
            {sortRoutes(routes).map((r) => (
              <TableRow key={r.prefix}>
                <TableCell className="font-mono">
                  {displayRoutePrefix(r.prefix)}
                </TableCell>
                <TableCell>{r.name || "未命名"}</TableCell>
                <TableCell>
                  <Switch
                    disabled={update.isPending}
                    checked={r.enabled}
                    onCheckedChange={(enabled) =>
                      save(
                        routes.map((item) =>
                          item.prefix === r.prefix
                            ? { ...item, enabled }
                            : item,
                        ),
                      )
                    }
                  />
                </TableCell>
                <TableCell>
                  {r.cachePolicy.cacheType ?? "current_high_cache"}
                </TableCell>
                <TableCell>
                  {
                    Object.entries(
                      runtime.data?.promptInjection.defaults ?? {},
                    ).filter(
                      ([id, value]) =>
                        r.promptInjection[id]?.enabled ?? value.enabled,
                    ).length
                  }
                </TableCell>
                <TableCell>
                  {
                    Object.values({
                      ...runtime.data?.routeFeatures.defaults,
                      ...r.features,
                    }).filter(
                      (v) =>
                        v !== undefined &&
                        v !== null &&
                        v !== false &&
                        v !== "off" &&
                        !(
                          typeof v === "object" &&
                          "enabled" in v &&
                          !v.enabled
                        ),
                    ).length
                  }
                </TableCell>
                <TableCell>
                  {r.accountRule.mode === "allow_all"
                    ? "全部"
                    : (accounts.data?.accounts ?? []).filter((a) =>
                        r.accountRule.mode === "allow_list"
                          ? r.accountRule.rules.includes("*") ||
                            r.accountRule.rules.includes(String(a.id))
                          : !r.accountRule.rules.includes("*") &&
                            !r.accountRule.rules.includes(String(a.id)),
                      ).length}
                </TableCell>
                <TableCell>
                  <div className="flex gap-1">
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => navigate(routeEditorPath(r.prefix))}
                    >
                      <Pencil />
                      编辑
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => {
                        setCopy(r);
                        setPrefix(`${r.prefix || "/root"}-copy`);
                        setName(`${r.name} 副本`);
                      }}
                    >
                      <Copy />
                      复制
                    </Button>
                    <span
                      title={
                        isBuiltinRoute(r.prefix)
                          ? "内置路由不可删除"
                          : undefined
                      }
                    >
                      <Button
                        variant="ghost"
                        size="sm"
                        disabled={isBuiltinRoute(r.prefix) || update.isPending}
                        onClick={async () => {
                          if (
                            await confirm({
                              title: "删除路由",
                              message: `删除 ${displayRoutePrefix(r.prefix)} 后该入口将不可用。`,
                              confirmText: "删除",
                              tone: "danger",
                            })
                          )
                            save(
                              routes.filter((item) => item.prefix !== r.prefix),
                            );
                        }}
                      >
                        <Trash2 />
                        删除
                      </Button>
                    </span>
                  </div>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      <ModalShell
        open={copy !== null}
        onClose={() => setCopy(null)}
        title="新增路由"
        footer={
          <Button disabled={update.isPending} onClick={create}>
            创建并编辑
          </Button>
        }
      >
        <div className="space-y-4">
          <Input
            aria-label="路由前缀"
            placeholder="/team"
            value={prefix}
            onChange={(e) => setPrefix(e.target.value)}
          />
          <Input
            aria-label="路由名称"
            placeholder="路由名称"
            value={name}
            onChange={(e) => setName(e.target.value)}
          />
        </div>
      </ModalShell>
    </PageContainer>
  );
}
