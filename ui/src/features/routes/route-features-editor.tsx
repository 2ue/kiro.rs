import { ConfigFields } from "@/components/patterns/config-fields";
import { Badge, Button, Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui";
import { SettingGroup, SettingRow, TriSelect } from "@/features/prompt-injection/route-injection-editor";
import { THINKING_TRIGGER_OPTIONS } from "@/lib/enum-labels";
import {
  defaultImageProcessing,
  defaultLocalRequestCompat,
  defaultMissingMaxTokens,
  defaultPayloadShaping,
} from "@/lib/runtime-config-defaults";
import type { RouteFeatureOverride, RouteFeatureSet, RoutePayloadGuardConfig } from "@/types/api";
import { RotateCcw } from "lucide-react";

type ObjectFeature = "missingMaxTokens" | "reasoningCompat" | "payloadGuard" | "payloadShaping" | "imageProcessing";
type BoolFeature = "historySanitize" | "thinkingModelName" | "returnRequestModel";

const FEATURE_TEXT: Record<Exclude<keyof RouteFeatureSet, "thinkingTrigger">, { label: string; desc: string }> = {
  missingMaxTokens: { label: "补全 max_tokens", desc: "请求没带 max_tokens 时自动补一个值或直接拒绝。开启后可在下方调整补充值。" },
  reasoningCompat: { label: "参数兼容", desc: "客户端 thinking / effort 等参数写法不规范时改写成合法形式，而不是报错。" },
  historySanitize: { label: "历史清洗", desc: "发往上游前清理历史消息里的孤立 tool_use / tool_result 等协议问题。" },
  thinkingModelName: { label: "-thinking 模型名", desc: "请求 claude-xxx-thinking 这类模型名时，自动补上 thinking 参数。关闭后这类模型名按普通模型处理。" },
  payloadGuard: { label: "请求体大小保护", desc: "请求体超过上限时按规则裁剪历史或交给客户端压缩，避免上游直接拒绝。" },
  payloadShaping: { label: "请求体内容处理", desc: "压缩工具说明、截短历史工具结果与网页抓取正文，减少无效 token。" },
  imageProcessing: { label: "图片处理", desc: "过大的图片按上限压缩或移除，避免上游拒绝整条请求。" },
  returnRequestModel: {
    label: "返回请求模型",
    desc: "开启时返回给调用方的 model 是调用方请求的模型，不暴露映射后或上游实际返回的模型；关闭时透传上游返回的模型（会暴露映射后的模型）。用量记录里的上游返回模型不受影响。",
  },
};

const GROUPS: Array<{ title: string; desc: string; keys: Array<keyof RouteFeatureSet> }> = [
  { title: "请求兼容", desc: "让写法不规范的客户端请求也能被上游接受。", keys: ["missingMaxTokens", "reasoningCompat", "historySanitize"] },
  { title: "thinking", desc: "决定这条路由上 thinking 何时开启、模型名怎么解析。", keys: ["thinkingModelName", "thinkingTrigger"] },
  { title: "请求体", desc: "大请求的裁剪、压缩与图片处理。", keys: ["payloadGuard", "payloadShaping", "imageProcessing"] },
  { title: "响应", desc: "返回给调用方的内容怎么处理。", keys: ["returnRequestModel"] },
];

function featureDefault(key: ObjectFeature): NonNullable<RouteFeatureSet[ObjectFeature]> {
  switch (key) {
    case "missingMaxTokens": return defaultMissingMaxTokens();
    case "reasoningCompat": return defaultLocalRequestCompat();
    case "payloadGuard":
      return { enabled: true, mode: "on_too_long", maxBytes: 4194304, safetyMarginBytes: 65536, trimHistory: true, externalEnabled: true, tooLongHandling: "client_compaction" } satisfies RoutePayloadGuardConfig;
    case "payloadShaping": return defaultPayloadShaping();
    case "imageProcessing": return defaultImageProcessing();
  }
}

function isOn(value: unknown): boolean {
  return value !== null && value !== undefined && value !== false && value !== "off" && !(typeof value === "object" && "enabled" in (value as object) && !(value as { enabled: boolean }).enabled);
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
  const set = <K extends keyof RouteFeatureSet>(key: K, next: RouteFeatureSet[K] | undefined) => {
    const updated = { ...value };
    if (next === undefined) delete updated[key];
    else updated[key] = next;
    onChange(updated);
  };
  const overridden = Object.keys(value).length;

  const renderBool = (key: BoolFeature) => (
    <SettingRow
      key={key}
      title={FEATURE_TEXT[key].label}
      badge={<Badge size="xs" tone="secondary">功能</Badge>}
      control={<TriSelect id={`route-feature-${key}`} value={value[key]} inherited={defaults[key]} onChange={(next) => set(key, next)} />}
    >
      <p className="text-xs leading-5 text-muted-foreground">{FEATURE_TEXT[key].desc}</p>
    </SettingRow>
  );

  const renderObject = (key: ObjectFeature) => {
    const own = value[key];
    const inheritedOn = isOn(defaults[key]);
    const state: boolean | undefined = own === undefined ? undefined : isOn(own);
    const effective = own === undefined ? defaults[key] : own;
    return (
      <SettingRow
        key={key}
        title={FEATURE_TEXT[key].label}
        badge={<Badge size="xs" tone="secondary">功能</Badge>}
        control={
          <TriSelect
            id={`route-feature-${key}`}
            value={state}
            inherited={inheritedOn}
            onChange={(next) => set(key, next === undefined ? undefined : next ? (isOn(effective) ? (effective as RouteFeatureSet[ObjectFeature]) : featureDefault(key)) : null)}
          />
        }
      >
        <p className="text-xs leading-5 text-muted-foreground">{FEATURE_TEXT[key].desc}</p>
        {own !== undefined && isOn(own) && typeof own === "object" && own !== null && (
          <div className="mt-3 rounded-md border bg-muted/20 p-3">
            <ConfigFields value={own} prefix={key} onChange={(next) => set(key, next as unknown as RouteFeatureSet[ObjectFeature])} />
          </div>
        )}
        {own === undefined && isOn(defaults[key]) && (
          <p className="text-[11px] leading-4 text-muted-foreground">沿用全局参数；选“开启”后可在这里单独调整。</p>
        )}
      </SettingRow>
    );
  };

  const renderThinkingTrigger = () => {
    const inheritedLabel = THINKING_TRIGGER_OPTIONS.find((o) => o.value === defaults.thinkingTrigger)?.label ?? defaults.thinkingTrigger;
    return (
      <SettingRow
        key="thinkingTrigger"
        title="thinking 触发方式"
        badge={<Badge size="xs" tone="secondary">功能</Badge>}
        control={
          <Select
            value={value.thinkingTrigger ?? "inherit"}
            onValueChange={(raw) => set("thinkingTrigger", raw === "inherit" ? undefined : (raw as RouteFeatureSet["thinkingTrigger"]))}
          >
            <SelectTrigger id="route-feature-thinking-trigger" size="sm" aria-label="thinking 触发方式"><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="inherit">{`跟随默认（当前${inheritedLabel}）`}</SelectItem>
              {THINKING_TRIGGER_OPTIONS.map((option) => <SelectItem key={option.value} value={option.value}>{option.label}</SelectItem>)}
            </SelectContent>
          </Select>
        }
      >
        <p className="text-xs leading-5 text-muted-foreground">
          按请求触发：用户消息里有 Claude Code 的深度思考信号（如 ultrathink）时开启并强制可见。总是触发：请求没有关闭 thinking 时都开启。不自动开启：只看客户端自己的 thinking 参数。
        </p>
      </SettingRow>
    );
  };

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="text-xs leading-5 text-muted-foreground">每一项单独设置，“跟随默认”使用运行配置里的全局默认。</p>
        <Button size="xs" variant="outline" disabled={overridden === 0} onClick={() => onChange({})}>
          <RotateCcw />
          全部恢复默认
        </Button>
      </div>
      {GROUPS.map((group) => (
        <SettingGroup key={group.title} title={group.title} desc={group.desc}>
          {group.keys.map((key) =>
            key === "thinkingTrigger" ? renderThinkingTrigger() : key === "historySanitize" || key === "thinkingModelName" || key === "returnRequestModel" ? renderBool(key) : renderObject(key as ObjectFeature),
          )}
        </SettingGroup>
      ))}
    </div>
  );
}
