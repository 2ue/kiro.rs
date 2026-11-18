import {
  ErrorState,
  Field,
  FieldGrid,
  LoadingState,
  PageContainer,
  PageHeader,
  SectionCard,
  useConfirm,
} from "@/components/patterns";
import { ConfigFields } from "@/components/patterns/config-fields";
import {
  Button,
  Input,
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
import { useUnsavedChangesGuard } from "@/hooks/use-unsaved-changes-guard";
import { extractErrorMessage } from "@/lib/utils";
import type { AccountsConfig, RuntimeConfig } from "@/types/api";
import { Save } from "lucide-react";
import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { toast } from "sonner";
import { ModelMappingSection } from "./runtime-sections";
const accountSections: Array<{ key: string; title: string; desc: string; matches: (key: string) => boolean }> = [
  { key: "capacity", title: "容量与排队", desc: "账号池整体的并发上限、排队与等待。", matches: (k) => /^(enabled|globalMax|maxQueued|capacityMode|dispatch)/.test(k) },
  { key: "retry", title: "重试与故障转移", desc: "同一请求在账号之间怎么重试，瞬态失败的账号怎么降权。", matches: (k) => /^(retry|samePool|transient)/.test(k) },
  { key: "quality", title: "质量感知调度", desc: "按失败率、延迟和负载给账号打分，优先把请求交给表现好的账号。", matches: (k) => /^(quality|degrade|probation|maxProbation|probe|recovery)/.test(k) },
  { key: "cooldown", title: "冷却", desc: "不同错误类型后账号暂停调度多久。", matches: (k) => /Cooldown/.test(k) },
  { key: "disable", title: "自动禁用", desc: "命中特定错误达到阈值后自动停用账号；默认关闭，单个账号可覆盖。", matches: (k) => k.startsWith("autoDisable") },
  { key: "timeouts", title: "超时与流式", desc: "请求总超时、流式空闲与首输出超时，以及 SSE 转发方式。", matches: (k) => /Timeout|streamResponseMode|streamPreOutput/.test(k) },
  { key: "uplift", title: "用量补偿", desc: "按路径整理 usage 的账号，在上游原始 usage 之上怎么补。", matches: (k) => k.startsWith("usageProjection") },
  { key: "diagnostics", title: "诊断", desc: "临时记录上游原始数据用于排查；默认关闭，开启后注意磁盘占用。", matches: (k) => /Debug/.test(k) },
];
export function RuntimePage() {
  const query = useRuntimeConfig(),
    update = useUpdateRuntimeConfig(),
    confirm = useConfirm(),
    models = useModelCapabilities(),
    [draft, setDraft] = useState<RuntimeConfig | null>(null),
    // 后端只回传脱敏后的代理密码；用户没在输入框里改过就不把它回传，避免把脱敏值当密码存进去。
    [proxyPasswordDirty, setProxyPasswordDirty] = useState(false);
  useEffect(() => {
    if (query.data) {
      setDraft(structuredClone(query.data));
      setProxyPasswordDirty(false);
    }
  }, [query.data]);
  useUnsavedChangesGuard(
    draft !== null && query.data !== undefined && JSON.stringify(draft) !== JSON.stringify(query.data) && !update.isPending,
  );
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
  const dirty = JSON.stringify(draft) !== JSON.stringify(query.data);
  const proxyPasswordSet = Boolean(query.data?.proxyPassword);
  const save = async () => {
    // 开启自动清理或缩短已开启的保留天数都会删数据，和审计页同一口径先确认。
    const before = query.data;
    const deletions: string[] = [];
    if (before) {
      if (draft.auditLogRetentionEnabled && (!before.auditLogRetentionEnabled || draft.auditLogRetentionDays < before.auditLogRetentionDays))
        deletions.push(`${draft.auditLogRetentionDays} 天以前的审计日志（含账号异动）`);
      if (draft.usageRecordRetentionEnabled && (!before.usageRecordRetentionEnabled || draft.usageRecordRetentionDays < before.usageRecordRetentionDays))
        deletions.push(`${draft.usageRecordRetentionDays} 天以前的用量明细（汇总与趋势保留）`);
    }
    if (deletions.length > 0) {
      const ok = await confirm({
        title: "开启自动清理？",
        message: `保存后，后台会在 1 小时内开始删除：${deletions.join("；")}。删除后无法恢复，需要留存请先备份。`,
        confirmText: "确认开启",
        tone: "danger",
      });
      if (!ok) return;
    }
    try {
      const {
        cachePolicy: _cache,
        reportedUsage: _usage,
        promptInjection: _injection,
        proxyPassword,
        ...runtime
      } = draft;
      await update.mutateAsync(proxyPasswordDirty ? { ...runtime, proxyPassword } : runtime);
      toast.success("运行配置已保存");
    } catch (error) {
      toast.error(extractErrorMessage(error));
    }
  };
  return (
    <PageContainer>
      <PageHeader
        actions={
          <Button onClick={save} disabled={update.isPending || !dirty} title={dirty ? undefined : "没有未保存的修改"}>
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
          <SectionCard title="请求准入" description="进入系统的请求先过这里：每实例、每 Key 的速率与并发限制，以及单请求的上游调用硬上限。">
            <ConfigFields
              value={{
                inferenceUpstreamMaxAttempts: draft.inferenceUpstreamMaxAttempts,
                streamPreOutputHoldSecs: draft.streamPreOutputHoldSecs,
                requestAdmission: draft.requestAdmission,
              }}
              onChange={(next) => setDraft({ ...draft, ...next })}
            />
          </SectionCard>
        </TabsContent>
        <TabsContent value="accounts" className="space-y-4">
          {accountSections.map(({ key, title, desc, matches }) => (
            <SectionCard key={key} title={title} description={desc}>
              <ConfigFields
                prefix={`accounts-${key}`}
                value={Object.fromEntries(Object.entries(draft.accounts).filter(([k]) => matches(k)))}
                onChange={(next) => set("accounts", { ...draft.accounts, ...next } as AccountsConfig)}
              />
            </SectionCard>
          ))}
        </TabsContent>
        <TabsContent value="models" className="space-y-4">
          <SectionCard title="模型解析" description="客户端请求的模型名怎么匹配到目录里的模型。">
            <ConfigFields
              value={{ modelResolutionMode: draft.modelResolutionMode }}
              onChange={(next) => set("modelResolutionMode", next.modelResolutionMode as RuntimeConfig["modelResolutionMode"])}
            />
          </SectionCard>
          <ModelMappingSection
            mapping={draft.modelMapping}
            capabilities={models.data}
            onChange={(v) => set("modelMapping", v)}
          />
        </TabsContent>
        <TabsContent value="proxy">
          <SectionCard title="出站代理与 TLS" description="账号默认走这里的代理；单个账号可在编辑弹窗里单独指定。">
            <FieldGrid min="16rem">
              <Field label="代理地址" htmlFor="runtime-proxy-url" description="留空直连">
                <Input id="runtime-proxy-url" placeholder="http://127.0.0.1:7890" value={draft.proxyUrl ?? ""} onChange={(e) => set("proxyUrl", e.target.value || null)} />
              </Field>
              <Field label="代理用户名" htmlFor="runtime-proxy-user">
                <Input id="runtime-proxy-user" value={draft.proxyUsername ?? ""} onChange={(e) => set("proxyUsername", e.target.value || null)} />
              </Field>
              <Field label="代理密码" htmlFor="runtime-proxy-pass" description={proxyPasswordSet && !proxyPasswordDirty ? "已设置；留空保持不变，输入新值则替换" : undefined}>
                <Input
                  id="runtime-proxy-pass"
                  type="password"
                  autoComplete="new-password"
                  placeholder={proxyPasswordSet ? "已设置，留空保持不变" : undefined}
                  value={proxyPasswordDirty ? (draft.proxyPassword ?? "") : ""}
                  onChange={(e) => {
                    setProxyPasswordDirty(true);
                    set("proxyPassword", e.target.value || null);
                  }}
                />
              </Field>
            </FieldGrid>
            <div className="mt-5 border-t pt-5">
              <ConfigFields value={{ tlsBackend: draft.tlsBackend }} onChange={(next) => set("tlsBackend", next.tlsBackend as RuntimeConfig["tlsBackend"])} />
            </div>
          </SectionCard>
        </TabsContent>
        <TabsContent value="retention">
          <SectionCard title="数据保留" description="审计日志、用量明细与内存用量记录保留多少。两个自动清理默认关闭，开启前请确认已有备份。">
            <ConfigFields
              value={{
                auditLogRetentionEnabled: draft.auditLogRetentionEnabled,
                auditLogRetentionDays: draft.auditLogRetentionDays,
                usageRecordRetentionEnabled: draft.usageRecordRetentionEnabled,
                usageRecordRetentionDays: draft.usageRecordRetentionDays,
                usageRecordLimit: draft.usageRecordLimit,
              }}
              onChange={(next) => setDraft({ ...draft, ...next })}
            />
          </SectionCard>
        </TabsContent>
        <TabsContent value="features">
          <SectionCard title="路由功能默认参数" description="请求体保护、参数兼容、图片处理与 max_tokens 补全在各路由的“路由功能”页签里编辑；这里的全局默认值是继承来源，新增功能默认关闭。">
            <Button variant="outline" asChild>
              <Link to="/routes">去路由页管理</Link>
            </Button>
          </SectionCard>
        </TabsContent>
      </Tabs>
    </PageContainer>
  );
}
