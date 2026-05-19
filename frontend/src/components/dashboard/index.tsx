import { useMemo, useState } from 'react'
import {
  Activity,
  BarChart3,
  CalendarDays,
  CheckCircle2,
  DollarSign,
} from 'lucide-react'
import {
  Bar,
  BarChart,
  CartesianGrid,
  Cell,
  Legend,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip as ReTooltip,
  XAxis,
  YAxis,
} from 'recharts'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useCredentialsList } from '@/hooks/use-credentials'
import { useUsageStats, useUsageSummary } from '@/hooks/use-usage'
import {
  formatNumber,
  formatPercent,
  formatUsd,
} from '@/lib/utils'
import type { UsageStatsQuery } from '@/types/api'

type RangePreset = 'today' | 'yesterday' | '24h' | '7d' | '30d' | 'custom'

interface RangeSpec {
  since: string
  until: string
  bucket: 'hour' | 'day'
}

function isoNow(): Date {
  return new Date()
}

function startOfLocalDay(d: Date): Date {
  const x = new Date(d)
  x.setHours(0, 0, 0, 0)
  return x
}

function endOfLocalDay(d: Date): Date {
  const x = new Date(d)
  x.setHours(23, 59, 59, 999)
  return x
}

function buildPresetRange(preset: RangePreset): RangeSpec {
  const now = isoNow()
  switch (preset) {
    case 'today': {
      const start = startOfLocalDay(now)
      return { since: start.toISOString(), until: now.toISOString(), bucket: 'hour' }
    }
    case 'yesterday': {
      const yest = new Date(now)
      yest.setDate(yest.getDate() - 1)
      return {
        since: startOfLocalDay(yest).toISOString(),
        until: endOfLocalDay(yest).toISOString(),
        bucket: 'hour',
      }
    }
    case '24h': {
      const since = new Date(now.getTime() - 24 * 3600 * 1000)
      return { since: since.toISOString(), until: now.toISOString(), bucket: 'hour' }
    }
    case '7d': {
      const since = new Date(now.getTime() - 7 * 24 * 3600 * 1000)
      return { since: since.toISOString(), until: now.toISOString(), bucket: 'day' }
    }
    case '30d': {
      const since = new Date(now.getTime() - 30 * 24 * 3600 * 1000)
      return { since: since.toISOString(), until: now.toISOString(), bucket: 'day' }
    }
    case 'custom':
    default: {
      // 默认给到今天作为兜底
      return buildPresetRange('today')
    }
  }
}

const PRESET_LABELS: Record<RangePreset, string> = {
  today: '今天',
  yesterday: '昨天',
  '24h': '最近 24 小时',
  '7d': '最近 7 天',
  '30d': '最近 30 天',
  custom: '自定义',
}

const COLORS = [
  'hsl(217, 91%, 60%)',
  'hsl(142, 71%, 45%)',
  'hsl(38, 92%, 50%)',
  'hsl(0, 84%, 60%)',
  'hsl(280, 70%, 60%)',
  'hsl(180, 70%, 45%)',
  'hsl(330, 75%, 60%)',
  'hsl(50, 95%, 55%)',
]

function fmtBucketLabel(iso: string, bucket: 'hour' | 'day'): string {
  const d = new Date(iso)
  if (bucket === 'hour') {
    return `${String(d.getMonth() + 1).padStart(2, '0')}/${String(d.getDate()).padStart(2, '0')} ${String(d.getHours()).padStart(2, '0')}:00`
  }
  return `${String(d.getMonth() + 1).padStart(2, '0')}/${String(d.getDate()).padStart(2, '0')}`
}

function toDateInputValue(iso: string): string {
  const d = new Date(iso)
  const y = d.getFullYear()
  const m = String(d.getMonth() + 1).padStart(2, '0')
  const day = String(d.getDate()).padStart(2, '0')
  return `${y}-${m}-${day}`
}

