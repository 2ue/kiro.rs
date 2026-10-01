import { useState } from 'react'
import { Bookmark, BookmarkPlus, Trash2 } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { usePersistedState } from '@/lib/use-persisted-state'

interface SavedView<T> {
  name: string
  search: T
}

/** 常用筛选视图：保存当前 URL 筛选条件，存在本地浏览器 */
export function SavedViews<T extends Record<string, unknown>>({
  scope,
  current,
  onApply,
  isEmpty,
}: {
  scope: string
  current: T
  onApply: (search: T) => void
  /** 当前没有任何筛选时禁用保存 */
  isEmpty: boolean
}) {
  const [views, setViews] = usePersistedState<SavedView<T>[]>(`views.${scope}`, [])
  const [naming, setNaming] = useState(false)
  const [name, setName] = useState('')
  const save = () => {
    const n = name.trim()
    if (!n) return
    setViews((v) => [...v.filter((x) => x.name !== n), { name: n, search: current }])
    setNaming(false)
    setName('')
  }
  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="ghost" size="sm">
            <Bookmark /> 视图{views.length ? `（${views.length}）` : ''}
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="w-60">
          <DropdownMenuLabel>常用视图</DropdownMenuLabel>
          {views.length === 0 && <p className="px-2 py-1.5 text-xs text-muted-foreground">设置好筛选条件后保存为视图</p>}
          {views.map((v) => (
            <DropdownMenuItem key={v.name} onSelect={() => onApply(v.search)} className="justify-between">
              <span className="truncate">{v.name}</span>
              <Button
                variant="ghost"
                size="icon-xs"
                aria-label={`删除视图 ${v.name}`}
                onClick={(e) => {
                  e.stopPropagation()
                  e.preventDefault()
                  setViews((list) => list.filter((x) => x.name !== v.name))
                }}
              >
                <Trash2 />
              </Button>
            </DropdownMenuItem>
          ))}
          <DropdownMenuSeparator />
          <DropdownMenuItem disabled={isEmpty} onSelect={() => setNaming(true)}>
            <BookmarkPlus /> 保存当前筛选
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      <Dialog open={naming} onOpenChange={setNaming}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle>保存为视图</DialogTitle>
          </DialogHeader>
          <Input
            autoFocus
            value={name}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && save()}
            placeholder="例如：已禁用 · Token 失效"
            aria-label="视图名称"
          />
          <DialogFooter>
            <Button onClick={save} disabled={!name.trim()}>
              保存
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  )
}
