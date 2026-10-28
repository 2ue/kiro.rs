import { useState } from 'react'
import { toast } from 'sonner'
import { Switch } from '@/components/ui'
import { PageContainer, SectionCard } from '@/components/patterns'
import type { CredentialAccountInfo, CredentialStatusItem } from '@/types/api'
import { CREDENTIAL_CARD_GRID, CredentialCard } from './credential-card'

/*
 * Development-only gallery of credential cards in every state the card can show. The data is made
 * up; ids start at 90001 so an action clicked here fails against the real backend instead of
 * touching a real account.
 */

const ago = (ms: number) => new Date(Date.now() - ms).toISOString()
const MIN = 60_000
const HOUR = 60 * MIN
const DAY = 24 * HOUR

/** Free and Students plans cannot run into overage; paid plans can, with no known cap. */
const NO_OVERAGE_PLANS = ['KIRO FREE', 'KIRO STUDENTS']

function quota(title: string, used: number, limit: number, extra: Partial<CredentialAccountInfo> = {}): CredentialAccountInfo {
  const capable = !NO_OVERAGE_PLANS.includes(title)
  return {
    subscriptionTitle: title,
    currentUsage: used,
    usageLimit: limit,
    remaining: Math.max(limit - used, 0),
    usagePercentage: limit > 0 ? (used / limit) * 100 : 0,
    creditLimit: limit,
    creditRemaining: Math.max(limit - used, 0),
    creditBase: limit,
    creditBonus: 0,
    overageStatus: 'DISABLED',
    overageCapability: capable ? 'OVERAGE_CAPABLE' : 'OVERAGE_INCAPABLE',
    overageCap: 0,
    overageRate: capable ? 0.04 : 0,
    currentOverages: 0,
    nextResetAt: Math.floor((Date.now() + 7 * DAY) / 1000),
    checkedAt: ago(10 * MIN),
    ...extra,
  }
}

let nextId = 90001

function card(overrides: Partial<CredentialStatusItem>): CredentialStatusItem {
  const id = nextId++
  return {
    id,
    createdAt: ago(30 * DAY),
    updatedAt: ago(2 * DAY),
    priority: 0,
    fast: 0,
    routeMode: 'allow_all',
    routeRules: [],
    disabled: false,
    failureCount: 0,
    isCurrent: false,
    expiresAt: null,
    authMethod: 'social',
    effectiveAuthRegion: 'us-east-1',
    effectiveApiRegion: 'us-east-1',
    hasProfileArn: false,
    email: `preview${id}@gmail.com`,
    subscriptionTitle: 'KIRO FREE',
    accountInfo: quota('KIRO FREE', 16.4, 50),
    successCount: 1280,
    lastUsedAt: ago(3 * MIN),
    hasProxy: false,
    effectiveProxySource: 'none',
    refreshFailureCount: 0,
    endpoint: 'ide',
    cooledDown: false,
    cooldownRemainingSecs: 0,
    rateLimited: false,
    rateLimitRemainingSecs: 0,
    inFlightRequests: 2,
    oldestInFlightAgeSecs: 14,
    newestInFlightIdleSecs: 1,
    maxConcurrentRequests: 30,
    inFlightLeaseMaxSecs: 600,
    warmupRemaining: 0,
    transientFailureStreak: 0,
    recentErrorRate: 0,
    supportedModels: [],
    inProbation: false,
    probationRemainingSecs: 0,
    schedulerSelectionCount: 1302,
    recentSchedulerSelectionCount60s: 12,
    schedulerSelectionPressure: 0.4,
    schedulerScore: 0.82,
    estimatedCostUsd: 3.42,
    originalCostUsd: 2.97,
    kiroMeteringUsage: 16.4,
    pricedRequests: 1280,
    unpricedRequests: 0,
    rpm: 100,
    rateLimitAutoDisableEnabled: true,
    ...overrides,
  }
}

interface Scenario {
  title: string
  credential: CredentialStatusItem
  loadingBalance?: boolean
}

interface ScenarioGroup {
  title: string
  scenarios: Scenario[]
}

const paid = (title: string, used: number, limit: number, extra: Partial<CredentialStatusItem> = {}): Partial<CredentialStatusItem> => ({
  subscriptionTitle: title,
  accountInfo: quota(title, used, limit),
  kiroMeteringUsage: used,
  estimatedCostUsd: used * 0.137,
  originalCostUsd: used * 0.126,
  ...extra,
})