export default function DashboardPage() {
  const [preset, setPreset] = useState<RangePreset>('today')
  const [customSince, setCustomSince] = useState<string>(
    toDateInputValue(buildPresetRange('7d').since),
  )
  const [customUntil, setCustomUntil] = useState<string>(
    toDateInputValue(new Date().toISOString()),
  )

  const range = useMemo<RangeSpec>(() => {
    if (preset !== 'custom') return buildPresetRange(preset)
    const since = startOfLocalDay(new Date(customSince)).toISOString()
    const until = endOfLocalDay(new Date(customUntil)).toISOString()
    const span = new Date(until).getTime() - new Date(since).getTime()
    return {
      since,
      until,
      bucket: span <= 48 * 3600 * 1000 ? 'hour' : 'day',
    }
  }, [preset, customSince, customUntil])

  const statsQuery: UsageStatsQuery = useMemo(
    () => ({ since: range.since, until: range.until, bucket: range.bucket }),
    [range],
  )

  const stats = useUsageStats(statsQuery)
  const summary = useUsageSummary()
  const credentials = useCredentialsList()

  const credentialLabels = useMemo(() => {
    const map = new Map<number, string>()
    for (const c of credentials.data?.credentials ?? []) {
      map.set(c.id, c.email || c.maskedApiKey || `凭据 #${c.id}`)
    }
    return map
  }, [credentials.data?.credentials])

  const timelineData = useMemo(
    () =>
      (stats.data?.timeline ?? []).map((b) => ({
        label: fmtBucketLabel(b.bucket, stats.data?.bucket ?? 'hour'),
        bucket: b.bucket,
        requests: b.requests,
        cost: Number(b.costUsd.toFixed(6)),
        tokens: b.tokens,
      })),
    [stats.data],
  )

  const byModelData = useMemo(
    () =>
      (stats.data?.byModel ?? [])
        .slice(0, 8)
        .map((m) => ({
          model: m.model.length > 22 ? `${m.model.slice(0, 20)}…` : m.model,
          fullModel: m.model,
          requests: m.requests,
          cost: Number(m.costUsd.toFixed(6)),
          tokens: m.tokens,
        })),
    [stats.data?.byModel],
  )

  const byCredentialData = useMemo(
    () =>
      (stats.data?.byCredential ?? [])
        .slice(0, 8)
        .map((c) => ({
          label:
            credentialLabels.get(c.credentialId) ?? `凭据 #${c.credentialId}`,
          credentialId: c.credentialId,
          requests: c.requests,
          cost: Number(c.costUsd.toFixed(6)),
          tokens: c.tokens,
        })),
    [stats.data?.byCredential, credentialLabels],
  )

  const successRate =
    summary.data && summary.data.totalRequests > 0
      ? summary.data.successRequests / summary.data.totalRequests
      : Number.NaN

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">仪表盘</h1>
          <p className="text-sm text-muted-foreground">
            按时间范围聚合的请求 / token / 美元成本,支持账号、模型、汇总维度
          </p>
        </div>

        {/* 时间范围选择器 */}
        <div className="flex flex-wrap items-center gap-2">
          <CalendarDays className="h-4 w-4 text-muted-foreground" />
          <div className="flex flex-wrap gap-1 rounded-md border bg-muted/40 p-1">
            {(
              ['today', 'yesterday', '24h', '7d', '30d'] as RangePreset[]
            ).map((p) => (
              <Button
                key={p}
                size="sm"
                variant={preset === p ? 'default' : 'ghost'}
                className="h-7 px-3"
                onClick={() => setPreset(p)}
              >
                {PRESET_LABELS[p]}
              </Button>
            ))}
            <Popover>
              <PopoverTrigger asChild>
                <Button
                  size="sm"
                  variant={preset === 'custom' ? 'default' : 'ghost'}
                  className="h-7 px-3"
                  onClick={() => setPreset('custom')}
                >
                  自定义
                </Button>
              </PopoverTrigger>
              <PopoverContent align="end" className="w-72 space-y-3">
                <div className="space-y-1">
                  <Label className="text-xs">开始日期</Label>
                  <Input
                    type="date"
                    value={customSince}
                    onChange={(e) => {
                      setCustomSince(e.target.value)
                      setPreset('custom')
                    }}
                  />
                </div>
                <div className="space-y-1">
                  <Label className="text-xs">结束日期</Label>
                  <Input
                    type="date"
                    value={customUntil}
                    onChange={(e) => {
                      setCustomUntil(e.target.value)
                      setPreset('custom')
                    }}
                  />
                </div>
                <div className="text-xs text-muted-foreground">
                  自动选择 bucket:≤ 48h 用小时,否则用天
                </div>
              </PopoverContent>
            </Popover>
          </div>
          <Select
            value={range.bucket}
            onValueChange={(v) => {
              // 用户手动改 bucket → 切到 custom 模式记住
              setPreset('custom')
              if (preset !== 'custom') {
                const cur = buildPresetRange(preset)
                setCustomSince(toDateInputValue(cur.since))
                setCustomUntil(toDateInputValue(cur.until))
              }
              // bucket 通过 range 自动算,这里只是触发 re-eval,实际值跟 span 走
              void v
            }}
          >
            <SelectTrigger className="h-8 w-24">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="hour">小时</SelectItem>
              <SelectItem value="day">天</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>

      {/* 4 个统计卡 — 范围内 */}
      <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="flex items-center gap-2 text-xs font-medium text-muted-foreground">
              <Activity className="h-3.5 w-3.5" />
              范围内请求
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-semibold tabular-nums">
              {formatNumber(stats.data?.rangeRequests ?? 0)}
            </div>
            <CardDescription>
              累计 {formatNumber(stats.data?.totalRequests ?? 0)} · 今日{' '}
              {formatNumber(stats.data?.todayRequests ?? 0)}
            </CardDescription>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="flex items-center gap-2 text-xs font-medium text-muted-foreground">
              <BarChart3 className="h-3.5 w-3.5" />
              范围内 Token
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-semibold tabular-nums">
              {formatNumber(stats.data?.rangeTokens ?? 0)}
            </div>
            <CardDescription>
              输出 {formatNumber(stats.data?.rangeOutputTokens ?? 0)}
            </CardDescription>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="flex items-center gap-2 text-xs font-medium text-muted-foreground">
              <DollarSign className="h-3.5 w-3.5" />
              范围内花费
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-semibold tabular-nums">
              {formatUsd(stats.data?.rangeCostUsd ?? 0, 6)}
            </div>
            <CardDescription>
              累计 {formatUsd(stats.data?.totalCostUsd ?? 0, 6)} · 今日{' '}
              {formatUsd(stats.data?.todayCostUsd ?? 0, 6)}
            </CardDescription>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="flex items-center gap-2 text-xs font-medium text-muted-foreground">
              <CheckCircle2 className="h-3.5 w-3.5" />
              成功率(全局)
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-semibold tabular-nums">
              {Number.isNaN(successRate) ? '—' : formatPercent(successRate, 1)}
            </div>
            <CardDescription>
              成功 {formatNumber(summary.data?.successRequests ?? 0)} · 失败{' '}
              {formatNumber(summary.data?.errorRequests ?? 0)}
            </CardDescription>
          </CardContent>
        </Card>
      </div>

      {/* 时间序列折线图 */}
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="text-base">趋势</CardTitle>
          <CardDescription>
            请求次数与花费随时间变化({stats.data?.bucket === 'hour' ? '按小时' : '按天'} ·{' '}
            {timelineData.length} 个数据点)
          </CardDescription>
        </CardHeader>
        <CardContent>
          {timelineData.length === 0 ? (
            <div className="py-12 text-center text-sm text-muted-foreground">
              范围内暂无数据
            </div>
          ) : (
            <ResponsiveContainer width="100%" height={280}>
              <LineChart data={timelineData} margin={{ top: 8, right: 24, left: 8, bottom: 8 }}>
                <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--border))" />
                <XAxis dataKey="label" tick={{ fontSize: 11 }} stroke="hsl(var(--muted-foreground))" />
                <YAxis
                  yAxisId="left"
                  tick={{ fontSize: 11 }}
                  stroke={COLORS[0]}
                  label={{ value: '请求', angle: -90, position: 'insideLeft', fontSize: 11 }}
                />
                <YAxis
                  yAxisId="right"
                  orientation="right"
                  tick={{ fontSize: 11 }}
                  stroke={COLORS[2]}
                  tickFormatter={(v) => `$${Number(v).toFixed(2)}`}
                  label={{ value: '$', angle: 90, position: 'insideRight', fontSize: 11 }}
                />
                <ReTooltip
                  contentStyle={{
                    background: 'hsl(var(--popover))',
                    border: '1px solid hsl(var(--border))',
                    borderRadius: 6,
                    fontSize: 12,
                  }}
                  formatter={(value: number, name: string) => {
                    if (name === '花费') return [`$${Number(value).toFixed(6)}`, name]
                    return [formatNumber(Number(value)), name]
                  }}
                />
                <Legend wrapperStyle={{ fontSize: 12 }} />
                <Line
                  yAxisId="left"
                  type="monotone"
                  dataKey="requests"
                  name="请求"
                  stroke={COLORS[0]}
                  strokeWidth={2}
                  dot={false}
                />
                <Line
                  yAxisId="right"
                  type="monotone"
                  dataKey="cost"
                  name="花费"
                  stroke={COLORS[2]}
                  strokeWidth={2}
                  dot={false}
                />
              </LineChart>
            </ResponsiveContainer>
          )}
        </CardContent>
      </Card>

      {/* 模型 / 账号 维度柱状图 */}
      <div className="grid gap-4 lg:grid-cols-2">
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-base">模型维度</CardTitle>
            <CardDescription>范围内按模型统计 Top 8 · 美元成本降序</CardDescription>
          </CardHeader>
          <CardContent>
            {byModelData.length === 0 ? (
              <div className="py-10 text-center text-sm text-muted-foreground">
                暂无数据
              </div>
            ) : (
              <ResponsiveContainer width="100%" height={Math.max(220, byModelData.length * 36)}>
                <BarChart
                  data={byModelData}
                  layout="vertical"
                  margin={{ top: 4, right: 24, left: 8, bottom: 4 }}
                >
                  <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--border))" horizontal={false} />
                  <XAxis
                    type="number"
                    tick={{ fontSize: 11 }}
                    stroke="hsl(var(--muted-foreground))"
                    tickFormatter={(v) => `$${Number(v).toFixed(2)}`}
                  />
                  <YAxis
                    type="category"
                    dataKey="model"
                    tick={{ fontSize: 11 }}
                    stroke="hsl(var(--muted-foreground))"
                    width={150}
                  />
                  <ReTooltip
                    contentStyle={{
                      background: 'hsl(var(--popover))',
                      border: '1px solid hsl(var(--border))',
                      borderRadius: 6,
                      fontSize: 12,
                    }}
                    formatter={(value: number, name: string, item) => {
                      if (name === '花费') {
                        return [
                          `$${Number(value).toFixed(6)}  (${formatNumber(
                            (item.payload as { requests: number }).requests,
                          )} 次 / ${formatNumber(
                            (item.payload as { tokens: number }).tokens,
                          )} t)`,
                          name,
                        ]
                      }
                      return [formatNumber(Number(value)), name]
                    }}
                  />
                  <Bar dataKey="cost" name="花费" radius={[0, 4, 4, 0]}>
                    {byModelData.map((_, i) => (
                      <Cell key={i} fill={COLORS[i % COLORS.length]} />
                    ))}
                  </Bar>
                </BarChart>
              </ResponsiveContainer>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-base">账号维度</CardTitle>
            <CardDescription>范围内按账号统计 Top 8 · 美元成本降序</CardDescription>
          </CardHeader>
          <CardContent>
            {byCredentialData.length === 0 ? (
              <div className="py-10 text-center text-sm text-muted-foreground">
                暂无数据
              </div>
            ) : (
              <ResponsiveContainer
                width="100%"
                height={Math.max(220, byCredentialData.length * 36)}
              >
                <BarChart
                  data={byCredentialData}
                  layout="vertical"
                  margin={{ top: 4, right: 24, left: 8, bottom: 4 }}
                >
                  <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--border))" horizontal={false} />
                  <XAxis
                    type="number"
                    tick={{ fontSize: 11 }}
                    stroke="hsl(var(--muted-foreground))"
                    tickFormatter={(v) => `$${Number(v).toFixed(2)}`}
                  />
                  <YAxis
                    type="category"
                    dataKey="label"
                    tick={{ fontSize: 11 }}
                    stroke="hsl(var(--muted-foreground))"
                    width={170}
                  />
                  <ReTooltip
                    contentStyle={{
                      background: 'hsl(var(--popover))',
                      border: '1px solid hsl(var(--border))',
                      borderRadius: 6,
                      fontSize: 12,
                    }}
                    formatter={(value: number, name: string, item) => {
                      if (name === '花费') {
                        return [
                          `$${Number(value).toFixed(6)}  (${formatNumber(
                            (item.payload as { requests: number }).requests,
                          )} 次)`,
                          name,
                        ]
                      }
                      return [formatNumber(Number(value)), name]
                    }}
                  />
                  <Bar dataKey="cost" name="花费" radius={[0, 4, 4, 0]}>
                    {byCredentialData.map((_, i) => (
                      <Cell key={i} fill={COLORS[i % COLORS.length]} />
                    ))}
                  </Bar>
                </BarChart>
              </ResponsiveContainer>
            )}
          </CardContent>
        </Card>
      </div>

      {/* 范围内汇总维度(简洁文本表格) */}
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="text-base">汇总维度</CardTitle>
          <CardDescription>
            {new Date(range.since).toLocaleString('zh-CN')} ~{' '}
            {new Date(range.until).toLocaleString('zh-CN')}
          </CardDescription>
        </CardHeader>
        <CardContent className="grid gap-4 md:grid-cols-2">
          <div className="space-y-2">
            <div className="text-xs text-muted-foreground">按模型(全部 {stats.data?.byModel.length ?? 0} 个)</div>
            {(stats.data?.byModel ?? []).length === 0 ? (
              <div className="text-xs text-muted-foreground">暂无</div>
            ) : (
              <ul className="space-y-1 text-xs">
                {stats.data?.byModel.slice(0, 10).map((m) => (
                  <li
                    key={m.model}
                    className="flex items-center justify-between gap-2 rounded-md border bg-muted/30 px-3 py-1.5"
                  >
                    <span className="truncate font-mono">{m.model}</span>
                    <span className="ml-auto text-muted-foreground">
                      {formatNumber(m.requests)} 次 · {formatNumber(m.tokens)} t
                    </span>
                    <span className="font-semibold tabular-nums">
                      {formatUsd(m.costUsd, 6)}
                    </span>
                  </li>
                ))}
              </ul>
            )}
          </div>

          <div className="space-y-2">
            <div className="text-xs text-muted-foreground">按账号(全部 {stats.data?.byCredential.length ?? 0} 个)</div>
            {(stats.data?.byCredential ?? []).length === 0 ? (
              <div className="text-xs text-muted-foreground">暂无</div>
            ) : (
              <ul className="space-y-1 text-xs">
                {stats.data?.byCredential.slice(0, 10).map((c) => (
                  <li
                    key={c.credentialId}
                    className="flex items-center justify-between gap-2 rounded-md border bg-muted/30 px-3 py-1.5"
                  >
                    <span className="truncate">
                      {credentialLabels.get(c.credentialId) ??
                        `凭据 #${c.credentialId}`}
                    </span>
                    <span className="ml-auto text-muted-foreground">
                      {formatNumber(c.requests)} 次
                    </span>
                    <span className="font-semibold tabular-nums">
                      {formatUsd(c.costUsd, 6)}
                    </span>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </CardContent>
      </Card>

      <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
        <Badge variant="outline">范围: {PRESET_LABELS[preset]}</Badge>
        <span>·</span>
        <span>
          {new Date(range.since).toLocaleString('zh-CN')} →{' '}
          {new Date(range.until).toLocaleString('zh-CN')}
        </span>
        <span>·</span>
        <span>bucket = {range.bucket}</span>
      </div>
    </div>
  )
}
