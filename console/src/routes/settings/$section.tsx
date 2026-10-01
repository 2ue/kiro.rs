import { createFileRoute } from '@tanstack/react-router'
import { opt, searchSchema } from '@/lib/search-schema'
import { SettingsPage } from '@/features/settings/settings-page'

export const Route = createFileRoute('/settings/$section')({
  validateSearch: searchSchema({ focus: opt.str() }),
  component: SettingsPage,
})
