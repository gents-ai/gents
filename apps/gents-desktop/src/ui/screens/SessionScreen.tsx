/* One session: start a new one, or read and continue an existing one.
   Built from the kit's conversation patterns over the desktop app's
   session projection: the timeline items are the bridge's own
   RenderedTimelineItem, rendered as they arrive. */
import { useEffect, useRef, useState } from 'react'
import { ArrowDown, ArrowLeft, Copy, PanelRight, Pencil, Split, Target } from 'lucide-react'
import { toast } from 'sonner'
import type { GoalView } from '@source-inc/gents-desktop-client'
import { Button } from '@gents/ui/components/button'
import { Input } from '@gents/ui/components/input'
import {
  AssistantMessage,
  Composer,
  ToolStep,
  ToolSteps,
  UserMessage,
  type ToolStepStatus,
} from '@gents/ui/conversation'
import type { Shell } from '@/hooks/useShell'
import { useResizableWidth } from '@/lib/resizable'
import { useMediaQuery } from '@/lib/media'
import { Sheet, SheetContent, SheetTitle } from '@gents/ui/components/sheet'
import { href, navigate } from '@/lib/router'
import { Hint } from './Hint'
import { ScrollArea } from '@gents/ui/components/scroll-area'
import { bashAccess, behaviorName, fileAccess, network } from './behavior'
import { AgentAvatar } from './AgentAvatar'
import { BehaviorPicker } from './BehaviorPicker'
import { HoldCard } from './HoldCard'
import { LoadingStatus } from './LoadingStatus'
import { SlashSkillMenu } from './SlashSkillMenu'
import { useSlashSkills } from './useSlashSkills'
import { Thinking } from './Thinking'
import { TracePanel } from './TracePanel'
import { BehaviorAvatar, BehaviorChip } from './parts'
import { CascadeDialog } from './CascadeDialog'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@gents/ui/components/alert-dialog'
import { Markdown } from './Markdown'
import { ToolBody } from './tool-views'
import { toolSummary } from './tool-summary'
import { sendStatus } from '@/lib/send-status'

const stepStatus = (kind: string): ToolStepStatus =>
  kind === 'completed' || kind === 'failed' || kind === 'cancelled' || kind === 'error'
    ? 'done'
    : kind === 'running' || kind === 'held'
      ? 'running'
      : 'pending'

/* the trace panel's open state outlives the session; a person who works
   with the trace open keeps it open */
function useTraceOpen() {
  const [open, setOpen] = useState(() => {
    try {
      return localStorage.getItem('gents-prototype-trace') === '1'
    } catch {
      return false
    }
  })
  useEffect(() => {
    try {
      localStorage.setItem('gents-prototype-trace', open ? '1' : '0')
    } catch {
      /* storage unavailable */
    }
  }, [open])
  return [open, setOpen] as const
}

function useBehaviorChoice(shell: Shell) {
  const behaviours = shell.selectedDeployment?.behaviors ?? []
  const [picked, setPicked] = useState<string | null>(null)
  const behaviorId =
    picked ??
    shell.mailboxCause?.behaviorId ??
    behaviours.find((b) => b.isDefault)?.behaviorId ??
    behaviours[0]?.behaviorId ??
    null
  return { behaviorId, setPicked }
}

