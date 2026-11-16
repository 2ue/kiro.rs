import { getRouteScenarioOptions } from "@/api/route-scenarios";
import { ModalShell } from "@/components/patterns";
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
import type { Account, AccountOverrides } from "@/types/api";
import { useQuery } from "@tanstack/react-query";
import { Plus, Save } from "lucide-react";
import { useState } from "react";
import type { AccountFormDraft } from "./account-utils";
const OVERRIDES: Array<[keyof AccountOverrides, string]> = [
  ["requestTimeoutSecs", "请求超时（秒）"],
  ["streamRequestTimeoutSecs", "流式请求超时（秒）"],
  ["streamIdleTimeoutSecs", "流式空闲超时（秒）"],
  ["streamFirstOutputTimeoutSecs", "首输出超时（秒）"],
  ["usageProjectionUpliftPercent", "输入放大（%）"],
  ["usageProjectionCostFloorEnabled", "启用成本下限"],
  ["usageProjectionCostFloorMarginPercent", "成本下限余量（%）"],
  ["usageProjectionOutputUpliftMinTokens", "输出放大最小量"],
  ["usageProjectionOutputUpliftPercent", "输出放大（%）"],
];
export function AccountFormModal({
  mode,
  pool,
  open,
  draft,
  saving,
  onDraftChange,
  onClose,
  onSubmit,
  onDiscoverSupportedModels,
}: {
  mode: "create" | "edit";
  pool?: Account | null;
  open: boolean;
  draft: AccountFormDraft;
  saving: boolean;
  onDraftChange: (
    v: AccountFormDraft | ((p: AccountFormDraft) => AccountFormDraft),
  ) => void;
  onClose: () => void;
  onSubmit: () => void;
  onDiscoverSupportedModels?: () => Promise<string[]>;
}) {
  const runtime = useRuntimeConfig(),
    routes = useQuery({
      queryKey: ["route-scenario-options"],
      queryFn: getRouteScenarioOptions,
      enabled: open,
    }),
    [discovering, setDiscovering] = useState(false);
  const set = <K extends keyof AccountFormDraft>(
    key: K,
    value: AccountFormDraft[K],
  ) => onDraftChange((p) => ({ ...p, [key]: value }));
  const choice = (
    key: keyof AccountFormDraft,
    label: string,
    values: string[],
  ) => (
    <label className="space-y-2 text-sm">
      <span>{label}</span>
      <Select
        value={String(draft[key])}
        onValueChange={(v) => set(key, v as never)}
      >
        <SelectTrigger>
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {values.map((v) => (
            <SelectItem key={v} value={v}>
              {v}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </label>
  );
  return (
    <ModalShell
      open={open}
      onClose={onClose}
      title={mode === "edit" ? `编辑账号 #${pool?.id}` : "新增账号"}
      width="max-w-4xl"
      footer={
        <Button onClick={onSubmit} disabled={saving}>
          <Save />
          保存
        </Button>
      }
    >
      <div className="space-y-6">
        <div className="grid gap-4 md:grid-cols-2">
          {(
            [
              ["name", "名称"],
              ["baseUrl", "上游地址"],
              ["apiKey", mode === "edit" ? "新密钥（留空保留）" : "请求密钥"],
              ["proxyUrl", "代理地址"],
              ["tagsText", "标签（逗号分隔）"],
            ] as const
          ).map(([key, label]) => (
            <label key={key} className="space-y-2 text-sm">
              <span>{label}</span>
              <Input
                type={key === "apiKey" ? "password" : "text"}
                value={draft[key]}
                onChange={(e) => set(key, e.target.value)}
                placeholder={
                  key === "proxyUrl" && mode === "edit"
                    ? "留空继承全局，修改时输入完整地址"
                    : undefined
                }
              />
            </label>
          ))}
          {(
            [
              ["priority", "优先级"],
              ["maxConcurrentRequests", "并发上限"],
              ["costMultiplier", "账号成本倍率"],
            ] as const
          ).map(([key, label]) => (
            <label className="space-y-2 text-sm" key={key}>
              <span>{label}</span>
              <Input
                type="number"
                step={key === "costMultiplier" ? "0.01" : "1"}
                min={key === "priority" ? undefined : 0}
                value={draft[key]}
                onChange={(e) => set(key, Number(e.target.value))}
              />
            </label>
          ))}
          {choice("authType", "认证方式", ["bearer", "x_api_key"])}
          {choice("headerProfile", "头部画像", [
            "generic",
            "anthropic_passthrough",
            "claude_code_mimic",
          ])}
          {choice("wireProfile", "HTTP 画像", ["default", "http1_title_case"])}
          {choice("tlsProfile", "TLS 画像", ["default", "native_tls"])}
          {choice("usageProjectionMode", "下游 usage 口径", [
            "pass_through",
            "current_path_policy",
          ])}
          {choice("streamResponseMode", "流式响应", [
            "inherit",
            "event_passthrough",
          ])}
          {choice("preOutputStreamRetryMode", "首输出前重试", [
            "inherit",
            "enabled",
            "disabled",
            "early_commit",
          ])}
          {choice("autoDisablePolicy", "自动禁用", [
            "inherit",
            "enabled",
            "disabled",
          ])}
        </div>
        <div className="flex flex-wrap gap-6">
          {(
            [
              ["enabled", "启用账号"],
              ["appendBetaQuery", "追加 beta query"],
              ["normalizeModelVersionDots", "归一模型版本点号"],
            ] as const
          ).map(([key, label]) => (
            <label key={key} className="flex items-center gap-3 text-sm">
              <Switch
                checked={draft[key]}
                onCheckedChange={(v) => set(key, v)}
              />
              {label}
            </label>
          ))}
        </div>
        <label className="block space-y-2 text-sm">
          覆盖请求头（每行 name: value）
          <Textarea
            rows={3}
            value={draft.headerOverridesText}
            onChange={(e) => set("headerOverridesText", e.target.value)}
          />
        </label>
        <section className="space-y-4">
          <h3 className="font-semibold">账号接受的路由</h3>
          {choice("routeMode", "规则模式", [
            "allow_all",
            "allow_list",
            "deny_list",
          ])}
          <RouteSelector
            routes={routes.data?.routes ?? []}
            value={draft.routeRules}
            disabled={draft.routeMode === "allow_all"}
            onChange={(v) => set("routeRules", v)}
          />
        </section>
        <section className="space-y-4">
          <div className="flex justify-between">
            <h3 className="font-semibold">支持的模型</h3>
            <Button
              size="sm"
              variant="outline"
              disabled={discovering || !onDiscoverSupportedModels}
              onClick={async () => {
                setDiscovering(true);
                try {
                  set(
                    "supportedModelsText",
                    (await onDiscoverSupportedModels!()).join("\n"),
                  );
                } finally {
                  setDiscovering(false);
                }
              }}
            >
              <Plus />
              发现模型
            </Button>
          </div>
          <SupportedModelTagsEditor
            value={draft.supportedModelsText.split("\n").filter(Boolean)}
            onChange={(v) => set("supportedModelsText", v.join("\n"))}
          />
        </section>
        <section className="space-y-4">
          <h3 className="font-semibold">模型映射</h3>
          {choice("modelMappingMode", "映射模式", [
            "passthrough",
            "passthrough_mapping",
            "direct_mapping",
            "processed_mapping",
          ])}
          <label className="flex items-center gap-3">
            <Switch
              checked={draft.modelMappingRequireMatch}
              onCheckedChange={(v) => set("modelMappingRequireMatch", v)}
            />
            必须命中规则
          </label>
          <Textarea
            rows={5}
            value={draft.modelMappingRulesText}
            onChange={(e) => set("modelMappingRulesText", e.target.value)}
            placeholder="每行 source -> target"
          />
        </section>
        <details className="space-y-4 rounded-xl border p-4">
          <summary className="cursor-pointer font-medium">覆盖全局参数</summary>
          <div className="mt-4 grid gap-4 md:grid-cols-2">
            {OVERRIDES.map(([key, label]) => {
              const global = runtime.data?.accounts[key];
              const current = draft.overrides[key];
              return (
                <label key={key} className="space-y-2 text-sm">
                  <span>{label}</span>
                  {key === "usageProjectionCostFloorEnabled" ? (
                    <Select
                      value={current == null ? "inherit" : String(current)}
                      onValueChange={(v) =>
                        set("overrides", {
                          ...draft.overrides,
                          [key]: v === "inherit" ? null : v === "true",
                        })
                      }
                    >
                      <SelectTrigger>
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="inherit">
                          继承全局（{global ? "开启" : "关闭"}）
                        </SelectItem>
                        <SelectItem value="true">开启</SelectItem>
                        <SelectItem value="false">关闭</SelectItem>
                      </SelectContent>
                    </Select>
                  ) : (
                    <Input
                      type="number"
                      min={0}
                      value={typeof current === "number" ? current : ""}
                      placeholder={`全局：${global ?? "—"}`}
                      onChange={(e) =>
                        set("overrides", {
                          ...draft.overrides,
                          [key]:
                            e.target.value === ""
                              ? null
                              : Number(e.target.value),
                        })
                      }
                    />
                  )}
                </label>
              );
            })}
          </div>
        </details>
        <label className="block space-y-2 text-sm">
          备注
          <Textarea
            value={draft.notes}
            onChange={(e) => set("notes", e.target.value)}
          />
        </label>
      </div>
    </ModalShell>
  );
}
