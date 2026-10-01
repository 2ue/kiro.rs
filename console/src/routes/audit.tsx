import { createFileRoute } from '@tanstack/react-router'
import { opt, searchSchema } from '@/lib/search-schema'
import { AuditPage } from '@/features/audit/audit-page'

export const Route = createFileRoute('/audit')({
  validateSearch: searchSchema({ page: opt.int(1) }),
  component: AuditPage,
})
