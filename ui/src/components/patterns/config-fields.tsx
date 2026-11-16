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
};
const enums: Record<string, string[]> = {
  capacityMode: ["fail_fast", "wait"],
  streamResponseMode: ["event_passthrough"],
  modelUnavailableCooldownMode: ["disabled", "model", "pool"],
  policy: ["reject", "default_value"],
  thinkingTrigger: ["off", "real_request", "always"],
  tooLongHandling: ["client_compaction", "trim_retry"],
  mode: ["preemptive", "on_too_long"],
  tlsBackend: ["rustls", "native-tls"],
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
        const options =
          key === "mode" &&
          [
            "preserve",
            "sample",
            "cap",
            "fixed",
            "scale",
            "move_delta",
          ].includes(String(current))
            ? ["preserve", "raw", "sample_max", "sample_target"]
            : key === "mode" && (current === "safe" || current === "light")
              ? ["safe", "light"]
              : enums[key];
        return (
          <div key={key} className="space-y-2">
            <Label htmlFor={id}>{labels[key] ?? key}</Label>
            <div className="text-xs text-muted-foreground">{key}</div>
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
                  {options.map((v) => (
                    <SelectItem key={v} value={v}>
                      {v}
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
          </div>
        );
      })}
    </div>
  );
}
