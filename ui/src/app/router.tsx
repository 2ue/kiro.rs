import { createBrowserRouter, Navigate, RouterProvider } from 'react-router-dom'
import { AppShell } from '@/layouts/app-shell'
import { AuthGate, useAuth } from './auth-gate'
import { CONSOLE_BASE_PATH } from '@/types/ui'
import { PlaceholderPage } from '@/features/_placeholder/placeholder-page'
import { OverviewPage } from '@/features/overview/overview-page'
import { CredentialsPage } from '@/features/credentials/credentials-page'

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
        { path: 'external-pools', element: <PlaceholderPage pageKey="external" /> },
        { path: 'proxies', element: <PlaceholderPage pageKey="proxies" /> },
        // 分析域
        { path: 'usage', element: <PlaceholderPage pageKey="usage" /> },
        { path: 'cost', element: <PlaceholderPage pageKey="cost" /> },
        { path: 'audit', element: <PlaceholderPage pageKey="audit" /> },
        // 设置域
        { path: 'runtime', element: <PlaceholderPage pageKey="runtime" /> },
        { path: 'models', element: <PlaceholderPage pageKey="models" /> },
        { path: 'security', element: <PlaceholderPage pageKey="security" /> },

        { path: '*', element: <Navigate to="overview" replace /> },
      ],
    },
  ],
  { basename: CONSOLE_BASE_PATH }
)

export function AppRouter() {
  return <RouterProvider router={router} />
}
