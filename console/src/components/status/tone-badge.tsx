import type { ComponentProps, ReactNode } from 'react'
import { cn } from '@/lib/utils'
import { TONE_BADGE, TONE_DOT, type Tone } from './tone'

export function ToneBadge({
  tone = 'neutral',
  icon,
  dot,
  className,
  children,
  ...props
}: ComponentProps<'span'> & { tone?: Tone; icon?: ReactNode; dot?: boolean }) {
  return (
    <span
      className={cn(
        'inline-flex h-5 shrink-0 items-center gap-1 rounded-md border px-1.5 text-xs font-medium whitespace-nowrap [&>svg]:size-3',
        TONE_BADGE[tone],
        className,
      )}
      {...props}
    >
      {dot && <span className={cn('size-1.5 rounded-full', TONE_DOT[tone])} aria-hidden />}
      {icon}
      {children}
    </span>
  )
}

export function StatusDot({ tone, pulse, className }: { tone: Tone; pulse?: boolean; className?: string }) {
  return (
    <span className={cn('relative inline-flex size-2 shrink-0', className)} aria-hidden>
      {pulse && <span className={cn('absolute inset-0 animate-ping rounded-full opacity-60', TONE_DOT[tone])} />}
      <span className={cn('relative inline-flex size-2 rounded-full', TONE_DOT[tone])} />
    </span>
  )
}
