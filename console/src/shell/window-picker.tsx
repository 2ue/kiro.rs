import { CalendarRange } from 'lucide-react'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { useUiPrefs, WINDOW_OPTIONS, type WindowKey } from '@/stores/ui-prefs'

/** 全局统计窗口；与后端 usage-dashboard 的 windowKey 一一对应 */
export function WindowPicker() {
  const windowKey = useUiPrefs((s) => s.windowKey)
  const setWindowKey = useUiPrefs((s) => s.setWindowKey)
  return (
    <Select value={windowKey} onValueChange={(v) => setWindowKey(v as WindowKey)}>
      <SelectTrigger size="sm" className="w-auto gap-1.5" aria-label="统计时间窗口">
        <CalendarRange className="size-3.5 text-muted-foreground" />
        <SelectValue />
      </SelectTrigger>
      <SelectContent align="end">
        {WINDOW_OPTIONS.map((o) => (
          <SelectItem key={o.key} value={o.key}>
            {o.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  )
}
