import { getRouteScenarioOptions } from "@/api/route-scenarios";
import { Field, FieldGrid, ModalShell } from "@/components/patterns";
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
import type { Account, AccountOverrides } from "@/types/api";
import { useQuery } from "@tanstack/react-query";
import { Plus, Save } from "lucide-react";
import { useState, type ReactNode } from "react";
import type { AccountFormDraft } from "./account-utils";

const OVERRIDES: Array<[keyof AccountOverrides, string, string]> = [
  ["requestTimeoutSecs", "请求超时", "秒"],
  ["streamRequestTimeoutSecs", "流式请求超时", "秒"],
  ["streamIdleTimeoutSecs", "流式空闲超时", "秒"],
  ["streamFirstOutputTimeoutSecs", "首输出等待上限", "秒"],
  ["usageProjectionUpliftPercent", "输入补偿", "%"],
  ["usageProjectionCostFloorEnabled", "成本底线", ""],
  ["usageProjectionCostFloorMarginPercent", "成本底线余量", "%"],
  ["usageProjectionOutputUpliftMinTokens", "输出补偿阈值", "token"],
  ["usageProjectionOutputUpliftPercent", "输出补偿", "%"],
];

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
}) {
  const runtime = useRuntimeConfig();
  const routes = useQuery({
    queryKey: ["route-scenario-options"],
    queryFn: getRouteScenarioOptions,
    enabled: open,
  });
  const [discovering, setDiscovering] = useState(false);
  const set = <K extends keyof AccountFormDraft>(key: K, value: AccountFormDraft[K]) =>
    onDraftChange((p) => ({ ...p, [key]: value }));

  const text = (key: "name" | "baseUrl" | "apiKey" | "proxyUrl" | "tagsText", label: string, extra?: { placeholder?: string; required?: boolean; description?: string; password?: boolean }) => (
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
      title={mode === "edit" ? `编辑账号 #${account?.id}` : "新增账号"}
      description="上游地址与密钥是必填项，其余保持默认即可开始使用。"
      width="max-w-4xl"
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={saving}>取消</Button>
          <Button onClick={onSubmit} disabled={saving}>
            <Save />
            保存
          </Button>
        </>
      }
    >
      <div className="space-y-8">
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
            {text("proxyUrl", "代理地址", { placeholder: "留空继承全局" })}
            {text("tagsText", "标签", { placeholder: "多个标签用逗号分隔" })}
            {toggle("enabled", "启用账号", "关闭后不参与调度，配置保留")}
          </FieldGrid>
        </Group>

        <Group title="调度设置" description="这些设置只影响当前账号，不改变全局排队和冷却策略。">
          <FieldGrid min="16rem">
            {number("priority", "优先级", { description: "数字越小越靠前；同优先级再按容量和状态分配" })}
            {number("maxConcurrentRequests", "单账号最大并发", { min: 1, unit: "个请求", description: "当前账号同时处理的最大请求数，不是 RPM" })}
            {number("costMultiplier", "成本倍率", { step: "0.01", min: 0, unit: "×", description: "账号成本 = 上游原始成本 × 倍率" })}
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
              disabled={discovering || !onDiscoverSupportedModels}
              onClick={async () => {
                setDiscovering(true);
                try {
                  set("supportedModelsText", (await onDiscoverSupportedModels!()).join("\n"));
                } finally {
                  setDiscovering(false);
                }
              }}
            >
              <Plus />
              从上游发现
            </Button>
          }
        >
          <SupportedModelTagsEditor
            value={draft.supportedModelsText.split("\n").filter(Boolean)}
            onChange={(v) => set("supportedModelsText", v.join("\n"))}
          />
        </Group>

        <Group title="模型处理" description="控制当前账号发出请求时的模型名称处理方式。">
          <FieldGrid min="16rem">
            {choice("modelMappingMode", "映射模式", MODEL_MAPPING_MODE_OPTIONS)}
            {toggle("modelMappingRequireMatch", "必须命中映射", "未命中任何规则时直接拒绝，而不是用原模型名")}
            {toggle("normalizeModelVersionDots", "未命中时点号转横杠", "未命中映射规则时，自动将版本号点号替换为横杠")}
          </FieldGrid>
          <Field label="映射规则" htmlFor="account-modelMappingRules" description="每行一条：claude-sonnet-4-5-20250929 -> claude-sonnet-4.5">
            <Textarea
              id="account-modelMappingRules"
              rows={5}
              value={draft.modelMappingRulesText}
              onChange={(e) => set("modelMappingRulesText", e.target.value)}
            />
          </Field>
        </Group>

        <details className="rounded-xl border">
          <summary className="cursor-pointer select-none px-4 py-3 text-sm font-semibold">
            覆盖全局参数
            <span className="ml-2 text-xs font-normal text-muted-foreground">留空表示沿用全局值</span>
          </summary>
          <div className="border-t px-4 pb-4 pt-4">
            <FieldGrid min="15rem">
              {OVERRIDES.map(([key, label, unit]) => {
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