export function SessionScreen({ shell }: { shell: Shell }) {
  const session = shell.selectedSession
  const [draft, setDraft] = useState('')
  const [cascadeFor, setCascadeFor] = useState<string | null>(null)
  const [retrying, setRetrying] = useState(false)
  const [loadingOlder, setLoadingOlder] = useState(false)
  const [forked, setForked] = useState<{ sessionId: string; title: string } | null>(null)
  const [traceOpenPref, setTracePref] = useTraceOpen()
  /* the remembered state is a desktop habit; on a phone the sheet opens only by hand */
  const [mobileTrace, setMobileTrace] = useState(false)
  /* the transcript column follows new content while the reader is near
     the bottom; a reader who has scrolled up is left where they are */
  const column = useRef<HTMLDivElement>(null)
  const viewport = () =>
    column.current?.querySelector<HTMLElement>('[data-slot=scroll-area-viewport]') ?? null
  const itemCount = session?.timelineItems.length ?? 0
  const liveLength = session?.activeResponseOverlay?.content?.length ?? 0
  const opened = useRef<string | null>(null)
  useEffect(() => {
    const el = viewport()
    if (!el) return
    /* a session just opened: land at its end */
    if (opened.current !== shell.selectedSessionId && itemCount > 0) {
      opened.current = shell.selectedSessionId
      el.scrollTop = el.scrollHeight
      return
    }
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 160
    if (nearBottom) el.scrollTop = el.scrollHeight
  }, [itemCount, liveLength, shell.selectedSessionId])
  /* away from the bottom, a button offers the way back; scrolling is the cue */
  const [atBottom, setAtBottom] = useState(true)
  useEffect(() => {
    const el = viewport()
    if (!el) return
    const check = () => setAtBottom(el.scrollHeight - el.scrollTop - el.clientHeight < 40)
    check()
    el.addEventListener('scroll', check, { passive: true })
    return () => el.removeEventListener('scroll', check)
  }, [shell.selectedSessionId, itemCount])
  const toBottom = () => {
    const el = viewport()
    if (el) el.scrollTo({ top: el.scrollHeight, behavior: 'smooth' })
  }
  /* once the full header scrolls out, a condensed one sticks to the top */
  const headerEnd = useRef<HTMLDivElement>(null)
  const [condensed, setCondensed] = useState(false)
  useEffect(() => {
    const el = headerEnd.current
    const root = viewport()
    if (!el || !root) return
    const io = new IntersectionObserver(([e]) => setCondensed(!e!.isIntersecting), { root })
    io.observe(el)
    return () => io.disconnect()
  }, [shell.selectedSessionId])
  const wide = useMediaQuery('(min-width: 768px)')
  const traceOpen = wide ? traceOpenPref : mobileTrace
  const setTraceOpen = (open: boolean) => (wide ? setTracePref(open) : setMobileTrace(open))
  const trace = useResizableWidth({
    key: 'gents-prototype-trace-width',
    initial: 520,
    min: 320,
    max: (container) => container * 0.6,
  })
  const choice = useBehaviorChoice(shell)
  const deployment = shell.selectedDeployment
  const agentName = deployment?.agentPrincipal.displayName ?? 'the agent'

  const send = async (text: string) => {
    const result = await shell.sendMessage(text, session?.behaviorId ?? choice.behaviorId)
    setDraft('')
    if (result && result.sessionId !== shell.selectedSessionId) {
      navigate({ name: 'session', sessionId: result.sessionId })
    }
  }

  const contextFor = (behaviorId?: string | null) => {
    const b = deployment?.behaviors.find((x) => x.behaviorId === behaviorId)
    return deployment?.contexts.find((c) => c.context_id === b?.contextId)
  }
  const startSlash = useSlashSkills(
    draft,
    setDraft,
    deployment?.skills ?? [],
    contextFor(choice.behaviorId),
  )
  const slash = useSlashSkills(
    draft,
    setDraft,
    deployment?.skills ?? [],
    contextFor(session?.behaviorId),
  )

  /* ---- start a new session ---- */
  if (!shell.selectedSessionId) {
    const env = deployment?.behaviorEnvironments.find((e) => e.behaviorId === choice.behaviorId)
    const chosenName = behaviorName(choice.behaviorId, deployment)
    const startStatus = sendStatus({
      clientOnline: Boolean(shell.snapshot?.client),
      deployment,
      behaviorId: choice.behaviorId,
      sending: shell.sending,
      inFlight: false,
      turnState: null,
    })
    return (
      <div
        key={shell.selectedAgentDid ?? 'new'}
        data-testid="session-screen"
        className="mx-auto grid min-h-full max-w-2xl content-center gap-6 px-6 py-16 animate-in fade-in-0 slide-in-from-bottom-2 duration-300 ease-out fill-mode-both motion-reduce:animate-none"
      >
        <div className="flex items-start gap-3">
          <AgentAvatar name={agentName} className="size-8" />
          <div>
            <h1 className="font-heading text-lg font-medium text-heading">
              Start a new chat with {agentName}
            </h1>
            <p className="mt-1 text-sm text-muted-foreground">
              {shell.mailboxCause
                ? 'Answering a mailbox item; the first message starts its request'
                : 'The first message creates the session automatically'}
            </p>
          </div>
        </div>
        <div data-testid="composer">
        <Composer
          value={draft}
          onChange={setDraft}
          onSend={send}
          models={[]}
          disabled={startStatus.kind === 'disabled'}
          above={
            <SlashSkillMenu
              items={startSlash.items}
              active={startSlash.active}
              onPick={startSlash.accept}
            />
          }
          onKeyDown={startSlash.onKeyDown}
          leading={
            <BehaviorPicker
              shell={shell}
              deployment={deployment}
              behaviorId={choice.behaviorId}
              onChange={choice.setPicked}
            />
          }
          sending={shell.sending}
          placeholder={startStatus.kind === 'disabled' ? startStatus.hint : 'Ask anything'}
        />
        </div>
        <p className="text-xs text-muted-foreground">
          {chosenName} <strong className="font-medium text-foreground">can</strong>{' '}
          {env
            ? `${fileAccess(env.fileAccess)} files and ${bashAccess(env.bashAccess)} commands`
            : '…'}
          , and <strong className="font-medium text-foreground">has access</strong> to{' '}
          {network(env?.networkAccess)}.
          {deployment && choice.behaviorId && (
            <>
              {' '}
              <a
                href={href({
                  name: 'agent',
                  agentDid: deployment.agentDid,
                  section: 'behaviors',
                  item: choice.behaviorId,
                })}
                className="underline decoration-border underline-offset-4 hover:text-foreground"
              >
                Configure
              </a>
            </>
          )}
        </p>
      </div>
    )
  }

  /* ---- an existing session ---- */
  const holdsHere = shell.holds.filter((h) => h.sessionId === session?.sessionId)
  const live = session?.timelineItems.find((i) => i.kind === 'liveAssistant')
  const inFlight = Boolean(shell.selectedTrackedRequestId)

  /* stop: the desktop previews the cascade first; with no children it
     interrupts at once, otherwise it asks */
  const stop = async () => {
    const requestId = session?.latestRequestId
    if (!requestId) return
    try {
      const preview = await shell.api.previewInterruptCascade({
        requestId,
        agentDid: shell.selectedAgentDid,
        includeTerminal: false,
      })
      const kids =
        preview.willInterrupt.length + preview.willDetach.length + preview.unknownPolicy.length
      if (kids > 0) return setCascadeFor(requestId)
      const r = await shell.api.interruptRequest({
        requestId,
        agentDid: shell.selectedAgentDid,
        cause: 'userCancelled',
        cascade: false,
        expectedPreviewSignature: null,
      })
      toast(
        r.accepted
          ? 'Interrupt requested'
          : r.alreadyInterrupted
            ? 'Already interrupted'
            : 'Not interrupted',
      )
    } catch (e) {
      toast(`Couldn't interrupt: ${String(e)}`)
    }
  }

  /* retry after an error, as the desktop offers it */
  const latest = session?.latestResponse
  const wasInterrupted =
    session?.turnState === 'interrupted' ||
    Boolean(latest?.interruptedAt) ||
    latest?.cancelCause?.cause === 'interrupted' ||
    latest?.cancelCause?.cause === 'userCancelled'
  const responseError = latest?.errorMessage?.trim() ?? ''
  const showError = Boolean(responseError) && !wasInterrupted && !inFlight
  const retry = async () => {
    const requestId = session?.latestRequestId
    if (!requestId) return
    setRetrying(true)
    try {
      await shell.api.retryRequest(requestId)
    } catch (e) {
      toast(`Couldn't retry: ${String(e)}`)
    } finally {
      setRetrying(false)
    }
  }

  /* older pages prepend; the viewport keeps its place */
  const loadOlder = async () => {
    const el = viewport()
    const before = el?.scrollHeight ?? 0
    setLoadingOlder(true)
    try {
      await shell.loadOlderSessionTimeline()
    } finally {
      setLoadingOlder(false)
    }
    requestAnimationFrame(() => {
      if (el) el.scrollTop += el.scrollHeight - before
    })
  }
  /* whether a message can go, in the desktop's words */
  const status = sendStatus({
    clientOnline: Boolean(shell.snapshot?.client),
    deployment,
    behaviorId: session?.behaviorId ?? null,
    sending: shell.sending,
    inFlight,
    turnState: session?.turnState,
  })

  /* PROTOTYPE ONLY: fork this session and open the copy */
  const fork = async () => {
    if (!session) return
    try {
      const sessionId = await shell.forkSession(session.sessionId)
      /* the copy exists; moving to it is the person's call */
      setForked({ sessionId, title: `${session.title ?? 'Session'} (fork)` })
    } catch (e) {
      toast(`Couldn't fork: ${String(e)}`)
    }
  }

  /* the desktop offers one action on a message: copy */
  const copyOnly = (text: string | null | undefined) => [
    {
      label: 'Copy',
      icon: <Copy />,
      onClick: () => {
        void navigator.clipboard?.writeText(text ?? '')
        toast('Copied')
      },
    },
  ]

  return (
    <div
      className="grid h-full min-h-0"
      data-testid="session-screen"
      style={{
        gridTemplateColumns: wide
          ? `minmax(0,1fr) ${traceOpen ? 'auto' : '0px'} ${traceOpen ? trace.width : 0}px`
          : 'minmax(0,1fr) 0px 0px',
        /* the columns ease when the panel opens or closes, not while it is dragged */
        transition: trace.isDragging
          ? undefined
          : 'grid-template-columns 260ms cubic-bezier(0.22, 1, 0.36, 1)',
      }}
    >
      <div className="relative flex min-h-0 min-w-0 flex-col">
        {/* takes no space in the flow (negative margin), so nothing shifts when it appears */}
        <div
          className={`absolute inset-x-0 top-0 z-20 ${condensed ? '' : 'pointer-events-none invisible'}`}
        >
          <div className="mx-auto flex h-12 w-full max-w-page items-center gap-3 border-b border-border/60 bg-background/95 px-6 backdrop-blur">
            <a
              href={href({ name: 'sessions' })}
              aria-label="Sessions"
              className="text-muted-foreground hover:text-foreground"
            >
              <ArrowLeft className="size-4" />
            </a>
            <BehaviorAvatar
              name={behaviorName(session?.behaviorId ?? null, deployment)}
              behaviorId={session?.behaviorId}
              className="size-6 text-[10px]"
            />
            <span className="min-w-0 flex-1 truncate font-heading text-sm font-medium text-heading">
              {session?.title}
            </span>
            <Hint label="Fork session">
              <Button variant="ghost" size="icon-xs" aria-label="Fork session" onClick={fork}>
                <Split />
              </Button>
            </Hint>
            <Hint label={traceOpen ? 'Close side panel' : 'Open side panel'}>
              <Button
                variant="ghost"
                size="icon-xs"
                aria-label={traceOpen ? 'Close side panel' : 'Open side panel'}
                className={traceOpen ? 'bg-accent text-foreground' : undefined}
                onClick={() => setTraceOpen(!traceOpen)}
              >
                <PanelRight />
              </Button>
            </Hint>
          </div>
        </div>
        {/* transcript and composer scroll together in the kit's scroll area;
            the composer sticks to the foot so the bar runs the full height */}
        <div ref={column} className="min-h-0 flex-1">
          <ScrollArea className="h-full">
            <div className="mx-auto flex min-h-full w-full max-w-page flex-col px-6 pt-4">
              <a
                href={href({ name: 'sessions' })}
                className="mb-4 inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
              >
                <ArrowLeft className="size-3.5" /> Sessions
              </a>
              <div className="flex items-start justify-between gap-4">
                <div>
                  {session ? (
                    <Title
                      key={session.sessionId + (session.title ?? '')}
                      title={session.title ?? 'Untitled session'}
                      onRename={async (title) => {
                        await shell.api.renameSession({
                          agentDid: shell.selectedAgentDid ?? session.agentDid ?? '',
                          sessionId: session.sessionId,
                          title,
                        })
                        await shell.refreshSnapshot()
                        toast('Renamed')
                      }}
                    />
                  ) : (
                    <h1 className="font-heading text-lg font-medium text-heading">
                      {shell.sessionLoad.phase}
                    </h1>
                  )}
                  <div className="mt-2">
                    <BehaviorChip
                      behaviorId={session?.behaviorId ?? null}
                      deployment={deployment}
                    />
                  </div>
                </div>
                <div className="flex gap-1">
                  <Hint label="Fork session">
                    <Button variant="ghost" size="icon-sm" aria-label="Fork session" onClick={fork}>
                      <Split />
                    </Button>
                  </Hint>
                  <Hint label={traceOpen ? 'Close side panel' : 'Open side panel'}>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={traceOpen ? 'Close side panel' : 'Open side panel'}
                      aria-pressed={traceOpen}
                      className={traceOpen ? 'bg-accent text-foreground' : undefined}
                      onClick={() => setTraceOpen(!traceOpen)}
                    >
                      <PanelRight />
                    </Button>
                  </Hint>
                </div>
              </div>

              {session?.goal && <Goal goal={session.goal} />}
              <div ref={headerEnd} aria-hidden="true" />
              <div className="mt-6 grid gap-5">
                {session?.timelinePage?.hasOlder && (
                  <Button
                    variant="ghost"
                    size="sm"
                    className="justify-self-center text-muted-foreground"
                    disabled={loadingOlder}
                    onClick={loadOlder}
                  >
                    {loadingOlder ? 'Loading older messages…' : 'Load older messages'}
                  </Button>
                )}
                {session?.timelineItems.map((item) => {
                  switch (item.kind) {
                    case 'userMessage':
                    case 'pendingUserTurn':
                      return (
                        <UserMessage key={item.itemKey} actions={copyOnly(item.content)}>
                          {item.content}
                        </UserMessage>
                      )
                    case 'assistantMessage':
                      return (
                        <AssistantMessage key={item.itemKey} actions={copyOnly(item.content)}>
                          {item.reasoning && <Reasoning text={item.reasoning} />}
                          <Markdown>{item.content ?? ''}</Markdown>
                        </AssistantMessage>
                      )
                    case 'toolGroup':
                      return (
                        <ToolSteps key={item.itemKey} title="Activity">
                          {item.tools.map((t) => {
                            const sum = toolSummary(t)
                            return (
                              <ToolStep
                                key={t.itemKey}
                                label={`${sum.kind} ${sum.primary}`.trim()}
                                status={stepStatus(t.statusKind)}
                              >
                                {t.statusKind !== 'running' && t.statusKind !== 'held' ? (
                                  <ToolBody tool={t} />
                                ) : undefined}
                              </ToolStep>
                            )
                          })}
                        </ToolSteps>
                      )
                    case 'liveAssistant':
                      return (
                        <AssistantMessage key={item.itemKey}>
                          {item.content && <Markdown>{item.content}</Markdown>}
                          <Thinking />
                        </AssistantMessage>
                      )
                  }
                })}
                {wasInterrupted && !inFlight && (
                  <p className="px-2 text-xs text-muted-foreground">
                    Interrupted
                    {latest?.cancelCause
                      ? ` · ${latest.cancelCause.cause} (${latest.cancelCause.source}, ${latest.cancelCause.confidence} confidence)`
                      : ''}
                    {latest?.cancelCause?.evidence.length
                      ? ` · ${latest.cancelCause.evidence.join('; ')}`
                      : ''}
                  </p>
                )}
                {showError && (
                  <div className="rounded-2xl border border-destructive/30 bg-destructive/5 px-4 py-3">
                    <p className="text-sm font-medium">The assistant could not finish this turn.</p>
                    <pre className="mt-1 font-mono text-[11px] whitespace-pre-wrap text-muted-foreground">
                      {responseError}
                    </pre>
                    {session?.retryEligibility?.eligible && (
                      <Button
                        size="sm"
                        variant="outline"
                        className="mt-3"
                        disabled={retrying}
                        onClick={retry}
                      >
                        {retrying ? 'Retrying…' : 'Retry'}
                      </Button>
                    )}
                  </div>
                )}
                {inFlight && !live && holdsHere.length === 0 && (
                  <AssistantMessage>
                    <Thinking />
                  </AssistantMessage>
                )}
              </div>
              <div className="sticky bottom-0 mt-auto bg-background pt-6 pb-6">
                {/* the way back sits on the footer's top edge, whatever the footer holds */}
                {!atBottom && (
                  <Button
                    variant="raised"
                    size="icon-sm"
                    aria-label="Back to bottom"
                    onClick={toBottom}
                    className="absolute top-0 left-1/2 z-10 -translate-x-1/2 -translate-y-1/2 rounded-full shadow-md"
                  >
                    <ArrowDown />
                  </Button>
                )}
                <LoadingStatus shell={shell} />
                {/* a held tool call blocks the turn, so it pins above the composer as the
                    desktop's HoldsPanel does: the first in full, any others as one row each */}
                {holdsHere[0] && (
                  <div className="mb-3">
                    <HoldCard
                      title={`${holdsHere[0].toolName} needs your approval`}
                      detail={<code className="font-mono text-xs">{holdsHere[0].args}</code>}
                      onApprove={() => shell.resolveHold(holdsHere[0]!.toolCallId, true)}
                      onDeny={() => shell.resolveHold(holdsHere[0]!.toolCallId, false)}
                    >
                      {holdsHere.length > 1 && (
                        <span className="ml-auto text-xs text-muted-foreground">
                          {holdsHere.length - 1} more waiting
                        </span>
                      )}
                    </HoldCard>
                    {holdsHere.slice(1).map((h) => (
                      <div
                        key={h.toolCallId}
                        className="mt-1 flex items-center gap-2 rounded-xl border border-border/60 bg-raised px-3 py-1.5 text-sm"
                      >
                        <span className="min-w-0 flex-1 truncate">
                          {h.toolName}
                          <code className="ml-2 font-mono text-xs text-muted-foreground">
                            {h.args}
                          </code>
                        </span>
                        <Button
                          size="sm"
                          variant="brand"
                          onClick={() => shell.resolveHold(h.toolCallId, true)}
                        >
                          Approve
                        </Button>
                        <Button
                          size="sm"
                          variant="outline"
                          onClick={() => shell.resolveHold(h.toolCallId, false)}
                        >
                          Deny
                        </Button>
                      </div>
                    ))}
                  </div>
                )}
                <div data-testid="composer">
                <Composer
                  value={draft}
                  onChange={setDraft}
                  onSend={send}
                  models={[]}
                  disabled={status.kind === 'disabled' && !inFlight}
                  above={
                    <SlashSkillMenu
                      items={slash.items}
                      active={slash.active}
                      onPick={slash.accept}
                    />
                  }
                  onKeyDown={slash.onKeyDown}
                  sending={shell.sending || inFlight}
                  onStop={inFlight ? stop : undefined}
                  placeholder={
                    status.kind === 'disabled' && !inFlight ? status.hint : 'Ask anything'
                  }
                />
                </div>
                {status.kind === 'disabled' && (
                  <p className="mt-2 px-1 text-xs text-muted-foreground">{status.hint}</p>
                )}
              </div>
            </div>
          </ScrollArea>
        </div>
      </div>
      {/* the drag handle: a hairline that darkens on hover; arrow keys resize too */}
      <div
        {...trace.handleProps}
        aria-label="Resize trace"
        aria-hidden={!traceOpen || !wide}
        tabIndex={traceOpen && wide ? 0 : -1}
        className="group flex w-3 cursor-col-resize items-center justify-center overflow-hidden outline-none focus-visible:bg-accent"
      >
        <div className="h-10 w-0.5 rounded-full bg-border transition-colors group-hover:bg-muted-foreground group-focus-visible:bg-ring" />
      </div>
      {/* the panel keeps its width inside a clipping cell, so it slides rather than squashes */}
      <div className="min-h-0 min-w-0 overflow-hidden py-4" aria-hidden={!traceOpen || !wide}>
        <div
          className="h-full pr-4 transition-transform duration-[260ms] ease-[cubic-bezier(0.22,1,0.36,1)]"
          style={{
            width: trace.width,
            transform: traceOpen ? 'translateX(0)' : 'translateX(24px)',
          }}
        >
          {wide && <TracePanel shell={shell} onClose={() => setTraceOpen(false)} />}
        </div>
      </div>
      {/* below md the panel is a sheet over the transcript, as a side panel should be */}
      {!wide && (
        <Sheet open={traceOpen} onOpenChange={setTraceOpen}>
          <SheetContent
            side="right"
            className="w-[92vw] max-w-md border-0 bg-transparent p-2 shadow-none"
          >
            <SheetTitle className="sr-only">Side panel</SheetTitle>
            <TracePanel shell={shell} onClose={() => setTraceOpen(false)} />
          </SheetContent>
        </Sheet>
      )}
      <AlertDialog open={forked !== null} onOpenChange={(o) => !o && setForked(null)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Forked</AlertDialogTitle>
            <AlertDialogDescription>
              A copy of this transcript is now its own session, "{forked?.title}". This one stays as
              it is. Open the fork, or stay here and find it later in the sessions list.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Stay here</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                const target = forked
                setForked(null)
                if (target) navigate({ name: 'session', sessionId: target.sessionId })
              }}
            >
              Open the fork
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      <CascadeDialog
        shell={shell}
        requestId={cascadeFor}
        onClose={() => setCascadeFor(null)}
        onResult={(text) => toast(text)}
      />
    </div>
  )
}

