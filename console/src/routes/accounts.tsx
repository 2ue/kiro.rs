import { createFileRoute } from '@tanstack/react-router'
import { AccountsPage } from '@/features/accounts/accounts-page'
import { accountsSearchSchema } from '@/features/accounts/search'

export const Route = createFileRoute('/accounts')({
  validateSearch: accountsSearchSchema,
  component: AccountsPage,
})
