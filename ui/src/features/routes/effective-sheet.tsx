import { ErrorState, LoadingState } from "@/components/patterns";
import {
  Sheet,
  SheetBody,
  SheetContent,
  SheetHeader,
  SheetTitle,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui";
function flatten(value: unknown, path = ""): Array<[string, unknown]> {
  if (value !== null && typeof value === "object" && !Array.isArray(value))
    return Object.entries(value).flatMap(([key, v]) =>
      flatten(v, path ? `${path}.${key}` : key),
    );
  return [[path, value]];
}
export function EffectiveSheet({
  open,
  onOpenChange,
  title,
  value,
  sources,
  loading,
  error,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  title: string;
  value?: unknown;
  sources?: Record<string, string>;
  loading?: boolean;
  error?: unknown;
}) {
  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent className="sm:max-w-3xl">
        <SheetHeader>
          <SheetTitle>{title}</SheetTitle>
        </SheetHeader>
        <SheetBody>
          {loading ? (
            <LoadingState />
          ) : error ? (
            <ErrorState title="读取生效配置失败" />
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>字段</TableHead>
                  <TableHead>生效值</TableHead>
                  <TableHead>来源层</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {flatten(value).map(([key, v]) => (
                  <TableRow key={key}>
                    <TableCell className="font-mono text-xs break-all">
                      {key}
                    </TableCell>
                    <TableCell className="break-all text-xs">
                      {JSON.stringify(v)}
                    </TableCell>
                    <TableCell>{sourceLayerLabel(sources?.[key] ?? "builtin_default")}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </SheetBody>
      </SheetContent>
    </Sheet>
  );
}

/** 生效参数的来源层：哪一层配置决定了这个值。 */
function sourceLayerLabel(layer: string): string {
  const labels: Record<string, string> = {
    builtin_default: "内置默认",
    global: "全局配置",
    global_template: "全局模板",
    template: "策略模板",
    route: "本路由",
    account: "本账号",
    account_override: "账号覆盖",
  };
  return labels[layer] ?? layer;
}
