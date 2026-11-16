import { injectionSetting } from "./injection-model";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui";
import type {
  InjectionCatalogEntry,
  InjectionItemOverride,
  RuntimeConfig,
} from "@/types/api";
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
  return (
    <div className="divide-y">
      {catalog.map((item) => (
        <div
          key={item.id}
          className="flex items-start justify-between gap-4 py-4"
        >
          <div>
            <div className="font-medium">{item.name}</div>
            <p className="text-sm text-muted-foreground">
              {item.impactWhenOff}
            </p>
          </div>
          <Select
            value={
              injection[item.id]?.enabled === undefined
                ? "inherit"
                : injection[item.id]?.enabled
                  ? "on"
                  : "off"
            }
            onValueChange={(value) => {
              const next = { ...injection };
              if (value === "inherit") delete next[item.id];
              else next[item.id] = { enabled: value === "on" };
              onChange(next);
            }}
          >
            <SelectTrigger className="w-52">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="inherit">
                继承全局（
                {injectionSetting(config.promptInjection.defaults, {}, item.id)
                  .enabled
                  ? "开启"
                  : "关闭"}
                ）
              </SelectItem>
              <SelectItem value="on">开启</SelectItem>
              <SelectItem value="off">关闭</SelectItem>
            </SelectContent>
          </Select>
        </div>
      ))}
    </div>
  );
}
