/* Sessions: a heading row with search and New, then plain rows on the
   ground: title, the behaviour's chip, and when it last moved. */
import { useState } from 'react'
import { ListFilter, MessageSquare, Plus, Search, X } from 'lucide-react'
import { Button } from '@gents/ui/components/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@gents/ui/components/dropdown-menu'
import { isLive } from '@/lib/live'
import { behaviorName } from './behavior'
import { Input } from '@gents/ui/components/input'
import { ScrollArea } from '@gents/ui/components/scroll-area'
import type { Shell } from '@/hooks/useShell'
import { href } from '@/lib/router'
import { BehaviorChip } from './parts'
import { when } from './time'
import { SessionStatus } from './SessionStatus'

export function SessionsScreen({ shell }: { shell: Shell }) {
  const deployment = shell.selectedDeployment
  const held = new Set(shell.holds.flatMap((h) => (h.sessionId ? [h.sessionId] : [])))
  const [query, setQuery] = useState<string | null>(null)
  /* the three axes the summary carries: behaviour, state, and what started it */
  const [behavior, setBehavior] = useState<string>('')
  const [state, setState] = useState<'' | 'live' | 'held' | 'failed'>('')
  const [source, setSource] = useState<'' | 'person' | 'task' | 'trigger'>('')
  const conversations = (deployment?.sessions ?? []).filter((c) => {
    if (query && !(c.title ?? '').toLowerCase().includes(query.toLowerCase())) return false
    if (behavior && c.behaviorId !== behavior) return false
    if (state === 'live' && !isLive(c.turnState)) return false
    if (state === 'held' && !held.has(c.sessionId)) return false
    if (state === 'failed' && c.turnState !== 'failed') return false
    if (source === 'task' && !c.taskId) return false
    if (source === 'trigger' && !c.triggerId) return false
    if (source === 'person' && (c.taskId || c.triggerId)) return false
    return true
  })
  const active = [
    behavior && {
      key: 'behavior',
      label: behaviorName(behavior, deployment),
      clear: () => setBehavior(''),
    },
    state && {
      key: 'state',
      label: { live: 'Live', held: 'Needs you', failed: 'Failed' }[state],
      clear: () => setState(''),
    },
    source && {
      key: 'source',
      label: { person: 'Started by a person', task: 'From a task', trigger: 'From a trigger' }[
        source
      ],
      clear: () => setSource(''),
    },
  ].filter(Boolean) as { key: string; label: string; clear: () => void }[]
  return (
    <ScrollArea className="h-full">
      <div className="mx-auto max-w-page px-6 py-6">
        <div className="flex h-10 items-center gap-2">
          <h1 className="font-heading text-lg font-medium text-heading">Sessions</h1>
          <div className="ml-auto flex items-center gap-2">
            <DropdownMenu>
              <DropdownMenuTrigger
                render={
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label="Filter sessions"
                    className={active.length ? 'bg-accent text-foreground' : undefined}
                  />
                }
              >
                <ListFilter />
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuGroup>
                  <DropdownMenuLabel>Behaviour</DropdownMenuLabel>
                  <DropdownMenuRadioGroup value={behavior} onValueChange={setBehavior}>
                    <DropdownMenuRadioItem value="">Any</DropdownMenuRadioItem>
                    {(deployment?.behaviors ?? []).map((b) => (
                      <DropdownMenuRadioItem key={b.behaviorId} value={b.behaviorId}>
                        {b.displayName}
                      </DropdownMenuRadioItem>
                    ))}
                  </DropdownMenuRadioGroup>
                </DropdownMenuGroup>
                <DropdownMenuSeparator />
                <DropdownMenuGroup>
                  <DropdownMenuLabel>State</DropdownMenuLabel>
                  <DropdownMenuRadioGroup
                    value={state}
                    onValueChange={(v) => setState(v as typeof state)}
                  >
                    <DropdownMenuRadioItem value="">Any</DropdownMenuRadioItem>
                    <DropdownMenuRadioItem value="live">Live</DropdownMenuRadioItem>
                    <DropdownMenuRadioItem value="held">Needs you</DropdownMenuRadioItem>
                    <DropdownMenuRadioItem value="failed">Failed</DropdownMenuRadioItem>
                  </DropdownMenuRadioGroup>
                </DropdownMenuGroup>
                <DropdownMenuSeparator />
                <DropdownMenuGroup>
                  <DropdownMenuLabel>Started by</DropdownMenuLabel>
                  <DropdownMenuRadioGroup
                    value={source}
                    onValueChange={(v) => setSource(v as typeof source)}
                  >
                    <DropdownMenuRadioItem value="">Anything</DropdownMenuRadioItem>
                    <DropdownMenuRadioItem value="person">A person</DropdownMenuRadioItem>
                    <DropdownMenuRadioItem value="task">A task</DropdownMenuRadioItem>
                    <DropdownMenuRadioItem value="trigger">A trigger</DropdownMenuRadioItem>
                  </DropdownMenuRadioGroup>
                </DropdownMenuGroup>
              </DropdownMenuContent>
            </DropdownMenu>
            {query === null ? (
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label="Search sessions"
                onClick={() => setQuery('')}
              >
                <Search />
              </Button>
            ) : (
              <div className="relative">
                <Input
                  autoFocus
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  onKeyDown={(e) => e.key === 'Escape' && setQuery(null)}
                  placeholder="Search sessions"
                  className="h-8 w-56 pr-8"
                  aria-label="Search sessions"
                />
                <Button
                  variant="ghost"
                  size="icon-xs"
                  aria-label="Clear search"
                  className="absolute top-1 right-1 text-muted-foreground"
                  onClick={() => setQuery(null)}
                >
                  <X />
                </Button>
              </div>
            )}
            <Button
              variant="brand"
              size="sm"
              nativeButton={false}
              render={<a href={href({ name: 'session', sessionId: null })} />}
            >
              New
            </Button>
          </div>
        </div>
        {active.length > 0 && (
          <div className="mt-3 flex flex-wrap items-center gap-1.5">
            {active.map((f) => (
              <button
                key={f.key}
                type="button"
                onClick={f.clear}
                className="flex h-7 items-center gap-1 rounded-full border border-border px-2.5 text-xs text-muted-foreground hover:text-foreground"
              >
                {f.label} <X className="size-3" />
              </button>
            ))}
          </div>
        )}
        <ul className="mt-4 divide-y divide-border/60">
          {conversations.map((c) => (
            <li key={c.sessionId}>
              <a
                href={href({ name: 'session', sessionId: c.sessionId })}
                className="-mx-3 grid grid-cols-[auto_1fr_auto_auto] items-center gap-4 rounded-lg px-3 py-3.5 hover:bg-accent"
              >
                <SessionStatus turnState={c.turnState} held={held.has(c.sessionId)} />
                <p className="truncate text-sm">{c.title ?? 'Untitled'}</p>
                <BehaviorChip
                  behaviorId={c.behaviorId}
                  deployment={deployment}
                  showName={false}
                  description={shell.behaviorDescriptions[c.behaviorId ?? '']}
                />
                <span className="w-20 text-right text-sm text-muted-foreground">
                  {when(c.updatedAt)}
                </span>
              </a>
            </li>
          ))}
          {conversations.length === 0 && (query || active.length > 0) && (
            <li className="py-8 text-center text-sm text-muted-foreground">No sessions match.</li>
          )}
          {conversations.length === 0 && !query && active.length === 0 && (
            <li className="grid min-h-[50vh] place-items-center animate-in fade-in-0 slide-in-from-bottom-2 duration-300 ease-out fill-mode-both motion-reduce:animate-none">
              <div className="text-center">
                <MessageSquare className="mx-auto size-6 text-muted-foreground" />
                <p className="mt-3 font-heading text-lg font-medium text-heading">
                  No sessions yet
                </p>
                <Button
                  variant="brand"
                  className="mt-5"
                  nativeButton={false}
                  render={<a href={href({ name: 'session', sessionId: null })} />}
                >
                  <Plus /> New session
                </Button>
              </div>
            </li>
          )}
        </ul>
      </div>
    </ScrollArea>
  )
}