/* the session's title, renamed in place the way the desktop's chat
   header does: a pencil beside it, Enter or blur saves, Escape reverts */
function Title({ title, onRename }: { title: string; onRename: (title: string) => Promise<void> }) {
  const [editing, setEditing] = useState(false)
  const [draft, setDraft] = useState(title)
  const submit = async () => {
    const next = draft.trim()
    setEditing(false)
    if (!next || next === title) {
      setDraft(title)
      return
    }
    try {
      await onRename(next)
    } catch (e) {
      toast(`Couldn't rename: ${String(e)}`)
      setDraft(title)
    }
  }
  if (editing)
    return (
      <form
        onSubmit={(e) => {
          e.preventDefault()
          void submit()
        }}
      >
        <Input
          autoFocus
          aria-label={`Rename ${title}`}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={() => void submit()}
          onKeyDown={(e) => {
            if (e.key === 'Escape') {
              setDraft(title)
              setEditing(false)
            }
          }}
          className="h-8 w-[28rem] max-w-full font-heading text-lg font-medium text-heading"
        />
      </form>
    )
  return (
    <span className="group/title flex items-center gap-1">
      <h1 className="font-heading text-lg font-medium text-heading">{title}</h1>
      <Hint label="Rename">
        <Button
          variant="quiet"
          size="icon-xs"
          aria-label={`Rename ${title}`}
          className="opacity-0 transition-opacity group-hover/title:opacity-100 focus-visible:opacity-100"
          onClick={() => setEditing(true)}
        >
          <Pencil />
        </Button>
      </Hint>
    </span>
  )
}

