import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { Hash, Monitor, Moon, Search, Sun, User } from 'lucide-react'
import {
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

export interface PaletteAction {
  id: string
  label: string
  group: string
  icon?: ReactNode
  keywords?: string
  run: () => void
}

interface PaletteCtx {
  open: () => void
  register: (actions: PaletteAction[]) => () => void
}

const Ctx = createContext<PaletteCtx | null>(null)

export function CommandPaletteProvider({ children }: { children: ReactNode }) {
  const [isOpen, setOpen] = useState(false)
  const [query, setQuery] = useState('')
  const [registered, setRegistered] = useState<Map<string, PaletteAction[]>>(new Map())
  const navigate = useNavigate()
  const setTheme = useUiPrefs((s) => s.setTheme)

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault()
        setOpen((v) => !v)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  const register = useCallback((actions: PaletteAction[]) => {
    const key = Math.random().toString(36).slice(2)
    setRegistered((prev) => new Map(prev).set(key, actions))
    return () =>
      setRegistered((prev) => {
        const next = new Map(prev)
        next.delete(key)
        return next
      })
  }, [])

  const ctx = useMemo(() => ({ open: () => setOpen(true), register }), [register])
  const run = (fn: () => void) => {
    setOpen(false)
    setQuery('')
    fn()
  }

  const actions = [...registered.values()].flat()
  const groups = [...new Set(actions.map((a) => a.group))]
  const trimmed = query.trim()
  const numericId = /^#?\d+$/.test(trimmed) ? trimmed.replace('#', '') : null

  return (
    <Ctx.Provider value={ctx}>
      {children}
      <CommandDialog open={isOpen} onOpenChange={setOpen} title="命令面板" description="搜索页面、账号或执行操作">
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
      </CommandDialog>
    </Ctx.Provider>
  )
}

export function useCommandPalette() {
  const ctx = useContext(Ctx)
  if (!ctx) throw new Error('useCommandPalette must be used within CommandPaletteProvider')
  return ctx
}

/** 页面注册上下文动作到命令面板；依赖变化时重新注册 */
export function usePaletteActions(actions: PaletteAction[], deps: unknown[]) {
  const { register } = useCommandPalette()
  useEffect(() => register(actions), deps) // eslint-disable-line react-hooks/exhaustive-deps
}
