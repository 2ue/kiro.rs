// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  bucketDescription,
  formatMsTick,
  formatQualityLabel,
  formatQualityTick,
  formatRateTick,
  pointsInWindow,
  qualityChartRows,
  rowsCrossDays,
} from '../src/features/account-risk/quality-history.ts'

const point = (sampledAt: string, recentErrorRate = 0, latencyEwmaMs: number | null = 1000) => ({
  sampledAt,
  recentErrorRate,
  ttftEwmaMs: null,
  latencyEwmaMs,
  sampleCount: 1,
})

test('横轴是 HH:mm（不再是“分:秒”），跨天时带日期', () => {
  // 用本地时间构造，结果与运行机器的时区无关。
  const morning = new Date(2026, 10, 19, 8, 5, 42).getTime()
  assert.equal(formatQualityTick(morning, false), '08:05')
  assert.equal(formatQualityTick(morning, true), '11/19 08:05')
  assert.equal(formatQualityTick(String(morning), false), '08:05')
  assert.equal(formatQualityLabel(morning), '11/19 08:05')
  assert.equal(formatQualityTick('not-a-time', false), '')
})

test('点分布在不同本地日期时才算跨天', () => {
  const at = (day: number, hour: number) => ({ t: new Date(2026, 10, day, hour, 0).getTime() })
  assert.equal(rowsCrossDays([at(19, 1), at(19, 23)]), false)
  assert.equal(rowsCrossDays([at(18, 23), at(19, 0)]), true)
  assert.equal(rowsCrossDays([at(19, 1)]), false)
  assert.equal(rowsCrossDays([]), false)
})

test('只保留查询窗口内的点：老后端忽略 from/to 时兜底裁剪', () => {
  const points = [
    point('2026-11-18T23:59:00Z'),
    point('2026-11-19T00:00:00Z'),
    point('2026-11-19T06:00:00Z'),
    point('2026-11-19T12:00:00Z'),
    point('2026-11-19T12:01:00Z'),
  ]
  const kept = pointsInWindow(points, '2026-11-19T00:00:00Z', '2026-11-19T12:00:00Z').map((item) => item.sampledAt)
  assert.deepEqual(kept, ['2026-11-19T00:00:00Z', '2026-11-19T06:00:00Z', '2026-11-19T12:00:00Z'])
  // 窗口解析不了时不裁。
  assert.equal(pointsInWindow(points, '', 'x').length, points.length)
})

test('图表行用时间戳做横轴键，缺失的延迟断线（NaN）', () => {
  const rows = qualityChartRows([point('2026-11-19T00:00:00Z', 0.02, null), point('bad')])
  assert.equal(rows.length, 1)
  assert.equal(rows[0].t, Date.parse('2026-11-19T00:00:00Z'))
  assert.equal(rows[0].errorRate, 0.02)
  assert.ok(Number.isNaN(rows[0].latency))
  assert.ok(Number.isNaN(rows[0].ttft))
})

test('纵轴：失败率按百分比，延迟按 ms', () => {
  assert.equal(formatRateTick(0), '0%')
  assert.equal(formatRateTick(0.05), '5%')
  assert.equal(formatRateTick(0.125), '12.5%')
  assert.equal(formatRateTick('0.1'), '10%')
  assert.equal(formatRateTick('x'), '')
  assert.equal(formatMsTick(1234.4), '1,234 ms')
  assert.equal(formatMsTick(0), '0 ms')
})

test('降采样桶宽说明：一分钟一个点或缺字段时不说明', () => {
  assert.equal(bucketDescription(undefined), '')
  assert.equal(bucketDescription(60), '')
  assert.equal(bucketDescription(600), '每 10 分钟一个点（桶内平均）')
  assert.equal(bucketDescription(7200), '每 2 小时一个点（桶内平均）')
  assert.equal(bucketDescription(90), '每 90 秒一个点（桶内平均）')
})
