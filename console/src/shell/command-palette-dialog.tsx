import { useState } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { Hash, Monitor, Moon, Search, Settings2, Sun, User } from 'lucide-react'
import {
  Command,
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandSeparator,
  CommandShortcut,
} from '@/components/ui/command'
import { useUiPrefs } from '@/stores/ui-prefs'
import { NAV_ITEMS } from './nav'
import { ALL_FIELDS } from '@/features/settings/registry'

import type { PaletteAction } from './command-palette'

export default function CommandPaletteDialog({
  open,
  setOpen,
  actions,
}: {
  open: boolean
  setOpen: (open: boolean) => void
  actions: PaletteAction[]
}) {
  const [query, setQuery] = useState('')
  const navigate = useNavigate()
  const setTheme = useUiPrefs((s) => s.setTheme)
  const run = (fn: () => void) => {
    setOpen(false)
    setQuery('')
    fn()
  }
  const groups = [...new Set(actions.map((a) => a.group))]
  const trimmed = query.trim()
  const numericId = /^#?\d+$/.test(trimmed) ? trimmed.replace('#', '') : null

  return (
    <CommandDialog open={open} onOpenChange={setOpen} title="命令面板" description="搜索页面、账号或执行操作">
      <Command>
        <CommandInput placeholder="搜索页面、账号邮箱/ID、请求 ID 或操作…" value={query} onValueChange={setQuery} />
        <CommandList>
          <CommandEmpty>没有匹配的结果</CommandEmpty>
          {trimmed && (
            <CommandGroup heading="搜索">
              {numericId && (
                <CommandItem
                  value={`account-id-${numericId}`}
                  onSelect={() => run(() => navigate({ to: '/accounts', search: { id: Number(numericId) } }))}
                >
                  <Hash /> 打开账号 #{numericId}
                </CommandItem>
              )}
              <CommandItem
                value={`search-accounts ${trimmed}`}
                onSelect={() => run(() => navigate({ to: '/accounts', search: { q: trimmed } }))}
              >
                <User /> 在账号中搜索 “{trimmed}”
              </CommandItem>
              <CommandItem
                value={`search-requests ${trimmed}`}
                onSelect={() => run(() => navigate({ to: '/requests', search: { q: trimmed } }))}
              >
                <Search /> 在请求中搜索 “{trimmed}”
              </CommandItem>
            </CommandGroup>
          )}
          <CommandGroup heading="页面">
            {NAV_ITEMS.map((item) => (
              <CommandItem
                key={item.to}
                value={`${item.label} ${item.keywords ?? ''} ${item.description}`}
                onSelect={() => run(() => navigate({ to: item.to }))}
              >
                <item.icon />
                <span>{item.label}</span>
                <span className="ml-2 truncate text-xs text-muted-foreground">{item.description}</span>
              </CommandItem>
            ))}
          </CommandGroup>
          {groups.map((group) => (
            <CommandGroup key={group} heading={group}>
              {actions
                .filter((a) => a.group === group)
                .map((a) => (
                  <CommandItem key={a.id} value={`${a.label} ${a.keywords ?? ''}`} onSelect={() => run(a.run)}>
                    {a.icon}
                    {a.label}
                  </CommandItem>
                ))}
            </CommandGroup>
          ))}
          {trimmed.length >= 2 && (
            <CommandGroup heading="配置项">
              {ALL_FIELDS.map((f) => (
                <CommandItem
                  key={f.path}
                  value={`config ${f.label} ${f.desc ?? ''} ${f.path} ${f.group}`}
                  onSelect={() =>
                    run(() =>
                      f.section === 'pools'
                        ? navigate({ to: '/pools', search: { tab: 'policy', focus: f.path } })
                        : navigate({ to: '/settings/$section', params: { section: f.section }, search: { focus: f.path } }),
                    )
                  }
                >
                  <Settings2 />
                  <span>{f.label}</span>
                  <span className="ml-2 truncate text-xs text-muted-foreground">
                    {f.sectionTitle} · {f.group}
                  </span>
                </CommandItem>
              ))}
            </CommandGroup>
          )}
          <CommandSeparator />
          <CommandGroup heading="外观">
            <CommandItem value="theme light 亮色" onSelect={() => run(() => setTheme('light'))}>
              <Sun /> 亮色主题
            </CommandItem>
            <CommandItem value="theme dark 暗色" onSelect={() => run(() => setTheme('dark'))}>
              <Moon /> 暗色主题
            </CommandItem>
            <CommandItem value="theme system 跟随系统" onSelect={() => run(() => setTheme('system'))}>
              <Monitor /> 跟随系统
              <CommandShortcut>主题</CommandShortcut>
            </CommandItem>
          </CommandGroup>
        </CommandList>
      </Command>
    </CommandDialog>
  )
}
