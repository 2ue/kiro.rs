import * as React from 'react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { ConfirmProvider } from '@/components/patterns'
import { Toaster, TooltipProvider } from '@/components/ui'

function isAdminAuthFailure(error: unknown) {
  const status = (error as { response?: { status?: number } } | null)?.response?.status
  return status === 401 || status === 403
}

function shouldRetryQuery(failureCount: number, error: unknown) {
  if (isAdminAuthFailure(error)) return false
  return failureCount < 1
}

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: shouldRetryQuery,
      staleTime: 5000,
      refetchOnWindowFocus: false,
    },
  },
})

export { queryClient }

export function AppProviders({ children }: { children: React.ReactNode }) {
  return (
    <QueryClientProvider client={queryClient}>
      <TooltipProvider delayDuration={200}>
        <ConfirmProvider>{children}</ConfirmProvider>
      </TooltipProvider>
      <Toaster />
    </QueryClientProvider>
  )
}
