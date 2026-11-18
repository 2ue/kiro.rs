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
  ["streamFirstOutputTimeoutSecs", "首输出超时", "秒"],
  ["usageProjectionUpliftPercent", "输入放大", "%"],
  ["usageProjectionCostFloorEnabled", "成本下限", ""],
  ["usageProjectionCostFloorMarginPercent", "成本下限余量", "%"],
  ["usageProjectionOutputUpliftMinTokens", "输出放大最小量", "token"],
  ["usageProjectionOutputUpliftPercent", "输出放大", "%"],
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

  const number = (key: "priority" | "maxConcurrentRequests" | "costMultiplier", label: string, extra: { step?: string; min?: number; description?: string }) => (
    <Field label={label} htmlFor={`account-${key}`} description={extra.description}>
      <Input
        id={`account-${key}`}
        type="number"
        step={extra.step ?? "1"}
        min={extra.min}
        value={draft[key]}
        onChange={(e) => set(key, Number(e.target.value))}
      />
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
    <Field label={label} htmlFor={`account-${key}`} description={description} inline>
      <Switch id={`account-${key}`} checked={draft[key]} onCheckedChange={(v) => set(key, v)} />
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
        <Group title="基本信息">
          <FieldGrid min="18rem">
            {text("name", "名称", { required: true })}
            {text("baseUrl", "上游地址", { required: true, placeholder: "https://api.example.com" })}
            {text("apiKey", mode === "edit" ? "密钥" : "密钥", {
              required: mode === "create",
              password: true,
              placeholder: mode === "edit" ? "留空保留现有密钥" : undefined,
            })}
            {choice("authType", "密钥发送方式", ACCOUNT_AUTH_TYPE_OPTIONS)}
            {text("proxyUrl", "代理地址", { placeholder: "留空继承全局代理" })}
            {text("tagsText", "标签", { placeholder: "多个标签用逗号分隔" })}
          </FieldGrid>
          <div className="flex flex-wrap gap-x-8 gap-y-2">
            {toggle("enabled", "启用账号")}
          </div>
        </Group>

        <Group title="调度与计费">
          <FieldGrid min="14rem">
            {number("priority", "优先级", { description: "数字越小越先被选中" })}
            {number("maxConcurrentRequests", "并发上限", { min: 1, description: "同时在飞的请求数" })}
            {number("costMultiplier", "成本倍率", { step: "0.01", min: 0, description: "账号成本 = 上游原始成本 × 倍率" })}
          </FieldGrid>
          <FieldGrid min="18rem">
            {choice("usageProjectionMode", "下游 usage 口径", USAGE_PROJECTION_MODE_OPTIONS)}
            {choice("autoDisablePolicy", "连续失败自动停用", AUTO_DISABLE_POLICY_OPTIONS)}
          </FieldGrid>
        </Group>

        <Group title="上游请求方式" description="默认值适用于绝大多数中转站；只有上游明确要求时才需要调整。">
          <FieldGrid min="18rem">
            {choice("headerProfile", "请求头风格", ACCOUNT_HEADER_PROFILE_OPTIONS)}
            {choice("wireProfile", "HTTP 传输", ACCOUNT_WIRE_PROFILE_OPTIONS)}
            {choice("tlsProfile", "TLS 实现", ACCOUNT_TLS_PROFILE_OPTIONS)}
            {choice("streamResponseMode", "流式响应处理", ACCOUNT_STREAM_RESPONSE_OPTIONS)}
            {choice("preOutputStreamRetryMode", "首输出前失败换号", STREAM_RETRY_MODE_OPTIONS)}
          </FieldGrid>
          <div className="flex flex-wrap gap-x-8 gap-y-2">
            {toggle("appendBetaQuery", "在 URL 上追加 beta 参数", "部分上游要求 ?beta=true")}
            {toggle("normalizeModelVersionDots", "模型版本号点号归一", "把 claude-3.5 这类写法转成 claude-3-5")}
          </div>
          <Field label="额外请求头" htmlFor="account-headerOverrides" description="每行一个，格式 name: value；会覆盖同名头">
            <Textarea
              id="account-headerOverrides"
              rows={3}
              value={draft.headerOverridesText}
              onChange={(e) => set("headerOverridesText", e.target.value)}
            />
          </Field>
        </Group>

        <Group title="接受的路由">
          <FieldGrid min="18rem">
            {choice("routeMode", "规则模式", RULE_MODE_OPTIONS)}
          </FieldGrid>
          <RouteSelector
            routes={routes.data?.routes ?? []}
            value={draft.routeRules}
            disabled={draft.routeMode === "allow_all"}
            onChange={(v) => set("routeRules", v)}
          />
        </Group>

        <Group
          title="支持的模型"
          description="留空表示不限制；填了列表后只有列表内的模型会调度到这个账号。"
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

        <Group title="模型映射">
          <FieldGrid min="18rem">
            {choice("modelMappingMode", "映射模式", MODEL_MAPPING_MODE_OPTIONS)}
          </FieldGrid>
          <div className="flex flex-wrap gap-x-8 gap-y-2">
            {toggle("modelMappingRequireMatch", "未命中规则时拒绝请求")}
          </div>
          <Field label="映射规则" htmlFor="account-modelMappingRules" description="每行一条，格式 source -> target">
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
            <FieldGrid min="14rem">
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
                  <Field key={key} label={unit ? `${label}（${unit}）` : label} htmlFor={id}>
                    <Input
                      id={id}
                      type="number"
                      min={0}
                      value={typeof current === "number" ? current : ""}
                      placeholder={`全局：${global ?? "—"}`}
                      onChange={(e) =>
                        set("overrides", {
                          ...draft.overrides,
                          [key]: e.target.value === "" ? null : Number(e.target.value),
                        })
                      }
                    />
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
