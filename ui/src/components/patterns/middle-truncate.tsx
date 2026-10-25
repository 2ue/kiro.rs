import * as React from 'react'
import { middleTruncate } from '@/lib/middle-truncate'
import { cn } from '@/lib/utils'

let measureContext: CanvasRenderingContext2D | null = null

function context(): CanvasRenderingContext2D | null {
  if (!measureContext) measureContext = document.createElement('canvas').getContext('2d')
  return measureContext
}

/**
 * One line of text that, when it does not fit, keeps its start and end and shows `***` in the
 * middle. Screen readers always get the full text.
 */
export function MiddleTruncate({ text, className }: { text: string; className?: string }) {
  const ref = React.useRef<HTMLSpanElement>(null)
  const [shown, setShown] = React.useState(text)

  React.useLayoutEffect(() => {
    const el = ref.current
    const ctx = context()
    if (!el || !ctx) return
    let active = true
    const update = () => {
      if (!active) return
      const width = el.getBoundingClientRect().width
      if (width <= 0) return
      const style = getComputedStyle(el)
      ctx.font = `${style.fontStyle} ${style.fontWeight} ${style.fontSize} ${style.fontFamily}`
      const spacing = Number.parseFloat(style.letterSpacing) || 0
      // A pixel of slack absorbs rounding between canvas and layout.
      setShown(middleTruncate(text, (candidate) => ctx.measureText(candidate).width + spacing * candidate.length <= width - 1))
    }
    update()
    const observer = new ResizeObserver(update)
    observer.observe(el)
    // Widths measured before the web font arrives are off; measure again once it has.
    void document.fonts?.ready.then(update)
    return () => {
      active = false
      observer.disconnect()
    }
  }, [text])

  const cut = shown !== text
  return (
    <span ref={ref} className={cn('block overflow-hidden whitespace-nowrap', className)}>
      <span aria-hidden={cut || undefined}>{shown}</span>
      {cut && <span className="sr-only">{text}</span>}
    </span>
  )
}
