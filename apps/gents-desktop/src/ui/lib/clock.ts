/* the current time as React state, ticking once a second only while
   something on screen is still running */
import { useSyncExternalStore } from 'react'

let now = Date.now()
const listeners = new Set<() => void>()
let timer: number | null = null
const start = () => {
  if (timer !== null) return
  timer = window.setInterval(() => {
    now = Date.now()
    for (const l of listeners) l()
  }, 1000)
}
const stop = () => {
  if (timer !== null && listeners.size === 0) {
    window.clearInterval(timer)
    timer = null
  }
}

export function useNow(active: boolean) {
  return useSyncExternalStore(
    (notify) => {
      if (!active) return () => undefined
      listeners.add(notify)
      start()
      return () => {
        listeners.delete(notify)
        stop()
      }
    },
    () => (active ? now : 0),
    () => 0,
  )
}
