import { createFileRoute } from '@tanstack/react-router'
import { z } from 'zod'
import { BillingPage } from '@/features/billing/billing-page'

export const Route = createFileRoute('/billing')({
  validateSearch: z.object({ tab: z.enum(['cost', 'quota', 'pools']).optional().catch(undefined) }),
  component: BillingPage,
})
