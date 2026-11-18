import { injectionSetting } from "./injection-model";
import { Badge, Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui";
import type { InjectionCatalogEntry, InjectionItemOverride, RuntimeConfig } from "@/types/api";
import { Link } from "react-router-dom";

/** 三态选择：跟随全局默认 / 开启 / 关闭。 */
export function TriSelect({
  id,
  value,
  inherited,
  disabled,
  onChange,
}: {
  id: string;
  value: boolean | undefined;
  inherited: boolean;
  disabled?: boolean;
  onChange: (value: boolean | undefined) => void;
}) {
  return (
    <Select
      value={value === undefined ? "inherit" : value ? "on" : "off"}
      disabled={disabled}
      onValueChange={(raw) => onChange(raw === "inherit" ? undefined : raw === "on")}
    >
      <SelectTrigger id={id} size="sm"><SelectValue /></SelectTrigger>
      <SelectContent>
        <SelectItem value="inherit">{`跟随默认（当前${inherited ? "开" : "关"}）`}</SelectItem>
        <SelectItem value="on">开启</SelectItem>
        <SelectItem value="off">关闭</SelectItem>
      </SelectContent>
    </Select>
  );
}

/** 一行设置：左侧标题、说明，右侧控件。与 main 的路由行为编辑器同一排版。 */
export function SettingRow({
  title,
  code,
  badge,
  note,
  control,
  children,
}: {
  title: string;
  code?: string;
  badge?: React.ReactNode;
  note?: string;
  control: React.ReactNode;
  children?: React.ReactNode;
}) {
  return (
    <div className="grid gap-x-4 gap-y-1.5 py-3 first:pt-0 last:pb-0 sm:grid-cols-[minmax(0,1fr)_14rem] sm:items-start">
      <div className="min-w-0 space-y-1">
        <div className="flex flex-wrap items-center gap-1.5">
          {code && <Badge size="xs" tone="neutral">{code}</Badge>}
          <span className="text-sm font-medium">{title}</span>
          {badge}
          {note && <span className="text-[11px] text-muted-foreground">· {note}</span>}
        </div>
        {children}
      </div>
      <div className="min-w-0">{control}</div>
    </div>
  );
}

/** 分组容器：标题 + 一句说明，内部按行分隔。 */
export function SettingGroup({ title, desc, children }: { title: string; desc: string; children: React.ReactNode }) {
  return (
    <section className="rounded-lg border bg-background">
      <header className="border-b px-4 py-2.5">
        <h3 className="text-[13px] font-semibold">{title}</h3>
        <p className="text-[11px] leading-4 text-muted-foreground">{desc}</p>
      </header>
      <div className="divide-y px-4 py-3">{children}</div>
    </section>
  );
}

const CATEGORY_LABEL: Record<InjectionCatalogEntry["category"], string> = {
  operator: "运营引导",
  protocol: "协议兼容",
  feature: "功能附带",
  recovery: "故障恢复",
};

export function RouteInjectionEditor({
  catalog,
  config,
  injection,
  onChange,
}: {
  catalog: InjectionCatalogEntry[];
  config: RuntimeConfig;
  injection: Partial<Record<string, InjectionItemOverride>>;
  onChange: (next: Partial<Record<string, InjectionItemOverride>>) => void;
}) {
  const strict = config.compatProfile === "anthropic-strict";
  return (
    <div className="space-y-4">
      <p className="text-xs leading-5 text-muted-foreground">
        每一项单独设置，“跟随默认”使用全局默认。注入的文本在
        <Link className="mx-1 text-primary underline-offset-2 hover:underline" to="/policy-templates">策略模板</Link>
        页面编辑。
      </p>
      <SettingGroup title="提示词注入" desc="决定发往上游的 system 里附加哪些提示词；开关按路由生效，文本全局共用。">
        {catalog.map((item) => {
          const own = injection[item.id]?.enabled;
          const inherited = injectionSetting(config.promptInjection.defaults, {}, item.id).enabled;
          const effective = own ?? inherited;
          const blocked = strict && item.strictBlocks ? "anthropic-strict 模式下不注入" : null;
          const text = item.texts[0] ? (config.promptInjection.texts[item.texts[0].key] ?? item.texts[0].defaultText) : (item.readOnlyVariants[0]?.defaultText ?? "");
          return (
            <SettingRow
              key={item.id}
              title={item.name}
              code={item.code}
              badge={
                <>
                  <Badge size="xs" tone="neutral">{CATEGORY_LABEL[item.category]}</Badge>
                  {item.recommendation === "keep_on" && <Badge size="xs" tone="warning">建议开</Badge>}
                </>
              }
              control={
                <TriSelect
                  id={`route-injection-${item.id}`}
                  value={own}
                  inherited={inherited}
                  onChange={(enabled) => {
                    const next = { ...injection };
                    if (enabled === undefined) delete next[item.id];
                    else next[item.id] = { enabled };
                    onChange(next);
                  }}
                />
              }
            >
              <p className="text-xs leading-5 text-muted-foreground">位置：{item.position}。{item.trigger}。</p>
              {item.id === "custom" && !text.trim() && <p className="text-xs leading-5 text-muted-foreground">还没有填写文本，开启也不会注入。</p>}
              {effective && blocked && <p className="text-xs text-warning">不生效：{blocked}</p>}
              {item.recommendation === "keep_on" && !effective && <p className="text-xs text-destructive">关闭后：{item.impactWhenOff}</p>}
              {text.trim() && (
                <p className="line-clamp-2 break-all font-mono text-[0.68rem] leading-4 text-muted-foreground/80" title={text}>
                  {text.slice(0, 200)}
                </p>
              )}
            </SettingRow>
          );
        })}
      </SettingGroup>
    </div>
  );
}
