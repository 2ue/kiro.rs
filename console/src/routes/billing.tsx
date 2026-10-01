import { createFileRoute } from '@tanstack/react-router'
import { opt, searchSchema } from '@/lib/search-schema'
import { BillingPage } from '@/features/billing/billing-page'

export const Route = createFileRoute('/billing')({
  validateSearch: searchSchema({ tab: opt.enum(['cost', 'quota', 'pools']) }),
  component: BillingPage,
})
