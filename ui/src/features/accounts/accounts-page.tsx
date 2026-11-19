import {
  clearAccountAutoDisabled,
  clearAccountCooldown,
  copyAccount,
  createAccount,
  deleteAccount,
  discoverAccountSupportedModels,
  discoverStoredAccountSupportedModels,
  setAccountEnabled,
  testAccount,
  updateAccount,
} from "@/api/accounts";
import {
  EmptyState,
  ErrorState,
  LoadingState,
  PAGE_SIZE_OPTIONS,
  PageContainer,
  PageHeader,
  Pagination,
  SectionCard,
  StatCard,
  StatGrid,
  Toolbar,
  useConfirm,
} from "@/components/patterns";
import { ProgressRing } from "@/components/charts";
import {
  Badge,
  Button,
  Checkbox,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui";
import { EffectiveSheet } from "@/features/routes/effective-sheet";
import { useAccountEffective, useAccounts, useAccountsStatus, usePromoteAccountModels } from "@/hooks/use-accounts";
import { useModelCapabilities } from "@/hooks/use-models";
import { useRuntimeConfig } from "@/hooks/use-runtime-config";
import { defaultAccountsConfig } from "@/lib/runtime-config-defaults";
import { buildAccountTestModelOptions, defaultAccountTestModelForOptions, DEFAULT_TEST_PROMPT } from "@/lib/test-models";
import { cn, extractErrorMessage } from "@/lib/utils";
import type { Account, AccountStatus } from "@/types/api";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { CheckCircle2, Download, Filter, ListChecks, Plus, Power, RefreshCw, RotateCcw, Trash2, Upload, Users, X } from "lucide-react";
import { useEffect, useMemo, useState, type ReactNode } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import { toast } from "sonner";
import { accountRiskLink } from "@/features/account-risk/risk-url-params";
import { buildBatchUpdate, type BatchEditDraft } from "./account-batch";
import { AccountBatchEditModal } from "./account-batch-edit-modal";
import { BatchRunModal, useBatchRun, type BatchTask } from "./account-batch-run";
import { AccountCard } from "./account-card";
import { AccountExportModal } from "./account-export-dialog";
import { AccountFormModal } from "./account-form-modal";
import { AccountImportModal } from "./account-import-dialog";
import {
  DEFAULT_ACCOUNT_FILTERS,
  MODEL_FILTER_ALL,
  MODEL_FILTER_UNRESTRICTED,
  RUNTIME_FLAG_OPTIONS,
  SORT_OPTIONS,
  TAG_FILTER_ALL,
  accountFacetCounts,
  filterAccounts,
  hasActiveFilters,
  modelFilterOptions,
  pageSelectionState,
  paginate,
  pruneSelection,
  sortAccounts,
  toggleSelectOne,
  toggleSelectPage,
  type AccountListFilters,
  type AccountSortBy,
  type DispatchFilter,
  type EnabledFilter,
  type ProxyFilter,
  type RuntimeFlag,
  type SortOrder,
} from "./account-list";
import { AccountQuickEditModal, type QuickEditField } from "./account-quick-edit";
import { AccountTestModal } from "./account-test-modal";
import { RUNTIME_LINKS } from "./account-usage";
import {
  accountFormFromAccount,
  buildAccountRequest,
  defaultAccountForm,
  diffAccountRequest,
  hasMaskedProxyCredentials,
  type AccountFormDraft,
} from "./account-utils";

/** 用户在确认框里点了取消：不是错误，静默结束这次操作。 */
class SilentCancel extends Error {
  constructor() {
    super("cancelled");
  }
}

type EditorState = {
  mode: "create" | "edit";
  account: Account | null;
  draft: AccountFormDraft;
  initialDraft: AccountFormDraft;
  /** 由“复制”打开时的来源账号。 */
  copiedFrom: { id: number; name: string } | null;
};

const RUNTIME_LINK_CLASS = "mx-0.5 text-primary underline-offset-4 hover:underline";

/** 分段选择（与 main 账号页的“启用状态 / 账号状况”一致）：每项带数量。 */
function Segmented<T extends string>({ label, value, items, onChange }: {
  label: string;
  value: T;
  items: Array<[T, string, number | undefined]>;
  onChange: (value: T) => void;
}) {
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
      <span className="w-14 shrink-0 text-xs text-muted-foreground">{label}</span>
      <div role="radiogroup" aria-label={label} className="inline-flex max-w-full flex-wrap gap-0.5 rounded-lg bg-muted p-0.5">
        {items.map(([item, text, count]) => (
          <button
            key={item}
            type="button"
            role="radio"
            aria-checked={value === item}
            onClick={() => onChange(item)}
            className={cn(
              "flex h-8 items-center gap-2 rounded-md px-3 text-xs font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/40",
              value === item ? "bg-card text-foreground shadow-sm" : "text-muted-foreground hover:text-foreground",
            )}
          >
            {text}
            <span className="tabular-nums text-muted-foreground">{count ?? "—"}</span>
          </button>
        ))}
      </div>
    </div>
  );
}

