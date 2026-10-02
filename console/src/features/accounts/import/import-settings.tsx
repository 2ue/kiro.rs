import { ChevronDown } from 'lucide-react'
import type { ProxyResource } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '@/components/ui/collapsible'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import { SelectControl } from '@/components/patterns/fields'
import { TagsInput } from '@/components/patterns/tags-input'
import { cn } from '@/lib/utils'
import { initialDefaults, type ImportDefaults, type ImportOptions, type ProxyAssignmentMode } from './defaults'

function Field({ label, hint, children, className }: { label: string; hint?: string; children: React.ReactNode; className?: string }) {
  return (
    <div className={cn('space-y-1.5', className)}>
      <Label className="text-xs">{label}</Label>
      {children}
      {hint && <p className="text-xs text-muted-foreground">{hint}</p>}
    </div>
  )
}

function SwitchRow({
  id,
  label,
  hint,
  checked,
  onChange,
}: {
  id: string
  label: string
  hint?: string
  checked: boolean
  onChange: (v: boolean) => void
}) {
  return (
    <div className="flex items-start justify-between gap-3 py-1">
      <div>
        <Label htmlFor={id} className="text-sm font-normal">
          {label}
        </Label>
        {hint && <p className="text-xs text-muted-foreground">{hint}</p>}
      </div>
      <Switch id={id} checked={checked} onCheckedChange={onChange} />
    </div>
  )
}

/**
 * 导入参数：字段与旧版"批量导入 / KAM 导入"一致——
 * 默认参数（只填充缺失字段）+ 自动发现模型 + 跳过 error 账号 + 验活方式。
 */
