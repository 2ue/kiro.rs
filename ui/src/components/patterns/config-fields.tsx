import type * as React from "react";
import { useState } from "react";
import {
  Input,
  Label,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Switch,
} from "@/components/ui";
import {
  ENUM_OPTIONS_BY_FIELD,
  IMAGE_PROCESSING_MODE_OPTIONS,
  PAYLOAD_GUARD_MODE_OPTIONS,
  REPORTED_USAGE_FIELD_MODE_OPTIONS,
  type EnumOption,
} from "@/lib/enum-labels";
/** 字段名 → 中文标签（单位单独放在 UNITS 里，渲染在控件旁） */
const labels: Record<string, string> = {
  rpm: "每分钟请求数",
  maxConcurrentRequests: "并发上限",
  maxQueuedRequests: "最大排队请求数",
  queueTimeoutMs: "排队等待",
  enabled: "启用",
  requestTimeoutSecs: "非流式总超时",
  streamRequestTimeoutSecs: "流式总超时",
  streamIdleTimeoutSecs: "流式空闲超时",
  streamFirstOutputTimeoutSecs: "首个输出超时",
  retryMaxAttempts: "最多故障转移",
  globalMaxConcurrentRequests: "全局最大并发",
  dispatchMaxWaitSecs: "最长排队等待",
  usageProjectionUpliftPercent: "缓存读写补偿",
  usageProjectionCostFloorEnabled: "启用成本底线",
  usageProjectionCostFloorMarginPercent: "成本补齐余量",
  usageProjectionOutputUpliftMinTokens: "输出补偿阈值",
  usageProjectionOutputUpliftPercent: "输出补偿",
  defaultValue: "max_tokens 补充值",
  policy: "缺失 max_tokens",
  maxBytes: "请求体上限",
  safetyMarginBytes: "安全余量",
  trimHistory: "裁剪历史",
  externalEnabled: "用于账号请求",
  mode: "模式",
  inferenceUpstreamMaxAttempts: "单请求推理发送硬上限",
  streamPreOutputHoldSecs: "首输出前静默保留",
  capacityMode: "满并发处理",
  retryStatusCodes: "跨池重试状态码",
  retryOnNetworkError: "网络错误跨池重试",
  retryOnProtocolError: "协议错误跨池重试",
  samePoolRetryCount: "同池重试次数",
  samePoolRetryStatusCodes: "同池重试状态码",
  samePoolRetryDelayMs: "同池重试间隔",
  transientFailurePriorityPenalty: "失败池临时降权",
  qualityAwareSchedulingEnabled: "启用质量感知调度",
  qualityEwmaAlpha: "质量平滑系数",
  qualitySampleTtlSecs: "质量数据有效期",
  qualityMinSamples: "最少样本数",
  qualityPriorityWeight: "优先级权重",
  qualityLoadWeight: "负载权重",
  qualityErrorWeight: "失败率权重",
  qualityLatencyWeight: "延迟权重",
  qualityProbationWeight: "临时降级权重",
  qualityTopK: "候选池大小 K",
  degradeWindowSecs: "劣化判定窗口",
  degradeErrorRateThreshold: "降级失败率阈值",
  degradeProbationSecs: "单次降级时长",
  maxProbationSecs: "最长降级时长",
  probeSharePercent: "降级期探测流量",
  recoveryRampSecs: "恢复爬坡时长",
  autoDisableEnabled: "启用自动禁用",
  autoDisableOnAuthError: "认证错误",
  autoDisableOnSecurityLock: "安全锁定",
  autoDisableOnQuotaExhausted: "额度耗尽",
  autoDisableOnMisconfiguredEndpoint: "配置错误",
  autoDisableOnChannelDisabled: "通道禁用",
  autoDisableFailureThreshold: "触发阈值",
  autoDisableWindowSecs: "统计窗口",
  autoDisableDurationSecs: "禁用时长",
  rateLimitCooldownSecs: "429 冷却",
  serverErrorCooldownSecs: "5xx 冷却",
  networkErrorCooldownSecs: "网络错误冷却",
  protocolErrorCooldownSecs: "协议/认证冷却",
  modelUnavailableCooldownMode: "模型不可用冷却范围",
  modelUnavailableCooldownSecs: "模型不可用冷却",
  streamPreOutputRetryEnabled: "流式首输出前错误换池",
  streamResponseMode: "SSE 默认转发",
  usageDebugEnabled: "usage 原始数据诊断",
  usageDebugDir: "诊断目录",
  usageDebugMaxBodyBytes: "单条原始片段上限",
  usageDebugMaxFiles: "最多诊断文件",
  wireDebugEnabled: "wire 交互诊断",
  wireDebugDir: "wire 诊断目录",
  wireDebugMaxBodyBytes: "wire 单条片段上限",
  wireDebugMaxFiles: "wire 最多诊断文件",
  modelResolutionMode: "模型解析策略",
  compatProfile: "兼容模式",
  tlsBackend: "TLS 后端",
  auditLogRetentionEnabled: "启用审计自动清理",
  auditLogRetentionDays: "审计日志保留",
  usageRecordRetentionEnabled: "启用用量明细自动清理",
  usageRecordRetentionDays: "用量明细保留",
  usageRecordLimit: "内存用量记录上限",
  thinkingTrigger: "自动触发 thinking",
  tooLongHandling: "输入过长处理",
  oversizedImageHandling: "超大图片处理",
  payloadGuardMode: "大小保护触发时机",
};
const UNITS: Record<string, string> = {
  queueTimeoutMs: "毫秒",
  requestTimeoutSecs: "秒",
  streamRequestTimeoutSecs: "秒",
  streamIdleTimeoutSecs: "秒",
  streamFirstOutputTimeoutSecs: "秒",
  retryMaxAttempts: "次",
  dispatchMaxWaitSecs: "秒",
  usageProjectionUpliftPercent: "%",
  usageProjectionCostFloorMarginPercent: "%",
  usageProjectionOutputUpliftMinTokens: "token",
  usageProjectionOutputUpliftPercent: "%",
  maxBytes: "字节",
  safetyMarginBytes: "字节",
  inferenceUpstreamMaxAttempts: "次",
  streamPreOutputHoldSecs: "秒",
  samePoolRetryDelayMs: "毫秒",
  qualitySampleTtlSecs: "秒",
  degradeWindowSecs: "秒",
  degradeProbationSecs: "秒",
  maxProbationSecs: "秒",
  probeSharePercent: "%",
  recoveryRampSecs: "秒",
  autoDisableFailureThreshold: "次",
  autoDisableWindowSecs: "秒",
  autoDisableDurationSecs: "秒",
  rateLimitCooldownSecs: "秒",
  serverErrorCooldownSecs: "秒",
  networkErrorCooldownSecs: "秒",
  protocolErrorCooldownSecs: "秒",
  modelUnavailableCooldownSecs: "秒",
  usageDebugMaxBodyBytes: "字节",
  wireDebugMaxBodyBytes: "字节",
  auditLogRetentionDays: "天",
  usageRecordRetentionDays: "天",
  usageRecordLimit: "条",
};
const DESCRIPTIONS: Record<string, string> = {
  globalMaxConcurrentRequests: "所有账号合计同时处理的请求上限；0 表示不限制。",
  maxQueuedRequests: "账号并发占满时最多允许排队的请求数；0 表示不排队。",
  dispatchMaxWaitSecs: "等待并发名额的最长时间；0 会按后端安全默认值处理。",
  capacityMode: "并发占满时是排队等待还是直接返回错误。",
  retryMaxAttempts: "同一请求在多个账号之间最多尝试多少次；0 表示不重试其它账号。",
  retryStatusCodes: "命中这些状态码时允许切换到其他账号；逗号分隔。",
  retryOnNetworkError: "连接、DNS、超时等没有 HTTP 状态码的错误，是否允许切换其他账号。",
  retryOnProtocolError: "上游返回成功状态码但内容是错误信封或协议污染时，是否允许切换其他账号。",
  samePoolRetryCount: "命中下面状态码时，先在同一个账号上重试；重试耗尽后才冷却并尝试其他账号。",
  samePoolRetryStatusCodes: "触发同账号重试的状态码；逗号分隔。",
  samePoolRetryDelayMs: "同一个账号重试之间的等待时间。",
  transientFailurePriorityPenalty: "账号出现可重试瞬态失败后，在失败窗口内临时增加有效优先级；默认 20 可让优先级 1 的故障账号让位给 10/20 的健康账号，0 表示关闭。",
  qualityAwareSchedulingEnabled: "关闭后完全退回“优先级 → 负载”的旧排序，也不再采样质量数据（账号风控页的质量分趋势不会有新点），可作为线上快速回退手段。",
  qualityTopK: "在评分最优的 K 个账号里加权随机挑选，避免多实例同时扑向同一个账号。1 表示总是选最优。",
  qualityEwmaAlpha: "EWMA 系数，有效窗口约为最近 1/系数 个请求。越大越灵敏，越小越稳定。",
  qualityMinSamples: "累计样本不足时按中性处理，不加分也不减分；避免新账号因冷启动数据难看而永远起不来。",
  qualitySampleTtlSecs: "超过该时长没有新请求，质量数据过期并回到中性值。",
  qualityErrorWeight: "量级刻意远高于延迟权重：报错比“慢”严重得多，必须能压过首字延迟的倾向。默认 100。",
  qualityLatencyWeight: "首字延迟与总耗时各自相对中位数归一化后共用该权重；慢一倍贡献 1 个权重单位。",
  qualityLoadWeight: "账号当前并发占用率的权重，用于在质量相近时摊平负载。",
  qualityProbationWeight: "被临时降级的账号额外增加的分数，使其排到末尾但不移出候选集。",
  qualityPriorityWeight: "仅用于管理端展示分数构成；优先级是硬分层，同层内该项为常数，不影响实际比较。",
  degradeErrorRateThreshold: "失败率达到该比例即临时降级。设为 1 表示只在全部失败时才降级。",
  degradeWindowSecs: "失败率的观察窗口长度。",
  degradeProbationSecs: "首次触发降级持续多久；连续再次触发会按 2 倍逐次延长。",
  maxProbationSecs: "连续降级指数退避的上限，不得低于单次降级时长。",
  probeSharePercent: "被降级账号仍保留的流量比例。设为 0 会让降级变成硬避让，账号将无法自证恢复。",
  recoveryRampSecs: "降级到期后流量线性回升的时长，避免账号刚恢复就被瞬间打满。0 表示立即全量恢复。",
  autoDisableEnabled: "连续命中下面勾选的错误类型达到阈值后自动停用账号；默认关闭。",
  autoDisableFailureThreshold: "统计窗口内累计多少次命中才触发自动禁用。",
  autoDisableWindowSecs: "失败计数的观察窗口。",
  autoDisableDurationSecs: "自动禁用持续多久后自动恢复；0 表示直到人工清除。",
  rateLimitCooldownSecs: "上游返回 429 后该账号暂停调度的时长。",
  serverErrorCooldownSecs: "上游返回 5xx 后该账号暂停调度的时长。",
  networkErrorCooldownSecs: "连接、DNS、超时等网络错误后的暂停时长。",
  protocolErrorCooldownSecs: "协议污染或认证错误后的暂停时长。",
  modelUnavailableCooldownMode: "上游明确说模型不可用时，冷却只针对该模型、整个账号，还是不写冷却。",
  modelUnavailableCooldownSecs: "模型不可用冷却持续多久。",
  requestTimeoutSecs: "非流式请求从发出到收到完整响应的总时限。",
  streamRequestTimeoutSecs: "流式请求从发出到流结束的总时限；0 表示不限。",
  streamIdleTimeoutSecs: "流式响应两次数据之间允许的最长间隔。",
  streamFirstOutputTimeoutSecs: "上游响应头之后等待首个内容事件的上限；超时后换账号重试或以 overloaded 结束。0 关闭。",
  streamPreOutputRetryEnabled: "stream 已返回 200 但还没提交内容前遇到 error、断流、EOF 或空闲超时时，允许换其他账号重放。单个账号可覆盖。",
  streamResponseMode: "作为账号默认值；单个账号仍可在编辑弹窗中覆盖。",
  usageProjectionUpliftPercent: "有上游缓存证据时按百分比补缓存字段，并遵守当前路径上限；0 关闭。",
  usageProjectionCostFloorEnabled: "用最终返回给调用方的 usage 覆盖原始上游成本，不低于上游原始成本。",
  usageProjectionCostFloorMarginPercent: "成本底线之上再补的余量百分比。",
  usageProjectionOutputUpliftMinTokens: "输出 token 超过该阈值才按百分比放大；0 关闭。",
  usageProjectionOutputUpliftPercent: "输出放大百分比。",
  usageDebugEnabled: "临时记录上游原始响应/SSE usage 样本、请求关联信息和本系统解析结果；默认关闭。",
  usageDebugDir: "诊断 JSON 文件写入目录。",
  usageDebugMaxBodyBytes: "限制原始请求/响应 body 与 SSE 前缀保存大小。",
  usageDebugMaxFiles: "进程启动后最多写入的诊断文件数；超出后跳过写入。",
  wireDebugEnabled: "记录发往上游的完整请求头与响应头、body 前缀，用于排查 header/TLS 兼容问题；默认关闭。",
  wireDebugDir: "wire 诊断文件写入目录。",
  wireDebugMaxBodyBytes: "限制 wire 诊断里单条 body 保存大小。",
  wireDebugMaxFiles: "进程启动后最多写入的 wire 诊断文件数。",
  inferenceUpstreamMaxAttempts: "单个请求累计向上游发起推理调用的硬上限，含重试与换账号；防止重试风暴。",
  streamPreOutputHoldSecs: "首个内容出现前，最多攒多久再决定是否换账号；0 表示不等待。",
  auditLogRetentionEnabled: "按下面的天数定期清理审计日志；这是会删数据的功能，默认关闭。",
  auditLogRetentionDays: "超过天数的审计日志自动清理。",
  usageRecordRetentionEnabled: "按下面的天数定期物理删除逐条用量明细；总览、趋势、排行用的汇总不受影响。这是会删数据的功能，默认关闭。",
  usageRecordRetentionDays: "超过天数的用量明细自动清理，含已软删的记录；超过天数没有活动的会话排行汇总也一起裁掉。",
  usageRecordLimit: "内存里保留的最近用量记录条数，用于控制台实时列表。",
  rpm: "每分钟允许的请求数；0 表示不限。",
  maxConcurrentRequests: "同时在飞的请求上限。",
  queueTimeoutMs: "排队超过该时长仍拿不到名额则直接返回错误。",
};
/** 对象型字段的分组标题与说明 */
const GROUP_META: Record<string, { title: string; desc?: string }> = {
  requestAdmission: { title: "每实例 · 每 Key 限制", desc: "速率、并发与排队限制按每个实例、每把下游 Key 分别计；外层限制优先，密钥层只能收紧。" },
};
/** 参数格网格：列宽 15rem 起自适应，与 ConfigFields 相同。 */
export const CONFIG_FIELD_GRID_CLASS = "grid gap-x-8 gap-y-5";
export const CONFIG_FIELD_GRID_STYLE = { gridTemplateColumns: "repeat(auto-fill, minmax(min(15rem, 100%), 1fr))" } as const;

