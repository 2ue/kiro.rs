import { ShieldCheck } from 'lucide-react'
import { Badge } from '@/components/ui'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import type { IdentityPromptConfig, IdentityRouteOverride } from '@/types/api'

type OverrideKey = keyof IdentityRouteOverride

const FIELDS: Array<{ key: OverrideKey; label: string; desc: string }> = [
  { key: 'enabled', label: '身份隐藏', desc: '这个路由是否启用身份隐藏；关闭后下面各项都不生效。' },
  { key: 'promptEnabled', label: '注入身份声明', desc: '在 system 最前面加一段简短的身份声明。' },
  { key: 'turnReminderEnabled', label: '每轮身份提醒', desc: '在当前用户消息末尾追加一句提醒，只作用于本地 Kiro 链路。' },
  { key: 'outputGuardEnabled', label: '输出过滤', desc: '返回前删除“我是 Kiro / 我不是 Kiro / 指令冲突”这类句子。' },
  { key: 'outputGuardStreamEnabled', label: '流式也过滤', desc: '流式回复按整句检查后下发，首字会稍晚。' },
  { key: 'applyToExternalPool', label: '应用到外部池', desc: '请求走外部池时也注入身份提示词。' },
]

/** Effective value of one identity switch for a route, ignoring the global route rules. */
function defaultValue(identity: IdentityPromptConfig, key: OverrideKey): boolean {
  switch (key) {
    case 'enabled':
      return identity.enabled
    case 'promptEnabled':
      return identity.promptEnabled
    case 'turnReminderEnabled':
      return identity.turnReminder.enabled
    case 'outputGuardEnabled':
      return identity.outputGuard.enabled
    case 'outputGuardStreamEnabled':
      return identity.outputGuard.streamEnabled
    case 'applyToExternalPool':
      return identity.applyToExternalPool
  }
}

function routeRulesMatch(identity: IdentityPromptConfig, prefix: string): boolean {
  const matches = identity.routeRules.some((rule) => {
    const r = rule.trim().replace(/\/$/, '')
    return r === '*' || prefix === r || prefix.startsWith(r + '/')
  })
  if (identity.routeMode === 'allow_list') return matches
  if (identity.routeMode === 'deny_list') return !matches
  return true
}

/** Whether identity masking is effectively on for a route prefix. */
export function identityEnabledForRoute(identity: IdentityPromptConfig, prefix: string, override?: IdentityRouteOverride): boolean {
  if (override?.enabled !== undefined) return override.enabled
  return identity.enabled && routeRulesMatch(identity, prefix)
}

/** Short badges describing a route's identity settings, for the route card. */
export function identitySummary(identity: IdentityPromptConfig, prefix: string, override?: IdentityRouteOverride): string[] {
  if (!identityEnabledForRoute(identity, prefix, override)) return ['身份隐藏关']
  const value = (key: OverrideKey) => override?.[key] ?? defaultValue(identity, key)
  const items = ['身份隐藏开']
  if (!value('promptEnabled')) items.push('不注入声明')
  if (!value('turnReminderEnabled')) items.push('无每轮提醒')
  if (!value('outputGuardEnabled')) items.push('不过滤输出')
  else if (!value('outputGuardStreamEnabled')) items.push('流式不过滤')
  return items
}

export function RouteIdentityEditor({
  prefix,
  identity,
  value,
  onChange,
}: {
  prefix: string
  identity: IdentityPromptConfig
  value: IdentityRouteOverride
  onChange: (next: IdentityRouteOverride) => void
}) {
  const enabled = identityEnabledForRoute(identity, prefix, value)
  const setField = (key: OverrideKey, raw: string) => {
    const next = { ...value }
    if (raw === 'inherit') delete next[key]
    else next[key] = raw === 'on'
    onChange(next)
  }
  const inheritLabel = (key: OverrideKey) => {
    if (key === 'enabled') {
      return `跟随默认（当前${identity.enabled && routeRulesMatch(identity, prefix) ? '开' : '关'}）`
    }
    return `跟随默认（当前${defaultValue(identity, key) ? '开' : '关'}）`
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        <ShieldCheck className="size-4 text-muted-foreground" aria-hidden />
        <div className="text-sm font-semibold">身份与渠道隐藏</div>
        <Badge size="xs" tone={enabled ? 'success' : 'neutral'}>{enabled ? '此路由已启用' : '此路由未启用'}</Badge>
      </div>
      <p className="text-xs leading-5 text-muted-foreground">
        每一项都可以单独设置；选“跟随默认”时使用运行配置里的身份隐藏默认值。提示词内容统一在运行配置中维护。
      </p>
      <div className="grid gap-3 md:grid-cols-2">
        {FIELDS.map((field) => {
          const current = value[field.key]
          const selectValue = current === undefined ? 'inherit' : current ? 'on' : 'off'
          const disabled = field.key !== 'enabled' && !enabled
          const id = `route-identity-${field.key}`
          return (
            <div key={field.key} className="min-w-0 space-y-1.5 rounded-md border bg-background p-3">
              <label className="text-sm font-medium" htmlFor={id}>{field.label}</label>
              <Select value={selectValue} disabled={disabled} onValueChange={(raw) => setField(field.key, raw)}>
                <SelectTrigger id={id} size="sm"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="inherit">{inheritLabel(field.key)}</SelectItem>
                  <SelectItem value="on">开启</SelectItem>
                  <SelectItem value="off">关闭</SelectItem>
                </SelectContent>
              </Select>
              <p className="text-xs leading-5 text-muted-foreground">{field.desc}</p>
            </div>
          )
        })}
      </div>
    </div>
  )
}
