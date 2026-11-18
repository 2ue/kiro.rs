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
const labels: Record<string, string> = {
  rpm: "每分钟请求数",
  maxConcurrentRequests: "并发上限",
  maxQueuedRequests: "最大排队请求数",
  queueTimeoutMs: "排队等待（毫秒）",
  enabled: "启用",
  requestTimeoutSecs: "非流式总超时（秒）",
  streamRequestTimeoutSecs: "流式总超时（秒）",
  streamIdleTimeoutSecs: "流式空闲超时（秒）",
  streamFirstOutputTimeoutSecs: "首个输出超时（秒）",
  retryMaxAttempts: "最多故障转移（次）",
  globalMaxConcurrentRequests: "全局最大并发",
  dispatchMaxWaitSecs: "最长排队等待（秒）",
  usageProjectionUpliftPercent: "缓存读写补偿（%）",
  usageProjectionCostFloorEnabled: "启用成本底线",
  usageProjectionCostFloorMarginPercent: "成本补齐余量（%）",
  usageProjectionOutputUpliftMinTokens: "输出补偿阈值（token）",
  usageProjectionOutputUpliftPercent: "输出补偿（%）",
  defaultValue: "max_tokens 补充值",
  policy: "缺失 max_tokens",
  maxBytes: "请求体上限（字节）",
  safetyMarginBytes: "安全余量（字节）",
  trimHistory: "裁剪历史",
  externalEnabled: "用于账号请求",
  mode: "模式",
  inferenceUpstreamMaxAttempts: "单请求推理发送硬上限（次）",
  streamPreOutputHoldSecs: "首输出前静默保留（秒）",
  capacityMode: "满并发处理",
  retryStatusCodes: "跨池重试状态码",
  retryOnNetworkError: "网络错误跨池重试",
  retryOnProtocolError: "协议错误跨池重试",
  samePoolRetryCount: "同池重试次数",
  samePoolRetryStatusCodes: "同池重试状态码",
  samePoolRetryDelayMs: "同池重试间隔（毫秒）",
  transientFailurePriorityPenalty: "失败池临时降权",
  qualityAwareSchedulingEnabled: "启用质量感知调度",
  qualityEwmaAlpha: "质量平滑系数",
  qualitySampleTtlSecs: "质量数据有效期（秒）",
  qualityMinSamples: "最少样本数",
  qualityPriorityWeight: "优先级权重",
  qualityLoadWeight: "负载权重",
  qualityErrorWeight: "失败率权重",
  qualityLatencyWeight: "延迟权重",
  qualityProbationWeight: "临时降级权重",
  qualityTopK: "候选池大小 K",
  degradeWindowSecs: "劣化判定窗口（秒）",
  degradeErrorRateThreshold: "降级失败率阈值",
  degradeProbationSecs: "单次降级时长（秒）",
  maxProbationSecs: "最长降级时长（秒）",
  probeSharePercent: "降级期探测流量（%）",
  recoveryRampSecs: "恢复爬坡时长（秒）",
  autoDisableEnabled: "启用自动禁用",
  autoDisableOnAuthError: "认证错误",
  autoDisableOnSecurityLock: "安全锁定",
  autoDisableOnQuotaExhausted: "额度耗尽",
  autoDisableOnMisconfiguredEndpoint: "配置错误",
  autoDisableOnChannelDisabled: "通道禁用",
  autoDisableFailureThreshold: "触发阈值（次）",
  autoDisableWindowSecs: "统计窗口（秒）",
  autoDisableDurationSecs: "禁用时长（秒）",
  rateLimitCooldownSecs: "429 冷却（秒）",
  serverErrorCooldownSecs: "5xx 冷却（秒）",
  networkErrorCooldownSecs: "网络错误冷却（秒）",
  protocolErrorCooldownSecs: "协议/认证冷却（秒）",
  modelUnavailableCooldownMode: "模型不可用冷却范围",
  modelUnavailableCooldownSecs: "模型不可用冷却（秒）",
  streamPreOutputRetryEnabled: "流式首输出前错误换池",
  streamResponseMode: "SSE 默认转发",
  usageDebugEnabled: "usage 原始数据诊断",
  usageDebugDir: "诊断目录",
  usageDebugMaxBodyBytes: "单条原始片段上限（字节）",
  usageDebugMaxFiles: "最多诊断文件",
  wireDebugEnabled: "wire 交互诊断",
  wireDebugDir: "wire 诊断目录",
  wireDebugMaxBodyBytes: "wire 单条片段上限（字节）",
  wireDebugMaxFiles: "wire 最多诊断文件",
  modelResolutionMode: "模型解析策略",
  compatProfile: "兼容模式",
  tlsBackend: "TLS 后端",
  auditLogRetentionDays: "审计日志保留（天）",
  usageRecordLimit: "内存用量记录上限（条）",
  thinkingTrigger: "自动触发 thinking",
  tooLongHandling: "输入过长处理",
  oversizedImageHandling: "超大图片处理",
  payloadGuardMode: "大小保护触发时机",
};
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
  return (
    <div className="grid gap-5 md:grid-cols-2">
      {Object.entries(fields).map(([key, current]) => {
        const id = `${prefix}-${key}`;
        if (
          current !== null &&
          typeof current === "object" &&
          !Array.isArray(current)
        )
          return (
            <div key={key} className="col-span-full space-y-3 border-t pt-4">
              <Label>{labels[key] ?? key}</Label>
              <ConfigFields
                prefix={id}
                value={current}
                onChange={(next) => onChange({ ...fields, [key]: next })}
              />
            </div>
          );
        const options: EnumOption[] | undefined =
          key === "mode"
            ? [REPORTED_USAGE_FIELD_MODE_OPTIONS, IMAGE_PROCESSING_MODE_OPTIONS, PAYLOAD_GUARD_MODE_OPTIONS]
                .find((table) => table.some((option) => option.value === String(current)))
            : ENUM_OPTIONS_BY_FIELD[key];
        const selected = options?.find((option) => option.value === String(current));
        return (
          <div key={key} className="min-w-0 space-y-2">
            <Label htmlFor={id} title={key}>{labels[key] ?? key}</Label>
            {typeof current === "boolean" ? (
              <Switch
                id={id}
                checked={current}
                onCheckedChange={(v) => onChange({ ...fields, [key]: v })}
              />
            ) : options ? (
              <Select
                value={String(current)}
                onValueChange={(v) => onChange({ ...fields, [key]: v })}
              >
                <SelectTrigger id={id}>
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
            ) : (
              <Input
                id={id}
                type={typeof current === "number" ? "number" : "text"}
                value={
                  Array.isArray(current)
                    ? current.join(", ")
                    : String(current ?? "")
                }
                onChange={(e) =>
                  onChange({
                    ...fields,
                    [key]:
                      typeof current === "number"
                        ? Number(e.target.value)
                        : Array.isArray(current)
                          ? e.target.value
                              .split(",")
                              .map((v) => v.trim())
                              .filter(Boolean)
                              .map((v) =>
                                current.every(
                                  (item) => typeof item === "number",
                                )
                                  ? Number(v)
                                  : v,
                              )
                              .filter(
                                (v) =>
                                  typeof v !== "number" || Number.isFinite(v),
                              )
                          : e.target.value,
                  })
                }
              />
            )}
            {selected?.description && (
              <div className="text-xs leading-4 text-muted-foreground">{selected.description}</div>
            )}
          </div>
        );
      })}
    </div>
  );
}
