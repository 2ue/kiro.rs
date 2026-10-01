import { createFileRoute } from '@tanstack/react-router'
import { z } from 'zod'
import { PoolsPage } from '@/features/pools/pools-page'

export const Route = createFileRoute('/pools')({
  validateSearch: z.object({
    id: z.number().int().optional().catch(undefined),
    tab: z.enum(['pools', 'policy']).optional().catch(undefined),
  }),
  component: PoolsPage,
})
