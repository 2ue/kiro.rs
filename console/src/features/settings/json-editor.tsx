import { useEffect, useState } from 'react'
import { CircleAlert } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Textarea } from '@/components/ui/textarea'
import { deepEqual } from '@/domain/config-diff'
import type { ConfigDraft } from './use-config-draft'

/** 复杂结构（缓存路径策略、上报整形、模型映射规则等）的 JSON 编辑；修改进入同一份草稿 */
export function JsonFieldEditor({ path, title, description, cfg }: { path: string; title: string; description: string; cfg: ConfigDraft }) {
  const value = cfg.get(path)
  const pretty = JSON.stringify(value ?? null, null, 2)
  const [text, setText] = useState(pretty)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    setText(pretty)
    setError(null)
  }, [pretty])
  const dirty = !deepEqual(value, cfg.baseValue(path))

  const apply = () => {
    try {
      const parsed = JSON.parse(text)
      setError(null)
      cfg.set(path, parsed)
    } catch (e) {
      setError(`JSON 格式错误：${(e as Error).message}`)
    }
  }

  return (
    <section className="rounded-xl border bg-card">
      <header className="flex items-center justify-between gap-2 border-b px-4 py-3">
        <div>
          <h3 className="flex items-center gap-2 text-sm font-semibold">
            {title}
            {dirty && <span className="size-1.5 rounded-full bg-primary" aria-label="已修改" />}
          </h3>
          <p className="mt-0.5 text-xs text-muted-foreground">{description}</p>
        </div>
        <div className="flex gap-2">
          {dirty && (
            <Button size="xs" variant="ghost" onClick={() => cfg.reset(path)}>
              撤销
            </Button>
          )}
          <Button size="xs" variant="outline" onClick={apply} disabled={text === pretty}>
            应用到草稿
          </Button>
        </div>
      </header>
      <div className="p-3">
        <Textarea
          value={text}
          onChange={(e) => setText(e.target.value)}
          rows={Math.min(24, Math.max(6, pretty.split('\n').length))}
          spellCheck={false}
          className="font-mono text-xs"
          aria-label={`${title} JSON`}
        />
        {error && (
          <p className="mt-2 flex items-center gap-1.5 text-xs text-danger">
            <CircleAlert className="size-3.5" /> {error}
          </p>
        )}
      </div>
    </section>
  )
}

export const JSON_EDITORS: Array<{ path: string; title: string; description: string }> = [
  { path: 'cachePolicy', title: '缓存策略（按路径）', description: '策略模板（currentHighCache / kiroRsTool）与 pathOverrides 路径绑定' },
  { path: 'definedCacheRoutes', title: '自定义缓存路径', description: '/dfcache/{name} 形式的自定义入口' },
  { path: 'reportedUsage', title: '上报用量整形', description: '对外上报 usage 的默认策略与按路径覆盖' },
  { path: 'promptCacheCreationControl', title: '缓存写入展示频次', description: '控制何时在对外 usage 中显示缓存写入' },
  { path: 'modelMapping.rules', title: '模型映射规则', description: '{ enabled, source, target, kind: version_equivalent | alias | fallback, note }' },
  { path: 'weightedCapacity.tiers', title: '加权容量分档', description: '命中不超过当前输入 token 的最高分档：{ minTokens, units }' },
]
