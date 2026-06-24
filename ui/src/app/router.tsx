import { createBrowserRouter, Navigate, RouterProvider } from 'react-router-dom'
import { AppShell } from '@/layouts/app-shell'
import { AuthGate, useAuth } from './auth-gate'
import { CONSOLE_BASE_PATH } from '@/types/ui'

import { DashboardPage } from '@/features/dashboard/dashboard-page'
import { CredentialsPage } from '@/features/credentials/credentials-page'
import { ValidationPage } from '@/features/validation/validation-page'
import { ProxiesPage } from '@/features/proxies/proxies-page'
import { ExternalPoolsPage } from '@/features/external-pools/external-pools-page'
import { UsagePage } from '@/features/usage/usage-page'
import { PricingPage } from '@/features/pricing/pricing-page'
import { AuditPage } from '@/features/audit/audit-page'
import { ConfigPage } from '@/features/config/config-page'

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
        { index: true, element: <Navigate to="dashboard" replace /> },
        { path: 'dashboard', element: <DashboardPage /> },
        { path: 'credentials', element: <CredentialsPage /> },
        { path: 'validation', element: <ValidationPage /> },
        { path: 'proxies', element: <ProxiesPage /> },
        { path: 'external-pools', element: <ExternalPoolsPage /> },
        { path: 'usage', element: <UsagePage /> },
        { path: 'pricing', element: <PricingPage /> },
        { path: 'audit', element: <AuditPage /> },
        { path: 'config', element: <ConfigPage /> },
        { path: '*', element: <Navigate to="dashboard" replace /> },
      ],
    },
  ],
  { basename: CONSOLE_BASE_PATH }
)

export function AppRouter() {
  return <RouterProvider router={router} />
}
