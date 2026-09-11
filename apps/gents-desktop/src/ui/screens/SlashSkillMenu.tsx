/* the skill list the composer shows above its textarea while a "/" line is being typed */
import type { SkillView } from '@source-inc/gents-desktop-client'
import { cn } from '@gents/ui/lib/utils'

export function SlashSkillMenu({
  items,
  active,
  onPick,
}: {
  items: SkillView[]
  active: number
  onPick: (skillId: string) => void
}) {
  if (!items.length) return null
  return (
    <ul
      role="listbox"
      aria-label="Skills"
      className="mx-2 mt-2 grid gap-0.5 border-b border-border/60 pb-2"
    >
      {items.map((s, i) => (
        <li key={s.skillId}>
          <button
            type="button"
            role="option"
            aria-selected={i === active}
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => onPick(s.skillId)}
            className={cn(
              'flex w-full items-baseline gap-3 rounded-lg px-2 py-1.5 text-left text-sm hover:bg-accent',
              i === active && 'bg-muted',
            )}
          >
            <span className="font-mono text-xs text-muted-foreground">/{s.skillId}</span>
            <span className="min-w-0 truncate">{s.displayName ?? s.name ?? ''}</span>
            {s.description && (
              <span className="ml-auto min-w-0 truncate text-xs text-muted-foreground">
                {s.description}
              </span>
            )}
          </button>
        </li>
      ))}
    </ul>
  )
}
