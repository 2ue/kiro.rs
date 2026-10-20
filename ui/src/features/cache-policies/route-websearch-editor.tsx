import { Search } from 'lucide-react'
import { Badge } from '@/components/ui'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'

/** Per-route model-driven WebSearch setting; `undefined` follows the runtime default. */
export function RouteWebSearchEditor({
  defaultEnabled,
  value,
  onChange,
}: {
  defaultEnabled: boolean
  value: boolean | undefined
  onChange: (next: boolean | undefined) => void
}) {
  const effective = value ?? defaultEnabled
  const selectValue = value === undefined ? 'inherit' : value ? 'on' : 'off'
  const id = 'route-websearch-mode'
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        <Search className="size-4 text-muted-foreground" aria-hidden />
        <div className="text-sm font-semibold">WebSearch</div>
        <Badge size="xs" tone={effective ? 'success' : 'neutral'}>{effective ? '模型驱动' : '模板回答'}</Badge>
      </div>
      <div className="min-w-0 space-y-1.5 rounded-md border bg-background p-3 md:max-w-md">
        <label className="text-sm font-medium" htmlFor={id}>模型驱动搜索</label>
        <Select value={selectValue} onValueChange={(raw) => onChange(raw === 'inherit' ? undefined : raw === 'on')}>
          <SelectTrigger id={id} size="sm"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="inherit">{`跟随默认（当前${defaultEnabled ? '开' : '关'}）`}</SelectItem>
            <SelectItem value="on">开启</SelectItem>
            <SelectItem value="off">关闭</SelectItem>
          </SelectContent>
        </Select>
        <p className="text-xs leading-5 text-muted-foreground">
          开启后请求带原生 web_search 工具时，由模型决定搜索词并根据结果写带引用的回答，每次搜索多调用 1 次模型，token 计入本次 usage。关闭时沿用模板回答。
        </p>
      </div>
    </div>
  )
}
