import { ChevronDown } from 'lucide-react'
import type { BatchCredentialImportDefaults, BatchCredentialImportResponse } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '@/components/ui/collapsible'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import { NumberInput, SelectControl } from '@/components/patterns/fields'
import { cn } from '@/lib/utils'
import { ErrorText } from '@/components/patterns/error-text'
import type { ProxyResource } from '@/api/types'

export type ProxyMode = 'none' | 'single' | 'roundrobin'

interface DefaultsProps {
  defaults: BatchCredentialImportDefaults
  setDefaults: React.Dispatch<React.SetStateAction<BatchCredentialImportDefaults>>
  tagsText: string
  setTagsText: (v: string) => void
  proxyMode: ProxyMode
  setProxyMode: (v: ProxyMode) => void
  proxyIds: number[]
  setProxyIds: React.Dispatch<React.SetStateAction<number[]>>
  duplicateMode: 'skip' | 'error'
  setDuplicateMode: (v: 'skip' | 'error') => void
  autoDiscover: boolean
  setAutoDiscover: (v: boolean) => void
  proxyOptions: ProxyResource[]
}

/** 第 3 步：导入默认参数；常用项平铺，其余折叠 */
export function ImportDefaults({
  defaults,
  setDefaults,
  tagsText,
  setTagsText,
  proxyMode,
  setProxyMode,
  proxyIds,
  setProxyIds,
  duplicateMode,
  setDuplicateMode,
  autoDiscover,
  setAutoDiscover,
  proxyOptions,
}: DefaultsProps) {
  return (
    <div className="space-y-4">
      <p className="text-xs text-muted-foreground">以下参数只应用到未单独设置该字段的账号。</p>
      <div className="grid gap-4 sm:grid-cols-2">
        <div className="space-y-1.5">
          <Label>优先级</Label>
          <NumberInput value={defaults.priority ?? 0} min={0} onChange={(v) => setDefaults((d) => ({ ...d, priority: v ?? 0 }))} />
        </div>
        <div className="space-y-1.5">
          <Label>最大并发</Label>
          <NumberInput
            value={defaults.maxConcurrentRequests ?? null}
            allowEmpty
            min={0}
            placeholder="继承全局"
            onChange={(v) => setDefaults((d) => ({ ...d, maxConcurrentRequests: v }))}
          />
        </div>
        <div className="space-y-1.5 sm:col-span-2">
          <Label htmlFor="import-tags">标签</Label>
          <Input
            id="import-tags"
            value={tagsText}
            onChange={(e) => setTagsText(e.target.value)}
            placeholder="逗号分隔，例如 team-a, 生产"
          />
        </div>
        <div className="space-y-1.5 sm:col-span-2">
          <Label>出站代理</Label>
          <SelectControl
            value={proxyMode}
            onChange={(v) => {
              setProxyMode(v)
              setProxyIds([])
            }}
            options={[
              { value: 'none', label: '不绑定（继承全局）' },
              { value: 'single', label: '全部绑定同一个代理资源' },
              { value: 'roundrobin', label: '按顺序轮流绑定多个代理资源' },
            ]}
          />
          {proxyMode !== 'none' && (
            <div className="flex flex-wrap gap-1.5 pt-1">
              {proxyOptions.length === 0 && <span className="text-xs text-muted-foreground">没有可用的代理资源</span>}
              {proxyOptions.map((p) => {
                const on = proxyIds.includes(p.id)
                return (
                  <Button
                    key={p.id}
                    size="xs"
                    variant={on ? 'default' : 'outline'}
                    onClick={() =>
                      setProxyIds((ids) => (proxyMode === 'single' ? [p.id] : on ? ids.filter((x) => x !== p.id) : [...ids, p.id]))
                    }
                  >
                    {p.name}
                  </Button>
                )
              })}
            </div>
          )}
        </div>
      </div>
      <Collapsible>
        <CollapsibleTrigger className="group flex items-center gap-1 text-xs font-medium text-muted-foreground hover:text-foreground">
          <ChevronDown className="size-3.5 transition-transform group-data-[state=open]:rotate-180" />
          高级
        </CollapsibleTrigger>
        <CollapsibleContent className="mt-3 grid gap-4 sm:grid-cols-2">
          <div className="space-y-1.5">
            <Label>每分钟请求上限</Label>
            <NumberInput
              value={defaults.rpm ?? null}
              allowEmpty
              min={0}
              placeholder="继承全局"
              onChange={(v) => setDefaults((d) => ({ ...d, rpm: v }))}
            />
          </div>
          <div className="space-y-1.5">
            <Label>预热请求数</Label>
            <NumberInput
              value={defaults.warmupRemaining ?? null}
              allowEmpty
              min={0}
              placeholder="使用全局"
              onChange={(v) => setDefaults((d) => ({ ...d, warmupRemaining: v ?? undefined }))}
            />
          </div>
          <div className="space-y-1.5">
            <Label>重复账号</Label>
            <SelectControl
              value={duplicateMode}
              onChange={setDuplicateMode}
              options={[
                { value: 'skip', label: '跳过已存在的账号' },
                { value: 'error', label: '视为错误' },
              ]}
            />
          </div>
          <div className="space-y-2">
            <ToggleLine
              label="导入后先保持禁用"
              checked={!!defaults.disabled}
              onChange={(v) => setDefaults((d) => ({ ...d, disabled: v }))}
            />
            <ToggleLine
              label="导入后开启超额"
              checked={!!defaults.enableOverageAfterImport}
              onChange={(v) => setDefaults((d) => ({ ...d, enableOverageAfterImport: v }))}
            />
            <ToggleLine label="自动发现支持的模型" checked={autoDiscover} onChange={setAutoDiscover} />
          </div>
        </CollapsibleContent>
      </Collapsible>
    </div>
  )
}

