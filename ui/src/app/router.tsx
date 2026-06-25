import { createBrowserRouter, Navigate, RouterProvider } from 'react-router-dom'
import { AppShell } from '@/layouts/app-shell'
import { AuthGate, useAuth } from './auth-gate'
import { CONSOLE_BASE_PATH } from '@/types/ui'

import { OverviewPage } from '@/features/overview/overview-page'
import { CredentialsPage } from '@/features/credentials/credentials-page'
import { ExternalPoolsPage } from '@/features/external-pools/external-pools-page'
import { ProxiesPage } from '@/features/proxies/proxies-page'
import { UsagePage } from '@/features/usage/usage-page'
import { CostPage } from '@/features/cost/cost-page'
import { AuditPage } from '@/features/audit/audit-page'
import { RuntimePage } from '@/features/runtime/runtime-page'
import { ModelsPage } from '@/features/models/models-page'
import { SecurityPage } from '@/features/security/security-page'

function ShellWithAuth() {
  const { logout } = useAuth()
  return <AppShell onLogout={logout} />
}

const router = createBrowserRouter(
  [
    {
      path: '/',
      element: (
        <AuthGate>
          <ShellWithAuth />
        </AuthGate>
      ),
      children: [
        { index: true, element: <Navigate to="overview" replace /> },
        // 总览
        { path: 'overview', element: <OverviewPage /> },
        // 资源域
        { path: 'credentials', element: <CredentialsPage /> },
        { path: 'external-pools', element: <ExternalPoolsPage /> },
        { path: 'proxies', element: <ProxiesPage /> },
        // 分析域
        { path: 'usage', element: <UsagePage /> },
        { path: 'cost', element: <CostPage /> },
        { path: 'audit', element: <AuditPage /> },
        // 设置域
        { path: 'runtime', element: <RuntimePage /> },
        { path: 'models', element: <ModelsPage /> },
        { path: 'security', element: <SecurityPage /> },

        { path: '*', element: <Navigate to="overview" replace /> },
      ],
    },
  ],
  { basename: CONSOLE_BASE_PATH }
)

export function AppRouter() {
  return <RouterProvider router={router} />
}