/* the goal a session runs under: what a task or trigger set as its
   objective, with the budget it has used */
function Goal({ goal }: { goal: GoalView }) {
  const used = goal.tokenBudget ? Math.round((goal.tokensUsed / goal.tokenBudget) * 100) : null
  return (
    <div className="mt-4 rounded-2xl border border-dashed border-border px-4 py-3">
      <div className="flex items-center gap-2 text-xs text-muted-foreground">
        <Target className="size-3.5" />
        <span className="font-mono uppercase tracking-wide">Goal</span>
        <span>· {goal.status ?? 'active'}</span>
        {goal.wrapupRequested && <span>· wrapping up</span>}
        {used !== null && (
          <span className="ml-auto font-mono">
            {used}% of {Math.round(goal.tokenBudget! / 1000)}k tokens
          </span>
        )}
      </div>
      {goal.objective && <p className="mt-1 text-sm">{goal.objective}</p>}
      {goal.lastBlockedReason && (
        <p className="mt-1 text-xs text-destructive">Blocked: {goal.lastBlockedReason}</p>
      )}
    </div>
  )
}

/* the model's reasoning, folded under the answer the way the desktop does */
function Reasoning({ text }: { text: string }) {
  return (
    <details className="group mb-2 text-xs text-muted-foreground">
      <summary className="cursor-pointer select-none">Thinking</summary>
      <div className="mt-1 border-l border-border pl-3">
        <Markdown>{text}</Markdown>
      </div>
    </details>
  )
}