/** 一个参数格：标签在上、控件居中、说明在下。ConfigFields 与需要联动的自定义分组共用同一排版。 */
export function ConfigFieldCell({
  id,
  label,
  title,
  description,
  children,
}: {
  id: string;
  label: React.ReactNode;
  title?: string;
  description?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div className="min-w-0 space-y-1.5">
      <Label htmlFor={id} title={title} className="block text-[13px] font-medium leading-5 text-foreground">{label}</Label>
      <div className="flex min-h-8 items-center gap-2">{children}</div>
      {description && <p className="text-[11px] leading-4 text-muted-foreground">{description}</p>}
    </div>
  );
}

/** 参数格里的开关：开关 + “开启/关闭”文字。 */
export function ConfigSwitchControl({
  id,
  checked,
  disabled,
  onChange,
}: {
  id: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <>
      <Switch id={id} checked={checked} disabled={disabled} onCheckedChange={onChange} />
      <span className="text-xs text-muted-foreground">{checked ? "开启" : "关闭"}</span>
    </>
  );
}

/** 参数格里的数字框：固定宽度，单位放在右侧。
 *  输入框清空或输入到一半（如 "-"）时不向上提交，失焦后恢复成当前值，避免清空被当成 0；
 *  `integer` 时取整并按 min/max 限幅，对应后端的整数字段。 */
