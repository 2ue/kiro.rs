import { Link } from 'react-router-dom'
import { Badge, Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui'
import { routeEditorPath, sortRoutes } from '@/features/routes/route-utils'
import type { InjectionCatalogEntry, PromptInjectionConfig } from '@/types/api'
import type { RouteConfig } from '@/types/routes'
import { routeInjectionMatrix } from './injection-rules'

const STATE_LABEL = { on: '开', off: '关', blocked: '不生效' } as const
const STATE_TONE = { on: 'success', off: 'neutral', blocked: 'warning' } as const

/**
 * 各路由的注入项生效情况（与 main 提示词注入页的“路由矩阵”一致）。点路由名进入该路由编辑页的注入页签。
 * `config` 传本页正在编辑的默认值与文本，矩阵随未保存的改动一起变化。
 */
export function InjectionRouteMatrix({
  routes,
  catalog,
  config,
  strict,
}: {
  routes: RouteConfig[]
  catalog: InjectionCatalogEntry[]
  config: Pick<PromptInjectionConfig, 'defaults' | 'texts'>
  strict: boolean
}) {
  const rows = routeInjectionMatrix(sortRoutes(routes), catalog, config, strict)
  return (
    <div className="overflow-hidden rounded-xl border bg-card">
      <Table className="min-w-[36rem] text-xs">
        <caption className="sr-only">各路由的注入项生效情况</caption>
        <TableHeader>
          <TableRow>
            <TableHead className="sticky left-0 bg-muted/95">路由</TableHead>
            {catalog.map((entry) => (
              <TableHead key={entry.id} title={entry.name}>
                {entry.code} {entry.name}
              </TableHead>
            ))}
          </TableRow>
        </TableHeader>
        <TableBody>
          {rows.map((row) => (
            <TableRow key={row.id}>
              <TableHead scope="row" className="sticky left-0 h-auto bg-card py-2 font-normal">
                <div className="flex flex-wrap items-center gap-1.5">
                  <Link
                    className="font-mono font-medium text-primary underline-offset-2 hover:underline"
                    to={routeEditorPath(row.id, 'injection')}
                    title={`编辑 ${row.prefix} 的提示词注入`}
                  >
                    {row.prefix}
                  </Link>
                  {!row.enabled && <Badge size="xs" tone="warning">已停用</Badge>}
                </div>
                {row.name && row.name !== row.prefix && (
                  <div className="mt-0.5 max-w-[14rem] truncate text-[0.7rem] text-muted-foreground" title={row.name}>{row.name}</div>
                )}
              </TableHead>
              {row.cells.map((cell) => (
                <TableCell
                  key={cell.id}
                  className="py-2"
                  title={cell.reason ?? (cell.explicit ? '路由上显式设置' : '跟随默认')}
                >
                  <Badge size="xs" tone={STATE_TONE[cell.state]}>
                    {STATE_LABEL[cell.state]}
                    {cell.explicit ? '·' : ''}
                  </Badge>
                </TableCell>
              ))}
            </TableRow>
          ))}
        </TableBody>
      </Table>
      <p className="border-t p-2 text-[0.7rem] leading-5 text-muted-foreground">
        带“·”的是路由上显式设置，其余跟随默认；“不生效”表示开着但不会注入（strict 兼容模式拦截，或文本为空）。
        按本页当前的默认值与文本计算，包含未保存的改动。点路由名进入该路由的注入编辑。
      </p>
    </div>
  )
}