/** 第 4 步：导入结果汇总与失败明细 */
export function ImportResult({ result }: { result: BatchCredentialImportResponse }) {
  return (
    <div className="space-y-4">
      <div className="grid grid-cols-3 gap-3">
        <ResultStat label="成功" value={result.success} tone="text-success" />
        <ResultStat label="跳过" value={result.skipped} tone="text-muted-foreground" />
        <ResultStat label="失败" value={result.failed} tone="text-danger" />
      </div>
      {result.items.some((i) => !i.ok || i.warning) && (
        <div className="divide-y rounded-lg border text-xs">
          {result.items
            .filter((i) => !i.ok || i.warning)
            .map((i) => (
              <div key={i.index} className="flex gap-3 px-3 py-2">
                <span className="num w-8 text-muted-foreground">{i.index + 1}</span>
                <span className="min-w-0 flex-1 truncate">{i.email ?? (i.credentialId ? `#${i.credentialId}` : '')}</span>
                {i.error && !i.skipped ? (
                  <ErrorText error={i.error} className="max-w-[60%] text-right" />
                ) : (
                  <span className={i.ok ? 'text-warning' : 'text-muted-foreground'}>
                    {i.error ?? i.warning ?? (i.skipped ? '已跳过' : '')}
                  </span>
                )}
              </div>
            ))}
        </div>
      )}
    </div>
  )
}

function ToggleLine({ label, checked, onChange }: { label: string; checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <label className="flex items-center justify-between gap-2 text-sm">
      {label}
      <Switch checked={checked} onCheckedChange={onChange} />
    </label>
  )
}

function ResultStat({ label, value, tone }: { label: string; value: number; tone: string }) {
  return (
    <div className="rounded-lg border p-3 text-center">
      <div className={cn('num text-2xl font-semibold', tone)}>{value}</div>
      <div className="text-xs text-muted-foreground">{label}</div>
    </div>
  )
}
