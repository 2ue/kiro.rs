import { createFileRoute } from '@tanstack/react-router'
import { SettingsPage } from '@/features/settings/settings-page'

export const Route = createFileRoute('/settings/$section')({ component: SettingsPage })
