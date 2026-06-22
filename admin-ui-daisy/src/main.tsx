import React from 'react'
import ReactDOM from 'react-dom/client'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { Toaster } from 'sonner'
import App from './App'
import { ConfirmProvider } from '@/components/ui'
import './styles.css'

function isAdminAuthFailure(error: unknown) {
  const status = (error as { response?: { status?: number } } | null)?.response?.status
  return status === 401 || status === 403
}

function shouldRetryQuery(failureCount: number, error: unknown) {
  if (isAdminAuthFailure(error)) {
    return false
  }
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

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <ConfirmProvider>
        <App />
        <Toaster richColors closeButton position="top-right" />
      </ConfirmProvider>
    </QueryClientProvider>
  </React.StrictMode>
)
