import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'
import { Loader2, RotateCcw, Save, Search } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import {
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from '@/components/ui/tabs'
import { useAppConfig, useUpdateAppConfig } from '@/hooks/use-app-config'
import {
  useLoadBalancingMode,
  useSetLoadBalancingMode,
} from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'

interface FieldDef {
  key: string
  label: string
  description: string
  type: 'string' | 'number' | 'boolean' | 'select'
  options?: { value: string; label: string }[]
  group: 'basic' | 'cache' | 'quota' | 'pricing'
}

const FIELDS: FieldDef[] = [
  {
    key: 'load_balancing_mode',
    label: '调度模式',
    description: 'priority 按优先级 / balanced 按使用量均衡',
    type: 'select',
    group: 'basic',
    options: [
      { value: 'priority', label: '优先级' },
      { value: 'balanced', label: '均衡负载' },
    ],
  },
  {
    key: 'compat_profile',
    label: '兼容 Profile',
    description: 'Anthropic 协议兼容档位',
    type: 'select',
    group: 'basic',
    options: [
      { value: 'claude-code', label: 'claude-code(Claude Code 客户端兼容)' },
      { value: 'anthropic-strict', label: 'anthropic-strict(严格)' },
      { value: 'debug', label: 'debug(暴露代理改写)' },
    ],
  },
  {
    key: 'extract_thinking',
    label: '解析 thinking',
    description: '非流式响应中是否解析 <thinking> 块为独立内容',
    type: 'boolean',
    group: 'basic',
  },
  {
    key: 'default_endpoint',
    label: '默认 endpoint',
    description: '凭据未指定 endpoint 时的回退',
    type: 'string',
    group: 'basic',
  },
  {
    key: 'expose_proxy_warnings',
    label: '暴露代理警告',
    description: '在响应头加 x-kiro-rs-warnings,便于排查改写动作',
    type: 'boolean',
    group: 'basic',
  },
  {
    key: 'prompt_cache_simulation_mode',
    label: '本地缓存模拟模式',
    description: '高仿真度的本地缓存表现,用于上游不返回 metadata 时',
    type: 'select',
    group: 'cache',
    options: [
      { value: 'disabled', label: '禁用(只用上游真实数据)' },
      { value: 'local-prompt-cache', label: '本地缓存推算' },
      { value: 'high-cache', label: '高缓存模拟' },
    ],
  },
  {
    key: 'prompt_cache_target_read_ratio',
    label: '目标缓存命中率',
    description: '0~1 之间,默认 0.98',
    type: 'number',
    group: 'cache',
  },
  {
    key: 'prompt_cache_token_scale',
    label: 'token 放大倍数',
    description: 'high-cache 模式下输入 token 放大,默认 1.6',
    type: 'number',
    group: 'cache',
  },
  {
    key: 'prompt_cache_max_simulated_input_tokens',
    label: '模拟输入上限',
    description: '触顶后做 soft-cap 抖动,默认 300000',
    type: 'number',
    group: 'cache',
  },
  {
    key: 'prompt_cache_cap_jitter_min_tokens',
    label: '触顶 soft-cap 最小扣减',
    description: 'high-cache 模拟到达上限时的最小回退量,默认 12000',
    type: 'number',
    group: 'cache',
  },
  {
    key: 'prompt_cache_cap_jitter_max_tokens',
    label: '触顶 soft-cap 最大扣减',
    description: 'high-cache 模拟到达上限时的最大回退量,默认 24000',
    type: 'number',
    group: 'cache',
  },
  {
    key: 'prompt_cache_scale_min_input_tokens',
    label: '放大启用门槛',
    description: '基础输入达到该 token 数才启用 token_scale,默认 20000',
    type: 'number',
    group: 'cache',
  },
  {
    key: 'high_cache_threshold',
    label: '高缓存阈值',
    description: '管理面板"高缓存请求"判定线,默认 10000',
    type: 'number',
    group: 'cache',
  },
  {
    key: 'session_binding_ttl_minutes',
    label: '会话绑定保留时长(分钟)',
    description: 'sticky session 在内存中的保留时间,默认 30',
    type: 'number',
    group: 'cache',
  },
  {
    key: 'quota_soft_fail_limit',
    label: '配额软超限阈值',
    description: '累计 N 次 402 才永久禁用,期间走冷却。建议 3',
    type: 'number',
    group: 'quota',
  },
  {
    key: 'quota_cooldown_minutes',
    label: '冷却时长(分钟)',
    description: '触发软超限后的暂停时长,期满自动恢复',
    type: 'number',
    group: 'quota',
  },
  {
    key: 'pricing_auto_sync_enabled',
    label: '启动自动同步',
    description: '启动时若 model_prices 为空则异步同步一次',
    type: 'boolean',
    group: 'pricing',
  },
  {
    key: 'pricing_source_url',
    label: '价格 JSON 来源',
    description: 'LiteLLM 兼容格式',
    type: 'string',
    group: 'pricing',
  },
  {
    key: 'balance_cache_ttl_seconds',
    label: '余额缓存 TTL(秒)',
    description: '凭据余额查询的缓存过期时长',
    type: 'number',
    group: 'pricing',
  },
]

export default function SettingsPage() {
  const config = useAppConfig()
  const update = useUpdateAppConfig()
  const lbMode = useLoadBalancingMode()
  const setLbMode = useSetLoadBalancingMode()

  const [draft, setDraft] = useState<Record<string, unknown>>({})
  const [search, setSearch] = useState('')

  useEffect(() => {
    if (!config.data) return
    const next: Record<string, unknown> = {}
    for (const item of config.data) {
      next[item.key] = item.value
    }
    setDraft(next)
  }, [config.data])

  const dirtyKeys = useMemo(() => {
    if (!config.data) return [] as string[]
    return config.data
      .filter((c) => JSON.stringify(c.value) !== JSON.stringify(draft[c.key]))
      .map((c) => c.key)
  }, [config.data, draft])

  const filtered = useMemo(() => {
    const lower = search.trim().toLowerCase()
    if (!lower) return FIELDS
    return FIELDS.filter(
      (f) =>
        f.key.toLowerCase().includes(lower) ||
        f.label.toLowerCase().includes(lower) ||
        f.description.toLowerCase().includes(lower),
    )
  }, [search])

  const handleSave = () => {
    if (dirtyKeys.length === 0) {
      toast.info('没有需要保存的更改')
      return
    }
    const payload: Record<string, unknown> = {}
    for (const key of dirtyKeys) {
      payload[key] = draft[key]
      // 类型清洗:数字字段
      const def = FIELDS.find((f) => f.key === key)
      if (def?.type === 'number' && typeof payload[key] === 'string') {
        payload[key] = Number(payload[key])
      }
    }
    update.mutate(payload, {
      onSuccess: (res) => toast.success(res.message),
      onError: (err) => toast.error(extractErrorMessage(err)),
    })
  }

  const handleReset = () => {
    if (!config.data) return
    const next: Record<string, unknown> = {}
    for (const item of config.data) next[item.key] = item.value
    setDraft(next)
    toast.info('已重置为最近一次保存的值')
  }

  const renderField = (def: FieldDef) => {
    const value = draft[def.key]
    const id = `cfg-${def.key}`
    return (
      <div
        key={def.key}
        className="grid gap-2 rounded-lg border bg-card p-3 sm:grid-cols-[1fr_240px]"
      >
        <div>
          <Label htmlFor={id} className="text-sm font-medium">
            {def.label}
          </Label>
          <p className="mt-1 text-xs text-muted-foreground">{def.description}</p>
          <code className="mt-1 inline-block rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">
            {def.key}
          </code>
        </div>
        <div className="flex items-center justify-end">
          {def.type === 'boolean' && (
            <Switch
              id={id}
              checked={Boolean(value)}
              onCheckedChange={(v) => setDraft({ ...draft, [def.key]: v })}
            />
          )}
          {def.type === 'string' && (
            <Input
              id={id}
              value={String(value ?? '')}
              onChange={(e) => setDraft({ ...draft, [def.key]: e.target.value })}
            />
          )}
          {def.type === 'number' && (
            <Input
              id={id}
              type="number"
              value={String(value ?? '')}
              onChange={(e) =>
                setDraft({
                  ...draft,
                  [def.key]:
                    e.target.value === '' ? '' : Number(e.target.value),
                })
              }
            />
          )}
          {def.type === 'select' && (
            <Select
              value={String(value ?? '')}
              onValueChange={(v) => setDraft({ ...draft, [def.key]: v })}
            >
              <SelectTrigger id={id}>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {def.options?.map((o) => (
                  <SelectItem key={o.value} value={o.value}>
                    {o.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
        </div>
      </div>
    )
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">设置</h1>
          <p className="text-sm text-muted-foreground">
            缓存、配额和调度配置保存后热生效。
            静态项(端口 / 数据库 / Admin Key)需重启。
          </p>
        </div>
        <div className="flex items-center gap-2">
          {dirtyKeys.length > 0 && (
            <Badge variant="warning">{dirtyKeys.length} 项未保存</Badge>
          )}
          <Button variant="outline" onClick={handleReset} disabled={dirtyKeys.length === 0}>
            <RotateCcw className="h-4 w-4" />
            重置
          </Button>
          <Button onClick={handleSave} disabled={dirtyKeys.length === 0 || update.isPending}>
            {update.isPending ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <Save className="h-4 w-4" />
            )}
            保存
          </Button>
        </div>
      </div>

      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="text-base">凭据调度</CardTitle>
        </CardHeader>
        <CardContent className="flex items-center gap-3">
          <span className="text-sm text-muted-foreground">当前模式:</span>
          {lbMode.isLoading ? (
            <Loader2 className="h-4 w-4 animate-spin" />
          ) : (
            <Badge variant="secondary">
              {lbMode.data?.mode === 'priority' ? '优先级' : '均衡负载'}
            </Badge>
          )}
          <Button
            variant="outline"
            size="sm"
            onClick={() =>
              setLbMode.mutate(
                lbMode.data?.mode === 'priority' ? 'balanced' : 'priority',
                {
                  onSuccess: () => toast.success('调度模式已切换'),
                  onError: (err) => toast.error(extractErrorMessage(err)),
                },
              )
            }
            disabled={setLbMode.isPending}
          >
            切换
          </Button>
        </CardContent>
      </Card>

      <div className="relative max-w-md">
        <Search className="absolute left-2.5 top-2.5 h-4 w-4 text-muted-foreground" />
        <Input
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="搜索配置项..."
          className="pl-8"
        />
      </div>

      <Tabs defaultValue="basic">
        <TabsList>
          <TabsTrigger value="basic">基础</TabsTrigger>
          <TabsTrigger value="cache">缓存模拟</TabsTrigger>
          <TabsTrigger value="quota">配额超限</TabsTrigger>
          <TabsTrigger value="pricing">计价同步</TabsTrigger>
        </TabsList>
        {(['basic', 'cache', 'quota', 'pricing'] as const).map((g) => (
          <TabsContent key={g} value={g} className="space-y-3">
            {filtered
              .filter((f) => f.group === g)
              .map(renderField)}
          </TabsContent>
        ))}
      </Tabs>
    </div>
  )
}
