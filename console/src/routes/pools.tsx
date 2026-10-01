import { createFileRoute } from '@tanstack/react-router'
import { opt, searchSchema } from '@/lib/search-schema'
import { PoolsPage } from '@/features/pools/pools-page'

export const Route = createFileRoute('/pools')({
  validateSearch: searchSchema({
    id: opt.int(),
    tab: opt.enum(['pools', 'policy']),
    focus: opt.str(),
  }),
  component: PoolsPage,
})