const GROUPS: ScenarioGroup[] = [
  {
    title: '运行状态',
    scenarios: [
      { title: '正常 · 调度器当前账号', credential: card({ email: 'daily.driver@gmail.com', isCurrent: true }) },
      {
        title: 'Fast 2 · 负载偏高（并发 73% / RPM 74%）',
        credential: card({ email: 'fast.tier.two@gmail.com', fast: 2, authMethod: 'idc', ...paid('KIRO PRO', 310, 1000), inFlightRequests: 22, recentSchedulerSelectionCount60s: 74 }),
      },
      {
        title: '并发已满',
        credential: card({ email: 'saturated@gmail.com', ...paid('KIRO PRO+', 880, 2000), inFlightRequests: 30, oldestInFlightAgeSecs: 182, recentSchedulerSelectionCount60s: 64 }),
      },
      {
        title: '冷却中（按模型）',
        credential: card({
          email: 'cooling.down@gmail.com',
          ...paid('KIRO PRO', 380, 1000),
          cooledDown: true,
          cooldownRemainingSecs: 42,
          cooldownReason: 'ThrottlingException',
          cooldowns: [{ model: 'claude-sonnet-4.5', global: false, remainingSecs: 42, reason: 'ThrottlingException' }],
          inFlightRequests: 0,
        }),
      },
      {
        title: '冷却中（全部模型）',
        credential: card({
          email: 'cooling.all.models@gmail.com',
          ...paid('KIRO PRO', 520, 1000),
          cooledDown: true,
          cooldownRemainingSecs: 245,
          cooldownReason: 'stream_error upstream eventstream ended without any frames',
          cooldowns: [{ global: true, remainingSecs: 245, reason: 'stream_error upstream eventstream ended without any frames' }],
          inFlightRequests: 0,
          transientFailureStreak: 2,
          lastErrorKind: 'stream',
          lastErrorReason: 'upstream eventstream ended without any frames',
          lastErrorAtMs: Date.now() - 20_000,
        }),
      },
      {
        title: '限流中（RPM 打满）',
        credential: card({ email: 'rate.limited@gmail.com', ...paid('KIRO PRO', 380, 1000), rateLimited: true, rateLimitRemainingSecs: 9, recentSchedulerSelectionCount60s: 100 }),
      },
      {
        title: '连续错误 3 次',
        credential: card({
          email: 'flaky.network@gmail.com',
          transientFailureStreak: 3,
          failureCount: 5,
          recentErrorRate: 0.18,
          lastErrorKind: 'Timeout',
          lastErrorReason: 'upstream request timed out after 120s',
          lastErrorAtMs: Date.now() - 4 * MIN,
        }),
      },
      {
        title: '新账号 · 预热 + 观察期',
        credential: card({
          email: 'fresh.account@gmail.com',
          createdAt: ago(2 * HOUR),
          warmupRemaining: 2,
          inProbation: true,
          probationRemainingSecs: 300,
          successCount: 1,
          accountInfo: quota('KIRO FREE', 0, 50),
          kiroMeteringUsage: 0,
          estimatedCostUsd: 0,
          originalCostUsd: 0,
        }),
      },
      {
        title: '全部配置标记 · Fast 3 · 并发/RPM 不限',
        credential: card({
          email: 'fully.configured@gmail.com',
          fast: 3,
          priority: 5,
          routeMode: 'deny_list',
          routeRules: ['/cc', '/v1/messages'],
          hasProxy: true,
          effectiveProxySource: 'resource',
          proxyResourceName: 'hk-residential-1',
          supportedModels: ['claude-sonnet-4.5', 'claude-haiku-4.5', 'claude-opus-4.5'],
          rateLimitAutoDisableEnabled: false,
          hasProfileArn: true,
          maxConcurrentRequests: 0,
          rpm: 0,
        }),
      },
    ],
  },
  {
    title: '额度与超额',
    scenarios: [
      { title: '额度 0%', credential: card({ email: 'untouched@gmail.com', ...paid('KIRO PRO', 0, 1000), successCount: 0, lastUsedAt: null }) },
      { title: '额度 50%', credential: card({ email: 'half.way@gmail.com', ...paid('KIRO PRO', 500, 1000) }) },
      { title: '额度 85%（偏高）', credential: card({ email: 'getting.close@gmail.com', ...paid('KIRO PRO+', 1700, 2000) }) },
      { title: '额度 97%（快用完）', credential: card({ email: 'almost.out@gmail.com', ...paid('KIRO PRO+', 1940, 2000) }) },
      {
        title: 'Free 额度用尽（不支持超额）',
        credential: card({ email: 'used.it.all@gmail.com', disabled: true, disabledReason: 'QuotaExceeded', ...paid('KIRO FREE', 50, 50), inFlightRequests: 0, lastUsedAt: ago(26 * HOUR) }),
      },
      {
        title: 'Pro · 超额已开启，尚未用到',
        credential: card({ email: 'overage.ready@gmail.com', ...paid('KIRO PRO', 455, 1000), accountInfo: quota('KIRO PRO', 455, 1000, { overageStatus: 'ENABLED' }) }),
      },
      {
        title: 'Pro · 超额 +34.5%',
        credential: card({
          email: 'in.overage@gmail.com',
          ...paid('KIRO PRO', 1345, 1000),
          accountInfo: quota('KIRO PRO', 1000, 1000, { overageStatus: 'ENABLED', currentOverages: 345 }),
        }),
      },
      {
        title: 'Power · 超额 +250%',
        credential: card({
          email: 'heavy.user@gmail.com',
          authMethod: 'idc',
          ...paid('KIRO POWER', 35000, 10000),
          accountInfo: quota('KIRO POWER', 35000, 10000, { overageStatus: 'ENABLED' }),
        }),
      },
    ],
  },
  {
    title: '订阅与账号类型',
    scenarios: [
      { title: 'Free · Social · IDE', credential: card({ email: 'free.social@gmail.com' }) },
      { title: 'Students · External IdP', credential: card({ email: 'campus.student@university.edu', authMethod: 'external_idp', ...paid('KIRO STUDENTS', 210, 1000) }) },
      { title: 'Pro · IdC', credential: card({ email: 'pro.idc@company.com', authMethod: 'idc', ...paid('KIRO PRO', 260, 1000) }) },
      { title: 'Pro+ · Social', credential: card({ email: 'pro.plus@gmail.com', ...paid('KIRO PRO+', 640, 2000) }) },
      { title: 'Pro Max · IdC · CLI 端点', credential: card({ email: 'pro.max@company.com', authMethod: 'idc', endpoint: 'cli', ...paid('KIRO PRO MAX', 1820, 5000) }) },
      { title: 'Power · IdC', credential: card({ email: 'power.user@company.com', authMethod: 'idc', ...paid('KIRO POWER', 4100, 10000) }) },
      { title: '无法识别的订阅名', credential: card({ email: 'custom.plan@gmail.com', ...paid('ENTERPRISE CUSTOM PLAN', 120, 3000) }) },
      {
        title: 'API Key · 额度未查询 · 从未使用',
        credential: card({
          email: undefined,
          maskedApiKey: 'ksk_****8f3a',
          authMethod: 'api_key',
          endpoint: 'api_key',
          subscriptionTitle: undefined,
          accountInfo: undefined,
          successCount: 0,
          lastUsedAt: null,
          inFlightRequests: 0,
          recentSchedulerSelectionCount60s: 0,
          kiroMeteringUsage: 0,
          estimatedCostUsd: 0,
          originalCostUsd: 0,
        }),
      },
      { title: '额度查询中', credential: card({ email: 'checking.balance@gmail.com' }), loadingBalance: true },
    ],
  },
  {
    title: '异常账号',
    scenarios: [
      { title: '手动禁用', credential: card({ email: 'paused.by.admin@gmail.com', disabled: true, disabledReason: 'Manual', inFlightRequests: 0 }) },
      {
        title: '账号暂停（额度也已用完）',
        credential: card({
          email: 'suspended.upstream@gmail.com',
          disabled: true,
          disabledReason: 'TemporarilySuspended',
          ...paid('KIRO FREE', 50, 50),
          lastErrorKind: 'AccountSuspended',
          lastErrorReason: 'TEMPORARILY_SUSPENDED: account is temporarily suspended',
          lastErrorAtMs: Date.now() - 3 * HOUR,
          inFlightRequests: 0,
        }),
      },
      {
        title: '账号封禁',
        credential: card({ email: 'banned.account@gmail.com', disabled: true, disabledReason: 'AccountSuspended', ...paid('KIRO PRO', 720, 1000), inFlightRequests: 0, lastUsedAt: ago(5 * DAY) }),
      },
      { title: '账号锁定', credential: card({ email: 'locked.account@gmail.com', disabled: true, disabledReason: 'AccountLocked', inFlightRequests: 0 }) },
      {
        title: 'Token 失效',
        credential: card({
          email: 'expired.token@gmail.com',
          authMethod: 'idc',
          disabled: true,
          disabledReason: 'InvalidRefreshToken',
          refreshFailureCount: 3,
          lastErrorKind: 'RefreshFailed',
          lastErrorReason: 'invalid_grant: refresh token is invalid or expired',
          lastErrorAtMs: Date.now() - 2 * DAY,
          inFlightRequests: 0,
          lastUsedAt: ago(2 * DAY),
        }),
      },
      { title: '连续失败禁用', credential: card({ email: 'too.many.failures@gmail.com', disabled: true, disabledReason: 'TooManyFailures', failureCount: 12, inFlightRequests: 0 }) },
      { title: '刷新失败禁用', credential: card({ email: 'refresh.broken@gmail.com', disabled: true, disabledReason: 'TooManyRefreshFailures', refreshFailureCount: 8, inFlightRequests: 0 }) },
      { title: '配置无效', credential: card({ email: 'bad.config@gmail.com', disabled: true, disabledReason: 'InvalidConfig', inFlightRequests: 0 }) },
      { title: '未知禁用原因（原样显示）', credential: card({ email: 'odd.reason@gmail.com', disabled: true, disabledReason: 'UpstreamPolicyHold', inFlightRequests: 0 }) },
    ],
  },
  {
    title: '文字长度与大数字',
    scenarios: [
      { title: '超长邮箱（整体）', credential: card({ email: 'virendrayadav9170643364.backup.secondary@gmail.com', authMethod: 'idc' }) },
      { title: '超长用户名 + 短域名', credential: card({ email: 'this.is.an.exceptionally.long.username.for.layout.testing@qq.com' }) },
      { title: '超长域名', credential: card({ email: 'ops@mail.engineering.platform-team.internal-corporate-domain.example.com', authMethod: 'idc' }) },
      { title: '无分隔的超长用户名', credential: card({ email: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa@gmail.com' }) },
      { title: '没有邮箱也没有 Key', credential: card({ email: undefined, maskedApiKey: undefined, authMethod: 'social' }) },
      {
        title: '大数字：积分、费用、成功次数',
        credential: card({
          email: 'big.numbers@gmail.com',
          authMethod: 'idc',
          ...paid('KIRO POWER', 123456.789, 200000),
          estimatedCostUsd: 16913.58,
          originalCostUsd: 15555.55,
          successCount: 12_345_678,
          priority: 999,
          inFlightRequests: 128,
          maxConcurrentRequests: 512,
          recentSchedulerSelectionCount60s: 1200,
          rpm: 3000,
        }),
      },
      {
        title: '多条路由规则 + 代理资源已禁用',
        credential: card({
          email: 'many.routes@gmail.com',
          routeMode: 'allow_list',
          routeRules: ['/cc', '/v1/messages', '/anthropic', '/openai'],
          hasProxy: true,
          effectiveProxySource: 'resource_disabled',
          proxyResourceName: 'us-datacenter-east-very-long-proxy-name',
        }),
      },
    ],
  },
]

const SCENARIO_COUNT = GROUPS.reduce((sum, group) => sum + group.scenarios.length, 0)

export function CredentialCardPreview() {
  const [showEmailSuffix, setShowEmailSuffix] = useState(true)
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set())
  const [detailId, setDetailId] = useState<number | null>(null)

  const toggleSelect = (id: number) => setSelectedIds((current) => {
    const next = new Set(current)
    if (next.has(id)) next.delete(id)
    else next.add(id)
    return next
  })

  return (
    <PageContainer>
      <SectionCard
        title="账号卡片预览"
        description={`开发环境专用，${SCENARIO_COUNT} 种情况，数据为虚构；卡片上的操作会请求不存在的账号并报错，不会影响真实数据。`}
        actions={(
          <label className="flex items-center gap-2 text-xs text-muted-foreground">
            <Switch checked={showEmailSuffix} onCheckedChange={setShowEmailSuffix} aria-label="显示邮箱后缀" />
            显示邮箱后缀
          </label>
        )}>
        <div className="space-y-6">
          {GROUPS.map((group) => (
            <section key={group.title} aria-label={group.title}>
              <h2 className="mb-2 text-sm font-semibold text-foreground">{group.title}</h2>
              <div className={CREDENTIAL_CARD_GRID}>
                {group.scenarios.map(({ title, credential, loadingBalance }) => (
                  <figure key={credential.id} className="m-0 flex flex-col gap-1.5">
                    <figcaption className="px-1 text-xs font-medium text-muted-foreground">{title}</figcaption>
                    <div className="flex-1">
                      <CredentialCard
                        credential={credential}
                        selected={selectedIds.has(credential.id)}
                        onToggleSelect={() => toggleSelect(credential.id)}
                        onQueryBalance={() => toast.info('预览数据不会查询额度')}
                        onTest={() => toast.info('预览数据不会发起测试')}
                        loadingBalance={Boolean(loadingBalance)}
                        expanded={detailId === credential.id}
                        onToggleExpanded={() => setDetailId((current) => (current === credential.id ? null : credential.id))}
                        showEmailSuffix={showEmailSuffix}
                      />
                    </div>
                  </figure>
                ))}
              </div>
            </section>
          ))}
        </div>
      </SectionCard>
    </PageContainer>
  )
}
