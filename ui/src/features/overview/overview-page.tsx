import { getUsageOverview } from "@/api/usage-dashboard";
import {
  EmptyState,
  ErrorState,
  LoadingState,
  PageContainer,
  PageHeader,
} from "@/components/patterns";
import {
  Button,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui";
import { formatCompact, formatDate, formatUsd } from "@/lib/format";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import {
  Bar,
  BarChart,
  CartesianGrid,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
export function OverviewPage() {
  const [range, setRange] = useState("today"),
    query = useQuery({
      queryKey: ["usage-overview", range],
      queryFn: () => getUsageOverview({ range }),
      refetchInterval: 30000,
    });
  if (query.isLoading) return <LoadingState />;
  if (query.error || !query.data)
    return (
      <ErrorState
        title="读取总览失败"
        action={<Button onClick={() => query.refetch()}>重试</Button>}
      />
    );
  const data = query.data,
    totals = data.totals;
  const cards = [
    ["请求数", formatCompact(totals.requests)],
    ["报告量", formatCompact(totals.inputTokens + totals.outputTokens)],
    ["上游原始量", formatCompact(data.rawTokens)],
    ["报告估值", formatUsd(data.billing.reportedCostUsd)],
    ["原始估值", formatUsd(data.billing.rawCostUsd)],
    ["账号成本", formatUsd(data.accountCostUsd)],
    ["差额", formatUsd(data.billing.reportedCostUsd - data.accountCostUsd)],
  ];
  return (
    <PageContainer>
      <PageHeader
        actions={
          <Select value={range} onValueChange={setRange}>
            <SelectTrigger className="w-48">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="today">今天</SelectItem>
              <SelectItem value="yesterday">昨天</SelectItem>
              <SelectItem value="7d">近 7 天</SelectItem>
              <SelectItem value="30d">近 30 天</SelectItem>
            </SelectContent>
          </Select>
        }
      />
      <div className="grid gap-5 border-y py-5 sm:grid-cols-2 xl:grid-cols-4">
        {cards.map(([label, value]) => (
          <div key={label}>
            <div className="text-sm text-muted-foreground">{label}</div>
            <div className="mt-2 text-2xl font-semibold tabular-nums">
              {value}
            </div>
          </div>
        ))}
      </div>
      <section className="space-y-4">
        <h2 className="font-semibold">请求趋势</h2>
        {data.series.length ? (
          <div className="h-72">
            <ResponsiveContainer>
              <BarChart data={data.series}>
                <CartesianGrid strokeDasharray="3 3" vertical={false} />
                <XAxis
                  dataKey="bucketStart"
                  tickFormatter={(v) => formatDate(String(v)).slice(0, 11)}
                />
                <YAxis />
                <Tooltip />
                <Bar
                  dataKey="metrics.requests"
                  name="请求数"
                  fill="currentColor"
                />
              </BarChart>
            </ResponsiveContainer>
          </div>
        ) : (
          <EmptyState title="暂无用量" />
        )}
      </section>
      <p className="text-xs text-muted-foreground">
        数据更新：{formatDate(data.generatedAt)}
      </p>
    </PageContainer>
  );
}