export function ConfigNumberControl({
  id,
  value,
  unit,
  min,
  max,
  step,
  integer,
  disabled,
  onChange,
}: {
  id: string;
  value: number;
  unit?: string;
  min?: number;
  max?: number;
  step?: number;
  integer?: boolean;
  disabled?: boolean;
  onChange: (value: number) => void;
}) {
  const [text, setText] = useState<string | null>(null);
  const normalize = (raw: number) => {
    let next = integer ? Math.round(raw) : raw;
    if (min !== undefined) next = Math.max(min, next);
    if (max !== undefined) next = Math.min(max, next);
    return next;
  };
  return (
    <>
      <Input
        id={id}
        type="number"
        inputMode={integer ? "numeric" : "decimal"}
        min={min}
        max={max}
        step={step ?? (integer ? 1 : undefined)}
        disabled={disabled}
        className="h-8 w-28 shrink-0 px-2 text-sm tabular-nums"
        value={text ?? String(value ?? "")}
        onChange={(e) => {
          const raw = e.target.value;
          setText(raw);
          const parsed = Number(raw);
          if (raw.trim() !== "" && Number.isFinite(parsed)) onChange(normalize(parsed));
        }}
        onBlur={() => setText(null)}
      />
      {unit && <span className="shrink-0 whitespace-nowrap text-xs text-muted-foreground">{unit}</span>}
    </>
  );
}

