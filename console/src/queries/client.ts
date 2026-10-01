import { MutationCache, QueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { ApiError, errorMessage } from '@/api/client'
import { translateError } from '@/domain/upstream-error'

declare module '@tanstack/react-query' {
  interface Register {
    mutationMeta: {
      /** 成功提示；不设置则不提示 */
      success?: string
      /** 失败提示前缀；设置为 false 时由调用方自行处理 */
      error?: string | false
    }
  }
}

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 5_000,
      refetchOnWindowFocus: false,
      refetchIntervalInBackground: false,
      retry: (count, error) => {
        if (error instanceof ApiError && error.status >= 400 && error.status < 500) return false
        return count < 2
      },
    },
  },
  mutationCache: new MutationCache({
    onSuccess: (_data, _vars, _ctx, mutation) => {
      const msg = mutation.meta?.success
      if (msg) toast.success(msg)
    },
    onError: (error, _vars, _ctx, mutation) => {
      const prefix = mutation.meta?.error
      if (prefix === false) return
      const t = translateError(errorMessage(error))
      const title = t?.title ?? errorMessage(error)
      toast.error(prefix ? `${prefix}：${title}` : title, { description: t?.hint })
    },
  }),
})
