import { Ban, CirclePlay } from 'lucide-react'
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from '@/components/ui/sheet'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/patterns/copy-button'
import { ErrorState, LoadingRows } from '@/components/patterns/data-state'
import { CredentialStatusBadge } from '@/components/status/credential-status-badge'
import { authMethodLabel } from '@/domain/labels'
import { useAccountActions } from '../actions'
import { useAccount } from '../queries'
import { DiagnosticsTab } from './diagnostics-tab'
import { MaintenanceTab } from './maintenance-tab'
import { ModelsTab } from './models-tab'
import { OverviewTab } from './overview-tab'
import { SettingsTab } from './settings-tab'

const TABS = [
  { key: 'overview', label: '概览' },
  { key: 'settings', label: '设置' },
  { key: 'models', label: '模型' },
  { key: 'requests', label: '请求' },
  { key: 'maintain', label: '维护' },
] as const

export function AccountSheet({
  id,
  tab,
  onTabChange,
  onClose,
}: {
  id: number | undefined
  tab?: string
  onTabChange: (tab: string) => void
  onClose: () => void
}) {
  const account = useAccount(id)
  const actions = useAccountActions()
  const row = account.data
  const active = TABS.some((t) => t.key === tab) ? tab! : 'overview'

  return (
    <Sheet open={typeof id === 'number'} onOpenChange={(open) => !open && onClose()}>
      <SheetContent className="w-full gap-0 sm:max-w-2xl">
        <SheetHeader className="border-b pr-12">
          <div className="flex items-center gap-2">
            <SheetTitle className="truncate">{row?.label ?? `账号 #${id}`}</SheetTitle>
            {row && <CredentialStatusBadge view={row.status} />}
          </div>
          <SheetDescription className="flex items-center gap-1.5">
            <span className="num">#{id}</span>
            {row?.email && <CopyButton value={row.email} label="复制邮箱" />}
            {row && <span>· {authMethodLabel(row.authMethod)}</span>}
            {row?.tags?.map((t) => (
              <span key={t} className="rounded bg-muted px-1 text-xs">
                {t}
              </span>
            ))}
          </SheetDescription>
          {row && (
            <div className="flex gap-2 pt-2">
              {row.disabled ? (
                <Button size="sm" onClick={() => actions.toggleDisabled([row.id], false)}>
                  <CirclePlay /> 启用
                </Button>
              ) : (
                <Button size="sm" variant="outline" onClick={() => actions.toggleDisabled([row.id], true)}>
                  <Ban /> 禁用
                </Button>
              )}
              <Button size="sm" variant="outline" onClick={() => actions.refreshInfo.mutate([row.id])} disabled={actions.refreshInfo.isPending}>
                刷新额度
              </Button>
            </div>
          )}
        </SheetHeader>
        {account.isLoading || (!row && !account.error) ? (
          <div className="p-4">
            <LoadingRows />
          </div>
        ) : account.error && !row ? (
          <ErrorState error={account.error} onRetry={() => account.refetch()} />
        ) : row ? (
          <Tabs value={active} onValueChange={onTabChange} className="min-h-0 flex-1 gap-0">
            <TabsList variant="line" className="w-full justify-start border-b px-4">
              {TABS.map((t) => (
                <TabsTrigger key={t.key} value={t.key} className="flex-none">
                  {t.label}
                </TabsTrigger>
              ))}
            </TabsList>
            <div className="min-h-0 flex-1 overflow-y-auto p-4">
              <TabsContent value="overview">
                <OverviewTab row={row} />
              </TabsContent>
              <TabsContent value="settings" className="h-full">
                <SettingsTab row={row} />
              </TabsContent>
              <TabsContent value="models">
                <ModelsTab row={row} />
              </TabsContent>
              <TabsContent value="requests">
                <DiagnosticsTab id={row.id} />
              </TabsContent>
              <TabsContent value="maintain">
                <MaintenanceTab row={row} onDeleted={onClose} />
              </TabsContent>
            </div>
          </Tabs>
        ) : null}
      </SheetContent>
    </Sheet>
  )
}
