import { createFileRoute } from '@tanstack/react-router'
import { ModelsPage } from '@/features/models/models-page'

export const Route = createFileRoute('/models')({ component: ModelsPage })
