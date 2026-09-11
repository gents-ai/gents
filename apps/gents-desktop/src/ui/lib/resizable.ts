import { useCallback, useEffect, useRef, useState } from 'react'

/* A width that a person drags, clamped and remembered. Returns the width
   and the props for the handle element: pointer events that resize the
   panel from the handle's left edge, keyboard arrows for accessibility. */
export function useResizableWidth({
  key,
  initial,
  min,
  max,
}: {
  key: string
  initial: number
  min: number
  /* a number, or a function of the container width */
  max: number | ((container: number) => number)
}) {
  const [width, setWidth] = useState(() => {
    try {
      const saved = Number(localStorage.getItem(key))
      return saved > 0 ? saved : initial
    } catch {
      return initial
    }
  })
  useEffect(() => {
    try {
      localStorage.setItem(key, String(Math.round(width)))
    } catch {
      /* storage unavailable */
    }
  }, [key, width])

  const clamp = useCallback(
    (w: number, container: number) =>
      Math.min(typeof max === 'function' ? max(container) : max, Math.max(min, w)),
    [min, max],
  )
  const dragging = useRef<{ startX: number; startWidth: number; container: number } | null>(null)
  const [isDragging, setIsDragging] = useState(false)

  const onPointerDown = (e: React.PointerEvent<HTMLElement>) => {
    const container = e.currentTarget.parentElement?.getBoundingClientRect().width ?? Infinity
    dragging.current = { startX: e.clientX, startWidth: width, container }
    setIsDragging(true)
    e.currentTarget.setPointerCapture(e.pointerId)
  }
  const onPointerMove = (e: React.PointerEvent<HTMLElement>) => {
    const d = dragging.current
    if (!d) return
    /* the panel sits to the right of the handle: dragging left widens it */
    setWidth(clamp(d.startWidth + (d.startX - e.clientX), d.container))
  }
  const onPointerUp = () => {
    dragging.current = null
    setIsDragging(false)
  }
  const onKeyDown = (e: React.KeyboardEvent<HTMLElement>) => {
    const step = e.shiftKey ? 64 : 16
    const container = e.currentTarget.parentElement?.getBoundingClientRect().width ?? Infinity
    if (e.key === 'ArrowLeft') setWidth((w) => clamp(w + step, container))
    if (e.key === 'ArrowRight') setWidth((w) => clamp(w - step, container))
  }

  return {
    width,
    isDragging,
    handleProps: {
      role: 'separator' as const,
      'aria-orientation': 'vertical' as const,
      'aria-valuenow': Math.round(width),
      tabIndex: 0,
      onPointerDown,
      onPointerMove,
      onPointerUp,
      onPointerCancel: onPointerUp,
      onKeyDown,
    },
  }
}
