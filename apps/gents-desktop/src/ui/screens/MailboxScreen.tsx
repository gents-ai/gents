/* Mailbox: what the agent filed for a person, after the Figma "Mailbox"
   section and the desktop's vocabulary. An item is a stamped envelope
   the agent wrote with file_mailbox_item: a kind (ask, gate, finished,
   failed, flag), an action it wants (ack: just read it; start_request:
   open a conversation on it; write_document: a document is expected),
   its source, and a summary. A rail down the left carries one glyph per
   kind; each card offers the action the desktop offers for it (Open
   source for ack, Open compose otherwise) and Dismiss. Held tool calls
   are not mailbox items; they live in the session. */
import {
  ArrowLeft,
  ArrowRight,
  CircleCheck,
  CircleHelp,
  CircleX,
  Flag,
  Inbox,
  OctagonPause,
  Plus,
  X,
} from 'lucide-react'
import type { MailboxItemView } from '@source-inc/gents-desktop-client'
import { Button } from '@gents/ui/components/button'
import { ScrollArea } from '@gents/ui/components/scroll-area'
import { cn } from '@gents/ui/lib/utils'
import type { Shell } from '@/hooks/useShell'
import { navigate } from '@/lib/router'
import { toast } from 'sonner'
import { useState } from 'react'
import { BehaviorAvatar } from './parts'
import { BehaviorHoverCard } from './HoverCards'
import { behaviorName } from './behavior'
import { when } from './time'

/* the kind glyph on the rail, and its word */
const KIND: Record<string, { icon: typeof CircleHelp; label: string; tone?: string }> = {
  ask: { icon: CircleHelp, label: 'Question' },
  gate: { icon: OctagonPause, label: 'Gate' },
  finished: { icon: CircleCheck, label: 'Finished' },
  failed: { icon: CircleX, label: 'Failed', tone: 'text-destructive' },
  flag: { icon: Flag, label: 'Flag' },
}

export function MailboxScreen({ shell }: { shell: Shell }) {
  const deployment = shell.selectedDeployment
  const items = (deployment?.mailboxItems ?? []).filter((m) => m.status === 'open')
  return (
    <ScrollArea className="h-full">
      <div className="mx-auto max-w-page px-6 py-6">
        <a
          href="#/sessions"
          className="inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
        >
          <ArrowLeft className="size-3" /> Back
        </a>
        {items.length > 0 && (
          <h1 className="mt-2 font-heading text-lg font-medium text-heading">
            Needs your attention
          </h1>
        )}
        {items.length > 0 && (
          <ol className="relative mt-5 grid gap-5 pl-10">
            <span
              aria-hidden="true"
              className="absolute top-3 bottom-3 left-[11px] w-px bg-border"
            />
            {items.map((m) => (
              <Item
                key={m.itemId}
                item={m}
                shell={shell}
                behavior={behaviorName(m.targetBehaviorId, deployment)}
              />
            ))}
          </ol>
        )}
        {items.length === 0 && (
          <div className="grid min-h-[60vh] place-items-center animate-in fade-in-0 slide-in-from-bottom-2 duration-300 ease-out fill-mode-both motion-reduce:animate-none">
            <div className="text-center">
              <Inbox className="mx-auto size-6 text-muted-foreground" />
              <p className="mt-3 font-heading text-lg font-medium text-heading">
                Nothing needs your attention
              </p>
              <Button variant="brand" className="mt-5" render={<a href="#/sessions/new" />}>
                <Plus /> New session
              </Button>
            </div>
          </div>
        )}
      </div>
    </ScrollArea>
  )
}

