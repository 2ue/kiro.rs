import {
  clearAccountAutoDisabled,
  clearAccountCooldown,
  createAccount,
  deleteAccount,
  discoverAccountSupportedModels,
  discoverStoredAccountSupportedModels,
  getAccountsStatus,
  setAccountEnabled,
  updateAccount,
} from "@/api/accounts";
import {
  EmptyState,
  ErrorState,
  LoadingState,
  PageContainer,
  PageHeader,
  SectionCard,
  StatCard,
  StatGrid,
  Toolbar,
  ToolbarActions,
  ToolbarSearch,
  useConfirm,
} from "@/components/patterns";
import { ProgressRing } from "@/components/charts";
import {
  Badge,
  Button,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui";
import { EffectiveSheet } from "@/features/routes/effective-sheet";
import { useAccountEffective, useAccounts, usePromoteAccountModels } from "@/hooks/use-accounts";
import { useRuntimeConfig } from "@/hooks/use-runtime-config";
import { defaultAccountsConfig } from "@/lib/runtime-config-defaults";
import { extractErrorMessage } from "@/lib/utils";
import type { Account, AccountQualityView, AccountStatus, CreateAccountRequest } from "@/types/api";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Eye, FlaskConical, Pencil, Plus, Power, RefreshCw, RotateCcw, Sparkles, Trash2, Users } from "lucide-react";
import { useMemo, useState } from "react";
import { toast } from "sonner";
import { AccountFormModal } from "./account-form-modal";
import { AccountTestModal } from "./account-test-modal";
import {
  accountFormFromAccount,
  accountHeaderProfileSummary,
  accountModelMappingSummary,
  accountRouteSummary,
  accountSupportedModelsSummary,
  accountUsageSummary,
  authLabel,
  defaultAccountForm,
  parseHeaderOverridesText,
  parseModelMappingRules,
  parseSupportedModelsText,
  streamRetrySummary,
  type AccountFormDraft,
} from "./account-utils";

type StatusFilter = "all" | "dispatchable" | "cooldown" | "disabled" | "auto_disabled";

function qualitySummary(quality?: AccountQualityView): string | null {
  if (!quality) return null;
  const parts = [
    `质量错误率 ${Math.round(quality.recentErrorRate * 100)}%`,
    quality.ttftEwmaMs != null ? `首字 ${Math.round(quality.ttftEwmaMs)}ms` : null,
    quality.latencyEwmaMs != null ? `总耗时 ${Math.round(quality.latencyEwmaMs)}ms` : null,
    `样本 ${quality.sampleCount}`,
  ].filter(Boolean) as string[];
  if (!quality.scoringActive) parts.push("冷启动");
  if (quality.inProbation) parts.push(`避让 ${quality.probationRemainingSecs}s`);
  else if (quality.recoveryProgress < 1) parts.push(`恢复 ${Math.round(quality.recoveryProgress * 100)}%`);
  return parts.join(" · ");
}

function statusOf(account: Account, runtime?: AccountStatus): StatusFilter {
  if (!account.enabled) return "disabled";
  if (account.autoDisabled) return "auto_disabled";
  if ((runtime?.cooldownRemainingSecs ?? 0) > 0) return "cooldown";
  return "dispatchable";
}

