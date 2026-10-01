import { useMemo, useState } from 'react'
import { Link, getRouteApi } from '@tanstack/react-router'
import { RotateCw, Search } from 'lucide-react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { systemApi } from '@/api/endpoints/system'
import type { LoadBalancingMode } from '@/api/types'
import { Button } from '@/components/ui/button'
import { InputGroup, InputGroupAddon, InputGroupInput } from '@/components/ui/input-group'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { Label } from '@/components/ui/label'
import { Page, PageHeader } from '@/components/patterns/page-header'
import { ErrorState, LoadingRows } from '@/components/patterns/data-state'
import { Stat, StatGrid } from '@/components/patterns/stat'
import { LOAD_BALANCING_LABEL } from '@/domain/labels'
import { deepEqual } from '@/domain/config-diff'
import { fmtInt } from '@/lib/format'
import { cn } from '@/lib/utils'
import { qk } from '@/queries/keys'
import { useLoadBalancing, useRuntimeConfig } from '@/queries/shared'
import { DirtyBar } from './dirty-bar'
import { GroupCard } from './field-renderer'
import { JSON_EDITORS, JsonFieldEditor } from './json-editor'
import { SETTINGS_SECTIONS } from './registry'
import { useConfigDraft, type ConfigDraft } from './use-config-draft'

const route = getRouteApi('/settings/$section')

const EXTRA_SECTIONS = [
  { id: 'advanced', title: '高级 JSON', description: '缓存路径策略、上报整形、映射规则等结构化配置' },
  { id: 'runtime', title: '运行态（只读）', description: '辅助请求、Token 刷新准入与启动代理' },
]

function sectionDirtyCount(cfg: ConfigDraft, id: string): number {
  if (id === 'advanced') return JSON_EDITORS.filter((e) => !deepEqual(cfg.get(e.path), cfg.baseValue(e.path))).length
  const section = SETTINGS_SECTIONS.find((s) => s.id === id)
  if (!section) return 0
  return section.groups.flatMap((g) => g.fields).filter((f) => !deepEqual(cfg.get(f.path), cfg.baseValue(f.path))).length
}

export function SettingsPage() {
  const { section } = route.useParams()
  const cfg = useConfigDraft()
  const [filter, setFilter] = useState('')
  const active = filter ? null : section

  const visibleSections = useMemo(() => (filter ? SETTINGS_SECTIONS : SETTINGS_SECTIONS.filter((s) => s.id === active)), [filter, active])

  return (
    <Page>
      <PageHeader
        title="运行配置"
        description="保存后新请求立即生效。外部池路由策略在「外部池 → 路由策略」中编辑。"
        actions={
          cfg.remoteChanged ? (
            <Button variant="outline" size="sm" onClick={cfg.reload}>
              <RotateCw /> 放弃修改并加载最新
            </Button>
          ) : undefined
        }
      />
      <div className="grid gap-6 lg:grid-cols-[14rem_minmax(0,1fr)]">
        <aside className="space-y-3 lg:sticky lg:top-16 lg:self-start">
          <InputGroup>
            <InputGroupAddon>
              <Search />
            </InputGroupAddon>
            <InputGroupInput placeholder="搜索配置项" value={filter} onChange={(e) => setFilter(e.target.value)} />
          </InputGroup>
          <nav className="flex gap-1 overflow-x-auto lg:flex-col" aria-label="配置分类">
            {[{ id: 'balancing', title: '负载均衡', description: '' }, ...SETTINGS_SECTIONS, ...EXTRA_SECTIONS].map((s) => {
              const count = cfg.ready ? sectionDirtyCount(cfg, s.id) : 0
              return (
                <Link
                  key={s.id}
                  to="/settings/$section"
                  params={{ section: s.id }}
                  onClick={() => setFilter('')}
                  className={cn(
                    'flex shrink-0 items-center justify-between gap-2 rounded-md px-2.5 py-1.5 text-sm transition-colors',
                    active === s.id ? 'bg-accent font-medium text-accent-foreground' : 'text-muted-foreground hover:bg-muted hover:text-foreground',
                  )}
                >
                  {s.title}
                  {count > 0 && <span className="num rounded-full bg-primary/15 px-1.5 text-xs text-primary">{count}</span>}
                </Link>
              )
            })}
          </nav>
        </aside>

        <div className="min-w-0 space-y-4">
          {active === 'balancing' && <BalancingSection />}
          {!cfg.ready ? (
            active !== 'balancing' && (cfg.query.error ? <ErrorState error={cfg.query.error} onRetry={() => cfg.query.refetch()} /> : <LoadingRows rows={8} />)
          ) : (
            <>
              {visibleSections.map((s) => (
                <div key={s.id} className="space-y-4">
                  {(filter || active === s.id) && (
                    <div>
                      <h2 className="text-base font-semibold">{s.title}</h2>
                      <p className="text-xs text-muted-foreground">{s.description}</p>
                    </div>
                  )}
                  {s.groups.map((g) => (
                    <GroupCard key={g.id} group={g} cfg={cfg} filter={filter} />
                  ))}
                </div>
              ))}
              {active === 'advanced' && (
                <div className="space-y-4">
                  <p className="text-xs text-muted-foreground">
                    这些结构字段较多，以 JSON 形式编辑。点击"应用到草稿"后，与其他修改一起通过底部保存条提交。
                  </p>
                  {JSON_EDITORS.map((e) => (
                    <JsonFieldEditor key={e.path} {...e} cfg={cfg} />
                  ))}
                </div>
              )}
              {active === 'runtime' && <RuntimeReadonly />}
            </>
          )}
          {cfg.ready && <DirtyBar cfg={cfg} />}
        </div>
      </div>
    </Page>
  )
}

