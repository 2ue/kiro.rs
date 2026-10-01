import { createFileRoute } from '@tanstack/react-router'
import { SchedulerPage } from '@/features/scheduler/scheduler-page'

export const Route = createFileRoute('/scheduler')({ component: SchedulerPage })
