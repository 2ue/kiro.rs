import * as React from 'react'
import { createBrowserRouter, Navigate, RouterProvider } from 'react-router-dom'
import { AppShell } from '@/layouts/app-shell'
import { AuthGate, useAuth } from './auth-gate'
import { CONSOLE_BASE_PATH } from '@/types/ui'
import { LoadingState } from '@/components/patterns'

// 路由级懒加载:每个页面单独成 chunk,首屏只加载当前页
const OverviewPage = React.lazy(() =>
  import('@/features/overview/overview-page').then((m) => ({ default: m.OverviewPage }))
)
const CredentialsPage = React.lazy(() =>
  import('@/features/credentials/credentials-page').then((m) => ({ default: m.CredentialsPage }))
)
const ExternalPoolsPage = React.lazy(() =>
  import('@/features/external-pools/external-pools-page').then((m) => ({ default: m.ExternalPoolsPage }))
)
const ProxiesPage = React.lazy(() =>
  import('@/features/proxies/proxies-page').then((m) => ({ default: m.ProxiesPage }))
)
const UsagePage = React.lazy(() =>
  import('@/features/usage/usage-page').then((m) => ({ default: m.UsagePage }))
)
const CostPage = React.lazy(() =>
  import('@/features/cost/cost-page').then((m) => ({ default: m.CostPage }))
)
const AuditPage = React.lazy(() =>
  import('@/features/audit/audit-page').then((m) => ({ default: m.AuditPage }))
)
const RuntimePage = React.lazy(() =>
  import('@/features/runtime/runtime-page').then((m) => ({ default: m.RuntimePage }))
)
const ModelsPage = React.lazy(() =>
  import('@/features/models/models-page').then((m) => ({ default: m.ModelsPage }))
)
const SecurityPage = React.lazy(() =>
  import('@/features/security/security-page').then((m) => ({ default: m.SecurityPage }))
)
const ValidationPage = React.lazy(() =>
  import('@/features/validation/validation-page').then((m) => ({ default: m.ValidationPage }))
)

function ShellWithAuth() {
  const { logout } = useAuth()
  return <AppShell onLogout={logout} />
}

/** 懒加载页面的加载兜底 */
function Lazy({ children }: { children: React.ReactNode }) {
  return (
    <React.Suspense fallback={<LoadingState text="加载页面..." />}>{children}</React.Suspense>
  )
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
        { path: 'overview', element: <Lazy><OverviewPage /></Lazy> },
        // 资源域
        { path: 'credentials', element: <Lazy><CredentialsPage /></Lazy> },
        { path: 'validation', element: <Lazy><ValidationPage /></Lazy> },
        { path: 'external-pools', element: <Lazy><ExternalPoolsPage /></Lazy> },
        { path: 'proxies', element: <Lazy><ProxiesPage /></Lazy> },
        // 分析域
        { path: 'usage', element: <Lazy><UsagePage /></Lazy> },
        { path: 'cost', element: <Lazy><CostPage /></Lazy> },
        { path: 'audit', element: <Lazy><AuditPage /></Lazy> },
        // 设置域
        { path: 'runtime', element: <Lazy><RuntimePage /></Lazy> },
        { path: 'models', element: <Lazy><ModelsPage /></Lazy> },
        { path: 'security', element: <Lazy><SecurityPage /></Lazy> },

        { path: '*', element: <Navigate to="overview" replace /> },
      ],
    },
  ],
  { basename: CONSOLE_BASE_PATH }
)

export function AppRouter() {
  return <RouterProvider router={router} />
}
