import { useEffect, useState, type ReactNode } from 'react'
import { useMutation } from '@tanstack/react-query'
import { ScanSearch } from 'lucide-react'
import { toast } from 'sonner'
import { poolsApi } from '@/api/endpoints/pools'
import type { CreateExternalPoolRequest, ExternalPool } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'
import { Sheet, SheetContent, SheetDescription, SheetFooter, SheetHeader, SheetTitle } from '@/components/ui/sheet'
import { ListInput, NumberInput, SelectControl, SettingRow, ToggleControl } from '@/components/patterns/fields'
import { draftFromPool, draftToRequest, emptyPoolDraft, HEADER_PROFILE_HINT, MAPPING_MODE_HINT, type PoolDraft } from './pool-form'
import { usePoolActions } from './queries'

function Group({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="py-3">
      <h3 className="mb-1 text-xs font-medium tracking-wide text-muted-foreground uppercase">{title}</h3>
      <div className="divide-y">{children}</div>
    </section>
  )
}

/** 新建或编辑外部池；字段按"连接 / 调度 / 路由与模型 / 协议细节"分组 */
export function PoolFormSheet({ pool, open, onOpenChange }: { pool?: ExternalPool; open: boolean; onOpenChange: (v: boolean) => void }) {
  const actions = usePoolActions()
  const [d, setD] = useState<PoolDraft>(emptyPoolDraft)
  useEffect(() => {
    if (open) setD(pool ? draftFromPool(pool) : emptyPoolDraft())
  }, [open, pool])
  const set = <K extends keyof PoolDraft>(k: K, v: PoolDraft[K]) => setD((x) => ({ ...x, [k]: v }))

  const discover = useMutation({
    mutationFn: () =>
      pool && !d.apiKey.trim()
        ? poolsApi.discoverStored(pool.id, { baseUrl: d.baseUrl.trim() || null, authType: d.authType })
        : poolsApi.discover({ baseUrl: d.baseUrl.trim(), apiKey: d.apiKey.trim(), authType: d.authType }),
    onSuccess: (r) => {
      set('supportedModels', r.supportedModels)
      toast.success(`发现 ${r.count} 个模型`)
    },
    meta: { error: '发现模型失败' },
  })

  const canSave = d.name.trim() && d.baseUrl.trim() && (pool || d.apiKey.trim())
  const save = async () => {
    const req = draftToRequest(d)
    if (pool) await actions.update.mutateAsync({ id: pool.id, req })
    else await actions.create.mutateAsync({ ...req, apiKey: d.apiKey.trim() } as CreateExternalPoolRequest)
    onOpenChange(false)
  }

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent className="w-full gap-0 sm:max-w-2xl">
        <SheetHeader className="border-b">
          <SheetTitle>{pool ? `编辑外部池「${pool.name}」` : '新建外部池'}</SheetTitle>
          <SheetDescription>Anthropic 兼容的第三方上游，作为本地账号的兜底或按策略直连</SheetDescription>
        </SheetHeader>
        <div className="min-h-0 flex-1 divide-y overflow-y-auto px-4">
          <Group title="连接">
            <SettingRow label="名称">
              <Input value={d.name} onChange={(e) => set('name', e.target.value)} />
            </SettingRow>
            <SettingRow label="Base URL" description="例如 https://api.example.com" stacked>
              <Input value={d.baseUrl} onChange={(e) => set('baseUrl', e.target.value)} className="font-mono text-xs" />
            </SettingRow>
            <SettingRow label="API Key" description={pool ? `当前：${pool.maskedApiKey ?? '已设置'}；留空表示不修改` : undefined}>
              <Input type="password" value={d.apiKey} onChange={(e) => set('apiKey', e.target.value)} placeholder={pool ? '不修改' : ''} />
            </SettingRow>
            <SettingRow label="认证方式">
              <SelectControl
                value={d.authType}
                onChange={(v) => set('authType', v)}
                options={[
                  { value: 'bearer', label: 'Authorization: Bearer' },
                  { value: 'x_api_key', label: 'x-api-key' },
                ]}
              />
            </SettingRow>
            <SettingRow label="启用">
              <ToggleControl checked={d.enabled} onChange={(v) => set('enabled', v)} />
            </SettingRow>
          </Group>

          <Group title="调度">
            <SettingRow label="优先级" description="数字越小越优先">
              <NumberInput value={d.priority} min={0} onChange={(v) => set('priority', v ?? 100)} />
            </SettingRow>
            <SettingRow label="最大并发">
              <NumberInput value={d.maxConcurrentRequests} min={1} suffix="并发" onChange={(v) => set('maxConcurrentRequests', v ?? 10)} />
            </SettingRow>
            <SettingRow label="自动禁用">
              <SelectControl
                value={d.autoDisablePolicy}
                onChange={(v) => set('autoDisablePolicy', v)}
                options={[
                  { value: 'inherit', label: '继承全局' },
                  { value: 'enabled', label: '单独启用' },
                  { value: 'disabled', label: '关闭' },
                ]}
              />
            </SettingRow>
            <SettingRow label="首输出前错误换池">
              <SelectControl
                value={d.preOutputStreamRetryMode}
                onChange={(v) => set('preOutputStreamRetryMode', v)}
                options={[
                  { value: 'inherit', label: '继承全局' },
                  { value: 'enabled', label: '启用' },
                  { value: 'disabled', label: '禁用' },
                ]}
              />
            </SettingRow>
          </Group>

          <Group title="路由与模型">
            <SettingRow label="入口路由">
              <SelectControl
                value={d.routeMode}
                onChange={(v) => set('routeMode', v)}
                options={[
                  { value: 'allow_all', label: '全部入口允许' },
                  { value: 'allow_list', label: '只允许下列入口' },
                  { value: 'deny_list', label: '禁止下列入口' },
                ]}
              />
            </SettingRow>
            {d.routeMode !== 'allow_all' && (
              <SettingRow label="路由规则" description="每行一个路径前缀，例如 /cc/v1" stacked>
                <ListInput value={d.routeRules} onChange={(v) => set('routeRules', v)} rows={3} />
              </SettingRow>
            )}
            <SettingRow label="支持的模型" description="为空表示不限制；调度时只把这些模型的请求发给该池" stacked>
              <div className="space-y-2">
                <ListInput
                  value={d.supportedModels}
                  onChange={(v) => set('supportedModels', v)}
                  rows={3}
                  parse={(raw) => [
                    ...new Set(
                      raw
                        .split(/[\s,，;；]+/)
                        .map((v) => v.trim())
                        .filter(Boolean),
                    ),
                  ]}
                />
                <Button
                  size="xs"
                  variant="outline"
                  onClick={() => discover.mutate()}
                  disabled={discover.isPending || !d.baseUrl.trim() || (!pool && !d.apiKey.trim())}
                >
                  <ScanSearch /> 从上游发现
                </Button>
              </div>
            </SettingRow>
            <SettingRow label="模型映射" description={MAPPING_MODE_HINT[d.modelMappingMode]} stacked>
              <SelectControl
                value={d.modelMappingMode}
                onChange={(v) => set('modelMappingMode', v)}
                options={[
                  { value: 'passthrough', label: '直接使用请求模型' },
                  { value: 'passthrough_mapping', label: '请求模型优先映射' },
                  { value: 'direct_mapping', label: '映射后内部处理' },
                  { value: 'processed_mapping', label: '内部处理后映射' },
                ]}
              />
            </SettingRow>
            {d.modelMappingMode !== 'passthrough' && (
              <>
                <SettingRow label="映射规则" description="每行一条：源模型 -> 目标模型" stacked>
                  <Textarea
                    rows={4}
                    value={d.modelMappingText}
                    onChange={(e) => set('modelMappingText', e.target.value)}
                    className="font-mono text-xs"
                    placeholder="claude-sonnet-4.5 -> claude-sonnet-4-5"
                  />
                </SettingRow>
                <SettingRow label="必须命中映射" description="未命中任何规则时拒绝请求">
                  <ToggleControl checked={d.modelMappingRequireMatch} onChange={(v) => set('modelMappingRequireMatch', v)} />
                </SettingRow>
                <SettingRow
                  label="未命中时点号转横杠"
                  description="例如 claude-opus-4.8 → claude-opus-4-8"
                  disabled={d.modelMappingRequireMatch}
                >
                  <ToggleControl
                    checked={d.normalizeModelVersionDots}
                    disabled={d.modelMappingRequireMatch}
                    onChange={(v) => set('normalizeModelVersionDots', v)}
                  />
                </SettingRow>
              </>
            )}
          </Group>

          <Group title="协议细节">
            <SettingRow
              label="请求体处理"
              description={
                d.requestBodyMode === 'raw_passthrough'
                  ? '请求体不经过本系统的解析与修正，原样转发'
                  : '经过标准 Anthropic 处理链路（图片、请求体保护、thinking 兼容等）'
              }
            >
              <SelectControl
                value={d.requestBodyMode}
                onChange={(v) => set('requestBodyMode', v)}
                options={[
                  { value: 'normalized', label: '标准处理' },
                  { value: 'raw_passthrough', label: 'Raw 透传' },
                ]}
              />
            </SettingRow>
            {d.requestBodyMode === 'raw_passthrough' && (
              <SettingRow label="写回顶层 model" description="按模型映射规则改写 raw JSON 的顶层 model">
                <ToggleControl
                  checked={d.rawModelMode === 'rewrite_top_level'}
                  onChange={(v) => set('rawModelMode', v ? 'rewrite_top_level' : 'none')}
                />
              </SettingRow>
            )}
            <SettingRow label="下游 usage 口径">
              <SelectControl
                value={d.usageProjectionMode}
                onChange={(v) => set('usageProjectionMode', v)}
                options={[
                  { value: 'pass_through', label: '透传上游 usage' },
                  { value: 'current_path_policy', label: '按入口路径整理' },
                ]}
              />
            </SettingRow>
            <SettingRow label="SSE 转发">
              <SelectControl
                value={d.streamResponseMode}
                onChange={(v) => set('streamResponseMode', v)}
                options={[
                  { value: 'inherit', label: '继承全局默认' },
                  { value: 'event_passthrough', label: '事件级透传' },
                ]}
              />
            </SettingRow>
            <SettingRow label="请求头策略" description={HEADER_PROFILE_HINT[d.headerProfile]}>
              <SelectControl
                value={d.headerProfile}
                onChange={(v) => set('headerProfile', v)}
                options={[
                  { value: 'generic', label: '泛用转发' },
                  { value: 'anthropic_passthrough', label: 'Anthropic 白名单' },
                  { value: 'claude_code_mimic', label: 'Claude Code 指纹' },
                ]}
              />
            </SettingRow>
            {d.headerProfile === 'generic' && (
              <SettingRow label="追加 beta=true">
                <ToggleControl checked={d.appendBetaQuery} onChange={(v) => set('appendBetaQuery', v)} />
              </SettingRow>
            )}
            <SettingRow label="请求头覆盖" description="每行一个 name: value" stacked>
              <Textarea
                rows={3}
                value={d.headerOverridesText}
                onChange={(e) => set('headerOverridesText', e.target.value)}
                className="font-mono text-xs"
              />
            </SettingRow>
            <SettingRow label="保留请求路径" description="把入口路径拼接到 Base URL 后">
              <ToggleControl checked={d.preservePath} onChange={(v) => set('preservePath', v)} />
            </SettingRow>
            <SettingRow label="传输层">
              <SelectControl
                value={d.wireProfile}
                onChange={(v) => set('wireProfile', v)}
                options={[
                  { value: 'default', label: '默认（允许 HTTP/2）' },
                  { value: 'http1_title_case', label: 'HTTP/1 Title-Case' },
                ]}
              />
            </SettingRow>
            <SettingRow label="TLS">
              <SelectControl
                value={d.tlsProfile}
                onChange={(v) => set('tlsProfile', v)}
                options={[
                  { value: 'default', label: '默认' },
                  { value: 'native_tls', label: 'Native TLS' },
                ]}
              />
            </SettingRow>
            <SettingRow label="备注" stacked>
              <Textarea rows={2} value={d.notes} onChange={(e) => set('notes', e.target.value)} />
            </SettingRow>
          </Group>
        </div>
        <SheetFooter className="flex-row justify-end border-t">
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            取消
          </Button>
          <Button onClick={save} disabled={!canSave || actions.create.isPending || actions.update.isPending}>
            {pool ? '保存' : '创建'}
          </Button>
        </SheetFooter>
      </SheetContent>
    </Sheet>
  )
}
