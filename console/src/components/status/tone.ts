/** 语义色调；所有徽章、圆点、进度条共用 */
export type Tone = 'success' | 'warning' | 'orange' | 'danger' | 'info' | 'primary' | 'neutral'

export const TONE_BADGE: Record<Tone, string> = {
  success: 'bg-success-subtle text-success border-success/20',
  warning: 'bg-warning-subtle text-warning border-warning/25',
  orange: 'bg-orange-subtle text-orange border-orange/25',
  danger: 'bg-danger-subtle text-danger border-danger/20',
  info: 'bg-info-subtle text-info border-info/20',
  primary: 'bg-accent text-accent-foreground border-primary/20',
  neutral: 'bg-muted text-muted-foreground border-border',
}

export const TONE_DOT: Record<Tone, string> = {
  success: 'bg-success',
  warning: 'bg-warning',
  orange: 'bg-orange',
  danger: 'bg-danger',
  info: 'bg-info',
  primary: 'bg-primary',
  neutral: 'bg-muted-foreground/50',
}

export const TONE_TEXT: Record<Tone, string> = {
  success: 'text-success',
  warning: 'text-warning',
  orange: 'text-orange',
  danger: 'text-danger',
  info: 'text-info',
  primary: 'text-primary',
  neutral: 'text-muted-foreground',
}
