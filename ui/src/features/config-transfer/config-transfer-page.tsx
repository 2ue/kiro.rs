import {
  exportConfig,
  importConfig,
  type ConfigPackage,
  type TransferReport,
} from "@/api/config-transfer";
import {
  EmptyState,
  PageContainer,
  PageHeader,
  useConfirm,
} from "@/components/patterns";
import {
  Button,
  Input,
  Switch,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui";
import { extractErrorMessage } from "@/lib/utils";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Download, Upload } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
/** 预览差异里每类条目的中文名，键与后端 TransferChange.kind 的取值一一对应。 */
const CHANGE_KIND_LABELS: Record<string, string> = {
  account: "账号",
  routeScenario: "路由场景",
  modelPricing: "模型价格",
  manualModel: "手动模型",
  requestApiKey: "请求 Key",
  runtimeConfig: "运行配置",
};

export function ConfigTransferPage() {
  const [secrets, setSecrets] = useState(false),
    [bundle, setBundle] = useState<ConfigPackage | null>(null),
    [report, setReport] = useState<TransferReport | null>(null),
    confirm = useConfirm(),
    client = useQueryClient();
  const exporter = useMutation({
    mutationFn: () => exportConfig(secrets),
    onSuccess: (value) => {
      const url = URL.createObjectURL(
        new Blob([JSON.stringify(value, null, 2)], {
          type: "application/json",
        }),
      );
      const a = document.createElement("a");
      a.href = url;
      a.download = `claude-pool-config-${Date.now()}.json`;
      a.click();
      URL.revokeObjectURL(url);
    },
    onError: (error) => toast.error(extractErrorMessage(error)),
  });
  const importer = useMutation({
    mutationFn: ({
      value,
      dryRun,
    }: {
      value: ConfigPackage;
      dryRun: boolean;
    }) => importConfig(value, dryRun),
    onSuccess: (value) => {
      setReport(value);
      if (!value.dryRun) {
        toast.success("配置已应用");
        client.invalidateQueries();
      }
    },
    onError: (error) => toast.error(extractErrorMessage(error)),
  });
  return (
    <PageContainer>
      <PageHeader
        actions={
          <>
            <label className="flex items-center gap-2 text-sm" title="导出的配置包里带上账号密钥与请求 Key 明文；不勾选时这些字段写成 <redacted>，只能用于预览">
              <Switch checked={secrets} onCheckedChange={setSecrets} />
              包含密钥
            </label>
            <Button
              disabled={exporter.isPending}
              onClick={() => exporter.mutate()}
            >
              <Download />
              导出配置
            </Button>
          </>
        }
      />
      <section className="space-y-4 rounded-xl border p-5">
        <h2 className="font-semibold">导入配置</h2>
        <p className="text-sm text-muted-foreground">
          选择文件后先预览差异。应用时按自然键全量覆盖；被移除的条目会删除，含
          &lt;redacted&gt; 的包只能预览。
        </p>
        <Input
          type="file"
          accept=".json,application/json"
          disabled={importer.isPending}
          onChange={async (e) => {
            setBundle(null);
            setReport(null);
            const file = e.target.files?.[0];
            if (file)
              try {
                const value = JSON.parse(await file.text()) as ConfigPackage;
                setBundle(value);
                importer.mutate({ value, dryRun: true });
              } catch {
                toast.error("配置文件不是有效 JSON");
              }
          }}
        />
        {report ? (
          <>
            <p className="text-sm">
              {report.unchanged} 条无变化，{report.changes.length} 条变更
            </p>
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>类型</TableHead>
                  <TableHead>自然键</TableHead>
                  <TableHead>动作</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {report.changes.map((change) => (
                  <TableRow key={`${change.kind}:${change.naturalKey}`}>
                    <TableCell>{CHANGE_KIND_LABELS[change.kind] ?? change.kind}</TableCell>
                    <TableCell>{change.naturalKey}</TableCell>
                    <TableCell>
                      {
                        { add: "新增", overwrite: "覆盖", delete: "删除" }[
                          change.action
                        ]
                      }
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
            <Button
              disabled={!bundle || !report.dryRun || importer.isPending}
              onClick={async () => {
                if (
                  bundle &&
                  (await confirm({
                    title: "应用导入配置",
                    message: `将全量覆盖配置，共 ${report.changes.length} 条变更。`,
                    confirmText: "确认应用",
                    tone: "danger",
                  }))
                )
                  importer.mutate({ value: bundle, dryRun: false });
              }}
            >
              <Upload />
              确认应用
            </Button>
          </>
        ) : (
          <EmptyState
            title={
              importer.isPending ? "正在校验配置" : "选择配置文件以预览差异"
            }
          />
        )}
      </section>
    </PageContainer>
  );
}