function Item({
  item: m,
  shell,
  behavior,
}: {
  item: MailboxItemView
  shell: Shell
  behavior: string
}) {
  const kind = KIND[m.kind] ?? { icon: CircleHelp, label: m.kind }
  const Icon = kind.icon
  /* ack: there is nothing to do but read it, so the arrow opens its source;
     anything else opens a conversation on it (the desktop's "Open compose") */
  const acknowledge = m.action === 'ack'
  const open = async () => {
    if (acknowledge) {
      if (m.sessionId) navigate({ name: 'session', sessionId: m.sessionId })
      return
    }
    try {
      const item = await shell.openMailboxItem(m.itemId)
      navigate(
        item.sessionId
          ? { name: 'session', sessionId: item.sessionId }
          : { name: 'session', sessionId: null },
      )
    } catch (e) {
      toast(`Couldn't open: ${String(e)}`)
    }
  }
  const [openedAt] = useState(() => Date.now())
  const deadline = m.deadlineAt ? Date.parse(m.deadlineAt) : null
  const overdue = deadline !== null && deadline < openedAt
  const due = deadline === null ? null : overdue ? 'overdue' : `due in ${span(deadline - openedAt)}`
  return (
    <li className="relative">
      <span
        className={cn(
          'absolute top-3 -left-10 grid size-6 place-items-center rounded-full bg-background text-muted-foreground',
          kind.tone,
        )}
        title={kind.label}
      >
        <Icon className="size-4" />
      </span>
      <article className="rounded-2xl border border-border/60 bg-raised px-4 pt-3 pb-5">
        <div className="flex items-start gap-3">
          <BehaviorHoverCard deployment={shell.selectedDeployment} behaviorId={m.targetBehaviorId}>
            <BehaviorAvatar
              name={behavior}
              behaviorId={m.targetBehaviorId}
              className="mt-0.5 cursor-default"
            />
          </BehaviorHoverCard>
          <div className="min-w-0 flex-1">
            <div className="flex items-baseline gap-3">
              <h2 className="min-w-0 flex-1 truncate font-heading text-sm font-medium text-heading">
                {m.title}
              </h2>
              <span className="shrink-0 text-xs text-muted-foreground">{when(m.createdAt)}</span>
            </div>
            {m.summary && <p className="mt-0.5 text-sm text-muted-foreground">{m.summary}</p>}
            <p className="mt-1.5 font-mono text-[11px] text-muted-foreground">
              {kind.label.toLowerCase()} · {m.sourceKind} · {m.sourceId}
              {m.action === 'write_document' && m.expectedCollection
                ? ` · expects ${m.expectedCollection}`
                : ''}
              {due && <span className={cn(overdue && 'text-destructive')}> · {due}</span>}
            </p>
            {m.payload && (
              <pre className="mt-2 max-h-40 overflow-auto rounded-md bg-surface px-3 py-2 font-mono text-[11px] leading-relaxed whitespace-pre-wrap text-foreground">
                {pretty(m.payload)}
              </pre>
            )}
          </div>
          {(m.sessionId || !acknowledge) && (
            <Button
              size="icon-sm"
              variant="ghost"
              className="-mr-1 self-center"
              aria-label={
                acknowledge ? 'Open source' : m.sessionId ? 'Open compose' : 'Start a conversation'
              }
              title={acknowledge ? 'Open source' : 'Open compose'}
              onClick={() => void open()}
            >
              <ArrowRight />
            </Button>
          )}
        </div>
      </article>
      <Button
        size="sm"
        variant="raised"
        className="-mt-3.5 ml-4 flex h-7 w-fit rounded-full px-3 text-xs"
        onClick={() => shell.dismissMailboxItem(m.itemId)}
      >
        <X className="size-3" /> Dismiss
      </Button>
    </li>
  )
}

const pretty = (value: string) => {
  try {
    return JSON.stringify(JSON.parse(value), null, 2)
  } catch {
    return value
  }
}

/* a span ahead, as a person would say it */
const span = (ms: number) => {
  const mins = Math.max(1, Math.round(ms / 60_000))
  if (mins < 60) return `${mins}m`
  if (mins < 1_440) return `${Math.round(mins / 60)}h`
  return `${Math.round(mins / 1_440)}d`
}
