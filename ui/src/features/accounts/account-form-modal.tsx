import { getRouteScenarioOptions } from "@/api/route-scenarios";
import { Callout, Field, FieldGrid, ModalShell, useConfirm } from "@/components/patterns";
import { TagsField } from "@/components/patterns/tags-field";
import {
  Button,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Switch,
  Textarea,
} from "@/components/ui";
import { RouteSelector } from "@/features/route-scenarios/route-selector";
import { SupportedModelTagsEditor } from "@/features/shared/supported-model-tags";
import { useRuntimeConfig } from "@/hooks/use-runtime-config";
import {
  ACCOUNT_AUTH_TYPE_OPTIONS,
  ACCOUNT_HEADER_PROFILE_OPTIONS,
  ACCOUNT_STREAM_RESPONSE_OPTIONS,
  ACCOUNT_TLS_PROFILE_OPTIONS,
  ACCOUNT_WIRE_PROFILE_OPTIONS,
  AUTO_DISABLE_POLICY_OPTIONS,
  MODEL_MAPPING_MODE_OPTIONS,
  RULE_MODE_OPTIONS,
  STREAM_RETRY_MODE_OPTIONS,
  USAGE_PROJECTION_MODE_OPTIONS,
  type EnumOption,
} from "@/lib/enum-labels";
import type { Account } from "@/types/api";
import { useQuery } from "@tanstack/react-query";
import { Eraser, ListPlus, Plus, Save, Upload } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Link } from "react-router-dom";
import { toast } from "sonner";
import { cn } from "@/lib/utils";
import {
  ACCOUNT_OVERRIDE_FIELDS,
  PROXY_MASK_HINT,
  appendModelMappingPreset,
  appendModelMappingPresets,
  appendModelMappingRules,
  modelMappingPresetClass,
  modelMappingPresetsForMode,
  hasMaskedProxyCredentials,
  parseModelMappingRules,
  type AccountFormDraft,
  type AccountModelMappingPreset,
} from "./account-utils";
import { RUNTIME_LINKS } from "./account-usage";

/** 快捷映射标签：点一下把这条规则追加到映射规则里，已存在的源模型跳过。与 main 外部池表单一致。 */
function ModelMappingPresetTags({ presets, disabled, onSelect }: {
  presets: AccountModelMappingPreset[]
  disabled?: boolean
  onSelect: (preset: AccountModelMappingPreset) => void
}) {
  if (!presets.length) return null
  return (
    <div className="flex flex-wrap gap-2">
      {presets.map((preset) => (
        <button
          key={`${preset.source}->${preset.target}`}
          type="button"
          className={cn("rounded-lg px-3 py-1 text-xs transition-colors disabled:cursor-not-allowed disabled:opacity-50", modelMappingPresetClass(preset.tone))}
          title={`${preset.source} -> ${preset.target}`}
          disabled={disabled}
          onClick={() => onSelect(preset)}
        >
          + {preset.label}
        </button>
      ))}
    </div>
  )
}

/** 分组标题：同一视觉层级，左侧竖线对齐统计卡的风格。 */
function Group({ title, description, actions, children }: { title: string; description?: string; actions?: ReactNode; children: ReactNode }) {
  return (
    <section className="space-y-4">
      <div className="flex items-start justify-between gap-3 border-b pb-2">
        <div>
          <h3 className="text-sm font-semibold">{title}</h3>
          {description && <p className="mt-0.5 text-xs text-muted-foreground">{description}</p>}
        </div>
        {actions}
      </div>
      {children}
    </section>
  );
}

