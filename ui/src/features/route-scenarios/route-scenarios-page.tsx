import { useAccounts } from "@/hooks/use-accounts";
import { RouteSelector } from "./route-selector";
import {
  applyRouteScenario,
  createRouteScenario,
  deleteRouteScenario,
  getCurrentRouteScenarioContent,
  getRouteScenarios,
  previewRouteScenario,
  updateRouteScenario,
} from "@/api/route-scenarios";
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
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  Textarea,
} from "@/components/ui";
import { formatDate } from "@/lib/format";
import { extractErrorMessage } from "@/lib/utils";
import type {
  RouteScenario,
  RouteScenarioContent,
  RouteScenarioPreview,
} from "@/types/api";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Pencil, Play, Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { policySummary } from "./account-rules";
export function RouteScenariosPage() {
  const accounts = useAccounts();
  const query = useQuery({
      queryKey: ["route-scenarios"],
      queryFn: getRouteScenarios,
    }),
    client = useQueryClient(),
    confirm = useConfirm();
  const [editor, setEditor] = useState<{
      id?: number;
      name: string;
      description: string;
      content: RouteScenarioContent;
    } | null>(null),
    [preview, setPreview] = useState<{
      scenario: RouteScenario;
      value: RouteScenarioPreview;
    } | null>(null);
  const mutation = useMutation({
    mutationFn: async (action: () => Promise<unknown>) => action(),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["route-scenarios"] });
      client.invalidateQueries({ queryKey: ["routes"] });
      client.invalidateQueries({ queryKey: ["accounts"] });
    },
    onError: (error) => toast.error(extractErrorMessage(error)),
  });
  if (query.isLoading) return <LoadingState />;
  if (query.error)
    return (
      <ErrorState
        title="读取路由场景失败"
        action={
          <Button variant="outline" onClick={() => query.refetch()}>
            重试
          </Button>
        }
      />
    );
  const scenarios = query.data?.scenarios ?? [];
  return (
    <PageContainer>
      <PageHeader
        actions={
          <Button
            disabled={mutation.isPending}
            onClick={() =>
              mutation.mutate(async () => {
                const content = await getCurrentRouteScenarioContent();
                setEditor({ name: "", description: "", content });
              })
            }
          >
            <Plus />
            保存当前快照
          </Button>
        }
      />
      {scenarios.length === 0 ? (
        <EmptyState
          title="暂无路由场景"
          description="把当前路由与账号规则保存为命名快照。"
        />
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>名称</TableHead>
              <TableHead>账号规则快照</TableHead>
              <TableHead>更新时间</TableHead>
              <TableHead>操作</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {scenarios.map((scenario) => (
              <TableRow key={scenario.id}>
                <TableCell>{scenario.name}</TableCell>
                <TableCell>
                  {scenario.content.externalAccounts.length} 个账号，
                  {scenario.content.routes.length} 条路由
                </TableCell>
                <TableCell>{formatDate(scenario.updatedAt)}</TableCell>
                <TableCell>
                  <div className="flex gap-2">
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() =>
                        setEditor({
                          id: scenario.id,
                          name: scenario.name,
                          description: scenario.description ?? "",
                          content: structuredClone(scenario.content),
                        })
                      }
                    >
                      <Pencil />
                      编辑
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      disabled={mutation.isPending}
                      onClick={() =>
                        mutation.mutate(async () =>
                          setPreview({
                            scenario,
                            value: await previewRouteScenario(scenario.id),
                          }),
                        )
                      }
                    >
                      <Play />
                      预览应用
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={async () => {
                        if (
                          await confirm({
                            title: "删除场景",
                            message: scenario.name,
                            confirmText: "删除",
                            tone: "danger",
                          })
                        )
                          mutation.mutate(() =>
                            deleteRouteScenario(scenario.id),
                          );
                      }}
                    >
                      <Trash2 />
                      删除
                    </Button>
                  </div>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      <ModalShell
        open={editor !== null}
        onClose={() => setEditor(null)}
        title="编辑路由场景"
        footer={
          <Button
            disabled={mutation.isPending}
            onClick={() => {
              if (editor)
                mutation.mutate(async () => {
                  if (!editor.name.trim()) throw new Error("请输入场景名称");
                  const request = {
                    name: editor.name,
                    description: editor.description,
                    content: editor.content,
                  };
                  if (editor.id) await updateRouteScenario(editor.id, request);
                  else await createRouteScenario(request);
                  setEditor(null);
                  toast.success("场景已保存");
                });
            }}
          >
            保存
          </Button>
        }
      >
        <div className="space-y-4">
          {editor && (
            <>
              <Input
                placeholder="场景名称"
                value={editor.name}
                onChange={(e) => setEditor({ ...editor, name: e.target.value })}
              />
              <Textarea
                placeholder="描述"
                value={editor.description}
                onChange={(e) =>
                  setEditor({ ...editor, description: e.target.value })
                }
              />
              <p className="text-sm text-muted-foreground">
                快照包含 {editor.content.routes.length} 条路由。账号规则：
              </p>
              {(accounts.data?.pools ?? []).map((account) => {
                const rule = editor.content.externalAccounts.find(
                  (a) => a.id === account.id,
                ) ?? {
                  id: account.id,
                  routeMode: "allow_all" as const,
                  routeRules: [],
                };
                const setRule = (next: typeof rule) =>
                  setEditor({
                    ...editor,
                    content: {
                      ...editor.content,
                      externalAccounts: [
                        ...editor.content.externalAccounts.filter(
                          (a) => a.id !== account.id,
                        ),
                        next,
                      ],
                    },
                  });
                return (
                  <div key={account.id} className="space-y-3 border-b pb-4">
                    <div>
                      {account.name} #{account.id} · {policySummary(rule)}
                    </div>
                    <Select
                      value={rule.routeMode}
                      onValueChange={(mode) =>
                        setRule({
                          ...rule,
                          routeMode: mode as typeof rule.routeMode,
                        })
                      }
                    >
                      <SelectTrigger>
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="allow_all">不限制</SelectItem>
                        <SelectItem value="allow_list">只允许列表</SelectItem>
                        <SelectItem value="deny_list">拒绝列表</SelectItem>
                      </SelectContent>
                    </Select>
                    <RouteSelector
                      routes={editor.content.routes.map((r) => r.prefix || "/")}
                      value={rule.routeRules}
                      disabled={rule.routeMode === "allow_all"}
                      onChange={(routeRules) =>
                        setRule({ ...rule, routeRules })
                      }
                    />
                  </div>
                );
              })}
            </>
          )}
        </div>
      </ModalShell>
      <ModalShell
        open={preview !== null}
        onClose={() => setPreview(null)}
        title="应用差异预览"
        footer={
          <Button
            disabled={mutation.isPending}
            onClick={() => {
              if (preview)
                mutation.mutate(async () => {
                  const result = await applyRouteScenario(preview.scenario.id);
                  if (result.failed)
                    throw new Error(`${result.failed} 条应用失败`);
                  setPreview(null);
                  toast.success("场景已应用");
                });
            }}
          >
            确认应用
          </Button>
        }
      >
        <div className="space-y-4">
          {preview?.value.changes.map((change, i) => (
            <div key={i} className="border-b pb-3 text-sm">
              <div className="font-medium">{change.label}</div>
              <div>
                {change.before} → {change.after}
              </div>
            </div>
          ))}
          <p className="text-sm">无变化：{preview?.value.unchanged}</p>
        </div>
      </ModalShell>
    </PageContainer>
  );
}
