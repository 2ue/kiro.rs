import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { QueryClientProvider } from '@tanstack/react-query'
import { createRouter, RouterProvider } from '@tanstack/react-router'
import { Toaster } from '@/components/ui/sonner'
import { TooltipProvider } from '@/components/ui/tooltip'
import { ConfirmProvider } from '@/components/patterns/confirm'
import { queryClient } from '@/queries/client'
import { AuthProvider } from '@/shell/auth'
import { ThemeSync } from '@/shell/theme'
import { routeTree } from './routeTree.gen'
import './index.css'

const router = createRouter({
  routeTree,
  basepath: '/console',
  defaultPreload: 'intent',
  scrollRestoration: true,
})

declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router
  }
}

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <AuthProvider>
        <TooltipProvider delayDuration={300}>
          <ConfirmProvider>
            <ThemeSync />
            <RouterProvider router={router} />
            <Toaster position="bottom-right" richColors closeButton />
          </ConfirmProvider>
        </TooltipProvider>
      </AuthProvider>
    </QueryClientProvider>
  </StrictMode>,
)