export function AccountFormModal({
  mode,
  account,
  open,
  draft,
  saving,
  onDraftChange,
  onClose,
  onSubmit,
  onDiscoverSupportedModels,
  copiedFrom,
  onDeleteCopy,
  tagOptions = [],
}: {
  mode: "create" | "edit";
  account?: Account | null;
  open: boolean;
  draft: AccountFormDraft;
  saving: boolean;
  onDraftChange: (v: AccountFormDraft | ((p: AccountFormDraft) => AccountFormDraft)) => void;
  onClose: () => void;
  onSubmit: () => void;
  onDiscoverSupportedModels?: () => Promise<string[]>;
  /** 由“复制”打开时的来源账号：显示副本提示和“删除这个副本”。 */
  copiedFrom?: { id: number; name: string } | null;
  onDeleteCopy?: () => void;
  /** 已有标签，标签输入框里点一下即可添加。 */
  tagOptions?: string[];
}) {
  const runtime = useRuntimeConfig();
  const routes = useQuery({
    queryKey: ["route-scenario-options"],
    queryFn: getRouteScenarioOptions,
    enabled: open,
  });
  const [discoveredModels, setDiscoveredModels] = useState<string[]>([])
  const [discovering, setDiscovering] = useState(false);
  const discoveryGeneration = useRef(0);
  const [quickImportText, setQuickImportText] = useState("");
  const confirm = useConfirm();
  // 异步操作（从上游发现模型）返回后要拿最新草稿比较，不能用点击时闭包里的旧值。
  const draftRef = useRef(draft);
  draftRef.current = draft;
  const set = <K extends keyof AccountFormDraft>(key: K, value: AccountFormDraft[K]) =>
    onDraftChange((p) => ({ ...p, [key]: value }));
  const mappingPresets = useMemo(() => modelMappingPresetsForMode(draft.modelMappingMode), [draft.modelMappingMode]);
  useEffect(() => {
    discoveryGeneration.current += 1;
    setDiscovering(false);
    if (!open) setQuickImportText("");
    return () => { discoveryGeneration.current += 1; };
  }, [open, account?.id]);

  const addMappingPreset = (preset: AccountModelMappingPreset) => {
    const result = appendModelMappingPreset(draft.modelMappingRulesText, preset);
    onDraftChange((p) => ({ ...p, modelMappingRulesText: result.text }));
    if (result.added) toast.success("模型映射规则已添加");
    else toast.info("该模型映射规则已存在");
  };
  const addAllMappingPresets = () => {
    const result = appendModelMappingPresets(draft.modelMappingRulesText, mappingPresets);
    onDraftChange((p) => ({ ...p, modelMappingRulesText: result.text }));
    if (result.added > 0) toast.success(`已添加 ${result.added} 条模型映射规则`);
    else toast.info("快捷模型映射规则都已存在");
  };
  const importMappingRules = () => {
    const rules = parseModelMappingRules(quickImportText);
    if (!rules.length) {
      toast.error("没有可导入的模型映射规则");
      return;
    }
    const result = appendModelMappingRules(draft.modelMappingRulesText, rules);
    onDraftChange((p) => ({ ...p, modelMappingRulesText: result.text }));
    if (result.added > 0) {
      toast.success(`已导入 ${result.added} 条模型映射规则`);
      setQuickImportText("");
    } else {
      toast.info("导入的模型映射规则都已存在");
    }
  };
  const clearMappingRules = async () => {
    const count = parseModelMappingRules(draft.modelMappingRulesText).length;
    if (!count) return;
    const ok = await confirm({
      title: "清空映射规则？",
      message: `会清空当前表单里的 ${count} 条映射规则，保存后才生效。`,
      confirmText: "清空",
      tone: "danger",
    });
    if (ok) set("modelMappingRulesText", "");
  };

  const text = (key: "name" | "baseUrl" | "apiKey" | "proxyUrl", label: string, extra?: { placeholder?: string; required?: boolean; description?: string; password?: boolean }) => (
    <Field label={label} htmlFor={`account-${key}`} required={extra?.required} description={extra?.description}>
      <Input
        id={`account-${key}`}
        type={extra?.password ? "password" : "text"}
        autoComplete={extra?.password ? "new-password" : "off"}
        value={draft[key]}
        placeholder={extra?.placeholder}
        onChange={(e) => set(key, e.target.value)}
      />
    </Field>
  );

  const number = (key: "priority" | "maxConcurrentRequests" | "costMultiplier", label: string, extra: { step?: string; min?: number; description?: string; unit?: string }) => (
    <Field label={label} htmlFor={`account-${key}`} description={extra.description}>
      <div className="flex min-h-9 items-center gap-2">
        <Input
          id={`account-${key}`}
          type="number"
          inputMode="decimal"
          className="w-32 shrink-0 tabular-nums"
          step={extra.step ?? "1"}
          min={extra.min}
          value={draft[key]}
          onChange={(e) => set(key, Number(e.target.value))}
        />
        {extra.unit && <span className="shrink-0 text-xs text-muted-foreground">{extra.unit}</span>}
      </div>
    </Field>
  );

  const choice = (key: keyof AccountFormDraft, label: string, options: EnumOption[]) => {
    const current = options.find((option) => option.value === String(draft[key]));
    return (
      <Field label={label} htmlFor={`account-${key}`} description={current?.description}>
        <Select value={String(draft[key])} onValueChange={(v) => set(key, v as never)}>
          <SelectTrigger id={`account-${key}`}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {options.map((option) => (
              <SelectItem key={option.value} value={option.value}>
                {option.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </Field>
    );
  };

  const toggle = (key: "enabled" | "appendBetaQuery" | "normalizeModelVersionDots" | "modelMappingRequireMatch", label: string, description?: string) => (
    <Field label={label} htmlFor={`account-${key}`} description={description}>
      <div className="flex min-h-9 items-center gap-2">
        <Switch id={`account-${key}`} checked={draft[key]} onCheckedChange={(v) => set(key, v)} />
        <span className="text-xs text-muted-foreground">{draft[key] ? "开启" : "关闭"}</span>
      </div>
    </Field>
  );

  return (
    <ModalShell
      open={open}
      onClose={onClose}
      title={copiedFrom ? `编辑副本 #${account?.id}` : mode === "edit" ? `编辑账号 #${account?.id}` : "新增账号"}
      description="上游地址与密钥是必填项，其余保持默认即可开始使用。"
      width="max-w-4xl"
      footer={
        <>
          {copiedFrom && onDeleteCopy && (
            <Button
              variant="ghost"
              className="mr-auto text-destructive hover:bg-destructive/10 hover:text-destructive"
              onClick={onDeleteCopy}
              disabled={saving}
            >
              删除这个副本
            </Button>
          )}
          <Button variant="outline" onClick={onClose} disabled={saving}>{copiedFrom ? "关闭（保留停用的副本）" : "取消"}</Button>
          <Button onClick={onSubmit} disabled={saving || discovering}>
            <Save />
            保存
          </Button>
        </>
      }
    >
      <div className="space-y-8">
        {copiedFrom && (
          <Callout tone="warning">
            这是从 #{copiedFrom.id}「{copiedFrom.name}」复制的副本，已带上原账号的密钥和全部配置，当前为停用状态。
            副本和原账号共用同一个上游 Key，同时启用会叠加这个 Key 上的并发；改好名称、密钥或地址后再打开启用。
            副本不在已保存的路由场景里，以后应用旧场景时它的路由规则会被重置为全部允许。
          </Callout>
        )}
        <Group title="连接信息" description="系统会使用这里的服务地址和 Key 连接账号。">
          <FieldGrid min="16rem">
            {text("name", "名称", { required: true })}
            {text("baseUrl", "服务地址", { required: true, placeholder: "https://api.example.com", description: "填写服务地址即可，通常不需要带具体请求路径" })}
            {text("apiKey", "密钥", {
              required: mode === "create",
              password: true,
              placeholder: mode === "edit" ? "留空保留现有密钥" : undefined,
            })}
            {choice("authType", "认证方式", ACCOUNT_AUTH_TYPE_OPTIONS)}
            {text("proxyUrl", "代理地址", {
              placeholder: "留空继承全局",
              description: mode === "edit" && hasMaskedProxyCredentials(account?.proxyUrl)
                ? PROXY_MASK_HINT
                : mode === "create"
                  ? "留空继承全局；支持 http、https、socks5、socks5h，需填写完整的用户名和密码（不能用 *** 占位）"
                  : "留空继承全局；支持 http、https、socks5、socks5h",
            })}
            {toggle("enabled", "启用账号", "关闭后不参与调度，配置保留")}
          </FieldGrid>
          <Field label="标签" htmlFor="account-tags" description="回车或逗号添加，点下面的已有标签直接加上；用于筛选和批量操作">
            <TagsField id="account-tags" value={draft.tags} options={tagOptions} onChange={(v) => set("tags", v)} disabled={saving} />
          </Field>
        </Group>

        <Group title="调度设置" description="这些设置只影响当前账号，不改变全局排队和冷却策略。">
          <FieldGrid min="16rem">
            {number("priority", "优先级", { description: "数字越小越靠前；同优先级再按容量和状态分配" })}
            {number("maxConcurrentRequests", "单账号最大并发", { min: 1, unit: "个请求", description: "当前账号同时处理的最大请求数，不是 RPM" })}
            {number("costMultiplier", "成本倍率", { step: "0.01", min: 0, unit: "×", description: "账号成本 = 上游原始成本 × 倍率" })}
            <Field label="原始用量记录与成本计算" description="最终值取上游最后明确返回的字段；最大值计算沿用原有成本口径。仅影响后续请求的原始统计。">
              <Select value={draft.rawUsageSource} disabled={saving} onValueChange={(v) => set("rawUsageSource", v as AccountFormDraft['rawUsageSource'])}>
                <SelectTrigger><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="legacy_max">最大值记录，最大值计算（默认）</SelectItem>
                  <SelectItem value="upstream_last">最终值记录，最终值计算</SelectItem>
                  <SelectItem value="upstream_last_max_cost">最终值记录，最大值计算</SelectItem>
                </SelectContent>
              </Select>
            </Field>
            {choice("usageProjectionMode", "下游 usage", USAGE_PROJECTION_MODE_OPTIONS)}
            {choice("autoDisablePolicy", "自动禁用策略", AUTO_DISABLE_POLICY_OPTIONS)}
          </FieldGrid>
        </Group>

        <Group title="上游 Header 兼容" description="控制转发到该账号时的请求头、Claude Code 指纹和 beta 查询参数。">
          <FieldGrid min="16rem">
            {choice("headerProfile", "Header profile", ACCOUNT_HEADER_PROFILE_OPTIONS)}
            {choice("wireProfile", "Wire profile", ACCOUNT_WIRE_PROFILE_OPTIONS)}
            {choice("tlsProfile", "TLS profile", ACCOUNT_TLS_PROFILE_OPTIONS)}
            {toggle("appendBetaQuery", "URL 追加 beta=true", "部分上游要求；选 Anthropic allowlist 或 Claude Code mimic 时自动追加")}
            {choice("streamResponseMode", "SSE 转发", ACCOUNT_STREAM_RESPONSE_OPTIONS)}
            {choice("preOutputStreamRetryMode", "首输出前流式恢复", STREAM_RETRY_MODE_OPTIONS)}
          </FieldGrid>
          <Field label="Header 覆盖" htmlFor="account-headerOverrides" description="每行一个 name: value；敏感头如 authorization、x-api-key、cookie 会被后端拒绝">
            <Textarea
              id="account-headerOverrides"
              rows={3}
              value={draft.headerOverridesText}
              onChange={(e) => set("headerOverridesText", e.target.value)}
            />
          </Field>
        </Group>

        <Group title="入口路由" description="只限制当前账号能承接哪些入口。">
          <div className="grid gap-4 md:grid-cols-[16rem_1fr]">
            {choice("routeMode", "路由模式", RULE_MODE_OPTIONS)}
            <Field label="路由" description={draft.routeMode === "allow_all" ? "不限制时无需选择" : "只能选路由页里已有的入口"}>
              <RouteSelector
                routes={routes.data?.routes ?? []}
                value={draft.routeRules}
                disabled={draft.routeMode === "allow_all"}
                onChange={(v) => set("routeRules", v)}
              />
            </Field>
          </div>
        </Group>

        <Group
          title="调度资格"
          description="只决定该账号是否允许承接某些模型；空列表表示不限制。不改变请求体里的 model，也不影响模型映射规则。"
          actions={
            <Button
              size="sm"
              variant="outline"
              disabled={saving || discovering || !onDiscoverSupportedModels}
              onClick={async () => {
                const generation = ++discoveryGeneration.current;
                const connection = JSON.stringify([draftRef.current.baseUrl, draftRef.current.apiKey, draftRef.current.authType]);
                setDiscovering(true);
                try {
                  const found = await onDiscoverSupportedModels!();
                  if (generation !== discoveryGeneration.current) return;
                  if (connection !== JSON.stringify([draftRef.current.baseUrl, draftRef.current.apiKey, draftRef.current.authType])) {
                    toast.info("连接信息已变更，请重新获取模型");
                    return;
                  }
                  const current = draftRef.current.supportedModelsText.split("\n").filter(Boolean);
                  const removed = current.filter((m) => !found.includes(m));
                  // 表单里已经填了列表且发现结果会去掉其中一些：先确认再覆盖，避免手填的资格被悄悄抹掉。
                  if (draftRef.current.modelFilterMode === "allowlist" && removed.length > 0) {
                    const ok = await confirm({
                      title: "用上游结果覆盖当前列表？",
                      message: `上游返回 ${found.length} 个模型，会移除当前列表里的 ${removed.length} 个：${removed.join("、")}。保存后该账号不再承接这些模型。`,
                      confirmText: "覆盖",
                      tone: "danger",
                    });
                    if (!ok || generation !== discoveryGeneration.current) return;
                  }
                  setDiscoveredModels(found);
                  if (draftRef.current.modelFilterMode === "allowlist") set("supportedModelsText", found.join("\n"));
                  toast.success(`已获取 ${found.length} 个模型，保存后生效`);
                } catch {
                  // 发现失败的提示由调用方给出，这里只负责不改表单。
                } finally {
                  if (generation === discoveryGeneration.current) setDiscovering(false);
                }
              }}
            >
              <Plus />
              从上游获取
            </Button>
          }
        >
          <SupportedModelTagsEditor
            mode={draft.modelFilterMode}
            onModeChange={(mode) => set("modelFilterMode", mode)}
            options={discoveredModels}
            value={draft.supportedModelsText.split("\n").filter(Boolean)}
            disabled={saving || discovering}
            onChange={(v) => set("supportedModelsText", v.join("\n"))}
          />
        </Group>

        <Group title="模型处理" description="控制当前账号发出请求时的模型名称处理方式。">
          <FieldGrid min="16rem">
            {choice("modelMappingMode", "映射模式", MODEL_MAPPING_MODE_OPTIONS)}
            {toggle("modelMappingRequireMatch", "必须命中映射", "未命中任何规则时直接拒绝，而不是用原模型名")}
            {toggle("normalizeModelVersionDots", "未命中时点号转横杠", "未命中映射规则时，自动将版本号点号替换为横杠")}
          </FieldGrid>
          {draft.modelMappingMode !== "passthrough" && (
            <>
              <Field label="映射规则" htmlFor="account-modelMappingRules" description="每行一条：claude-sonnet-4-5-20250929 -> claude-sonnet-4.5">
                <div className="space-y-2">
                  <div className="flex flex-wrap justify-end gap-2">
                    <Button type="button" size="xs" variant="ghost" disabled={saving || !mappingPresets.length} onClick={addAllMappingPresets}>
                      <ListPlus />
                      全部添加
                    </Button>
                    <Button type="button" size="xs" variant="ghost" disabled={saving || !draft.modelMappingRulesText.trim()} onClick={() => void clearMappingRules()}>
                      <Eraser />
                      清空
                    </Button>
                  </div>
                  <Textarea
                    id="account-modelMappingRules"
                    rows={5}
                    value={draft.modelMappingRulesText}
                    onChange={(e) => set("modelMappingRulesText", e.target.value)}
                  />
                </div>
              </Field>
              <ModelMappingPresetTags presets={mappingPresets} disabled={saving} onSelect={addMappingPreset} />
              <Field label="快捷导入" htmlFor="account-modelMappingQuickImport" description="粘贴多行 source -> target（也认 =>、→、=），点击解析导入后追加到上方规则，已存在的源模型跳过。">
                <div className="space-y-2">
                  <Textarea
                    id="account-modelMappingQuickImport"
                    rows={3}
                    value={quickImportText}
                    placeholder={"claude-sonnet-4-5 -> claude-sonnet-4.5\nclaude-opus-4-6 -> claude-opus-4.6"}
                    onChange={(e) => setQuickImportText(e.target.value)}
                  />
                  <div className="flex justify-end">
                    <Button type="button" size="xs" variant="outline" disabled={saving || !quickImportText.trim()} onClick={importMappingRules}>
                      <Upload />
                      解析导入
                    </Button>
                  </div>
                </div>
              </Field>
            </>
          )}
        </Group>

        <details className="rounded-xl border">
          <summary className="cursor-pointer select-none px-4 py-3 text-sm font-semibold">
            覆盖全局参数
            <span className="ml-2 text-xs font-normal text-muted-foreground">留空表示沿用全局值</span>
          </summary>
          <div className="space-y-3 border-t px-4 pb-4 pt-4">
            <p className="text-xs text-muted-foreground">
              全局值在运行配置 → 账号池的
              <Link to={RUNTIME_LINKS.timeouts} target="_blank" rel="noreferrer" className="mx-1 text-primary underline-offset-4 hover:underline">超时与流式</Link>
              和
              <Link to={RUNTIME_LINKS.uplift} target="_blank" rel="noreferrer" className="mx-1 text-primary underline-offset-4 hover:underline">用量补偿</Link>
              分组统一保存（新标签页打开，不影响这里未保存的修改）。
            </p>
            <FieldGrid min="15rem">
              {ACCOUNT_OVERRIDE_FIELDS.map(([key, label, unit]) => {
                const global = runtime.data?.accounts[key];
                const current = draft.overrides[key];
                const id = `account-override-${key}`;
                if (key === "usageProjectionCostFloorEnabled") {
                  return (
                    <Field key={key} label={label} htmlFor={id}>
                      <Select
                        value={current == null ? "inherit" : String(current)}
                        onValueChange={(v) =>
                          set("overrides", { ...draft.overrides, [key]: v === "inherit" ? null : v === "true" })
                        }
                      >
                        <SelectTrigger id={id}>
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          <SelectItem value="inherit">继承全局（{global ? "开启" : "关闭"}）</SelectItem>
                          <SelectItem value="true">开启</SelectItem>
                          <SelectItem value="false">关闭</SelectItem>
                        </SelectContent>
                      </Select>
                    </Field>
                  );
                }
                return (
                  <Field key={key} label={label} htmlFor={id} description={`留空沿用全局 ${global ?? "—"}${unit ? ` ${unit}` : ""}`}>
                    <div className="flex min-h-9 items-center gap-2">
                      <Input
                        id={id}
                        type="number"
                        inputMode="decimal"
                        className="w-32 shrink-0 tabular-nums"
                        min={0}
                        value={typeof current === "number" ? current : ""}
                        placeholder={String(global ?? "")}
                        onChange={(e) =>
                          set("overrides", {
                            ...draft.overrides,
                            [key]: e.target.value === "" ? null : Number(e.target.value),
                          })
                        }
                      />
                      {unit && <span className="shrink-0 text-xs text-muted-foreground">{unit}</span>}
                    </div>
                  </Field>
                );
              })}
            </FieldGrid>
          </div>
        </details>

        <Field label="备注" htmlFor="account-notes">
          <Textarea id="account-notes" rows={2} value={draft.notes} onChange={(e) => set("notes", e.target.value)} />
        </Field>
      </div>
    </ModalShell>
  );
}