function BalancingSection() {
  const lb = useLoadBalancing()
  const queryClient = useQueryClient()
  const set = useMutation({
    mutationFn: (mode: LoadBalancingMode) => systemApi.setLoadBalancing(mode),
    onSuccess: (r) => queryClient.setQueryData(qk.system.loadBalancing, r),
    meta: { success: '负载均衡模式已切换，立即生效', error: '切换失败' },
  })
  return (
    <section className="rounded-xl border bg-card">
      <header className="border-b px-4 py-3">
        <h3 className="text-sm font-semibold">负载均衡模式</h3>
        <p className="mt-0.5 text-xs text-muted-foreground">决定请求如何分配给本地账号；切换后立即生效，无需保存</p>
      </header>
      <RadioGroup value={lb.data?.mode} onValueChange={(v) => set.mutate(v as LoadBalancingMode)} className="grid gap-2 p-4 sm:grid-cols-2" disabled={set.isPending}>
        {(Object.keys(LOAD_BALANCING_LABEL) as LoadBalancingMode[]).map((mode) => (
          <Label
            key={mode}
            htmlFor={`lb-${mode}`}
            className={cn('flex cursor-pointer items-start gap-3 rounded-lg border p-3 font-normal', lb.data?.mode === mode && 'border-primary/60 bg-accent/40')}
          >
            <RadioGroupItem id={`lb-${mode}`} value={mode} className="mt-0.5" />
            <span>
              <span className="block text-sm font-medium">{LOAD_BALANCING_LABEL[mode].label}</span>
              <span className="block text-xs text-muted-foreground">{LOAD_BALANCING_LABEL[mode].desc}</span>
            </span>
          </Label>
        ))}
      </RadioGroup>
    </section>
  )
}

function RuntimeReadonly() {
  const c = useRuntimeConfig().data
  if (!c) return null
  const a = c.auxiliaryUpstreamRuntime
  const t = c.tokenRefreshAdmissionRuntime
  return (
    <div className="space-y-4">
      <section className="rounded-xl border bg-card p-4">
        <h3 className="mb-3 text-sm font-semibold">辅助上游请求</h3>
        <StatGrid cols={4}>
          <Stat label="在途 / 上限">
            {a.inFlight} / {a.configuredLimit}
          </Stat>
          <Stat label="峰值">{a.peakInFlight}</Stat>
          <Stat label="拒绝">{fmtInt(a.rejected)}</Stat>
          <Stat label="刷新客户端缓存">
            {a.refreshClientCacheEntries} / {a.refreshClientCacheMaxEntries}
          </Stat>
          <Stat label="缓存命中 / 未命中">
            {fmtInt(a.refreshClientHits)} / {fmtInt(a.refreshClientMisses)}
          </Stat>
        </StatGrid>
      </section>
      <section className="rounded-xl border bg-card p-4">
        <h3 className="mb-3 text-sm font-semibold">Token 刷新准入</h3>
        <StatGrid cols={4}>
          <Stat label="协调模式">{t.authority === 'redis_global' ? 'Redis 全局' : t.authority === 'process_local' ? '单进程' : 'Redis 降级'}</Stat>
          <Stat label="配置 RPM / 突发">
            {t.configuredRpm} / {t.configuredBurst}
          </Stat>
          <Stat label="已放行">{fmtInt(t.admitted)}</Stat>
          <Stat label="被限流">{fmtInt(t.rateLimited)}</Stat>
          <Stat label="协调拒绝">{fmtInt(t.coordinationRejected)}</Stat>
          <Stat label="Redis 错误">{fmtInt(t.redisErrors)}</Stat>
        </StatGrid>
      </section>
      <section className="rounded-xl border bg-card p-4">
        <h3 className="mb-1 text-sm font-semibold">启动期全局代理</h3>
        <p className="mb-3 text-xs text-muted-foreground">在配置文件中设置，重启后生效</p>
        <StatGrid>
          <Stat label="代理地址" mono>
            {c.proxyUrl || '未配置'}
          </Stat>
          <Stat label="用户名">{c.proxyUsername || '—'}</Stat>
        </StatGrid>
      </section>
    </div>
  )
}