/**
 * 通用配置表单。
 *
 * 排版规则（所有参数类表单共用）：
 * - 每个字段是一格：标签在上、控件居中、说明在下，开关和输入框一样占一格，不再“标签左、开关最右”。
 * - 网格列宽 14–20rem 自适应，一行最多三格；整体限宽 64rem，宽屏不会把输入框拉成一整行。
 * - 数字框固定 7rem 并带单位；文本框占满本格；下拉占满本格。
 * - 嵌套对象（如请求准入）作为一个带标题的小分区，内部仍按同一网格排。
 */
export function ConfigFields({
  value,
  onChange,
  prefix = "",
}: {
  value: object;
  onChange: (next: Record<string, unknown>) => void;
  prefix?: string;
}) {
  const fields = value as Record<string, unknown>;
  const entries = Object.entries(fields);
  const isGroup = (v: unknown) => v !== null && typeof v === "object" && !Array.isArray(v);
  const leaves = entries.filter(([, v]) => !isGroup(v));
  const groups = entries.filter(([, v]) => isGroup(v));
  const setField = (key: string, next: unknown) => onChange({ ...fields, [key]: next });

  return (
    <div className="max-w-5xl space-y-6">
      {leaves.length > 0 && (
        <div className={CONFIG_FIELD_GRID_CLASS} style={CONFIG_FIELD_GRID_STYLE}>
          {leaves.map(([key, current]) => {
            const id = `${prefix}-${key}`;
            const label = labels[key] ?? key;
            const options: EnumOption[] | undefined =
              key === "mode"
                ? [REPORTED_USAGE_FIELD_MODE_OPTIONS, IMAGE_PROCESSING_MODE_OPTIONS, PAYLOAD_GUARD_MODE_OPTIONS].find((table) => table.some((option) => option.value === String(current)))
                : ENUM_OPTIONS_BY_FIELD[key];
            const selected = options?.find((option) => option.value === String(current));
            const description = selected?.description ?? DESCRIPTIONS[key];
            const isBool = typeof current === "boolean";
            const isNumber = typeof current === "number";
            const isList = Array.isArray(current);
            return (
              <ConfigFieldCell key={key} id={id} label={label} title={key} description={description}>
                {isBool ? (
                  <ConfigSwitchControl id={id} checked={current as boolean} onChange={(v) => setField(key, v)} />
                ) : options ? (
                  <Select value={String(current)} onValueChange={(v) => setField(key, v)}>
                    <SelectTrigger id={id} size="sm" className="w-full"><SelectValue /></SelectTrigger>
                    <SelectContent>
                      {options.map((option) => <SelectItem key={option.value} value={option.value}>{option.label}</SelectItem>)}
                    </SelectContent>
                  </Select>
                ) : (
                  <>
                    <Input
                      id={id}
                      type={isNumber ? "number" : "text"}
                      inputMode={isNumber ? "decimal" : undefined}
                      className={isNumber ? "h-8 w-28 shrink-0 px-2 text-sm tabular-nums" : "h-8 w-full text-sm"}
                      value={isList ? (current as unknown[]).join(", ") : String(current ?? "")}
                      onChange={(e) =>
                        setField(
                          key,
                          isNumber
                            ? Number(e.target.value)
                            : isList
                              ? e.target.value
                                  .split(",")
                                  .map((v) => v.trim())
                                  .filter(Boolean)
                                  .map((v) => ((current as unknown[]).every((item) => typeof item === "number") ? Number(v) : v))
                                  .filter((v) => typeof v !== "number" || Number.isFinite(v))
                              : e.target.value,
                        )
                      }
                    />
                    {UNITS[key] && <span className="shrink-0 whitespace-nowrap text-xs text-muted-foreground">{UNITS[key]}</span>}
                  </>
                )}
              </ConfigFieldCell>
            );
          })}
        </div>
      )}
      {groups.map(([key, current]) => {
        const id = `${prefix}-${key}`;
        const meta = GROUP_META[key];
        return (
          <section key={key} className="space-y-4 border-t pt-5 first:border-t-0 first:pt-0">
            <header>
              <h4 className="text-sm font-semibold">{meta?.title ?? labels[key] ?? key}</h4>
              {meta?.desc && <p className="mt-0.5 text-xs leading-5 text-muted-foreground">{meta.desc}</p>}
            </header>
            <ConfigFields prefix={id} value={current as object} onChange={(next) => setField(key, next)} />
          </section>
        );
      })}
    </div>
  );
}
