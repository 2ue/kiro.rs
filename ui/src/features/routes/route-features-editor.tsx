import { ConfigFields } from "@/components/patterns/config-fields";
import {
  Button,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Switch,
} from "@/components/ui";
import {
  defaultImageProcessing,
  defaultLocalRequestCompat,
  defaultMissingMaxTokens,
  defaultPayloadShaping,
} from "@/lib/runtime-config-defaults";
import type {
  RouteFeatureOverride,
  RouteFeatureSet,
  RoutePayloadGuardConfig,
} from "@/types/api";
const META: Array<[keyof RouteFeatureSet, string]> = [
  ["missingMaxTokens", "补全 max_tokens"],
  ["reasoningCompat", "请求兼容"],
  ["historySanitize", "历史清洗"],
  ["thinkingModelName", "thinking 模型名"],
  ["thinkingTrigger", "自动触发 thinking"],
  ["payloadGuard", "Payload 大小防护"],
  ["payloadShaping", "Payload 内容处理"],
  ["imageProcessing", "图片处理"],
];
function featureDefault(key: keyof RouteFeatureSet): unknown {
  switch (key) {
    case "missingMaxTokens":
      return defaultMissingMaxTokens();
    case "reasoningCompat":
      return defaultLocalRequestCompat();
    case "payloadGuard":
      return {
        enabled: true,
        mode: "on_too_long",
        maxBytes: 4194304,
        safetyMarginBytes: 65536,
        trimHistory: true,
        externalEnabled: true,
        tooLongHandling: "client_compaction",
      } satisfies RoutePayloadGuardConfig;
    case "payloadShaping":
      return defaultPayloadShaping();
    case "imageProcessing":
      return defaultImageProcessing();
    case "thinkingTrigger":
      return "real_request";
    default:
      return true;
  }
}
export function RouteFeaturesEditor({
  value,
  defaults,
  onChange,
}: {
  value: RouteFeatureOverride;
  defaults: RouteFeatureSet;
  onChange: (next: RouteFeatureOverride) => void;
}) {
  return (
    <div className="space-y-4">
      {META.map(([key, label]) => {
        const current = value[key] === undefined ? defaults[key] : value[key];
        const enabled =
          current !== null &&
          current !== false &&
          current !== "off" &&
          !(
            typeof current === "object" &&
            "enabled" in current &&
            !current.enabled
          );
        return (
          <section key={key} className="space-y-4 rounded-xl border p-5">
            <div className="flex items-center justify-between gap-4">
              <div>
                <h3 className="font-medium">{label}</h3>
                <p className="text-xs text-muted-foreground">
                  {value[key] === undefined ? "继承全局" : "路由覆盖"}
                </p>
              </div>
              <div className="flex items-center gap-3">
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => {
                    const next = { ...value };
                    delete next[key];
                    onChange(next);
                  }}
                >
                  继承
                </Button>
                <Switch
                  checked={enabled}
                  onCheckedChange={(v) =>
                    onChange({
                      ...value,
                      [key]: v
                        ? featureDefault(key)
                        : key === "thinkingTrigger"
                          ? "off"
                          : typeof current === "boolean"
                            ? false
                            : null,
                    })
                  }
                />
              </div>
            </div>
            {enabled && typeof current === "object" && current !== null && (
              <ConfigFields
                value={current}
                prefix={key}
                onChange={(next) => onChange({ ...value, [key]: next })}
              />
            )}
            {enabled && key === "thinkingTrigger" && (
              <Select
                value={String(current)}
                onValueChange={(v) =>
                  onChange({
                    ...value,
                    thinkingTrigger: v as RouteFeatureSet["thinkingTrigger"],
                  })
                }
              >
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="real_request">按请求触发</SelectItem>
                  <SelectItem value="always">始终触发</SelectItem>
                </SelectContent>
              </Select>
            )}
          </section>
        );
      })}
    </div>
  );
}
