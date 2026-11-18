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
  maxQueuedRequests: "排队上限",
  queueTimeoutMs: "排队等待（毫秒）",
  enabled: "启用",
  requestTimeoutSecs: "请求超时（秒）",
  streamRequestTimeoutSecs: "流式请求超时（秒）",
  streamIdleTimeoutSecs: "流式空闲超时（秒）",
  streamFirstOutputTimeoutSecs: "首输出超时（秒）",
  retryMaxAttempts: "跨账号重试次数",
  globalMaxConcurrentRequests: "全局并发上限",
  dispatchMaxWaitSecs: "调度等待（秒）",
  usageProjectionUpliftPercent: "输入放大（%）",
  usageProjectionCostFloorEnabled: "启用成本下限",
  usageProjectionCostFloorMarginPercent: "成本下限余量（%）",
  usageProjectionOutputUpliftMinTokens: "输出放大最小量",
  usageProjectionOutputUpliftPercent: "输出放大（%）",
  defaultValue: "默认 max_tokens",
  policy: "缺少 max_tokens 的处理",
  maxBytes: "字节上限",
  safetyMarginBytes: "安全余量（字节）",
  trimHistory: "裁剪历史",
  externalEnabled: "用于账号请求",
  mode: "模式",
  inferenceUpstreamMaxAttempts: "单次请求最多尝试账号数",
  streamPreOutputHoldSecs: "首输出前缓冲（秒）",
  capacityMode: "账号槽位满时",
  retryStatusCodes: "可跨账号重试的状态码",
  retryOnNetworkError: "网络错误跨账号重试",
  retryOnProtocolError: "协议错误跨账号重试",
  samePoolRetryCount: "同账号重试次数",
  samePoolRetryStatusCodes: "同账号重试的状态码",
  samePoolRetryDelayMs: "同账号重试间隔（毫秒）",
  transientFailurePriorityPenalty: "瞬态失败降权分",
  qualityAwareSchedulingEnabled: "质量感知调度",
  qualityEwmaAlpha: "质量平滑系数",
  qualitySampleTtlSecs: "质量样本有效期（秒）",
  qualityMinSamples: "参与评分最少样本数",
  qualityPriorityWeight: "优先级权重",
  qualityLoadWeight: "负载权重",
  qualityErrorWeight: "失败率权重",
  qualityLatencyWeight: "首字延迟权重",
  qualityProbationWeight: "临时避让权重",
  qualityTopK: "候选池大小",
  degradeWindowSecs: "劣化判定窗口（秒）",
  degradeErrorRateThreshold: "触发降级的失败率",
  degradeProbationSecs: "临时降级时长（秒）",
  maxProbationSecs: "最长降级时长（秒）",
  probeSharePercent: "降级期探测流量（%）",
  recoveryRampSecs: "恢复爬坡时长（秒）",
  autoDisableEnabled: "连续失败自动停用",
  autoDisableOnAuthError: "鉴权失败即停用",
  autoDisableOnSecurityLock: "安全锁定即停用",
  autoDisableOnQuotaExhausted: "额度耗尽即停用",
  autoDisableOnMisconfiguredEndpoint: "端点配置错误即停用",
  autoDisableOnChannelDisabled: "上游渠道已禁用即停用",
  autoDisableFailureThreshold: "停用阈值（次）",
  autoDisableWindowSecs: "失败计数窗口（秒）",
  autoDisableDurationSecs: "停用时长（秒）",
  rateLimitCooldownSecs: "限流冷却（秒）",
  serverErrorCooldownSecs: "服务端错误冷却（秒）",
  networkErrorCooldownSecs: "网络错误冷却（秒）",
  protocolErrorCooldownSecs: "协议错误冷却（秒）",
  modelUnavailableCooldownMode: "模型不可用时冷却范围",
  modelUnavailableCooldownSecs: "模型不可用冷却（秒）",
  streamPreOutputRetryEnabled: "首输出前失败换号",
  streamResponseMode: "流式响应处理",
  usageDebugEnabled: "记录 usage 诊断文件",
  usageDebugDir: "usage 诊断目录",
  usageDebugMaxBodyBytes: "usage 诊断单文件上限（字节）",
  usageDebugMaxFiles: "usage 诊断文件数上限",
  wireDebugEnabled: "记录请求交互诊断文件",
  wireDebugDir: "交互诊断目录",
  wireDebugMaxBodyBytes: "交互诊断单文件上限（字节）",
  wireDebugMaxFiles: "交互诊断文件数上限",
  modelResolutionMode: "模型名解析",
  compatProfile: "兼容档位",
  tlsBackend: "TLS 实现",
  auditLogRetentionDays: "审计日志保留（天）",
  usageRecordLimit: "内存用量记录上限（条）",
  thinkingTrigger: "thinking 触发方式",
  tooLongHandling: "输入过长时",
  oversizedImageHandling: "超大图片处理",
  payloadGuardMode: "payload 裁剪时机",
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
