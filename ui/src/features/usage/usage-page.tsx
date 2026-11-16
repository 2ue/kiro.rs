import { UsageCleanupModal } from "./usage-cleanup-modal";
import type { UsageRecordsPageQuery } from "@/types/usage";
import { getUsageRecordsPage } from "@/api/usage-dashboard";
import {
  EmptyState,
  ErrorState,
  LoadingState,
  PageContainer,
  PageHeader,
} from "@/components/patterns";
import {
  Button,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui";
import { formatCompact, formatDate, formatUsd } from "@/lib/format";
import type { UsageRecord } from "@/types/api";
import { useQuery } from "@tanstack/react-query";
import { Eye, RefreshCw } from "lucide-react";
import { useState } from "react";
import { UsageDetailModal } from "./usage-detail-modal";
export function UsagePage() {
  const [page, setPage] = useState(1),
    [search, setSearch] = useState(""),
    [selected, setSelected] = useState<UsageRecord | null>(null);
  const [cleanup, setCleanup] = useState(false),
    [status, setStatus] = useState("_all"),
    [model, setModel] = useState(""),
    [account, setAccount] = useState(""),
    [since, setSince] = useState(""),
    [until, setUntil] = useState("");
  const filters: UsageRecordsPageQuery = {
    page,
    limit: 50,
    q: search || undefined,
    status:
      status === "_all"
        ? undefined
        : (status as UsageRecordsPageQuery["status"]),
    model: model || undefined,
    accountId: account ? Number(account) : undefined,
    since: since ? new Date(since).toISOString() : undefined,
    until: until ? new Date(until).toISOString() : undefined,
  };
  const query = useQuery({
    queryKey: ["usage-records-page", filters],
    queryFn: () => getUsageRecordsPage(filters),
  });
  if (query.isLoading) return <LoadingState />;
  if (query.error)
    return (
      <ErrorState
        title="读取用量失败"
        action={<Button onClick={() => query.refetch()}>重试</Button>}
      />
    );
  const records = query.data?.records ?? [];
  return (
    <PageContainer>
      <PageHeader
        actions={
          <>
            <Button variant="outline" onClick={() => setCleanup(true)}>
              清理记录
            </Button>
            <Button variant="outline" onClick={() => query.refetch()}>
              <RefreshCw />
              刷新
            </Button>
          </>
        }
      />
      <Input
        className="max-w-md"
        placeholder="搜索请求 id、账号或模型"
        value={search}
        onChange={(e) => {
          setSearch(e.target.value);
          setPage(1);
        }}
      />
      <div className="grid gap-3 md:grid-cols-3">
        <Input
          placeholder="模型"
          value={model}
          onChange={(e) => {
            setModel(e.target.value);
            setPage(1);
          }}
        />
        <Input
          type="number"
          placeholder="账号 id"
          value={account}
          onChange={(e) => {
            setAccount(e.target.value);
            setPage(1);
          }}
        />
        <Select
          value={status}
          onValueChange={(v) => {
            setStatus(v);
            setPage(1);
          }}
        >
          <SelectTrigger>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {[
              ["_all", "全部状态"],
              ["success", "成功"],
              ["error", "失败"],
              ["stream_error", "流式错误"],
              ["upstream_timeout", "上游超时"],
              ["client_dropped", "客户端断开"],
            ].map(([v, label]) => (
              <SelectItem key={v} value={v}>
                {label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <label className="text-sm">
          起始时间
          <Input
            type="datetime-local"
            value={since}
            onChange={(e) => {
              setSince(e.target.value);
              setPage(1);
            }}
          />
        </label>
        <label className="text-sm">
          结束时间
          <Input
            type="datetime-local"
            value={until}
            onChange={(e) => {
              setUntil(e.target.value);
              setPage(1);
            }}
          />
        </label>
      </div>
      {records.length === 0 ? (
        <EmptyState title="暂无请求记录" />
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              {[
                "时间",
                "账号",
                "模型",
                "路由",
                "状态",
                "输入",
                "输出",
                "报告估值",
                "原始估值",
                "账号成本",
                "详情",
              ].map((title) => (
                <TableHead key={title}>{title}</TableHead>
              ))}
            </TableRow>
          </TableHeader>
          <TableBody>
            {records.map((record) => (
              <TableRow key={record.id}>
                <TableCell>{formatDate(record.createdAt)}</TableCell>
                <TableCell>
                  {record.accountName ?? record.accountId ?? "—"}
                </TableCell>
                <TableCell>{record.model}</TableCell>
                <TableCell>{record.endpoint}</TableCell>
                <TableCell>{record.status}</TableCell>
                <TableCell>{formatCompact(record.totalInputTokens)}</TableCell>
                <TableCell>{formatCompact(record.outputTokens)}</TableCell>
                <TableCell>{formatUsd(record.estimatedCostUsd)}</TableCell>
                <TableCell>{formatUsd(record.originalCostUsd)}</TableCell>
                <TableCell>{formatUsd(record.accountCostUsd)}</TableCell>
                <TableCell>
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => setSelected(record)}
                  >
                    <Eye />
                  </Button>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      <div className="flex items-center justify-end gap-4">
        <Button
          variant="outline"
          disabled={page === 1}
          onClick={() => setPage(page - 1)}
        >
          上一页
        </Button>
        <span className="text-sm">第 {page} 页</span>
        <Button
          variant="outline"
          disabled={records.length < 50}
          onClick={() => setPage(page + 1)}
        >
          下一页
        </Button>
      </div>
      <UsageCleanupModal
        open={cleanup}
        onClose={() => {
          setCleanup(false);
          query.refetch();
        }}
      />
      <UsageDetailModal
        record={selected}
        open={selected !== null}
        onClose={() => setSelected(null)}
      />
    </PageContainer>
  );
}
