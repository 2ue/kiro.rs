import * as React from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Copy, Download, Eraser, Pencil, Play, Plus, RotateCcw, Route, Save, Trash2, Upload, X } from "lucide-react";
import { toast } from "sonner";
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
  Textarea,
} from "@/components/ui";
import {
  Callout,
  EmptyState,
  ErrorState,
  LoadingState,
  ModalShell,
  PageContainer,
  PageHeader,
  SectionCard,
  useConfirm,
} from "@/components/patterns";
import {
  applyRouteScenario,
  createRouteScenario,
  deleteRouteScenario,
  getCurrentRouteScenarioContent,
  getRouteScenarioOptions,
  getRouteScenarios,
  previewRouteScenario,
  updateRouteScenario,
} from "@/api/route-scenarios";
import { useUnsavedChangesGuard } from "@/hooks/use-unsaved-changes-guard";
import { formatDate } from "@/lib/format";
import { extractErrorMessage } from "@/lib/utils";
import type {
  AccountRoutePolicy,
  RoutePolicy,
  RouteRuleMode,
  RouteScenario,
  RouteScenarioAccountOption,
  RouteScenarioApplyResponse,
  RouteScenarioContent,
  RouteScenarioItemKey,
  RouteScenarioPreview,
} from "@/types/api";
import { displayRoutePrefix, isBuiltinRoute, normalizeRoutePrefix, sortRoutes } from "@/features/routes/route-utils";
import { CACHE_STRATEGIES } from "@/features/cache-policies/route-cache-editor";
import { RouteSelector } from "./route-selector";
import {
  accountRuleGroups,
  assignPolicy,
  buildAccountRows,
  filterRows,
  initialBatchPolicy,
  policySummary,
  selectByClick,
  type AccountRow,
} from "./account-rules";

const MODE_LABELS: Record<RouteRuleMode, string> = {
  allow_all: "不限制",
  allow_list: "仅允许所选路由",
  deny_list: "禁止所选路由",
};

const SCENARIO_FILE_FORMAT = "claude-pool-route-scenario";

/** 每批渲染的账号行数；全选与批量操作始终覆盖整个列表。 */
const RENDER_STEP = 200;

function emptyContent(routes: RouteScenarioContent["routes"]): RouteScenarioContent {
  return { routes, externalAccounts: [] };
}