export function AccountsPage() {
  const query = useAccounts();
  const runtimeConfig = useRuntimeConfig();
  const client = useQueryClient();
  const confirm = useConfirm();
  const promote = usePromoteAccountModels();
  const statuses = useQuery({ queryKey: ["accounts-status"], queryFn: getAccountsStatus, refetchInterval: 5000 });
  const [search, setSearch] = useState("");
  const [tag, setTag] = useState("_all");
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  const [editor, setEditor] = useState<{ account: Account | null; draft: AccountFormDraft } | null>(null);
  const [testing, setTesting] = useState<Account | null>(null);
  const [effectiveId, setEffectiveId] = useState<number | null>(null);
  const effective = useAccountEffective(effectiveId);

  const invalidate = () => {
    for (const key of ["accounts", "accounts-status", "account-effective", "usage-records"]) client.invalidateQueries({ queryKey: [key] });
  };
  const mutation = useMutation({
    mutationFn: async (action: () => Promise<unknown>) => action(),
    onSuccess: invalidate,
    onError: (error) => toast.error(extractErrorMessage(error)),
  });
  const run = (action: () => Promise<unknown>, success: string) =>
    mutation.mutate(async () => {
      await action();
      toast.success(success);
    });

  const statusMap = useMemo(() => {
    const map = new Map<number, AccountStatus>();
    statuses.data?.accounts.forEach((item) => map.set(item.account.id, item));
    return map;
  }, [statuses.data]);

  if (query.isLoading) return <LoadingState text="加载账号..." />;
  if (query.error) return <ErrorState title="读取账号失败" message={extractErrorMessage(query.error)} action={<Button variant="outline" onClick={() => query.refetch()}>重试</Button>} />;

  const accounts = query.data?.accounts ?? [];
  const config = { ...defaultAccountsConfig(), ...runtimeConfig.data?.accounts };
  const tags = [...new Set(accounts.flatMap((a) => a.tags ?? []))].sort();
  const keyword = search.trim().toLowerCase();
  const visible = accounts.filter((a) => {
    if (keyword && !`${a.name} ${a.id} ${a.baseUrl} ${(a.tags ?? []).join(" ")}`.toLowerCase().includes(keyword)) return false;
    if (tag !== "_all" && !a.tags?.includes(tag)) return false;
    if (statusFilter !== "all" && statusOf(a, statusMap.get(a.id)) !== statusFilter) return false;
    return true;
  });

  const summary = statuses.data?.summary;
  const total = accounts.length;
  const dispatchable = summary?.dispatchable ?? [...statusMap.values()].filter((s) => s.dispatchable).length;
  const inFlight = summary?.inFlightTotal ?? [...statusMap.values()].reduce((sum, s) => sum + s.inFlight, 0);
  const capacity = accounts.reduce((sum, a) => sum + a.maxConcurrentRequests, 0);
  const concurrencyPct = capacity > 0 ? Math.round((inFlight / capacity) * 100) : 0;
  const inCooldown = summary?.inCooldown ?? [...statusMap.values()].filter((s) => s.cooldownRemainingSecs > 0).length;
  const autoDisabled = summary?.autoDisabled ?? accounts.filter((a) => a.autoDisabled).length;
  const currentPathCount = accounts.filter((a) => a.usageProjectionMode === "current_path_policy").length;

  const save = () => {
    if (!editor) return;
    mutation.mutate(async () => {
      const { tagsText, modelMappingRulesText, supportedModelsText, headerOverridesText, streamResponseMode, proxyUrl, ...form } = editor.draft;
      const request: CreateAccountRequest = {
        ...form,
        tags: tagsText.split(/[,，]/).map((t) => t.trim()).filter(Boolean),
        proxyUrl: proxyUrl || null,
        headerOverrides: parseHeaderOverridesText(headerOverridesText),
        modelMappingRules: parseModelMappingRules(modelMappingRulesText),
        supportedModels: parseSupportedModelsText(supportedModelsText),
        streamResponseMode: streamResponseMode === "inherit" ? null : streamResponseMode,
      };
      if (editor.account) {
        const { apiKey, proxyUrl: nextProxy, ...rest } = request;
        await updateAccount(editor.account.id, {
          ...rest,
          ...(proxyUrl !== (editor.account.proxyUrl ?? "") ? { proxyUrl: nextProxy } : {}),
          ...(apiKey ? { apiKey } : {}),
        });
        toast.success("账号已更新");
      } else {
        await createAccount(request);
        toast.success("账号已添加");
      }
      setEditor(null);
    });
  };

  return (
    <PageContainer>
      <PageHeader
        actions={
          <Button onClick={() => setEditor({ account: null, draft: defaultAccountForm() })}>
            <Plus />
            添加账号
          </Button>
        }
      />

      <StatGrid min="11rem">
        <StatCard
          title="账号"
          value={total}
          desc={`启用 ${summary?.enabled ?? accounts.filter((a) => a.enabled).length} 个`}
          tone="info"
          icon={<ProgressRing value={concurrencyPct} size={40} strokeWidth={4} color="hsl(var(--info))" label={`${concurrencyPct}%`} />}
        />
        <StatCard title="可调度" value={dispatchable} desc={dispatchable > 0 ? "当前能接请求的账号" : "没有账号可接请求"} tone={dispatchable > 0 ? "success" : "warning"} icon={<Users />} />
        <StatCard title="并发" value={`${inFlight}/${capacity || 0}`} desc={summary?.queuedRequests != null ? `排队 ${summary.queuedRequests}` : "在飞 / 总容量"} tone="default" />
        <StatCard title="冷却中" value={inCooldown} desc="冷却结束后自动恢复" tone={inCooldown > 0 ? "warning" : "default"} />
        <StatCard title="自动禁用" value={autoDisabled} desc={autoDisabled > 0 ? "需人工清除或等待到期" : "无"} tone={autoDisabled > 0 ? "error" : "default"} />
        <StatCard title="按路径整理 usage" value={`${currentPathCount} 个`} desc="其余透传上游 usage" />
      </StatGrid>

      <SectionCard
        title="账号列表"
        description="单个账号的配置只影响自身；全局调度、冷却、补偿策略在运行配置页统一保存。"
        actions={
          <Button variant="outline" size="sm" onClick={() => statuses.refetch()} disabled={statuses.isFetching}>
            <RefreshCw className={statuses.isFetching ? "animate-spin" : undefined} />
            刷新状态
          </Button>
        }
      >
        <Toolbar className="mb-4">
          <ToolbarSearch value={search} onChange={setSearch} placeholder="搜索名称、ID、地址或标签" />
          <ToolbarActions>
            <Select value={statusFilter} onValueChange={(v) => setStatusFilter(v as StatusFilter)}>
              <SelectTrigger size="sm" className="w-40" aria-label="按状态筛选"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="all">全部状态</SelectItem>
                <SelectItem value="dispatchable">可调度</SelectItem>
                <SelectItem value="cooldown">冷却中</SelectItem>
                <SelectItem value="auto_disabled">自动禁用</SelectItem>
                <SelectItem value="disabled">已停用</SelectItem>
              </SelectContent>
            </Select>
            {tags.length > 0 && (
              <Select value={tag} onValueChange={setTag}>
                <SelectTrigger size="sm" className="w-40" aria-label="按标签筛选"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="_all">全部标签</SelectItem>
                  {tags.map((t) => <SelectItem value={t} key={t}>{t}</SelectItem>)}
                </SelectContent>
              </Select>
            )}
            <span className="text-xs text-muted-foreground">{visible.length} / {total}</span>
          </ToolbarActions>
        </Toolbar>

        {accounts.length === 0 ? (
          <EmptyState icon={<Users />} title="还没有账号" description="添加第一个账号后，请求才会有地方可去。" action={<Button onClick={() => setEditor({ account: null, draft: defaultAccountForm() })}><Plus />添加账号</Button>} />
        ) : visible.length === 0 ? (
          <EmptyState title="没有匹配的账号" description="换个关键字或筛选条件。" />
        ) : (
          <div className="space-y-3">
            {visible.map((account) => {
              const runtime = statusMap.get(account.id);
              const flying = runtime?.inFlight ?? 0;
              const cap = account.maxConcurrentRequests;
              const usePct = cap > 0 ? Math.round((flying / cap) * 100) : 0;
              const healthy = account.enabled && !account.autoDisabled;
              const quality = qualitySummary(runtime?.quality);
              return (
                <div key={account.id} className="rounded-lg border border-border/70 bg-card p-4 shadow-sm transition hover:shadow-md">
                  <div className="flex flex-col gap-3 lg:flex-row lg:items-start lg:justify-between">
                    <div className="flex min-w-0 items-start gap-3">
                      <ProgressRing
                        value={usePct}
                        size={44}
                        strokeWidth={4}
                        color={healthy ? "hsl(var(--success))" : "hsl(var(--destructive))"}
                        label={`${usePct}%`}
                        className="mt-0.5 shrink-0"
                      />
                      <div className="min-w-0 space-y-1.5">
                        <div className="flex flex-wrap items-center gap-2">
                          <span className="font-semibold">#{account.id} {account.name}</span>
                          <Badge tone={account.enabled ? "success" : "neutral"}>{account.enabled ? "启用" : "停用"}</Badge>
                          {account.autoDisabled && <Badge tone="error">自动禁用</Badge>}
                          <Badge tone={runtime?.dispatchable ? "info" : "neutral"}>{runtime?.dispatchable ? "可调度" : runtime?.skippedReason || "不可调度"}</Badge>
                          {(account.tags ?? []).map((t) => <Badge key={t} size="xs" tone="secondary">{t}</Badge>)}
                        </div>
                        <div className="text-sm text-muted-foreground">
                          {account.baseUrl} · {account.maskedApiKey || "未显示 Key"} · 并发 {flying}/{cap} · 优先级 {account.priority}
                          {account.costMultiplier !== 1 && ` · 成本 ×${account.costMultiplier}`}
                          {account.proxyUrl && " · 走代理"}
                        </div>
                        <div className="text-xs text-muted-foreground">
                          {accountUsageSummary(account, config)} · {streamRetrySummary(account, config)} · {accountRouteSummary(account)} · {accountHeaderProfileSummary(account)} · 认证：{authLabel(account.authType)} · 模型：{accountModelMappingSummary(account)} · {accountSupportedModelsSummary(account)}
                          {runtime?.cooldownRemainingSecs ? ` · 冷却 ${runtime.cooldownRemainingSecs}s${runtime.cooldownReason ? `（${runtime.cooldownReason}）` : ""}` : ""}
                          {runtime?.transientFailureStreak ? ` · 失败窗口 ${runtime.transientFailureStreak} 次/${runtime.transientFailureTtlSecs}s` : ""}
                        </div>
                        {quality && <div className="text-xs text-muted-foreground">{quality}</div>}
                        {account.autoDisabledLastError && <div className="text-xs text-destructive">{account.autoDisabledLastError}</div>}
                      </div>
                    </div>
                    <div className="flex flex-wrap gap-1.5 lg:shrink-0 lg:justify-end">
                      <Button variant="ghost" size="xs" onClick={() => setEditor({ account, draft: accountFormFromAccount(account) })}><Pencil className="h-3.5 w-3.5" />编辑</Button>
                      <Button variant="ghost" size="xs" onClick={() => setTesting(account)}><FlaskConical className="h-3.5 w-3.5" />测试</Button>
                      <Button variant="ghost" size="xs" onClick={() => setEffectiveId(account.id)}><Eye className="h-3.5 w-3.5" />生效参数</Button>
                      <Button variant="ghost" size="xs" disabled={mutation.isPending} onClick={() => run(() => setAccountEnabled(account.id, !account.enabled), account.enabled ? "已停用" : "已启用")}>
                        <Power className="h-3.5 w-3.5" />{account.enabled ? "停用" : "启用"}
                      </Button>
                      <Button
                        variant="ghost"
                        size="xs"
                        disabled={mutation.isPending}
                        onClick={() =>
                          run(async () => {
                            const found = await discoverStoredAccountSupportedModels(account.id);
                            await updateAccount(account.id, { supportedModels: found.supportedModels });
                          }, "已刷新支持的模型")
                        }
                      >
                        <Sparkles className="h-3.5 w-3.5" />发现模型
                      </Button>
                      <Button
                        variant="ghost"
                        size="xs"
                        disabled={promote.isPending}
                        onClick={async () => {
                          try {
                            const value = await promote.mutateAsync(account.id);
                            toast.success(`已加入目录 ${value.count} 个模型`);
                          } catch (error) {
                            toast.error(extractErrorMessage(error));
                          }
                        }}
                      >
                        加入目录
                      </Button>
                      {(runtime?.cooldownRemainingSecs ?? 0) > 0 && (
                        <Button variant="ghost" size="xs" disabled={mutation.isPending} onClick={() => run(() => clearAccountCooldown(account.id), "冷却状态已清除")}>
                          <RotateCcw className="h-3.5 w-3.5" />清除冷却
                        </Button>
                      )}
                      {account.autoDisabled && (
                        <Button variant="ghost" size="xs" disabled={mutation.isPending} onClick={() => run(() => clearAccountAutoDisabled(account.id), "自动禁用状态已清除")}>
                          <RotateCcw className="h-3.5 w-3.5" />清除禁用
                        </Button>
                      )}
                      <Button
                        variant="ghost"
                        size="xs"
                        className="text-destructive hover:bg-destructive/10 hover:text-destructive"
                        disabled={mutation.isPending}
                        onClick={async () => {
                          const ok = await confirm({ title: "删除账号", message: `删除账号「${account.name}」？此操作无法撤销。`, confirmText: "删除", tone: "danger" });
                          if (ok) run(() => deleteAccount(account.id), "账号已删除");
                        }}
                      >
                        <Trash2 className="h-3.5 w-3.5" />删除
                      </Button>
                    </div>
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </SectionCard>

      {editor && (
        <AccountFormModal
          mode={editor.account ? "edit" : "create"}
          account={editor.account}
          open
          draft={editor.draft}
          saving={mutation.isPending}
          onDraftChange={(v) => setEditor({ ...editor, draft: typeof v === "function" ? v(editor.draft) : v })}
          onClose={() => setEditor(null)}
          onSubmit={save}
          onDiscoverSupportedModels={async () => {
            try {
              const result =
                editor.account && !editor.draft.apiKey
                  ? await discoverStoredAccountSupportedModels(editor.account.id)
                  : await discoverAccountSupportedModels({ baseUrl: editor.draft.baseUrl, apiKey: editor.draft.apiKey || undefined, authType: editor.draft.authType });
              return result.supportedModels;
            } catch (error) {
              toast.error(extractErrorMessage(error));
              throw error;
            }
          }}
        />
      )}
      <AccountTestModal account={testing} open={testing !== null} onClose={() => setTesting(null)} onDone={invalidate} />
      <EffectiveSheet
        open={effectiveId !== null}
        onOpenChange={(v) => { if (!v) setEffectiveId(null); }}
        title={`账号 #${effectiveId} 生效参数`}
        value={effective.data?.parameters}
        sources={effective.data?.sources}
        loading={effective.isLoading}
        error={effective.error}
      />
    </PageContainer>
  );
}
