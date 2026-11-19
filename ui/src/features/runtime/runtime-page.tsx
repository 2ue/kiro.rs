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
import { FileText, Route as RouteIcon, Save, Users } from "lucide-react";
import { useEffect, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { toast } from "sonner";
import { proxyPasswordIsSet, proxyPasswordPatch, type ProxyPasswordAction } from "./proxy-password";
import { ModelMappingSection } from "./runtime-sections";
import { ACCOUNT_SECTIONS, RUNTIME_TABS, accountSectionAnchor, resolveRuntimeTab } from "./runtime-tabs";
import { UsageCompensationFields } from "./usage-compensation-fields";
export function RuntimePage() {
  // 页签跟随 ?tab=：页签名直达页签，账号池分组名（如 cooldown）打开账号池并定位到该组。
  const [searchParams, setSearchParams] = useSearchParams();
  const { tab, section } = resolveRuntimeTab(searchParams.get("tab"));
  const setTab = (next: string) => setSearchParams({ tab: next }, { replace: true });
  const query = useRuntimeConfig(),
    update = useUpdateRuntimeConfig(),
    confirm = useConfirm(),
    models = useModelCapabilities(),
    [draft, setDraft] = useState<RuntimeConfig | null>(null),
    // 后端只回传代理密码的掩码，草稿里的 proxyPassword 原样不动、也不提交；
    // 新密码单独放在输入框状态里，清除要经过危险确认，保存时由 proxyPasswordPatch 决定怎么带。
    [proxyPasswordInput, setProxyPasswordInput] = useState(""),
    [proxyPasswordAction, setProxyPasswordAction] = useState<ProxyPasswordAction>("keep");
  useEffect(() => {
    if (query.data) {
      setDraft(structuredClone(query.data));
      setProxyPasswordInput("");
      setProxyPasswordAction("keep");
    }
  }, [query.data]);
  const loaded = draft !== null;
  useEffect(() => {
    if (!section || !loaded) return;
    requestAnimationFrame(() => document.getElementById(accountSectionAnchor(section))?.scrollIntoView({ block: "start", behavior: "smooth" }));
  }, [section, loaded]);
  const proxyPasswordDirty = proxyPasswordInput.length > 0 || proxyPasswordAction === "clear";
  useUnsavedChangesGuard(
    draft !== null &&
      query.data !== undefined &&
      (JSON.stringify(draft) !== JSON.stringify(query.data) || proxyPasswordDirty) &&
      !update.isPending,
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
  const dirty = JSON.stringify(draft) !== JSON.stringify(query.data) || proxyPasswordDirty;
  const proxyPasswordSet = proxyPasswordIsSet(query.data?.proxyPassword);
  const clearProxyPassword = async () => {
    const ok = await confirm({
      title: "清除代理密码？",
      message: "保存配置后，出站代理不再带密码认证；如果代理要求认证，走全局代理的账号请求会失败。点“保存配置”前可以撤销。",
      confirmText: "清除密码",
      tone: "danger",
    });
    if (!ok) return;
    setProxyPasswordInput("");
    setProxyPasswordAction("clear");
  };
  const proxyPasswordDescription =
    proxyPasswordAction === "clear"
      ? "保存配置后清除代理密码"
      : proxyPasswordSet
        ? proxyPasswordInput
          ? "保存后替换为新密码；清空输入框则保持原密码"
          : "已设置；留空保持不变，输入新值则替换"
        : undefined;
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
        proxyPassword: _proxyPassword,
        ...runtime
      } = draft;
      await update.mutateAsync({ ...runtime, ...proxyPasswordPatch(proxyPasswordInput, proxyPasswordAction) });
      // 只改了密码时回包仍是同一个掩码，查询缓存的结构共享会保留旧引用、上面的 effect 不会触发，这里显式复位。
      setProxyPasswordInput("");
      setProxyPasswordAction("keep");
      toast.success("运行配置已保存");
    } catch (error) {
      toast.error(extractErrorMessage(error));
    }
  };
  return (
    <PageContainer>
      <PageHeader
        actions={
          <>
            <Button asChild variant="outline">
              <Link to="/routes">
                <RouteIcon />
                管理路由
              </Link>
            </Button>
            <Button onClick={save} disabled={update.isPending || !dirty} title={dirty ? undefined : "没有未保存的修改"}>
              <Save />
              保存配置
            </Button>
          </>
        }
      />
      <Tabs value={tab} onValueChange={setTab}>
        <TabsList className="flex-wrap">
          {RUNTIME_TABS.map(({ key, label }) => (
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
          <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border bg-card px-4 py-3 shadow-sm">
            <p className="min-w-0 flex-1 text-xs leading-5 text-muted-foreground">
              这里是账号池的全局默认；单个账号的覆盖、接受的路由与模型在账号页修改，批量调整账号与路由的对应关系用路由场景。
            </p>
            <div className="flex shrink-0 flex-wrap gap-2">
              <Button asChild variant="outline" size="sm">
                <Link to="/accounts"><Users />在账号页修改</Link>
              </Button>
              <Button asChild variant="outline" size="sm">
                <Link to="/route-scenarios"><RouteIcon />路由场景</Link>
              </Button>
            </div>
          </div>
          {ACCOUNT_SECTIONS.map(({ key, title, desc, matches }) => (
            <div key={key} id={accountSectionAnchor(key)} className="scroll-mt-4">
              <SectionCard title={title} description={desc}>
                {key === "uplift" ? (
                  <UsageCompensationFields value={draft.accounts} onChange={(next) => set("accounts", next)} />
                ) : (
                  <ConfigFields
                    prefix={`accounts-${key}`}
                    value={Object.fromEntries(Object.entries(draft.accounts).filter(([k]) => matches(k)))}
                    onChange={(next) => set("accounts", { ...draft.accounts, ...next } as AccountsConfig)}
                  />
                )}
              </SectionCard>
            </div>
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
              <Field label="代理密码" htmlFor="runtime-proxy-pass" description={proxyPasswordDescription}>
                <div className="flex min-w-0 items-center gap-2">
                  <Input
                    id="runtime-proxy-pass"
                    type="password"
                    autoComplete="new-password"
                    className="min-w-0 flex-1"
                    placeholder={proxyPasswordAction === "clear" ? "保存后清除" : proxyPasswordSet ? "已设置，留空保持不变" : "未设置"}
                    disabled={proxyPasswordAction === "clear"}
                    value={proxyPasswordInput}
                    onChange={(e) => setProxyPasswordInput(e.target.value)}
                  />
                  {proxyPasswordAction === "clear" ? (
                    <Button variant="outline" size="sm" className="shrink-0" onClick={() => setProxyPasswordAction("keep")}>
                      撤销
                    </Button>
                  ) : (
                    proxyPasswordSet && (
                      <Button variant="outline" size="sm" className="shrink-0 text-destructive hover:text-destructive" onClick={clearProxyPassword}>
                        清除密码
                      </Button>
                    )
                  )}
                </div>
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
        <TabsContent value="features" className="space-y-4">
          <SectionCard title="路由功能默认参数" description="请求体保护、参数兼容、图片处理与 max_tokens 补全在各路由的“路由功能”页签里编辑；这里的全局默认值是继承来源，新增功能默认关闭。">
            <Button variant="outline" asChild>
              <Link to="/routes">去路由页管理</Link>
            </Button>
          </SectionCard>
          <SectionCard
            title="提示词注入"
            description="代理写进上游请求的提示词（任务质量、语言约束、自定义、身份声明）每一项单独设置：默认开关与文本在策略模板页编辑，和路由的关联在路由编辑页的“提示词注入”页签设置。"
          >
            <div className="flex flex-wrap gap-2">
              <Button variant="outline" size="sm" asChild>
                <Link to="/policy-templates?tab=injection"><FileText />打开提示词注入</Link>
              </Button>
              <Button variant="outline" size="sm" asChild>
                <Link to="/policy-templates?tab=matrix"><RouteIcon />路由 × 注入矩阵</Link>
              </Button>
            </div>
          </SectionCard>
        </TabsContent>
      </Tabs>
    </PageContainer>
  );
}
