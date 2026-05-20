import { lazy, Suspense } from 'react'
import { createBrowserRouter, Navigate, Outlet } from 'react-router-dom'
import { AppShell } from '@/components/layout/app-shell'
import { LoginPage } from '@/components/login/login-page'
import { useAuth } from '@/store/auth'

const DashboardPage = lazy(() => import('@/components/dashboard'))
const CredentialsPage = lazy(() => import('@/components/credentials'))
const UsagePage = lazy(() => import('@/components/usage'))
const PricingPage = lazy(() => import('@/components/pricing'))
const SettingsPage = lazy(() => import('@/components/settings'))

function ProtectedShell() {
  const isAuthed = useAuth((s) => s.isAuthed)
  if (!isAuthed) return <Navigate to="/login" replace />
  return (
    <AppShell>
      <Suspense fallback={<div className="p-6 text-muted-foreground">加载中...</div>}>
        <Outlet />
      </Suspense>
    </AppShell>
  )
}

export const router = createBrowserRouter(
  [
    { path: '/login', element: <LoginPage /> },
    {
      path: '/',
      element: <ProtectedShell />,
      children: [
        { index: true, element: <Navigate to="/dashboard" replace /> },
        { path: 'dashboard', element: <DashboardPage /> },
        { path: 'credentials', element: <CredentialsPage /> },
        { path: 'usage', element: <UsagePage /> },
        { path: 'pricing', element: <PricingPage /> },
        { path: 'settings', element: <SettingsPage /> },
      ],
    },
    { path: '*', element: <Navigate to="/" replace /> },
  ],
  {
    basename: '/console',
  },
)
