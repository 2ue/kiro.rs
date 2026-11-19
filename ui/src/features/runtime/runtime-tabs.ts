// 运行配置页的页签与账号池分组，以及 `?tab=` 直达解析。只有纯逻辑，`node --test` 可以直接加载。

export const RUNTIME_TABS = [
  { key: "admission", label: "请求准入" },
  { key: "accounts", label: "账号池" },
  { key: "models", label: "模型解析" },
  { key: "proxy", label: "代理与 TLS" },
  { key: "retention", label: "数据保留" },
  { key: "features", label: "路由功能默认参数" },
] as const;
export type RuntimeTab = (typeof RUNTIME_TABS)[number]["key"];

/** 账号池页签里的分组：按字段名归组，其它页面可以用 `?tab={key}` 直达某一组。 */
export const ACCOUNT_SECTIONS = [
  { key: "capacity", title: "容量与排队", desc: "账号池整体的并发上限、排队与等待。", matches: (k: string) => /^(enabled|globalMax|maxQueued|capacityMode|dispatch)/.test(k) },
  { key: "retry", title: "重试与故障转移", desc: "同一请求在账号之间怎么重试，瞬态失败的账号怎么降权。", matches: (k: string) => /^(retry|samePool|transient)/.test(k) },
  { key: "quality", title: "质量感知调度", desc: "按失败率、延迟和负载给账号打分，优先把请求交给表现好的账号。", matches: (k: string) => /^(quality|degrade|probation|maxProbation|probe|recovery)/.test(k) },
  { key: "cooldown", title: "冷却", desc: "不同错误类型后账号暂停调度多久。", matches: (k: string) => /Cooldown/.test(k) },
  { key: "disable", title: "自动禁用", desc: "命中特定错误达到阈值后自动停用账号；默认关闭，单个账号可覆盖。", matches: (k: string) => k.startsWith("autoDisable") },
  { key: "timeouts", title: "超时与流式", desc: "请求总超时、流式空闲与首输出超时，以及 SSE 转发方式。", matches: (k: string) => /Timeout|streamResponseMode|streamPreOutput/.test(k) },
  { key: "uplift", title: "用量补偿", desc: "按路由策略整理 usage 的账号，在上游原始 usage 之上怎么补；透传上游 usage 的账号不受影响。", matches: (k: string) => k.startsWith("usageProjection") },
  { key: "diagnostics", title: "诊断", desc: "临时记录上游原始数据用于排查；默认关闭，开启后注意磁盘占用。", matches: (k: string) => /Debug/.test(k) },
] as const;
export type AccountSectionKey = (typeof ACCOUNT_SECTIONS)[number]["key"];

/** 账号池分组在页面上的锚点 id。 */
export function accountSectionAnchor(key: AccountSectionKey): string {
  return `runtime-accounts-${key}`;
}

/**
 * `?tab=` 解析：页签名直达页签；账号池分组名（如 cooldown、uplift）打开账号池页签并定位到该组。
 * 不认识的值回到第一个页签。
 */
export function resolveRuntimeTab(value: string | null | undefined): { tab: RuntimeTab; section: AccountSectionKey | null } {
  const key = (value ?? "").trim();
  if (RUNTIME_TABS.some((tab) => tab.key === key)) return { tab: key as RuntimeTab, section: null };
  const section = ACCOUNT_SECTIONS.find((item) => item.key === key);
  if (section) return { tab: "accounts", section: section.key };
  return { tab: "admission", section: null };
}
