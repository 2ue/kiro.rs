import { createFileRoute } from '@tanstack/react-router'
import { z } from 'zod'
import { AuditPage } from '@/features/audit/audit-page'

export const Route = createFileRoute('/audit')({
  validateSearch: z.object({ page: z.number().int().min(1).optional().catch(undefined) }),
  component: AuditPage,
})
