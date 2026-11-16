import {
  ErrorState,
  LoadingState,
  PageContainer,
  PageHeader,
} from "@/components/patterns";
import { ConfigFields } from "@/components/patterns/config-fields";
import {
  Button,
  Input,
  Switch,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from "@/components/ui";
import { useModelCapabilities } from "@/hooks/use-models";
import {
  useRuntimeConfig,
  useUpdateRuntimeConfig,
} from "@/hooks/use-runtime-config";
import { extractErrorMessage } from "@/lib/utils";
import type { AccountsConfig, RuntimeConfig } from "@/types/api";
import { Save } from "lucide-react";
import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { toast } from "sonner";
import { ModelMappingSection } from "./runtime-sections";
const accountSections: Array<[string, string, (key: string) => boolean]> = [
  [
    "capacity",
    "容量与重试",
    (k) =>
      /^(enabled|globalMax|maxQueued|capacityMode|dispatch|retry|samePool|transient)/.test(
        k,
      ),
  ],
  [
    "quality",
    "质量调度",
    (k) => /^(quality|degrade|probation|maxProbation|probe|recovery)/.test(k),
  ],
  ["disable", "自动禁用", (k) => k.startsWith("autoDisable")],
  ["cooldown", "冷却", (k) => /Cooldown/.test(k)],
  [
    "timeouts",
    "超时与流式",
    (k) => /Timeout|streamResponseMode|streamPreOutput/.test(k),
  ],
  ["uplift", "用量放大", (k) => k.startsWith("usageProjection")],
  ["diagnostics", "诊断", (k) => /Debug/.test(k)],
];
export function RuntimePage() {
  const query = useRuntimeConfig(),
    update = useUpdateRuntimeConfig(),
    models = useModelCapabilities(),
    [draft, setDraft] = useState<RuntimeConfig | null>(null);
  useEffect(() => {
    if (query.data) setDraft(structuredClone(query.data));
  }, [query.data]);
  if (query.isLoading) return <LoadingState />;
  if (query.error || !draft)
    return (
      <ErrorState
        title="读取运行配置失败"
        action={
          <Button variant="outline" onClick={() => query.refetch()}>
            重试
          </Button>
        }
      />
    );
  const set = <K extends keyof RuntimeConfig>(
    key: K,
    value: RuntimeConfig[K],
  ) => setDraft({ ...draft, [key]: value });
  const save = async () => {
    try {
      const {
        cachePolicy: _cache,
        reportedUsage: _usage,
        promptInjection: _injection,
        ...runtime
      } = draft;
      await update.mutateAsync(runtime);
      toast.success("运行配置已保存");
    } catch (error) {
      toast.error(extractErrorMessage(error));
    }
  };
  return (
    <PageContainer>
      <PageHeader
        actions={
          <Button onClick={save} disabled={update.isPending}>
            <Save />
            保存配置
          </Button>
        }
      />
      <Tabs defaultValue="admission">
        <TabsList className="flex-wrap">
          {[
            ["admission", "请求准入"],
            ["accounts", "账号池"],
            ["models", "模型解析"],
            ["proxy", "代理与 TLS"],
            ["retention", "数据保留"],
            ["features", "路由功能默认参数"],
          ].map(([key, label]) => (
            <TabsTrigger key={key} value={key}>
              {label}
            </TabsTrigger>
          ))}
        </TabsList>
        <TabsContent value="admission">
          <ConfigFields
            value={{
              requestAdmission: draft.requestAdmission,
              inferenceUpstreamMaxAttempts: draft.inferenceUpstreamMaxAttempts,
              streamPreOutputHoldSecs: draft.streamPreOutputHoldSecs,
            }}
            onChange={(next) => setDraft({ ...draft, ...next })}
          />
        </TabsContent>
        <TabsContent value="accounts" className="space-y-6">
          {accountSections.map(([key, label, matches]) => (
            <section key={key} className="space-y-4 border-b pb-6">
              <h3 className="font-semibold">{label}</h3>
              <ConfigFields
                prefix={`accounts-${key}`}
                value={Object.fromEntries(
                  Object.entries(draft.accounts).filter(([key]) =>
                    matches(key),
                  ),
                )}
                onChange={(next) =>
                  set("accounts", {
                    ...draft.accounts,
                    ...next,
                  } as AccountsConfig)
                }
              />
            </section>
          ))}
        </TabsContent>
        <TabsContent value="models">
          <ConfigFields
            value={{ modelResolutionMode: draft.modelResolutionMode }}
            onChange={(next) =>
              set(
                "modelResolutionMode",
                next.modelResolutionMode as RuntimeConfig["modelResolutionMode"],
              )
            }
          />
          <ModelMappingSection
            mapping={draft.modelMapping}
            capabilities={models.data}
            onChange={(v) => set("modelMapping", v)}
          />
        </TabsContent>
        <TabsContent value="proxy" className="space-y-5">
          <label className="block space-y-2">
            代理地址
            <Input
              value={draft.proxyUrl ?? ""}
              onChange={(e) => set("proxyUrl", e.target.value || null)}
            />
          </label>
          <label className="block space-y-2">
            代理用户名
            <Input
              value={draft.proxyUsername ?? ""}
              onChange={(e) => set("proxyUsername", e.target.value || null)}
            />
          </label>
          <label className="block space-y-2">
            代理密码
            <Input
              type="password"
              value={draft.proxyPassword ?? ""}
              onChange={(e) => set("proxyPassword", e.target.value || null)}
            />
          </label>
          <ConfigFields
            value={{ tlsBackend: draft.tlsBackend }}
            onChange={(next) =>
              set("tlsBackend", next.tlsBackend as RuntimeConfig["tlsBackend"])
            }
          />
        </TabsContent>
        <TabsContent value="retention" className="space-y-5">
          <label className="flex gap-3">
            <Switch
              checked={draft.auditLogRetentionEnabled}
              onCheckedChange={(v) => set("auditLogRetentionEnabled", v)}
            />
            启用审计自动清理
          </label>
          <ConfigFields
            value={{
              auditLogRetentionDays: draft.auditLogRetentionDays,
              usageRecordLimit: draft.usageRecordLimit,
            }}
            onChange={(next) => setDraft({ ...draft, ...next })}
          />
        </TabsContent>
        <TabsContent value="features" className="space-y-4">
          <p className="text-sm text-muted-foreground">
            Payload、请求兼容、图片处理与 max_tokens
            的参数在各路由的“路由功能”中编辑。全局默认值作为继承来源；新增功能默认关闭。
          </p>
          <Button variant="outline" asChild>
            <Link to="/routes">管理路由功能</Link>
          </Button>
        </TabsContent>
      </Tabs>
    </PageContainer>
  );
}
