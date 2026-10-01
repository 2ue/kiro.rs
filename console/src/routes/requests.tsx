import { createFileRoute } from '@tanstack/react-router'
import { RequestsPage } from '@/features/requests/requests-page'
import { requestsSearchSchema } from '@/features/requests/search'

export const Route = createFileRoute('/requests')({
  validateSearch: requestsSearchSchema,
  component: RequestsPage,
})
