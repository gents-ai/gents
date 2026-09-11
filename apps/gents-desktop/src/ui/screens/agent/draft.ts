import { useState } from 'react'
import { toast } from 'sonner'

/* a draft of T that persists on commit; `saved` is what the bridge holds */
export function useDraft<T extends object>(saved: T, persist: (next: T) => Promise<unknown>) {
  const [draft, setDraft] = useState<T>(saved)
  const dirty = JSON.stringify(draft) !== JSON.stringify(saved)
  const save = async (next: T) => {
    try {
      await persist(next)
      toast('Saved')
    } catch (e) {
      toast(`Save failed: ${e instanceof Error ? e.message : String(e)}`)
    }
  }
  const set = <K extends keyof T>(k: K, v: T[K]) => setDraft((d) => ({ ...d, [k]: v }))
  const choose = <K extends keyof T>(k: K, v: T[K]) => {
    const next = { ...draft, [k]: v }
    setDraft(next)
    void save(next)
  }
  const commit = () => {
    if (dirty) void save(draft)
  }
  const onEnter = (e: React.KeyboardEvent<HTMLElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) (e.target as HTMLElement).blur()
  }
  return { draft, set, choose, commit, onEnter, dirty }
}

/* one item per line; the desktop app splits on newline or comma */
export const toLines = (items: string[]) => items.join('\n')
export const fromLines = (text: string) =>
  text
    .split(/[\n,]/)
    .map((s) => s.trim())
    .filter(Boolean)

/* the desktop app's number hints */
export const intOrNull = (s: string) => (s.trim() === '' ? null : Number.parseInt(s, 10))
export const floatOrNull = (s: string) => (s.trim() === '' ? null : Number.parseFloat(s))
export const str = (n: number | null | undefined) => (n == null ? '' : String(n))

/* ids for new documents, minted in the handler, never in render */
export const newId = (prefix: string) => `${prefix}-${Date.now().toString(36)}`