/** 路由规则编辑：模式 + 路由选择器（路由只能选，不能手填）。 */
function PolicyEditor({
  value,
  routes,
  onChange,
  idPrefix,
}: {
  value: RoutePolicy;
  routes: string[];
  onChange: (next: RoutePolicy) => void;
  idPrefix: string;
}) {
  return (
    <div className="grid gap-3 md:grid-cols-[14rem_1fr]">
      <div className="space-y-1.5">
        <label className="text-xs font-medium text-muted-foreground" htmlFor={`${idPrefix}-mode`}>模式</label>
        <Select
          value={value.routeMode}
          onValueChange={(mode) => onChange({ routeMode: mode as RouteRuleMode, routeRules: mode === "allow_all" ? [] : value.routeRules })}
        >
          <SelectTrigger id={`${idPrefix}-mode`} size="sm"><SelectValue /></SelectTrigger>
          <SelectContent>
            {(Object.keys(MODE_LABELS) as RouteRuleMode[]).map((mode) => (
              <SelectItem key={mode} value={mode}>{MODE_LABELS[mode]}</SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      <div className="min-w-0 space-y-1.5">
        <div className="text-xs font-medium text-muted-foreground">路由</div>
        <RouteSelector
          routes={routes}
          value={value.routeRules}
          disabled={value.routeMode === "allow_all"}
          onChange={(routeRules) => onChange({ ...value, routeRules })}
        />
      </div>
    </div>
  );
}

/**
 * 全部账号及其在场景里的规则。可勾选任意数量账号后统一设置一条规则；再次设置会覆盖。
 * 未列出的账号应用时恢复为"不限制"。
 */
function AccountRulesEditor({
  idPrefix,
  title,
  description,
  accounts,
  value,
  routes,
  onChange,
}: {
  idPrefix: string;
  title: string;
  description: string;
  accounts: RouteScenarioAccountOption[];
  value: AccountRoutePolicy[];
  routes: string[];
  onChange: (next: AccountRoutePolicy[]) => void;
}) {
  const [search, setSearch] = React.useState("");
  const [filter, setFilter] = React.useState("all");
  const [selected, setSelected] = React.useState<Set<number>>(new Set());
  const [editing, setEditing] = React.useState<{ ids: number[]; batch: boolean } | null>(null);
  const lastClicked = React.useRef<number | null>(null);
  const [renderLimit, setRenderLimit] = React.useState(RENDER_STEP);

  const rows = React.useMemo(() => buildAccountRows(accounts, value), [accounts, value]);
  const groups = React.useMemo(() => accountRuleGroups(value), [value]);
  const keyword = search.trim();
  // 某条规则的最后一个账号改掉后，该筛选项消失，回退到全部。
  const activeFilter = filter.startsWith("rule:") && !groups.some((group) => `rule:${group.key}` === filter) ? "all" : filter;
  const visible = React.useMemo(() => filterRows(rows, activeFilter, search), [rows, activeFilter, search]);
  const selectedRows = rows.filter((row) => selected.has(row.id));
  const selectedIds = selectedRows.map((row) => row.id);
  const visibleSelected = visible.filter((row) => selected.has(row.id)).length;
  const headerState: boolean | "indeterminate" = visibleSelected === 0 ? false : visibleSelected === visible.length ? true : "indeterminate";
  const selectedWithRule = selectedRows.filter((row) => row.rule).length;
  const rendered = visible.slice(0, renderLimit);

  const updateSelected = (update: (next: Set<number>) => void) =>
    setSelected((prev) => {
      const next = new Set(prev);
      update(next);
      return next;
    });
  const toggleAllVisible = () =>
    updateSelected((next) => {
      for (const row of visible) {
        if (headerState === true) next.delete(row.id);
        else next.add(row.id);
      }
    });
  const invertVisible = () =>
    updateSelected((next) => {
      for (const row of visible) {
        if (next.has(row.id)) next.delete(row.id);
        else next.add(row.id);
      }
    });
  const clickRow = (index: number, shift: boolean) => {
    setSelected((prev) => selectByClick(prev, visible, index, lastClicked.current, shift));
    lastClicked.current = visible[index]?.id ?? null;
  };
  const assign = (ids: number[], policy: RoutePolicy, batch: boolean) => {
    onChange(assignPolicy(value, ids, policy));
    if (batch) setSelected(new Set());
    setEditing(null);
  };

  return (
    <SectionCard title={title} description={description}>
      <div className="space-y-3">
        <div className="flex flex-wrap items-center gap-2">
          <Input
            className="h-8 w-56"
            placeholder="搜索名称、ID 或规则"
            value={search}
            onChange={(event) => { setSearch(event.target.value); setRenderLimit(RENDER_STEP); }}
            aria-label={`搜索${title}`}
          />
          <Select value={activeFilter} onValueChange={(next) => { setFilter(next); setRenderLimit(RENDER_STEP); }}>
            <SelectTrigger size="sm" className="w-56" aria-label={`按规则筛选${title}`}><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="all">全部账号（{rows.length}）</SelectItem>
              <SelectItem value="unset">未设置（{rows.length - value.length}）</SelectItem>
              <SelectItem value="set">已设置（{value.length}）</SelectItem>
              {groups.map((group) => (
                <SelectItem key={group.key} value={`rule:${group.key}`}>{group.summary}（{group.count}）</SelectItem>
              ))}
            </SelectContent>
          </Select>
          <span className="ml-auto text-xs text-muted-foreground">已设置 {value.length} / {rows.length}，其余应用时不限制</span>
        </div>

        <div className="flex min-h-9 flex-wrap items-center gap-2 rounded-md border bg-muted/40 px-3 py-1.5">
          <label className="flex items-center gap-2 text-sm">
            <Checkbox checked={headerState} onCheckedChange={toggleAllVisible} disabled={visible.length === 0} aria-label={`全选当前列表的${title}`} />
            全选{keyword || activeFilter !== "all" ? "筛选结果" : ""}（{visible.length}）
          </label>
          <Button size="xs" variant="ghost" onClick={invertVisible} disabled={visible.length === 0}>反选</Button>
          {selectedIds.length > 0 ? (
            <div className="ml-auto flex flex-wrap items-center gap-2">
              <span className="text-sm font-medium">
                已选 {selectedIds.length} 个
                {selectedIds.length > visibleSelected && <span className="font-normal text-muted-foreground">（{selectedIds.length - visibleSelected} 个不在当前列表）</span>}
              </span>
              <Button size="sm" onClick={() => setEditing({ ids: selectedIds, batch: true })}><Route />设置路由规则</Button>
              <Button size="sm" variant="outline" onClick={() => assign(selectedIds, { routeMode: "allow_all", routeRules: [] }, true)} disabled={selectedWithRule === 0}>
                <Eraser />清除设置
              </Button>
              <Button size="sm" variant="ghost" onClick={() => setSelected(new Set())}>取消选择</Button>
            </div>
          ) : (
            <span className="ml-auto text-xs text-muted-foreground">勾选账号后统一设置；Shift 点击可连续选择</span>
          )}
        </div>

        {visible.length === 0 ? (
          <p className="py-6 text-center text-sm text-muted-foreground">{rows.length === 0 ? "这里还没有账号" : "没有匹配的账号"}</p>
        ) : (
          <div className="max-h-[32rem] divide-y overflow-y-auto rounded-md border">
            {rendered.map((row, index) => (
              <div key={row.id} className="flex items-center gap-3 px-3 py-2 hover:bg-muted/30">
                <Checkbox
                  id={`${idPrefix}-account-${row.id}`}
                  checked={selected.has(row.id)}
                  onClick={(event) => { event.preventDefault(); clickRow(index, event.shiftKey); }}
                  aria-label={`选择账号 #${row.id}`}
                />
                <label htmlFor={`${idPrefix}-account-${row.id}`} className="flex min-w-0 flex-1 cursor-pointer items-center gap-2">
                  <span className="shrink-0 font-mono text-xs text-muted-foreground">#{row.id}</span>
                  <span className="min-w-0 truncate text-sm">{row.label}</span>
                  {row.missing && <Badge tone="error" className="shrink-0 whitespace-nowrap">已删除，应用时跳过</Badge>}
                </label>
                <span className={row.rule ? "max-w-[45%] shrink-0 truncate text-sm" : "shrink-0 text-xs text-muted-foreground"} title={row.rule ? policySummary(row.rule) : undefined}>
                  {row.rule ? policySummary(row.rule) : "未设置"}
                </span>
                <div className="flex shrink-0 gap-0.5">
                  <Button size="icon-xs" variant="ghost" title="设置规则" aria-label={`设置账号 #${row.id} 的规则`} onClick={() => setEditing({ ids: [row.id], batch: false })}><Pencil /></Button>
                  <Button
                    size="icon-xs"
                    variant="ghost"
                    title="清除设置"
                    aria-label={`清除账号 #${row.id} 的设置`}
                    className={row.rule ? undefined : "invisible"}
                    onClick={() => assign([row.id], { routeMode: "allow_all", routeRules: [] }, false)}
                  >
                    <X />
                  </Button>
                </div>
              </div>
            ))}
            {visible.length > rendered.length && (
              <div className="flex items-center justify-center gap-2 p-2 text-xs text-muted-foreground">
                已显示 {rendered.length} / {visible.length}，全选和批量操作对全部 {visible.length} 个生效
                <Button size="xs" variant="ghost" onClick={() => setRenderLimit((limit) => limit + RENDER_STEP)}>再显示 {Math.min(RENDER_STEP, visible.length - rendered.length)} 个</Button>
              </div>
            )}
          </div>
        )}
      </div>
      {editing && (
        <AssignPolicyDialog
          idPrefix={idPrefix}
          ids={editing.ids}
          rows={rows}
          routes={routes}
          onClose={() => setEditing(null)}
          onApply={(policy) => assign(editing.ids, policy, editing.batch)}
        />
      )}
    </SectionCard>
  );
}

/** 给一组账号设一条规则。每次打开重新挂载，初始值取自这些账号当前的规则。 */
function AssignPolicyDialog({
  idPrefix,
  ids,
  rows,
  routes,
  onClose,
  onApply,
}: {
  idPrefix: string;
  ids: number[];
  rows: AccountRow[];
  routes: string[];
  onClose: () => void;
  onApply: (policy: RoutePolicy) => void;
}) {
  const targetIds = new Set(ids);
  const targets = rows.filter((row) => targetIds.has(row.id));
  const existing = targets.filter((row) => row.rule);
  const [policy, setPolicy] = React.useState<RoutePolicy>(() => initialBatchPolicy(targets));
  const needsRoutes = policy.routeMode !== "allow_all" && policy.routeRules.length === 0;
  const single = targets.length === 1 ? targets[0] : null;

  return (
    <ModalShell
      open
      onClose={onClose}
      title={single ? `设置账号 #${single.id} 的规则` : `设置 ${ids.length} 个账号的规则`}
      description={single ? single.label : "所选账号统一使用下面的规则，原来的规则会被替换。"}
      footer={
        <>
          <Button variant="outline" onClick={onClose}>取消</Button>
          <Button onClick={() => onApply(policy)} disabled={needsRoutes}>
            {policy.routeMode === "allow_all" ? "清除设置" : single ? "确定" : `应用到 ${ids.length} 个账号`}
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        <PolicyEditor value={policy} routes={routes} onChange={setPolicy} idPrefix={`${idPrefix}-assign`} />
        {policy.routeMode === "allow_all" && <p className="text-xs text-muted-foreground">不限制等于不设置，这些账号会从场景里移除，应用时恢复为不限制。</p>}
        {needsRoutes && <p className="text-xs text-muted-foreground">请选择路由。在路由列表里点“全选”可一次选中现有全部路由。</p>}
        {policy.routeMode === "deny_list" && routes.length > 0 && routes.every((route) => policy.routeRules.includes(route)) && (
          <p className="text-xs text-muted-foreground">已禁止现有全部路由；之后新增的路由不在这条规则里，仍会进入这些账号。</p>
        )}
        {!single && existing.length > 0 && <Callout tone="warning">其中 {existing.length} 个账号已有规则，会被替换为这条规则。</Callout>}
      </div>
    </ModalShell>
  );
}

type EditorState = { id: number | null; name: string; description: string; content: RouteScenarioContent };

function strategyLabel(type?: string): string {
  return CACHE_STRATEGIES.find(([key]) => key === type)?.[1] ?? "模拟高缓存";
}

/** 场景里包含的路由定义一览：只读，告诉用户应用后入口会变成什么样。 */
function RoutesSnapshot({ routes }: { routes: RouteScenarioContent["routes"] }) {
  const list = sortRoutes(routes);
  if (list.length === 0) return <p className="text-sm text-muted-foreground">场景里没有路由定义，应用时只写账号规则。</p>;
  return (
    <div className="divide-y rounded-md border text-sm">
      {list.map((route) => (
        <div key={route.id || route.prefix} className="flex flex-wrap items-center gap-2 px-3 py-2">
          <span className="font-mono">{displayRoutePrefix(route.prefix)}</span>
          {isBuiltinRoute(route.prefix) && <Badge size="xs" tone="secondary">内置</Badge>}
          {!route.enabled && <Badge size="xs" tone="warning">停用</Badge>}
          <span className="text-muted-foreground">{route.name || "未命名"}</span>
          <span className="ml-auto flex flex-wrap gap-1.5">
            <Badge size="xs" tone="neutral">{strategyLabel(route.cachePolicy?.cacheType)}</Badge>
            <Badge size="xs" tone="info">账号 {route.accountRule.mode === "allow_all" ? "不限" : `${route.accountRule.rules.length} 条`}</Badge>
            <Badge size="xs" tone="info">模型 {route.modelRule.mode === "allow_all" ? "不限" : `${route.modelRule.rules.length} 条`}</Badge>
          </span>
        </div>
      ))}
    </div>
  );
}

function ScenarioEditor({
  state,
  routes,
  accounts,
  saving,
  onChange,
  onCancel,
  onSave,
}: {
  state: EditorState;
  routes: string[];
  accounts: RouteScenarioAccountOption[];
  saving: boolean;
  onChange: (next: EditorState) => void;
  onCancel: () => void;
  onSave: () => void;
}) {
  const setContent = (patch: Partial<RouteScenarioContent>) => onChange({ ...state, content: { ...state.content, ...patch } });
  const known = new Set(routes.map((route) => normalizeRoutePrefix(route)));
  const unknownRoutes = Array.from(new Set(state.content.externalAccounts.flatMap((policy) => policy.routeRules).filter((route) => !known.has(normalizeRoutePrefix(route)))));

  return (
    <div className="space-y-4">
      <SectionCard
        title={state.id ? "编辑场景" : "新建场景"}
        description="场景保存一份完整的路由定义和各账号的路由规则。应用时整体写回，调度方式不变。"
        actions={
          <div className="flex gap-2">
            <Button variant="outline" onClick={onCancel} disabled={saving}>取消</Button>
            <Button onClick={onSave} disabled={saving}><Save />{saving ? "保存中..." : "保存场景"}</Button>
          </div>
        }
      >
        <div className="grid gap-3 md:grid-cols-2">
          <div className="space-y-1.5">
            <label className="text-sm font-medium" htmlFor="scenario-name">名称</label>
            <Input id="scenario-name" value={state.name} maxLength={64} onChange={(event) => onChange({ ...state, name: event.target.value })} placeholder="例如：日常、高峰期只走主账号" />
          </div>
          <div className="space-y-1.5">
            <label className="text-sm font-medium" htmlFor="scenario-description">说明</label>
            <Textarea id="scenario-description" className="min-h-9" value={state.description} maxLength={500} onChange={(event) => onChange({ ...state, description: event.target.value })} placeholder="可选" />
          </div>
        </div>
        {unknownRoutes.length > 0 && (
          <Callout tone="warning" className="mt-3">
            这些路由已不在路由列表里：{unknownRoutes.join("、")}。请在下面移除，或先到路由页补回。
          </Callout>
        )}
      </SectionCard>

      <SectionCard title="路由定义" description="应用场景时会用这份路由定义整体替换当前路由。要改路由本身请到路由页，改完后用“从当前设置生成”重新生成场景。">
        <RoutesSnapshot routes={state.content.routes} />
      </SectionCard>

      <AccountRulesEditor
        idPrefix="account"
        title="账号路由规则"
        description="勾选账号后统一设置它能接哪些路由，再次设置会替换原规则。未设置的账号应用时恢复为“不限制”。"
        accounts={accounts}
        value={state.content.externalAccounts}
        routes={routes}
        onChange={(externalAccounts) => setContent({ externalAccounts })}
      />
    </div>
  );
}

function itemKey(item: RouteScenarioItemKey) {
  return `${item.kind}:${item.id}`;
}

function PreviewDialog({
  scenario,
  preview,
  loading,
  error,
  onRetry,
  applying,
  onClose,
  onApply,
}: {
  scenario: RouteScenario | null;
  preview: RouteScenarioPreview | undefined;
  loading: boolean;
  error: unknown;
  onRetry: () => void;
  applying: boolean;
  onClose: () => void;
  onApply: () => void;
}) {
  return (
    <ModalShell
      open={Boolean(scenario)}
      onClose={onClose}
      width="max-w-3xl"
      title={`应用场景 · ${scenario?.name ?? ""}`}
      description="确认下面的变化后应用。没有变化的项不会写入。"
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={applying}>取消</Button>
          <Button onClick={onApply} disabled={loading || applying || !preview}><Play />{applying ? "应用中..." : "确认应用"}</Button>
        </>
      }
    >
      {error ? (
        <ErrorState
          title="无法计算这次应用会改什么"
          message={extractErrorMessage(error)}
          action={<Button variant="outline" size="sm" onClick={onRetry}>重试</Button>}
        />
      ) : loading || !preview ? (
        <LoadingState text="计算变化..." />
      ) : (
        <div className="space-y-3">
          {preview.uncoveredExternalRoutes.length > 0 && (
            <Callout tone="error">这些路由在应用后没有任何可用账号，请求会失败：{preview.uncoveredExternalRoutes.join("、")}</Callout>
          )}
          {preview.unknownRoutes.length > 0 && <Callout tone="warning">场景里有已不存在的路由：{preview.unknownRoutes.join("、")}</Callout>}
          {preview.missingAccounts.length > 0 && (
            <Callout tone="warning">场景里有已删除的账号，应用时跳过：{preview.missingAccounts.map((item) => `#${item.id}`).join("、")}</Callout>
          )}
          <p className="text-sm text-muted-foreground">将修改 {preview.changes.length} 项，{preview.unchanged} 项保持不变。</p>
          {preview.changes.length > 0 && (
            <div className="max-h-96 divide-y overflow-y-auto rounded-md border text-sm">
              {preview.changes.map((change) => (
                <div key={itemKey(change)} className="grid gap-1 p-2.5 md:grid-cols-[12rem_1fr]">
                  <div className="min-w-0">
                    {change.id === 0 ? (
                      <div className="truncate font-medium">{change.label}</div>
                    ) : (
                      <>
                        <div className="text-xs text-muted-foreground">账号 #{change.id}</div>
                        <div className="truncate font-medium" title={change.label}>{change.label}</div>
                      </>
                    )}
                  </div>
                  <div className="min-w-0 text-xs leading-5">
                    <div className="text-muted-foreground line-through">{change.before}</div>
                    <div>{change.after}</div>
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </ModalShell>
  );
}

function ResultDialog({
  result,
  retrying,
  onRetry,
  onClose,
}: {
  result: { scenario: RouteScenario; response: RouteScenarioApplyResponse } | null;
  retrying: boolean;
  onRetry: () => void;
  onClose: () => void;
}) {
  const response = result?.response;
  const failed = response?.items.filter((item) => !item.success) ?? [];
  return (
    <ModalShell
      open={Boolean(result)}
      onClose={onClose}
      width="max-w-3xl"
      title={`应用结果 · ${result?.scenario.name ?? ""}`}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={retrying}>关闭</Button>
          {failed.length > 0 && <Button onClick={onRetry} disabled={retrying}><RotateCcw />{retrying ? "重试中..." : `重试失败的 ${failed.length} 项`}</Button>}
        </>
      }
    >
      {response && (
        <div className="space-y-3">
          <div className="flex flex-wrap gap-2">
            <Badge tone="success">成功 {response.success}</Badge>
            <Badge tone={response.failed ? "error" : "neutral"}>失败 {response.failed}</Badge>
            <Badge tone="neutral">无变化跳过 {response.skipped}</Badge>
          </div>
          {response.items.length === 0 ? (
            <p className="text-sm text-muted-foreground">所有项已经是场景里的值，没有写入。</p>
          ) : (
            <div className="max-h-96 divide-y overflow-y-auto rounded-md border text-sm">
              {[...failed, ...response.items.filter((item) => item.success)].map((item) => (
                <div key={itemKey(item)} className="flex items-start gap-3 p-2.5">
                  <Badge tone={item.success ? "success" : "error"}>{item.success ? "成功" : "失败"}</Badge>
                  <div className="min-w-0">
                    <div className="truncate font-medium">{item.id === 0 ? item.label : `账号 #${item.id} · ${item.label}`}</div>
                    {item.error && <div className="text-xs text-destructive">{item.error}</div>}
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </ModalShell>
  );
}

function downloadScenario(scenario: RouteScenario) {
  const file = { format: SCENARIO_FILE_FORMAT, version: 1, name: scenario.name, description: scenario.description ?? "", content: scenario.content };
  const blob = new Blob([`${JSON.stringify(file, null, 2)}\n`], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = `route-scenario-${scenario.name.replace(/[^\p{L}\p{N}._-]+/gu, "-") || scenario.id}.json`;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

export function parseScenarioFile(text: string): { name: string; description: string; content: RouteScenarioContent } {
  const data = JSON.parse(text);
  if (!data || data.format !== SCENARIO_FILE_FORMAT || typeof data.content !== "object") {
    throw new Error(`不是路由场景文件：缺少 "format": "${SCENARIO_FILE_FORMAT}"`);
  }
  const policy = (value: unknown): RoutePolicy => {
    const raw = (value ?? {}) as Partial<RoutePolicy>;
    const mode = raw.routeMode && raw.routeMode in MODE_LABELS ? raw.routeMode : "allow_all";
    return { routeMode: mode, routeRules: Array.isArray(raw.routeRules) ? raw.routeRules.filter((rule): rule is string => typeof rule === "string") : [] };
  };
  const accounts = (value: unknown): AccountRoutePolicy[] =>
    (Array.isArray(value) ? value : [])
      .filter((entry) => entry && Number.isInteger(entry.id) && entry.id > 0)
      .map((entry) => ({ id: entry.id, ...policy(entry) }));
  return {
    name: typeof data.name === "string" ? data.name : "",
    description: typeof data.description === "string" ? data.description : "",
    content: {
      routes: Array.isArray(data.content.routes) ? data.content.routes : [],
      externalAccounts: accounts(data.content.externalAccounts),
    },
  };
}

export function RouteScenariosPage() {
  const queryClient = useQueryClient();
  const confirm = useConfirm();
  const scenarios = useQuery({ queryKey: ["route-scenarios"], queryFn: getRouteScenarios });
  const options = useQuery({ queryKey: ["route-scenario-options"], queryFn: getRouteScenarioOptions });
  const [editor, setEditor] = React.useState<EditorState | null>(null);
  const editorInitial = React.useRef<string>("");
  const openEditor = (state: EditorState) => {
    editorInitial.current = JSON.stringify(state);
    setEditor(state);
  };
  const editorDirty = editor !== null && JSON.stringify(editor) !== editorInitial.current;
  const cancelEditor = async () => {
    if (editorDirty) {
      const ok = await confirm({
        title: "放弃未保存的修改？",
        message: "这个场景的修改还没有保存，取消后会丢失。",
        confirmText: "放弃修改",
        tone: "danger",
      });
      if (!ok) return;
    }
    setEditor(null);
  };
  const [previewing, setPreviewing] = React.useState<RouteScenario | null>(null);
  const [result, setResult] = React.useState<{ scenario: RouteScenario; response: RouteScenarioApplyResponse } | null>(null);
  const fileInput = React.useRef<HTMLInputElement>(null);

  const preview = useQuery({
    queryKey: ["route-scenario-preview", previewing?.id],
    queryFn: () => previewRouteScenario(previewing!.id),
    enabled: Boolean(previewing),
    staleTime: 0,
    // 失败立刻把错误交给弹窗显示，用户手动重试；默认的 3 次退避重试会让弹窗转圈十几秒。
    retry: false,
  });

  const refreshAfterApply = () => {
    for (const key of ["route-scenarios", "route-scenario-options", "runtime-config", "routes", "accounts", "accounts-status"]) {
      queryClient.invalidateQueries({ queryKey: [key] });
    }
  };

  const save = useMutation({
    mutationFn: (state: EditorState) => {
      const req = { name: state.name.trim(), description: state.description.trim() || undefined, content: state.content };
      return state.id ? updateRouteScenario(state.id, req) : createRouteScenario(req);
    },
    onSuccess: () => {
      toast.success("场景已保存");
      setEditor(null);
      queryClient.invalidateQueries({ queryKey: ["route-scenarios"] });
    },
    onError: (error) => toast.error(`保存失败：${extractErrorMessage(error)}`),
  });
  useUnsavedChangesGuard(editorDirty && !save.isPending);

  const apply = useMutation({
    mutationFn: ({ scenario, only }: { scenario: RouteScenario; only: RouteScenarioItemKey[] }) => applyRouteScenario(scenario.id, only),
    onSuccess: (response, { scenario, only }) => {
      setPreviewing(null);
      refreshAfterApply();
      if (only.length > 0 && result) {
        const retried = new Map(response.items.map((item) => [itemKey(item), item]));
        const items = result.response.items.map((item) => retried.get(itemKey(item)) ?? item);
        const success = items.filter((item) => item.success).length;
        setResult({ scenario, response: { ...result.response, items, success, failed: items.length - success } });
      } else {
        setResult({ scenario, response });
      }
      if (response.failed === 0) toast.success(only.length > 0 ? "失败项已重试成功" : `场景“${scenario.name}”已应用`);
      else toast.error(`${response.failed} 项写入失败，可在结果里重试`);
    },
    onError: (error) => toast.error(`应用失败：${extractErrorMessage(error)}`),
  });

  const remove = async (scenario: RouteScenario) => {
    const ok = await confirm({ title: "删除场景？", message: `删除“${scenario.name}”只删除这份场景，不会改动当前路由设置。`, confirmText: "删除", tone: "danger" });
    if (!ok) return;
    try {
      await deleteRouteScenario(scenario.id);
      toast.success("场景已删除");
      queryClient.invalidateQueries({ queryKey: ["route-scenarios"] });
    } catch (error) {
      toast.error(`删除失败：${extractErrorMessage(error)}`);
    }
  };

  const startFromCurrent = async () => {
    try {
      const content = await getCurrentRouteScenarioContent();
      openEditor({ id: null, name: "", description: "", content });
    } catch (error) {
      toast.error(`读取当前设置失败：${extractErrorMessage(error)}`);
    }
  };

  const startEmpty = async () => {
    try {
      // 新建场景也要带上当前路由定义，否则应用时会把路由清空。
      const content = await getCurrentRouteScenarioContent();
      openEditor({ id: null, name: "", description: "", content: emptyContent(content.routes) });
    } catch (error) {
      toast.error(`读取当前路由失败：${extractErrorMessage(error)}`);
    }
  };

  const importFile = async (file: File) => {
    try {
      const parsed = parseScenarioFile(await file.text());
      openEditor({ id: null, ...parsed });
    } catch (error) {
      toast.error(`导入失败：${extractErrorMessage(error)}`);
    }
  };

  if (scenarios.isLoading || options.isLoading) return <LoadingState text="加载路由场景..." />;
  if (scenarios.error || options.error) return <ErrorState message={extractErrorMessage(scenarios.error ?? options.error)} />;

  const list = scenarios.data?.scenarios ?? [];
  const routes = options.data?.routes ?? [];
  const accounts = options.data?.externalAccounts ?? [];
  const activeId = scenarios.data?.activeScenarioId ?? null;

  if (editor) {
    return (
      <PageContainer>
        <PageHeader />
        <ScenarioEditor
          state={editor}
          routes={routes}
          accounts={accounts}
          saving={save.isPending}
          onChange={setEditor}
          onCancel={() => void cancelEditor()}
          onSave={() => {
            if (!editor.name.trim()) {
              toast.error("请填写场景名称");
              return;
            }
            save.mutate(editor);
          }}
        />
      </PageContainer>
    );
  }

  return (
    <PageContainer>
      <PageHeader
        actions={
          <div className="flex flex-wrap gap-2">
            <Button variant="outline" onClick={() => fileInput.current?.click()}><Upload />导入</Button>
            <Button variant="outline" onClick={startFromCurrent}><Copy />从当前设置生成</Button>
            <Button onClick={startEmpty}><Plus />新建场景</Button>
          </div>
        }
      />
      <input
        ref={fileInput}
        type="file"
        accept="application/json,.json"
        className="hidden"
        onChange={(event) => {
          const file = event.target.files?.[0];
          event.target.value = "";
          if (file) void importFile(file);
        }}
      />
      {list.length === 0 ? (
        <EmptyState
          icon={<Route />}
          title="还没有路由场景"
          description="场景是一份可以随时切换的路由与账号规则组合。可以从当前设置生成第一个场景，再按需要修改。"
          action={<Button onClick={startFromCurrent}><Copy />从当前设置生成</Button>}
        />
      ) : (
        <div className="grid gap-4 xl:grid-cols-2">
          {list.map((scenario) => {
            const active = scenario.id === activeId;
            const routeCount = scenario.content.routes.length;
            const disabledRoutes = scenario.content.routes.filter((route) => !route.enabled).length;
            const ruled = scenario.content.externalAccounts.length;
            return (
              <SectionCard
                key={scenario.id}
                title={
                  <span className="flex flex-wrap items-center gap-2">
                    {scenario.name}
                    {active && <Badge tone={scenarios.data?.activeInSync ? "success" : "warning"} dot>{scenarios.data?.activeInSync ? "使用中" : "使用中（已被手动修改）"}</Badge>}
                  </span>
                }
                description={scenario.description || undefined}
                actions={
                  <div className="flex shrink-0 gap-1">
                    <Button size="icon-xs" variant="ghost" title="编辑" aria-label={`编辑 ${scenario.name}`} onClick={() => openEditor({ id: scenario.id, name: scenario.name, description: scenario.description ?? "", content: structuredClone(scenario.content) })}><Pencil /></Button>
                    <Button size="icon-xs" variant="ghost" title="复制" aria-label={`复制 ${scenario.name}`} onClick={() => openEditor({ id: null, name: `${scenario.name} 副本`, description: scenario.description ?? "", content: structuredClone(scenario.content) })}><Copy /></Button>
                    <Button size="icon-xs" variant="ghost" title="导出 JSON" aria-label={`导出 ${scenario.name}`} onClick={() => downloadScenario(scenario)}><Download /></Button>
                    <Button size="icon-xs" variant="ghost" className="text-destructive hover:text-destructive" title="删除" aria-label={`删除 ${scenario.name}`} onClick={() => void remove(scenario)}><Trash2 /></Button>
                  </div>
                }
              >
                <div className="space-y-2 text-sm">
                  <div className="flex flex-wrap gap-1.5">
                    {sortRoutes(scenario.content.routes).map((route) => (
                      <Badge key={route.id || route.prefix} size="xs" tone={route.enabled ? "neutral" : "warning"} className="font-mono">
                        {displayRoutePrefix(route.prefix)}{route.enabled ? "" : "（停用）"}
                      </Badge>
                    ))}
                  </div>
                  <div className="text-muted-foreground">
                    {routeCount} 条路由{disabledRoutes > 0 && `（${disabledRoutes} 条停用）`} · {ruled > 0 ? `${ruled} 个账号有单独规则，其余不限制` : "所有账号不限制路由"}
                  </div>
                  <div className="flex items-center justify-between gap-2 pt-1">
                    <span className="text-xs text-muted-foreground">更新于 {formatDate(scenario.updatedAt)}</span>
                    <Button size="sm" onClick={() => setPreviewing(scenario)}><Play />应用</Button>
                  </div>
                </div>
              </SectionCard>
            );
          })}
        </div>
      )}
      <PreviewDialog
        scenario={previewing}
        preview={preview.data}
        loading={preview.isFetching}
        error={preview.error}
        onRetry={() => void preview.refetch()}
        applying={apply.isPending}
        onClose={() => setPreviewing(null)}
        onApply={() => previewing && apply.mutate({ scenario: previewing, only: [] })}
      />
      <ResultDialog
        result={result}
        retrying={apply.isPending}
        onClose={() => setResult(null)}
        onRetry={() => {
          if (!result) return;
          const only = result.response.items.filter((item) => !item.success).map(({ kind, id }) => ({ kind, id }));
          apply.mutate({ scenario: result.scenario, only });
        }}
      />
    </PageContainer>
  );
}