function FilterField({ label, children, className }: { label: string; children: ReactNode; className?: string }) {
  return (
    <label className={cn("block min-w-0", className)}>
      <span className="mb-1 block h-4 whitespace-nowrap text-[0.68rem] font-medium leading-4 text-muted-foreground">{label}</span>
      {children}
    </label>
  );
}

export function AccountsPage() {
  const query = useAccounts();
  const runtimeConfig = useRuntimeConfig();
  const statuses = useAccountsStatus();
  const modelCapabilities = useModelCapabilities();
  const client = useQueryClient();
  const confirm = useConfirm();
  const navigate = useNavigate();
  const promote = usePromoteAccountModels();
  const batch = useBatchRun();
  const [searchParams] = useSearchParams();
  const [filters, setFilters] = useState<AccountListFilters>(() => ({ ...DEFAULT_ACCOUNT_FILTERS, keyword: searchParams.get("q") ?? "" }));
  const [sortBy, setSortBy] = useState<AccountSortBy>("default");
  const [sortOrder, setSortOrder] = useState<SortOrder>("desc");
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(50);
  const [selected, setSelected] = useState<ReadonlySet<number>>(() => new Set());
  const [editor, setEditor] = useState<EditorState | null>(null);
  const [testing, setTesting] = useState<Account | null>(null);
  const [effectiveId, setEffectiveId] = useState<number | null>(null);
  const [quickEdit, setQuickEdit] = useState<{ account: Account; field: QuickEditField } | null>(null);
  const [batchEditOpen, setBatchEditOpen] = useState(false);
  /** 批量修改点了“应用”后正在取最新账号列表：弹窗保持打开，按钮转圈。 */
  const [batchEditSubmitting, setBatchEditSubmitting] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const [exportOpen, setExportOpen] = useState(false);
  const effective = useAccountEffective(effectiveId);

  const openEditor = (next: { mode: "create" | "edit"; account: Account | null; draft: AccountFormDraft; copiedFrom?: EditorState["copiedFrom"] }) =>
    setEditor({ ...next, copiedFrom: next.copiedFrom ?? null, initialDraft: structuredClone(next.draft) });

  const invalidate = () => {
    for (const key of ["accounts", "accounts-status", "account-effective", "usage-records"]) client.invalidateQueries({ queryKey: [key] });
  };
  const mutation = useMutation({
    mutationFn: async (action: () => Promise<unknown>) => action(),
    onSuccess: invalidate,
    onError: (error) => {
      if (error instanceof SilentCancel) return;
      toast.error(extractErrorMessage(error));
    },
  });
  const run = (action: () => Promise<unknown>, success: string) =>
    mutation.mutate(async () => {
      await action();
      toast.success(success);
    });

  const accounts = useMemo(() => query.data?.accounts ?? [], [query.data]);
  const statusMap = useMemo(() => {
    const map = new Map<number, AccountStatus>();
    statuses.data?.accounts.forEach((item) => map.set(item.account.id, item));
    return map;
  }, [statuses.data]);
  // 分段上的数字随其它筛选变化（对齐 main 的 credential_filter_facets）；顶部统计卡用全局计数，不跟筛选变。
  const facets = useMemo(() => accountFacetCounts(accounts, statusMap, filters), [accounts, statusMap, filters]);
  const globalFacets = useMemo(() => accountFacetCounts(accounts, statusMap), [accounts, statusMap]);
  const filtered = useMemo(
    () => sortAccounts(filterAccounts(accounts, statusMap, filters), statusMap, sortBy, sortOrder),
    [accounts, statusMap, filters, sortBy, sortOrder],
  );
  const paged = paginate(filtered, page, pageSize);
  const tags = useMemo(() => [...new Set(accounts.flatMap((a) => a.tags ?? []))].sort((a, b) => a.localeCompare(b)), [accounts]);
  const modelOptions = useMemo(
    () => modelFilterOptions(accounts, (modelCapabilities.data?.models ?? []).map((item) => item.model)),
    [accounts, modelCapabilities.data],
  );

  // 换筛选或排序时回到第一页并清空勾选（与 main 一致）；翻页保留勾选，方便跨页批量操作。
  const filterKey = JSON.stringify([filters, sortBy, sortOrder]);
  useEffect(() => {
    setPage(1);
    setSelected(new Set());
  }, [filterKey]);
  // 删除账号或列表刷新后，去掉已不存在的勾选。
  useEffect(() => {
    if (query.data) setSelected((prev) => pruneSelection(prev, query.data.accounts.map((a) => a.id)));
  }, [query.data]);
  useEffect(() => {
    if (paged.page !== page) setPage(paged.page);
  }, [paged.page, page]);

  if (query.isLoading) return <LoadingState text="加载账号..." />;
  if (query.error) return <ErrorState title="读取账号失败" message={extractErrorMessage(query.error)} action={<Button variant="outline" onClick={() => query.refetch()}>重试</Button>} />;

  const config = { ...defaultAccountsConfig(), ...runtimeConfig.data?.accounts };
  const setFilter = <K extends keyof AccountListFilters>(key: K, value: AccountListFilters[K]) => setFilters((prev) => ({ ...prev, [key]: value }));
  const toggleRuntime = (flag: RuntimeFlag) =>
    setFilters((prev) => ({ ...prev, runtime: prev.runtime.includes(flag) ? prev.runtime.filter((item) => item !== flag) : [...prev.runtime, flag] }));
  const filteredActive = hasActiveFilters(filters);

  const summary = statuses.data?.summary;
  const total = accounts.length;
  const dispatchable = summary?.dispatchable ?? globalFacets.dispatch.dispatchable;
  const inFlight = summary?.inFlightTotal ?? [...statusMap.values()].reduce((sum, s) => sum + s.inFlight, 0);
  const capacity = accounts.reduce((sum, a) => sum + a.maxConcurrentRequests, 0);
  const concurrencyPct = capacity > 0 ? Math.round((inFlight / capacity) * 100) : 0;
  const inCooldown = summary?.inCooldown ?? globalFacets.runtime.cooldown;
  const autoDisabled = summary?.autoDisabled ?? globalFacets.runtime.auto_disabled;
  const currentPathCount = accounts.filter((a) => a.usageProjectionMode === "current_path_policy").length;

  const pageIds = paged.items.map((a) => a.id);
  const pageState = pageSelectionState(pageIds, selected);
  const selectedAccounts = accounts.filter((a) => selected.has(a.id));
  const selectedEnabled = selectedAccounts.filter((a) => a.enabled);
  const selectedDisabled = selectedAccounts.filter((a) => !a.enabled);
  const selectedCooling = selectedAccounts.filter((a) => (statusMap.get(a.id)?.cooldownRemainingSecs ?? 0) > 0);
  const selectedAutoDisabled = selectedAccounts.filter((a) => a.autoDisabled);
  const allDisabled = accounts.filter((a) => !a.enabled);
  const batchBusy = Boolean(batch.state?.running);

  /** 改状态的批量操作结束后清空勾选（与 main 一致）；批量测试保留勾选，方便接着处理失败的账号。 */
  const startBatch = (title: string, list: Account[], task: BatchTask, options: { description?: string; keepSelection?: boolean } = {}) => {
    if (!list.length) {
      toast.error("没有符合条件的账号");
      return;
    }
    batch.start({
      title,
      description: options.description,
      items: list.map((a) => ({ id: a.id, label: a.name })),
      task,
      onFinished: () => {
        invalidate();
        if (!options.keepSelection) setSelected(new Set());
      },
    });
  };

  const twoStepConfirm = async (first: { title: string; message: ReactNode; confirmText: string }, second: { title: string; message: ReactNode; confirmText: string }) => {
    if (!(await confirm({ ...first, tone: "danger" }))) return false;
    return confirm({ ...second, tone: "danger" });
  };

  const batchTest = () => {
    const catalog = modelCapabilities.data?.models;
    const byId = new Map(selectedAccounts.map((a) => [a.id, a]));
    startBatch(`批量测试（${selectedAccounts.length} 个账号）`, selectedAccounts, async (item) => {
      const account = byId.get(item.id);
      const model = defaultAccountTestModelForOptions(buildAccountTestModelOptions(catalog, account?.supportedModels));
      const result = await testAccount(item.id, { model, prompt: DEFAULT_TEST_PROMPT });
      const detail = [`模型 ${result.model || model}`, result.status ? `HTTP ${result.status}` : null, result.response ? `响应：${result.response.slice(0, 60)}` : null]
        .filter(Boolean)
        .join(" · ");
      return result.ok
        ? { status: "success", message: detail }
        : { status: "failed", message: `${result.message || "测试失败"}（${detail}）` };
    }, {
      description: "每个账号用单个测试时的默认模型（账号有支持模型列表时取列表里的模型）发一条最小请求，验证地址、密钥与请求头配置。",
      keepSelection: true,
    });
  };

  const batchSetEnabled = async (enabled: boolean) => {
    const targets = enabled ? selectedDisabled : selectedEnabled;
    if (!targets.length) return toast.error(enabled ? "选中项里没有停用的账号" : "选中项里没有启用的账号");
    if (!enabled) {
      const ok = await confirm({
        title: `停用 ${targets.length} 个账号？`,
        message: "停用后这些账号立即不再接新请求，进行中的请求照常完成；配置保留，随时可以再启用。",
        confirmText: "停用",
      });
      if (!ok) return;
    }
    startBatch(enabled ? `批量启用（${targets.length} 个账号）` : `批量停用（${targets.length} 个账号）`, targets, async (item) => {
      await setAccountEnabled(item.id, enabled);
      return { status: "success", message: enabled ? "已启用" : "已停用" };
    });
  };

  const batchClearCooldown = () =>
    startBatch(`批量清除冷却（${selectedCooling.length} 个账号）`, selectedCooling, async (item) => {
      await clearAccountCooldown(item.id);
      return { status: "success", message: "冷却已清除" };
    });

  const batchClearAutoDisabled = () =>
    startBatch(`批量清除自动禁用（${selectedAutoDisabled.length} 个账号）`, selectedAutoDisabled, async (item) => {
      await clearAccountAutoDisabled(item.id);
      return { status: "success", message: "自动禁用已清除" };
    });

  /**
   * 删除类批量操作在确认之后、执行之前取一次最新账号列表，按最新状态决定每个账号怎么处理；
   * 取不到最新列表就不执行，避免按过期状态删掉期间被重新启用的账号。
   */
  const fetchLatestAccounts = async (action: string): Promise<Map<number, Account> | null> => {
    try {
      const fresh = (await query.refetch({ throwOnError: true })).data?.accounts ?? [];
      return new Map(fresh.map((a) => [a.id, a]));
    } catch (error) {
      toast.error(`读取最新账号失败，未执行${action}：${extractErrorMessage(error)}`);
      return null;
    }
  };

  /** 只删执行时仍为停用的账号；期间被重新启用或已不存在的跳过。 */
  const deleteIfStillDisabled = (latest: ReadonlyMap<number, Account>): BatchTask => async (item) => {
    const account = latest.get(item.id);
    if (!account) return { status: "skipped", message: "状态已变化：账号已不存在" };
    if (account.enabled) return { status: "skipped", message: "状态已变化：已被重新启用，未删除" };
    await deleteAccount(item.id);
    return { status: "success", message: "已删除" };
  };

  const batchDisableAndDelete = async () => {
    const targets = selectedAccounts;
    if (!targets.length) return toast.error("请先选择账号");
    const ok = await twoStepConfirm(
      {
        title: "禁用并删除账号",
        message: `将先停用 ${targets.length} 个账号，其中 ${selectedEnabled.length} 个当前启用的账号会立即停止接收新请求，进行中的请求继续完成；随后删除这些账号。是否继续？`,
        confirmText: "继续禁用并删除",
      },
      {
        title: "再次确认危险操作",
        message: `这是最后一次确认：停用并删除这 ${targets.length} 个账号？删除后不能在页面上恢复，路由场景里引用它们的条目也会失效。`,
        confirmText: "确认禁用并删除",
      },
    );
    if (!ok) return;
    const latest = await fetchLatestAccounts("禁用并删除");
    if (!latest) return;
    startBatch(`禁用并删除（${targets.length} 个账号）`, targets, async (item) => {
      // 按执行前取到的最新启用状态决定是否先停用。
      const account = latest.get(item.id);
      if (!account) return { status: "skipped", message: "状态已变化：账号已不存在" };
      if (account.enabled) await setAccountEnabled(item.id, false);
      await deleteAccount(item.id);
      return { status: "success", message: account.enabled ? "已停用并删除" : "已删除" };
    }, { description: "执行前已重新读取账号状态：仍启用的先停用再删除，期间已被删除的跳过。" });
  };

  const batchDeleteDisabled = async () => {
    const targets = selectedDisabled;
    if (!targets.length) return toast.error("选中项中没有已停用的账号");
    const skipped = selectedAccounts.length - targets.length;
    const ok = await twoStepConfirm(
      {
        title: "批量删除账号",
        message: `将删除 ${targets.length} 个已停用账号${skipped ? `，跳过 ${skipped} 个未停用账号` : ""}。删除后无法恢复。是否继续？`,
        confirmText: "继续删除",
      },
      { title: "再次确认删除", message: `这是最后一次确认：永久删除这 ${targets.length} 个账号？`, confirmText: "确认永久删除" },
    );
    if (!ok) return;
    const latest = await fetchLatestAccounts("删除");
    if (!latest) return;
    startBatch(`删除已停用（${targets.length} 个账号）`, targets, deleteIfStillDisabled(latest), {
      description: "执行前已重新读取账号状态：只删除仍为停用的账号，期间被重新启用或已删除的跳过。",
    });
  };

  const clearAllDisabled = async () => {
    const targets = allDisabled;
    if (!targets.length) return toast.error("没有可清除的已停用账号");
    const ok = await twoStepConfirm(
      { title: "清除已停用账号", message: `确定清除所有 ${targets.length} 个已停用账号？此操作无法撤销。`, confirmText: "清除全部" },
      { title: "再次确认清除", message: `这是最后一次确认：永久清除全部 ${targets.length} 个已停用账号？`, confirmText: "确认清除全部" },
    );
    if (!ok) return;
    const latest = await fetchLatestAccounts("清除");
    if (!latest) return;
    startBatch(`清除全部已停用（${targets.length} 个账号）`, targets, deleteIfStillDisabled(latest), {
      description: "执行前已重新读取账号状态：只删除仍为停用的账号，期间被重新启用或已删除的跳过。",
    });
  };

  const submitBatchEdit = async (draft: BatchEditDraft) => {
    if (batchEditSubmitting) return;
    const ids = [...selected];
    // overrides 在后端整体替换：先取最新的账号列表，再逐个在现有值上合并。
    // 取成功才关弹窗；取失败时弹窗保持打开、填写的内容不丢，可以直接再点应用。
    setBatchEditSubmitting(true);
    let fresh: Account[];
    try {
      fresh = (await query.refetch({ throwOnError: true })).data?.accounts ?? [];
    } catch (error) {
      toast.error(`读取最新账号失败，未做任何修改，填写的内容已保留，可稍后再点应用：${extractErrorMessage(error)}`);
      return;
    } finally {
      setBatchEditSubmitting(false);
    }
    setBatchEditOpen(false);
    const byId = new Map(fresh.map((a) => [a.id, a]));
    const targets = ids.flatMap((id) => (byId.has(id) ? [byId.get(id)!] : []));
    startBatch(`批量修改（${targets.length} 个账号）`, targets, async (item) => {
      const account = byId.get(item.id)!;
      const plan = buildBatchUpdate(account, draft);
      if (plan.changes.length === 0) return { status: "skipped", message: plan.warnings.length ? plan.warnings.join("；") : "没有变化" };
      if (Object.keys(plan.update).length > 0) await updateAccount(item.id, plan.update);
      if (plan.enabled !== undefined) await setAccountEnabled(item.id, plan.enabled);
      return { status: "success", message: [...plan.changes, ...plan.warnings].join("；") };
    }, { description: "逐个账号只提交变化的字段；覆盖全局参数在账号现有值上合并。" });
  };

  const copy = (account: Account) =>
    mutation.mutate(async () => {
      const created = await copyAccount(account.id);
      toast.success(`已复制为 #${created.id}「${created.name}」（已停用）`);
      openEditor({ mode: "edit", account: created, draft: accountFormFromAccount(created), copiedFrom: { id: account.id, name: account.name } });
    });

  const deleteCopy = async () => {
    const copied = editor?.account;
    if (!copied) return;
    const ok = await confirm({ title: "删除副本", message: `删除刚复制的「${copied.name}」？原账号不受影响。`, confirmText: "删除", tone: "danger" });
    if (!ok) return;
    mutation.mutate(async () => {
      await deleteAccount(copied.id);
      toast.success("副本已删除");
      setEditor(null);
    });
  };

  const discoverModels = (account: Account) =>
    run(async () => {
      // 先发现、再比对、再确认：覆盖的是调度资格列表，错了会让账号接不到模型。
      const found = await discoverStoredAccountSupportedModels(account.id);
      const next = found.supportedModels;
      const current = account.supportedModels ?? [];
      const removed = current.filter((m) => !next.includes(m));
      const added = next.filter((m) => !current.includes(m));
      if (removed.length === 0 && added.length === 0) {
        toast.info("上游返回的模型列表与当前一致，无需更新");
        throw new SilentCancel();
      }
      const ok = await confirm({
        title: `用上游结果覆盖 ${account.name} 的支持模型？`,
        message: (
          <div className="space-y-1">
            <div>上游返回 {next.length} 个模型，当前列表 {current.length} 个。</div>
            {added.length > 0 && <div>新增：{added.join("、")}</div>}
            {removed.length > 0 && <div className="text-destructive">移除：{removed.join("、")}（移除后该账号不再承接这些模型）</div>}
          </div>
        ),
        confirmText: "覆盖",
        tone: removed.length > 0 ? "danger" : "default",
      });
      if (!ok) throw new SilentCancel();
      await updateAccount(account.id, { supportedModels: next });
    }, "已刷新支持的模型");

  const promoteModels = async (account: Account) => {
    try {
      const value = await promote.mutateAsync(account.id);
      toast.success(`已加入目录 ${value.count} 个模型`);
    } catch (error) {
      toast.error(extractErrorMessage(error));
    }
  };

  const save = () => {
    if (!editor) return;
    const request = buildAccountRequest(editor.draft);
    if (editor.mode === "edit" && editor.account) {
      // 编辑（含复制出的副本）只 PUT 变化的字段：后端按部分更新处理，没提交的字段沿用库里的值，
      // 打码的代理地址原样不动时不会提交，也就不会覆盖已保存的凭据。
      const accountId = editor.account.id;
      const copied = Boolean(editor.copiedFrom);
      const update = diffAccountRequest(buildAccountRequest(editor.initialDraft), request);
      if (Object.keys(update).length === 0) {
        toast.info("没有改动");
        setEditor(null);
        return;
      }
      mutation.mutate(async () => {
        await updateAccount(accountId, update);
        toast.success(copied ? "副本已保存" : "账号已更新");
        setEditor(null);
      });
      return;
    }
    if (hasMaskedProxyCredentials(request.proxyUrl)) {
      toast.error("新建账号的代理地址不能带 ***：*** 只是已保存凭据的打码，新账号没有可沿用的凭据，请填写完整的用户名和密码。");
      return;
    }
    mutation.mutate(async () => {
      await createAccount(request);
      toast.success("账号已添加");
      setEditor(null);
    });
  };

  const clearFilters = () => setFilters(DEFAULT_ACCOUNT_FILTERS);
  const createButton = (
    <Button onClick={() => openEditor({ mode: "create", account: null, draft: defaultAccountForm() })}>
      <Plus />
      添加账号
    </Button>
  );

  return (
    <PageContainer>
      <PageHeader
        actions={
          <div className="flex flex-wrap items-center gap-1.5">
            <Button variant="outline" onClick={() => setImportOpen(true)}>
              <Upload />
              导入
            </Button>
            <Button variant="outline" onClick={() => setExportOpen(true)} disabled={total === 0}>
              <Download />
              导出
            </Button>
            {createButton}
          </div>
        }
      />

      <StatGrid min="11rem">
        <StatCard
          title="账号"
          value={total}
          desc={`启用 ${summary?.enabled ?? globalFacets.enabled.enabled} 个`}
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
        description={
          <>
            单个账号的配置只影响自身；全局
            <Link to={RUNTIME_LINKS.capacity} className={RUNTIME_LINK_CLASS}>调度</Link>、
            <Link to={RUNTIME_LINKS.cooldown} className={RUNTIME_LINK_CLASS}>冷却</Link>、
            <Link to={RUNTIME_LINKS.uplift} className={RUNTIME_LINK_CLASS}>补偿策略</Link>
            在运行配置页统一保存。{filteredActive ? `筛选后 ${filtered.length} / 共 ${total} 个。` : `共 ${total} 个账号。`}
          </>
        }
        actions={
          <Button variant="outline" size="sm" onClick={() => statuses.refetch()} disabled={statuses.isFetching}>
            <RefreshCw className={statuses.isFetching ? "animate-spin" : undefined} />
            刷新状态
          </Button>
        }
      >
        <div className="space-y-2 pb-2">
          <Segmented<EnabledFilter>
            label="启用状态"
            value={filters.enabled}
            onChange={(v) => setFilter("enabled", v)}
            items={[["all", "全部", facets.enabled.all], ["enabled", "启用", facets.enabled.enabled], ["disabled", "停用", facets.enabled.disabled]]}
          />
          <Segmented<DispatchFilter>
            label="调度状况"
            value={filters.dispatch}
            onChange={(v) => setFilter("dispatch", v)}
            items={[
              ["all", "全部", facets.dispatch.all],
              ["dispatchable", "可调度", statuses.data ? facets.dispatch.dispatchable : undefined],
              ["blocked", "不可调度", statuses.data ? facets.dispatch.blocked : undefined],
            ]}
          />
          <div role="group" aria-label="运行状态" className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
            <span className="w-14 shrink-0 text-xs text-muted-foreground">运行状态</span>
            <div className="flex flex-wrap gap-1.5">
              {RUNTIME_FLAG_OPTIONS.map(({ value, label, title }) => {
                const active = filters.runtime.includes(value);
                return (
                  <button
                    key={value}
                    type="button"
                    aria-pressed={active}
                    title={title}
                    onClick={() => toggleRuntime(value)}
                    className={cn(
                      "flex h-7 items-center gap-1.5 rounded-full border px-2.5 text-xs transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/40",
                      active ? "border-primary/40 bg-primary/10 text-primary" : "border-border text-muted-foreground hover:bg-muted",
                    )}
                  >
                    {label}
                    <span className="tabular-nums">{facets.runtime[value]}</span>
                  </button>
                );
              })}
              {filters.runtime.length > 0 && (
                <Button variant="ghost" size="xs" className="text-muted-foreground" onClick={() => setFilter("runtime", [])}>清除运行状态</Button>
              )}
            </div>
            {filters.runtime.length > 1 && <span className="text-[0.68rem] text-muted-foreground">同时满足所选状态</span>}
          </div>
        </div>

        <Toolbar className="mb-3">
          <div className="grid w-full min-w-0 items-end gap-2 sm:grid-cols-2 lg:grid-cols-[minmax(11rem,1.4fr)_minmax(9rem,1fr)_minmax(7rem,9rem)_minmax(7rem,9rem)_minmax(8rem,10rem)_5.5rem_auto]">
            <FilterField label="快速定位">
              <Input value={filters.keyword} onChange={(e) => setFilter("keyword", e.target.value)} placeholder="#ID / 名称 / 地址 / 标签" className="h-8 text-xs" />
            </FilterField>
            <FilterField label="可用模型">
              <Select value={filters.model} onValueChange={(v) => setFilter("model", v)}>
                <SelectTrigger size="sm" aria-label="按可用模型筛选"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value={MODEL_FILTER_ALL}>全部模型</SelectItem>
                  <SelectItem value={MODEL_FILTER_UNRESTRICTED}>未限制模型的账号</SelectItem>
                  {modelOptions.map((model) => <SelectItem key={model} value={model}>{model}</SelectItem>)}
                </SelectContent>
              </Select>
            </FilterField>
            <FilterField label="标签">
              <Select value={filters.tag} onValueChange={(v) => setFilter("tag", v)}>
                <SelectTrigger size="sm" aria-label="按标签筛选"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value={TAG_FILTER_ALL}>全部标签</SelectItem>
                  {tags.map((t) => <SelectItem value={t} key={t}>{t}</SelectItem>)}
                </SelectContent>
              </Select>
            </FilterField>
            <FilterField label="代理">
              <Select value={filters.proxy} onValueChange={(v) => setFilter("proxy", v as ProxyFilter)}>
                <SelectTrigger size="sm" aria-label="按代理筛选"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">全部</SelectItem>
                  <SelectItem value="with_proxy">配置了代理</SelectItem>
                  <SelectItem value="without_proxy">未配置代理</SelectItem>
                </SelectContent>
              </Select>
            </FilterField>
            <FilterField label="排序">
              <Select value={sortBy} onValueChange={(v) => setSortBy(v as AccountSortBy)}>
                <SelectTrigger size="sm" aria-label="排序"><SelectValue /></SelectTrigger>
                <SelectContent>
                  {SORT_OPTIONS.map((option) => <SelectItem key={option.value} value={option.value}>{option.label}</SelectItem>)}
                </SelectContent>
              </Select>
            </FilterField>
            <FilterField label="方向">
              <Select value={sortOrder} onValueChange={(v) => setSortOrder(v as SortOrder)} disabled={sortBy === "default"}>
                <SelectTrigger size="sm" aria-label="排序方向"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="desc">降序</SelectItem>
                  <SelectItem value="asc">升序</SelectItem>
                </SelectContent>
              </Select>
            </FilterField>
            <div className="flex min-w-0 items-end">
              {filteredActive && (
                <Button variant="ghost" size="sm" onClick={clearFilters} className="h-8 w-full">
                  <X />清除
                </Button>
              )}
            </div>
          </div>
        </Toolbar>

        {selected.size > 0 && (
          <div className="mb-3 flex flex-wrap items-center gap-2 rounded-lg bg-primary/5 px-3 py-2 animate-in fade-in slide-in-from-top-2 duration-150">
            <Badge tone="primary">已选 {selected.size}</Badge>
            <Button variant="outline" size="xs" disabled={batchBusy} onClick={batchTest}><CheckCircle2 />批量测试</Button>
            <Button variant="outline" size="xs" disabled={batchBusy} onClick={() => setBatchEditOpen(true)}><Filter />批量修改</Button>
            <Button variant="outline" size="xs" disabled={batchBusy || selectedDisabled.length === 0} onClick={() => void batchSetEnabled(true)}><Power />启用 ({selectedDisabled.length})</Button>
            <Button variant="outline" size="xs" disabled={batchBusy || selectedEnabled.length === 0} onClick={() => void batchSetEnabled(false)}><Power />停用 ({selectedEnabled.length})</Button>
            <Button variant="outline" size="xs" disabled={batchBusy || selectedCooling.length === 0} onClick={batchClearCooldown}><RotateCcw />清除冷却 ({selectedCooling.length})</Button>
            <Button variant="outline" size="xs" disabled={batchBusy || selectedAutoDisabled.length === 0} onClick={batchClearAutoDisabled}><RotateCcw />清除自动禁用 ({selectedAutoDisabled.length})</Button>
            <Button variant="outline" size="xs" onClick={() => setExportOpen(true)}><Download />导出 ({selected.size})</Button>
            <Button variant="outline" size="xs" className="text-destructive hover:bg-destructive/10 hover:text-destructive" disabled={batchBusy} onClick={() => void batchDisableAndDelete()}>
              <Trash2 />禁用并删除 ({selected.size})
            </Button>
            <Button variant="outline" size="xs" className="text-destructive hover:bg-destructive/10 hover:text-destructive" disabled={batchBusy || selectedDisabled.length === 0} title="只处理选中项里已停用的账号" onClick={() => void batchDeleteDisabled()}>
              <Trash2 />删除已停用 ({selectedDisabled.length})
            </Button>
            <Button variant="ghost" size="xs" onClick={() => setSelected(new Set())}>取消</Button>
          </div>
        )}

        {accounts.length > 0 && (
          <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
            <div className="flex flex-wrap items-center gap-2">
              <Checkbox
                checked={pageState === "all" ? true : pageState === "some" ? "indeterminate" : false}
                disabled={pageIds.length === 0}
                onCheckedChange={() => setSelected((prev) => toggleSelectPage(pageIds, prev))}
                aria-label="全选当前页"
              />
              <span className="text-xs text-muted-foreground">{selected.size > 0 ? `已选 ${selected.size} 个` : "全选当前页"}</span>
              {pageState === "all" && filtered.length > pageIds.length && selected.size < filtered.length && (
                <Button variant="link" size="xs" className="h-auto px-1" onClick={() => setSelected(new Set(filtered.map((a) => a.id)))}>
                  <ListChecks />选择全部 {filtered.length} 个{filteredActive ? "筛选结果" : "账号"}
                </Button>
              )}
            </div>
            <div className="flex flex-wrap items-center gap-2">
              {batch.state && !batch.state.open && (
                <Button variant="outline" size="xs" onClick={batch.show}>
                  {batch.state.running ? <RefreshCw className="animate-spin" /> : <ListChecks />}
                  {batch.state.running ? "批量操作进行中" : "上次批量结果"}
                </Button>
              )}
              {allDisabled.length > 0 && (
                <Button variant="destructive" size="xs" disabled={batchBusy} onClick={() => void clearAllDisabled()}>
                  <Trash2 />清除全部已停用 ({allDisabled.length})
                </Button>
              )}
            </div>
          </div>
        )}

        {accounts.length === 0 ? (
          <EmptyState
            icon={<Users />}
            title="还没有账号"
            description="添加第一个账号后，请求才会有地方可去；也可以批量导入。"
            action={
              <div className="flex gap-2">
                <Button variant="outline" onClick={() => setImportOpen(true)}><Upload />导入</Button>
                {createButton}
              </div>
            }
          />
        ) : filtered.length === 0 ? (
          <EmptyState title="没有匹配的账号" description="换个关键字或筛选条件。" action={<Button variant="outline" size="sm" onClick={clearFilters}>清除筛选</Button>} />
        ) : (
          <div className="space-y-3">
            {paged.items.map((account) => (
              <AccountCard
                key={account.id}
                account={account}
                runtime={statusMap.get(account.id)}
                config={config}
                selected={selected.has(account.id)}
                busy={mutation.isPending}
                promoting={promote.isPending}
                onToggleSelect={() => setSelected((prev) => toggleSelectOne(account.id, prev))}
                onEdit={() => openEditor({ mode: "edit", account, draft: accountFormFromAccount(account) })}
                onCopy={() => copy(account)}
                onTest={() => setTesting(account)}
                onDiagnose={() => navigate(accountRiskLink(account.id))}
                onOpenUsage={() => navigate(`/usage?accountId=${account.id}`)}
                onShowEffective={() => setEffectiveId(account.id)}
                onToggleEnabled={() => run(() => setAccountEnabled(account.id, !account.enabled), account.enabled ? "已停用" : "已启用")}
                onQuickEdit={(field) => setQuickEdit({ account, field })}
                onDiscoverModels={() => discoverModels(account)}
                onPromoteModels={() => void promoteModels(account)}
                onClearCooldown={() => run(() => clearAccountCooldown(account.id), "冷却状态已清除")}
                onClearAutoDisabled={() => run(() => clearAccountAutoDisabled(account.id), "自动禁用状态已清除")}
                onDelete={async () => {
                  const ok = await confirm({ title: "删除账号", message: `删除账号「${account.name}」？此操作无法撤销。`, confirmText: "删除", tone: "danger" });
                  if (ok) run(() => deleteAccount(account.id), "账号已删除");
                }}
              />
            ))}
          </div>
        )}

        {filtered.length > 0 && (
          <div className="mt-3 border-t pt-3">
            <Pagination
              page={paged.page}
              pageCount={paged.pageCount}
              total={filtered.length}
              pageSize={pageSize}
              pageSizeOptions={PAGE_SIZE_OPTIONS}
              onPageChange={setPage}
              onPageSizeChange={(next) => {
                setPageSize(next);
                setPage(1);
              }}
            />
          </div>
        )}
      </SectionCard>

      {editor && (
        <AccountFormModal
          mode={editor.mode}
          account={editor.account}
          open
          draft={editor.draft}
          saving={mutation.isPending}
          copiedFrom={editor.copiedFrom}
          onDeleteCopy={() => void deleteCopy()}
          tagOptions={tags}
          // 函数式更新：异步的“从上游发现模型”返回时基于最新草稿改，不覆盖期间的其它编辑。
          onDraftChange={(v) => setEditor((prev) => prev && { ...prev, draft: typeof v === "function" ? v(prev.draft) : v })}
          onClose={async () => {
            if (mutation.isPending) return;
            if (JSON.stringify(editor.draft) !== JSON.stringify(editor.initialDraft)) {
              const ok = await confirm({
                title: "放弃未保存的修改？",
                message: editor.copiedFrom
                  ? "副本的修改还没有保存，关闭后会丢失；副本本身会以停用状态保留。"
                  : editor.mode === "edit" ? "这个账号的修改还没有保存，关闭后会丢失。" : "新账号还没有保存，关闭后填写的内容会丢失。",
                confirmText: "放弃修改",
                tone: "danger",
              });
              if (!ok) return;
            }
            setEditor(null);
          }}
          onSubmit={save}
          onDiscoverSupportedModels={async () => {
            try {
              const overrides = { baseUrl: editor.draft.baseUrl, apiKey: editor.draft.apiKey || undefined, authType: editor.draft.authType };
              // 编辑已有账号时走账号接口：表单里改的地址、密钥覆盖已保存的值，
              // 代理、Header profile 与 TLS profile 仍按该账号的配置，和真实转发一致。
              const result = editor.account
                ? await discoverStoredAccountSupportedModels(editor.account.id, overrides)
                : await discoverAccountSupportedModels(overrides);
              return result.supportedModels;
            } catch (error) {
              toast.error(extractErrorMessage(error));
              throw error;
            }
          }}
        />
      )}
      <AccountTestModal account={testing} open={testing !== null} onClose={() => setTesting(null)} onDone={invalidate} />
      <AccountQuickEditModal
        key={quickEdit ? `${quickEdit.account.id}-${quickEdit.field}` : "closed"}
        target={quickEdit}
        status={quickEdit ? statusMap.get(quickEdit.account.id) : undefined}
        onClose={() => setQuickEdit(null)}
        onSaved={invalidate}
      />
      <AccountBatchEditModal
        open={batchEditOpen}
        count={selected.size}
        tagOptions={tags}
        submitting={batchEditSubmitting}
        onClose={() => setBatchEditOpen(false)}
        onSubmit={(draft) => void submitBatchEdit(draft)}
      />
      <AccountImportModal open={importOpen} onClose={() => setImportOpen(false)} existingAccounts={accounts} tagOptions={tags} onDone={invalidate} />
      <AccountExportModal
        open={exportOpen}
        onClose={() => setExportOpen(false)}
        selectedIds={[...selected]}
        filteredIds={filtered.map((a) => a.id)}
        totalCount={total}
        filtered={filteredActive}
      />
      <BatchRunModal batch={batch} />
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