export function ImportSettings({
  defaults,
  setDefaults,
  options,
  setOptions,
  proxyResources,
  tagOptions,
  showSkipError,
  testModel,
}: {
  defaults: ImportDefaults
  setDefaults: (d: ImportDefaults) => void
  options: ImportOptions
  setOptions: (o: ImportOptions) => void
  proxyResources: ProxyResource[]
  tagOptions: string[]
  showSkipError: boolean
  testModel: string
}) {
  const d = defaults
  const set = <K extends keyof ImportDefaults>(k: K, v: ImportDefaults[K]) => setDefaults({ ...d, [k]: v })
  const setOpt = <K extends keyof ImportOptions>(k: K, v: ImportOptions[K]) => setOptions({ ...options, [k]: v })
  const setProxyMode = (mode: ProxyAssignmentMode) =>
    setDefaults({
      ...d,
      proxyMode: mode,
      proxyResourceId: mode === 'single' ? d.proxyResourceId : '',
      proxyResourceIds: mode === 'round_robin' ? d.proxyResourceIds : [],
      proxyUrl: mode === 'single' ? d.proxyUrl : '',
      proxyUsername: mode === 'single' ? d.proxyUsername : '',
      proxyPassword: mode === 'single' ? d.proxyPassword : '',
    })
  const proxyLocked = !!d.proxyResourceId

  return (
    <div className="space-y-4">
      <section className="space-y-3 rounded-lg border p-3">
        <div className="flex items-start justify-between gap-2">
          <div>
            <h3 className="text-sm font-medium">默认参数</h3>
            <p className="text-xs text-muted-foreground">只填充每条账号中缺失的字段，账号自身的值优先。</p>
          </div>
          <Button size="xs" variant="ghost" onClick={() => setDefaults(initialDefaults())}>
            清空
          </Button>
        </div>
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label="导入后状态" hint="账号自身 disabled 字段优先">
            <SelectControl
              value={d.disabled ? 'true' : 'false'}
              onChange={(v) => set('disabled', v === 'true')}
              options={[
                { value: 'false', label: '启用' },
                { value: 'true', label: '禁用' },
              ]}
            />
          </Field>
          <div className="flex items-end">
            <SwitchRow
              id="imp-overage"
              label="导入后尝试开启超额"
              checked={d.enableOverageAfterImport}
              onChange={(v) => set('enableOverageAfterImport', v)}
            />
          </div>
          <Field label="账号标签" hint="文件中已有标签优先；没有标签的账号使用这里的默认标签" className="sm:col-span-2">
            <TagsInput value={d.tags} options={tagOptions} onChange={(v) => set('tags', v)} />
          </Field>
          <Field label="默认优先级">
            <Input inputMode="numeric" value={d.priority} onChange={(e) => set('priority', e.target.value)} placeholder="0" />
          </Field>
          <Field label="默认账号并发" hint="留空继承全局，0 不限">
            <Input inputMode="numeric" value={d.maxConcurrentRequests} onChange={(e) => set('maxConcurrentRequests', e.target.value)} />
          </Field>
          <Field label="默认账号 RPM" hint="留空继承全局，0 不限">
            <Input inputMode="numeric" value={d.rpm} onChange={(e) => set('rpm', e.target.value)} />
          </Field>
          <Field label="代理分配方式" hint="轮换按导入顺序循环分配；账号自身代理配置优先">
            <SelectControl
              value={d.proxyMode}
              onChange={setProxyMode}
              options={[
                { value: 'single', label: '统一代理' },
                { value: 'round_robin', label: '多个代理轮换' },
                { value: 'none', label: '不设置代理' },
              ]}
            />
          </Field>
          {d.proxyMode === 'round_robin' && (
            <Field
              label="轮换代理资源"
              hint={`已选 ${d.proxyResourceIds.length} 个，按选择顺序循环分配给账号 1、2、3…`}
              className="sm:col-span-2"
            >
              {proxyResources.length ? (
                <div className="grid max-h-40 gap-1 overflow-y-auto rounded-md border p-2 sm:grid-cols-2">
                  {proxyResources.map((r) => {
                    const id = String(r.id)
                    const on = d.proxyResourceIds.includes(id)
                    return (
                      <label key={r.id} className="flex cursor-pointer items-center gap-2 rounded px-2 py-1 text-sm hover:bg-muted/60">
                        <Checkbox
                          checked={on}
                          onCheckedChange={(v) =>
                            set('proxyResourceIds', v ? [...d.proxyResourceIds, id] : d.proxyResourceIds.filter((x) => x !== id))
                          }
                        />
                        <span className="truncate">{r.name}</span>
                      </label>
                    )
                  })}
                </div>
              ) : (
                <p className="rounded-md border border-dashed p-2 text-xs text-muted-foreground">暂无可用代理资源</p>
              )}
            </Field>
          )}
          {d.proxyMode === 'single' && (
            <>
              <Field label="代理资源">
                <SelectControl
                  value={d.proxyResourceId || '__none__'}
                  onChange={(v) =>
                    setDefaults(
                      v === '__none__'
                        ? { ...d, proxyResourceId: '' }
                        : { ...d, proxyResourceId: v, proxyUrl: '', proxyUsername: '', proxyPassword: '' },
                    )
                  }
                  options={[{ value: '__none__', label: '不绑定' }, ...proxyResources.map((r) => ({ value: String(r.id), label: r.name }))]}
                />
              </Field>
              <Field label="直连代理 URL">
                <Input
                  value={d.proxyUrl}
                  disabled={proxyLocked}
                  onChange={(e) => set('proxyUrl', e.target.value)}
                  placeholder="socks5h://..."
                  className="font-mono text-xs"
                />
              </Field>
              <Field label="代理用户名">
                <Input
                  value={d.proxyUsername}
                  disabled={proxyLocked}
                  onChange={(e) => set('proxyUsername', e.target.value)}
                  placeholder="可选"
                />
              </Field>
              <Field label="代理密码">
                <Input
                  type="password"
                  value={d.proxyPassword}
                  disabled={proxyLocked}
                  onChange={(e) => set('proxyPassword', e.target.value)}
                  placeholder="可选"
                />
              </Field>
            </>
          )}
        </div>
        <Collapsible>
          <CollapsibleTrigger className="group flex items-center gap-1 text-xs font-medium text-muted-foreground hover:text-foreground">
            <ChevronDown className="size-3.5 transition-transform group-data-[state=open]:rotate-180" />
            Region / 端点 / Machine ID
          </CollapsibleTrigger>
          <CollapsibleContent className="mt-3 grid gap-3 sm:grid-cols-2">
            <Field label="Region 兼容">
              <Input
                value={d.region}
                onChange={(e) => setDefaults({ ...d, region: e.target.value, authRegion: d.authRegion || e.target.value })}
                placeholder="us-east-1"
                className="font-mono text-xs"
              />
            </Field>
            <Field label="Auth Region">
              <Input
                value={d.authRegion}
                onChange={(e) => set('authRegion', e.target.value)}
                placeholder="us-east-1"
                className="font-mono text-xs"
              />
            </Field>
            <Field label="API Region">
              <Input
                value={d.apiRegion}
                onChange={(e) => set('apiRegion', e.target.value)}
                placeholder="us-east-1"
                className="font-mono text-xs"
              />
            </Field>
            <Field label="端点">
              <Input value={d.endpoint} onChange={(e) => set('endpoint', e.target.value)} placeholder="ide / cli" />
            </Field>
            <Field label="Machine ID">
              <Input value={d.machineId} onChange={(e) => set('machineId', e.target.value)} />
            </Field>
          </CollapsibleContent>
        </Collapsible>
      </section>

      <section className="space-y-1 rounded-lg border p-3">
        <SwitchRow
          id="imp-discover"
          label="自动发现模型限制"
          hint="导入后探测账号支持的模型并写入白名单"
          checked={options.autoDiscoverSupportedModels}
          onChange={(v) => setOpt('autoDiscoverSupportedModels', v)}
        />
        {showSkipError && (
          <SwitchRow
            id="imp-skip-error"
            label="跳过 error 状态账号"
            hint="来源文件（如 KAM 导出）中标记为 error 的账号不导入"
            checked={options.skipErrorAccounts}
            onChange={(v) => setOpt('skipErrorAccounts', v)}
          />
        )}
      </section>

      <section className="space-y-2 rounded-lg border p-3">
        <h3 className="text-sm font-medium">验活方式</h3>
        <SwitchRow
          id="imp-skip-verify"
          label="跳过验活"
          hint="只导入，不检查账号是否可用"
          checked={options.skipVerify}
          onChange={(v) => setOpt('skipVerify', v)}
        />
        {!options.skipVerify && (
          <div className="space-y-2">
            <SelectControl
              value={options.verifyMode}
              onChange={(v) => setOpt('verifyMode', v)}
              options={[
                { value: 'subscription_only', label: '查询订阅/积分（不请求模型）' },
                { value: 'model_and_subscription', label: '只测试模型' },
              ]}
            />
            {options.verifyMode === 'model_and_subscription' && (
              <>
                <p className="text-xs text-muted-foreground">将使用 {testModel} 发送一次真实请求，会消耗少量额度。</p>
                <SwitchRow
                  id="imp-refresh-info"
                  label="同步查询订阅/积分"
                  checked={options.refreshInfoAfterModelTest}
                  onChange={(v) => setOpt('refreshInfoAfterModelTest', v)}
                />
              </>
            )}
            <p className="text-xs text-muted-foreground">验活失败的账号会被自动删除（回滚），可在列表中直接重试。</p>
          </div>
        )}
      </section>
    </div>
  )
}
