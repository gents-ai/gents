/* One session: start a new one, or read and continue an existing one.
   Built from the kit's conversation patterns over the desktop app's
   session projection: the timeline items are the bridge's own
   RenderedTimelineItem, rendered as they arrive. */
import {
  Fragment,
  createContext,
  useCallback,
  memo,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type RefObject,
} from "react";
import {
  ArrowDown,
  ArrowLeft,
  ChevronDown,
  Copy,
  PanelRight,
  Play,
  Pencil,
  Split,
  Target,
  Timer,
  X,
  Zap,
} from "lucide-react";
import { toast } from "sonner";
import type {
  DeploymentView,
  DerivedCancelCauseView,
  RenderedToolCallView,
  DesktopSessionSnapshot,
  GoalView,
  RenderedTimelineItem,
} from "@source-inc/gents-desktop-client";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import type { SendStatus } from "@source-inc/gents-desktop-chat";
import { Badge } from "@gents/ui/components/badge";
import { Button } from "@gents/ui/components/button";
import { cn } from "@gents/ui/lib/utils";
import { Input } from "@gents/ui/components/input";
import {
  AssistantMessage,
  scrollParent,
  Composer,
  ToolStep,
  ToolSteps,
  UserMessage,
  type ToolStepStatus,
} from "@gents/ui/conversation";
import type { Shell } from "@/hooks/useShell";
import { anchor, scrollViewport, useFollowTail } from "@/lib/scroll";
import { useResizableWidth } from "@/lib/resizable";
import { ROOMY_WINDOW, useMediaQuery } from "@/lib/media";
import { Sheet, SheetContent, SheetTitle } from "@gents/ui/components/sheet";
import { href, navigate } from "@/lib/router";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@gents/ui/components/collapsible";
import { Hint } from "./Hint";
import { firstFailure, foldWorkers, workerStory, type ToolRun } from "./tool-runs";
import { DeploymentContext, useDeployment } from "./deployment-context";
import { ToolIcon } from "./tool-icon";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { bashAccess, behaviorName, fileAccess, network } from "./behavior";
import { AgentAvatar } from "./AgentAvatar";
import { BehaviorPicker } from "./BehaviorPicker";
import { HoldCard } from "./HoldCard";
import { LoadingStatus } from "./LoadingStatus";
import { SlashSkillMenu } from "./SlashSkillMenu";
import { useSlashSkills } from "./useSlashSkills";
import { Thinking } from "./Thinking";
import { activityStatus, isStopping } from "./activity-status";
import { TracePanel } from "./TracePanel";
import { BehaviorAvatar, BehaviorChip } from "./parts";
import { BehaviorHoverCard } from "./HoverCards";
import { CascadeDialog } from "./CascadeDialog";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@gents/ui/components/alert-dialog";
import { Markdown } from "./Markdown";
import { ToolBody } from "./tool-views";
import { WorkerStep, isWorkerStep } from "./WorkerStep";
import { useWorkers, type Workers } from "./workers";
import { useParentWork, type ParentWork } from "./parentWork";
import { WorkerActionsContext, type WorkerActions } from "./WorkerActions";
import { ArrowUpRight } from "lucide-react";
import { toolSummary } from "./tool-summary";
import { Popover, PopoverContent, PopoverTrigger } from "@gents/ui/components/popover";
import { useExclusivePopover } from "@/hooks/useExclusivePopover";

const stepStatus = (kind: string): ToolStepStatus =>
  kind === "completed" || kind === "failed" || kind === "cancelled" || kind === "error"
    ? "done"
    : kind === "running" || kind === "held"
      ? "running"
      : "pending";

function formatTokens(value: number) {
  if (value < 1_000) return String(value);
  const amount = value / 1_000;
  return `${amount >= 10 ? Math.round(amount) : amount.toFixed(1).replace(/\.0$/, "")}k`;
}

/** Add only local composer emptiness; every other blocker belongs to ClientShell. */
export function presentedComposerSendStatus(
  draft: string,
  canonicalNonEmptyStatus: SendStatus,
): SendStatus {
  return draft.trim()
    ? canonicalNonEmptyStatus
    : { kind: "disabled", reason: "composerEmpty", hint: "Type a message to send" };
}

/* how full the context is, as a stroked ring: the track is the window, the
   arc is what the conversation has used; past the compaction threshold the
   arc takes the brand color, so the number beside it need not */
function ContextRing({
  used,
  window,
  threshold,
}: {
  used: number;
  window: number;
  threshold: number;
}) {
  /* a 20px ring with a 7px radius: enough arc to read at a glance */
  const r = 7;
  const c = 2 * Math.PI * r;
  const share = Math.min(1, used / window);
  const nearCompaction = threshold > 0 && used >= threshold;
  return (
    <svg
      viewBox="0 0 20 20"
      className="size-5 shrink-0 -rotate-90"
      aria-hidden="true"
      data-testid="context-ring"
      data-share={share.toFixed(2)}
    >
      <circle
        cx="10"
        cy="10"
        r={r}
        fill="none"
        stroke="currentColor"
        strokeOpacity="0.2"
        strokeWidth="2"
      />
      <circle
        cx="10"
        cy="10"
        r={r}
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeDasharray={`${share * c} ${c}`}
        className={nearCompaction ? "text-brand" : ""}
      />
    </svg>
  );
}

function SessionContext({
  context,
  compact = false,
}: {
  context: DesktopSessionSnapshot["context"];
  /* in the compact header: the ring alone, the numbers in its tooltip and popover */
  compact?: boolean;
}) {
  const popover = useExclusivePopover();
  const used = Math.max(0, context.estimatedConversationTokens);
  const window = Math.max(
    1,
    context.lastRequest?.contextWindow ?? context.contextWindow,
  );
  const threshold = Math.max(
    0,
    context.lastRequest?.compactionThresholdTokens ?? context.compactionThresholdTokens,
  );
  const hoverTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const canHover = () => globalThis.matchMedia?.("(hover: hover)").matches ?? true;
  const hoverOpen = () => {
    if (!canHover()) return;
    if (hoverTimer.current) clearTimeout(hoverTimer.current);
    hoverTimer.current = setTimeout(() => popover.onOpenChange(true), 150);
  };
  const hoverClose = () => {
    if (!canHover()) return;
    if (hoverTimer.current) clearTimeout(hoverTimer.current);
    hoverTimer.current = setTimeout(() => popover.onOpenChange(false), 200);
  };
  return (
    <Popover
      open={popover.open}
      onOpenChange={popover.onOpenChange}
      onOpenChangeComplete={popover.onOpenChangeComplete}
    >
      {/* the details open on hover as well as click where there is a pointer;
          on touch a tap opens them. Leaving trigger and popup both closes. */}
      <PopoverTrigger
        render={
          compact ? (
            <Button
              variant="ghost"
              size="icon-xs"
              data-testid="context-meter-compact"
              aria-label={`Context ~${formatTokens(used)} of ${formatTokens(window)}`}
            />
          ) : (
            <Button
              variant="quiet"
              size="sm"
              data-testid="context-meter"
              className="gap-2"
            />
          )
        }
        onMouseEnter={hoverOpen}
        onMouseLeave={hoverClose}
      >
        <ContextRing used={used} window={window} threshold={threshold} />
        {!compact && (
          <span className="tabular-nums">
            ~{formatTokens(used)} / {formatTokens(window)}
          </span>
        )}
      </PopoverTrigger>
      <PopoverContent
        ref={popover.popupRef}
        aria-label="Session context details"
        align="start"
        className="w-80"
        data-testid="context-details"
        onMouseEnter={hoverOpen}
        onMouseLeave={hoverClose}
      >
        <div className="flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <p className="font-heading text-sm font-medium text-heading">
              Conversation context
            </p>
            <p className="mt-1 text-sm text-muted-foreground">
              {used.toLocaleString()} estimated tokens of {window.toLocaleString()}
            </p>
          </div>
          <Button
            variant="ghost"
            size="icon-xs"
            aria-label="Close context details"
            onClick={() => popover.onOpenChange(false)}
          >
            <X />
          </Button>
        </div>
        <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-5 gap-y-1.5 text-sm">
          <dt className="text-muted-foreground">Compacts at</dt>
          <dd className="text-right font-mono text-xs">{threshold.toLocaleString()}</dd>
          <dt className="text-muted-foreground">Durable transcript</dt>
          <dd className="text-right font-mono text-xs">
            {context.estimatedDurableTokens.toLocaleString()}
          </dd>
        </dl>
      </PopoverContent>
    </Popover>
  );
}

/* the trace panel's open state outlives the session; a person who works
   with the trace open keeps it open */
function useTraceOpen() {
  const [open, setOpen] = useState(() => {
    try {
      return localStorage.getItem("gents-prototype-trace") === "1";
    } catch {
      return false;
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem("gents-prototype-trace", open ? "1" : "0");
    } catch {
      /* storage unavailable */
    }
  }, [open]);
  return [open, setOpen] as const;
}

export function useBehaviorChoice(shell: Shell) {
  return {
    // Read the same effective selection that owns composer admission. Defaults,
    // mailbox routing, and agent changes are resolved by the shell, not here.
    behaviorId: shell.selectedBehaviorId,
    setPicked: shell.selectBehavior,
  };
}

/** Display the existing workflow owner's observation, never infer queue health. */
export function SessionSubmissionStatus({
  error,
  activityStatus,
}: Pick<Shell, "error" | "activityStatus">) {
  return (
    <>
      {activityStatus && !error && (
        <div
          role="status"
          title={activityStatus.detail}
          className="mt-2 px-1 text-xs text-muted-foreground"
        >
          <span>{activityStatus.label}</span>
        </div>
      )}
      {error && (
        <p role="alert" className="mt-2 px-1 text-sm text-destructive">
          {error}
        </p>
      )}
    </>
  );
}

/* an earlier request's failure, shown where it happened; the session has
   moved on to a later request, so there is nothing to retry here */

/* where this session came from: the parent that started it, from
   provenance; how the two are bound (await mode, cancel policy) waits in
   the link's title rather than the header line */
function ParentLine({ work }: { work: ParentWork }) {
  if (!work.parent) return null;
  const edge = work.edge;
  const how = [
    edge?.awaitMode === "background" ? "runs in the background" : edge?.awaitMode,
    edge?.cancelPolicy ? `${edge.cancelPolicy} on cancel` : null,
  ]
    .filter(Boolean)
    .join(" · ");
  return (
    <span className="flex min-w-0 items-center gap-1 text-xs text-muted-foreground">
      Started by
      <a
        href={href({ name: "session", sessionId: work.parent.sessionId })}
        title={how || undefined}
        className="flex min-w-0 items-center gap-0.5 truncate hover:text-foreground hover:underline"
      >
        {work.parent.title ?? "its parent"}
        <ArrowUpRight className="size-3 shrink-0" />
      </a>
    </span>
  );
}

/* A session nobody started by typing says who did. The list can filter on
   it and could not show it; the session itself said nothing at all, so a
   run that arrived overnight looked like one a person had asked for. */
function StartedByAutomation({
  summary,
  agentDid,
}: {
  summary: SessionSummary;
  agentDid: string | null;
}) {
  const how =
    summary.triggerKind === "schedule"
      ? "on a schedule"
      : summary.triggerKind === "event"
        ? "by an event"
        : summary.triggerKind
          ? `by a ${summary.triggerKind}`
          : null;
  const name = summary.taskName ?? "a task";
  return (
    <span className="flex min-w-0 items-center gap-1 text-xs text-muted-foreground">
      {/* a task is a Play and a schedule is a Timer, the way the config
          names them: the mark is the thing it points at */}
      {summary.triggerKind === "schedule" ? (
        <Timer className="size-3 shrink-0" />
      ) : summary.triggerKind === "event" ? (
        <Zap className="size-3 shrink-0" />
      ) : (
        <Play className="size-3 shrink-0" />
      )}
      Started by
      {/* the task is a thing that exists and can be changed, so the name
         goes to it: reading why a run happened and deciding it should not
         happen again are the same errand */}
      {agentDid && summary.taskId ? (
        <a
          href={href({
            name: "agent",
            agentDid,
            section: "tasks",
            item: summary.taskId,
          })}
          className="truncate hover:text-foreground hover:underline"
        >
          {name}
        </a>
      ) : (
        <span className="truncate">{name}</span>
      )}
      {how && <span className="shrink-0">· {how}</span>}
    </span>
  );
}

function copyActions(text: string | null | undefined) {
  return [
    {
      label: "Copy",
      icon: <Copy />,
      onClick: () => {
        void navigator.clipboard?.writeText(text ?? "");
        toast("Copied");
      },
    },
  ];
}

/* what the parent knows about its delegated work; memoised items read it
   from context so a lineage refresh re-renders only the worker rows */
const WorkersContext = createContext<Workers>({
  byChildRequest: () => null,
  byToolCall: () => null,
  loaded: false,
});

/* the parent this session works for, if any; a turn the parent sent is
   labeled as such above the message */
const ParentContext = createContext<ParentWork | null>(null);

/* one tool call as a row, wherever it sits: loose in the group, or among
   the calls a run folded together */
function Step({ tool, workers }: { tool: RenderedToolCallView; workers: Workers }) {
  if (isWorkerStep(tool)) return <WorkerStep tool={tool} workers={workers} />;
  const summary = toolSummary(tool);
  return (
    <ToolStep
      label={`${summary.kind} ${summary.primary}`.trim()}
      icon={<ToolIcon tool={tool} />}
      status={stepStatus(tool.statusKind)}
    >
      {tool.statusKind !== "running" && tool.statusKind !== "held" ? (
        <ToolBody tool={tool} />
      ) : undefined}
    </ToolStep>
  );
}

/* A worker's scattered steps as one row: who it was, where it got to, and
   what was done to it along the way. Its steps keep their order inside. */
function WorkerRunStep({
  tools,
  workers,
}: {
  tools: RenderedToolCallView[];
  workers: Workers;
}) {
  const deployment = useDeployment();
  const first = tools[0]!;
  const p = first.presentation;
  const name = (p.kind === "subagent" && p.name) || "a worker";
  const last = tools[tools.length - 1]!;
  const failed = tools.some(
    (t) => t.statusKind === "error" || t.statusKind === "failed",
  );
  /* the row is about one agent, so it wears that agent's mark: the same
     avatar the session list and the parent's turns use. A worker with
     no session summary has no behavior to wear, and falls back to the
     kind's glyph. */
  const child = p.kind === "subagent" && p.childRequestId;
  const behaviorId =
    (child && workers.byChildRequest(child)?.summary?.behaviorId) || null;
  return (
    <ToolStep
      label={name}
      icon={
        behaviorId ? (
          <BehaviorAvatar
            name={behaviorName(behaviorId, deployment)}
            behaviorId={behaviorId}
            className="size-4 text-[8px]"
          />
        ) : (
          <ToolIcon tool={first} />
        )
      }
      detail={workerStory(tools)}
      status={last.statusKind === "running" ? "running" : failed ? "pending" : "done"}
    >
      <ToolSteps className="-mx-2">
        {tools.map((tool) => (
          <WorkerStep key={tool.itemKey} tool={tool} workers={workers} />
        ))}
      </ToolSteps>
    </ToolStep>
  );
}

/* what the run was made of, and what went wrong in it if anything did */
function runDetail(run: Extract<ToolRun, { kind: "run" }>) {
  /* what went wrong leads, and says what it was: the reason a person opens
     a folded run is almost always the exception inside it, and a bare count
     of failures makes them go looking for it */
  const bad = firstFailure(run.tools);
  if (bad) {
    const summary = toolSummary(bad);
    const others = run.failures > 1 ? ` · ${run.failures - 1} more failed` : "";
    return `${summary.kind} ${summary.primary}`.trim() + " failed" + others;
  }
  const top = run.tally
    .slice(0, 4)
    .map((t: { label: string; count: number }) => `${t.label} ${t.count}`);
  const rest = run.tally.length - top.length;
  return [...top, rest > 0 ? `+${rest} more` : null].filter(Boolean).join(" · ");
}

const TranscriptItem = memo(function TranscriptItem({
  item,
  status = null,
}: {
  item: RenderedTimelineItem;
  status?: string | null;
}) {
  const workers = useContext(WorkersContext);
  const parentWork = useContext(ParentContext);
  switch (item.kind) {
    case "userMessage":
    case "pendingUserTurn": {
      const kind = item.content ? (parentWork?.sentBy(item.content) ?? null) : null;
      const message = (
        <UserMessage actions={copyActions(item.content)}>{item.content}</UserMessage>
      );
      if (!kind || !parentWork?.parent) return message;
      /* a turn the parent sent wears the parent's mark, the way any other
         sender would; its state, where the mark cannot say it, is a chip
         seated on the bubble's bottom edge */
      const state =
        item.kind === "pendingUserTurn"
          ? "Queued"
          : kind === "interruption"
            ? "Interrupt"
            : null;
      return (
        /* the mark hangs in the transcript's right gutter, seated on the
           first line's center: the bubble's own my-1 and py-3 put that 26px
           down, half the avatar is 12 */
        <div className={cn("relative", state && "mb-2")}>
          <BehaviorAvatar
            name={parentWork.parentBehaviorName ?? parentWork.parent.title ?? "parent"}
            behaviorId={parentWork.parent.behaviorId}
            /* in the gutter where there is one; seated on the bubble's
               top corner when the screen is too narrow to spare it */
            className="absolute -top-1 right-2 size-6 text-[10px] ring-2 ring-background sm:top-3.5 sm:-right-8 sm:ring-0"
            aria-hidden={false}
            role="img"
            aria-label={`Sent by ${parentWork.parent.title ?? "the parent"}`}
            title={`Sent by ${parentWork.parent.title ?? "the parent"}`}
          />
          <div className="relative min-w-0">
            {message}
            {state && (
              <Badge
                variant="outline"
                className="absolute right-4 bottom-0 translate-y-1/2 border-border/60 bg-background text-[10px] font-normal text-muted-foreground"
              >
                {state}
              </Badge>
            )}
          </div>
        </div>
      );
    }
    case "assistantMessage":
      return (
        /* a turn that thought and then acted leaves reasoning with nothing
           said after it: that is a think, not an empty answer, so it carries
           no action bar and no blank line where prose would be */
        <AssistantMessage actions={item.content ? copyActions(item.content) : false}>
          {item.reasoning && <Reasoning text={item.reasoning} />}
          {item.content ? <Markdown>{item.content}</Markdown> : null}
        </AssistantMessage>
      );
    case "toolGroup":
      return (
        <ToolSteps>
          {foldWorkers(item.tools).map((entry) =>
            entry.kind === "one" ? (
              <Step key={entry.tool.itemKey} tool={entry.tool} workers={workers} />
            ) : entry.kind === "worker" ? (
              <WorkerRunStep key={entry.key} tools={entry.tools} workers={workers} />
            ) : (
              <ToolStep
                key={entry.key}
                label={entry.label}
                icon={<ToolIcon tool={entry.tools[0]!} />}
                detail={runDetail(entry)}
                status={entry.failures ? "pending" : "done"}
                /* the moment a run forms, three rows become one with nothing
                   to see: it arrives with a beat so the fold is noticed */
                className="animate-in fade-in-0 slide-in-from-top-1 duration-200 motion-reduce:animate-none"
              >
                {/* its own group, so opening a step inside a run does not
                    fold the run away from under it */}
                <ToolSteps className="-mx-2">
                  {entry.tools.map((tool) => (
                    <Step key={tool.itemKey} tool={tool} workers={workers} />
                  ))}
                </ToolSteps>
              </ToolStep>
            ),
          )}
        </ToolSteps>
      );
    case "liveAssistant":
      return (
        <div data-testid="live-assistant">
          <AssistantMessage>
            {item.content && <Markdown>{item.content}</Markdown>}
            {status && <Thinking label={status} />}
          </AssistantMessage>
        </div>
      );
  }
});

const STOP_SOURCES: Record<string, string> = {
  requestInterrupt: "a stop request on this request",
  parentCascade: "a stop request on its parent",
  requestLifecycle: "the request's lifecycle state",
  deadline: "its deadline",
};

function StoppedNotice({ cause }: { cause: DerivedCancelCauseView | null }) {
  const headline =
    cause?.cause === "userCancelled"
      ? "You stopped this response."
      : cause?.cause === "deadline"
        ? "This response stopped at its deadline."
        : "This response was stopped.";
  const at = cause?.at ? new Date(cause.at) : null;
  return (
    <Collapsible
      className="px-2 text-xs text-muted-foreground"
      data-testid="stopped-notice"
    >
      <p className="flex items-center gap-2">
        {headline}
        {cause && (
          <CollapsibleTrigger className="cursor-pointer underline decoration-border underline-offset-4 hover:text-foreground">
            Details
          </CollapsibleTrigger>
        )}
      </p>
      {cause && (
        <CollapsibleContent>
          <dl className="mt-2 grid grid-cols-[max-content_minmax(0,1fr)] gap-x-3 gap-y-1 font-mono text-[11px]">
            <dt>stopped by</dt>
            <dd>{STOP_SOURCES[cause.source] ?? cause.source}</dd>
            {at && !Number.isNaN(at.getTime()) && (
              <>
                <dt>at</dt>
                <dd>{at.toLocaleTimeString()}</dd>
              </>
            )}
            {cause.evidence.map((line) => (
              <Fragment key={line}>
                <dt>evidence</dt>
                <dd className="break-all">{line}</dd>
              </Fragment>
            ))}
          </dl>
        </CollapsibleContent>
      )}
    </Collapsible>
  );
}

type TranscriptActions = Pick<Shell, "loadOlderSessionTimeline" | "retryMessage">;

export const TranscriptPanel = memo(function TranscriptPanel({
  actionsRef,
  holdsCount,
  inFlight,
  stopping = false,
  ownerRef,
  session,
  workers,
  parentWork,
  workerActions,
  deployment,
}: {
  actionsRef: RefObject<TranscriptActions>;
  holdsCount: number;
  inFlight: boolean;
  stopping?: boolean;
  ownerRef: RefObject<HTMLDivElement | null>;
  session: DesktopSessionSnapshot | null;
  workers: Workers;
  parentWork: ParentWork;
  workerActions: WorkerActions;
  deployment: DeploymentView | null;
}) {
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [retrying, setRetrying] = useState(false);
  const sessionIdRef = useRef(session?.sessionId ?? null);
  useLayoutEffect(() => {
    sessionIdRef.current = session?.sessionId ?? null;
  }, [session?.sessionId]);

  const live = session?.timelineItems.find((item) => item.kind === "liveAssistant");
  const status = inFlight
    ? activityStatus(session?.timelineItems ?? [], stopping)
    : null;
  const wasInterrupted = session?.turnState === "interrupted";
  const responseError =
    session?.turnState === "failed"
      ? "The request failed before a response was available. Check the request trace for details."
      : "";
  const showError = Boolean(responseError) && !wasInterrupted && !inFlight;

  const loadOlder = async () => {
    const viewport = scrollViewport(ownerRef.current);
    const heightBefore = viewport?.scrollHeight ?? 0;
    const sessionId = session?.sessionId ?? null;
    setLoadingOlder(true);
    try {
      if (!(await actionsRef.current.loadOlderSessionTimeline())) return;
    } finally {
      setLoadingOlder(false);
    }
    requestAnimationFrame(() => {
      if (viewport && sessionIdRef.current === sessionId) {
        viewport.scrollTop += viewport.scrollHeight - heightBefore;
      }
    });
  };

  const retry = async () => {
    const requestId = session?.latestRequestId;
    if (!requestId) return;
    setRetrying(true);
    try {
      await actionsRef.current.retryMessage(requestId);
    } catch (error) {
      toast(`Couldn't retry: ${String(error)}`);
    } finally {
      setRetrying(false);
    }
  };

  return (
    /* the condensed header floats over the scroller, so a step opening
       near the top must stop below it, not under it (h-12 plus a little) */
    <div
      /* the composer is sticky over the foot of this scroller, so the
         transcript keeps its height clear: without it the last thing in a
         session sits underneath the composer, and anything that sticks to
         the bottom lands there too */
      className="mt-6 grid grid-cols-[minmax(0,1fr)] gap-5 pb-[var(--composer-h,0px)] [--step-scroll-inset:56]"
      data-testid="transcript-panel"
    >
      {session?.timelinePage?.hasOlder && (
        <Button
          variant="ghost"
          size="sm"
          data-testid="transcript-load-older"
          className="justify-self-center text-muted-foreground"
          disabled={loadingOlder}
          onClick={loadOlder}
        >
          {loadingOlder ? "Loading older messages…" : "Load older messages"}
        </Button>
      )}
      <DeploymentContext.Provider value={deployment}>
        <WorkersContext.Provider value={workers}>
          <WorkerActionsContext.Provider value={workerActions}>
            <ParentContext.Provider value={parentWork}>
              {session?.timelineItems.map((item) => (
                <TranscriptItem
                  key={item.itemKey}
                  item={item}
                  status={item.kind === "liveAssistant" ? status : null}
                />
              ))}
            </ParentContext.Provider>
          </WorkerActionsContext.Provider>
        </WorkersContext.Provider>
      </DeploymentContext.Provider>
      {wasInterrupted && !inFlight && (
        <StoppedNotice cause={session?.latestRequestOutcome?.cancelCause ?? null} />
      )}
      {showError && (
        <div className="rounded-2xl border border-destructive/30 bg-destructive/5 px-4 py-3">
          <p className="text-sm font-medium">
            The assistant could not finish this turn.
          </p>
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
              {retrying ? "Retrying…" : "Retry"}
            </Button>
          )}
        </div>
      )}
      {status && !live && holdsCount === 0 && (
        <AssistantMessage>
          <Thinking label={status} />
        </AssistantMessage>
      )}
    </div>
  );
});

export function SessionScreen({ shell }: { shell: Shell }) {
  const session = shell.selectedSession;
  const { draft, setDraft } = shell;
  const [cascadeFor, setCascadeFor] = useState<string | null>(null);
  const [requestedStop, setRequestedStop] = useState<string | null>(null);
  const [forked, setForked] = useState<{ sessionId: string; title: string } | null>(
    null,
  );
  const [traceOpenPref, setTracePref] = useTraceOpen();
  /* the remembered state is a desktop habit; on a phone the sheet opens only by hand */
  const [mobileTrace, setMobileTrace] = useState(false);
  /* the transcript column follows new content while the reader is near
     the bottom; a reader who has scrolled up is left where they are */
  const column = useRef<HTMLDivElement>(null);
  const viewport = () => scrollViewport(column.current);
  const transcriptContentSignal = useMemo(
    () =>
      (session?.timelineItems ?? [])
        .map((item) => {
          switch (item.kind) {
            case "assistantMessage":
            case "liveAssistant":
              return `${item.itemKey}:${item.content?.length ?? 0}:${item.reasoning?.length ?? 0}`;
            case "userMessage":
              return `${item.itemKey}:${item.content?.length ?? 0}`;
            case "pendingUserTurn":
              return `${item.itemKey}:${item.content.length}`;
            case "toolGroup":
              return `${item.itemKey}:${item.tools
                .map(
                  (tool) =>
                    `${tool.itemKey}:${tool.statusKind}:${tool.partialOutputTail?.length ?? 0}`,
                )
                .join(",")}`;
          }
        })
        .join("|"),
    [session?.timelineItems],
  );
  const transcriptActions = useRef<TranscriptActions>({
    loadOlderSessionTimeline: shell.loadOlderSessionTimeline,
    retryMessage: shell.retryMessage,
  });
  useLayoutEffect(() => {
    transcriptActions.current = {
      loadOlderSessionTimeline: shell.loadOlderSessionTimeline,
      retryMessage: shell.retryMessage,
    };
  }, [shell.loadOlderSessionTimeline, shell.retryMessage]);
  /* away from the bottom, a button offers the way back; scrolling is the cue */
  const { atBottom, toBottom } = useFollowTail(
    column,
    shell.selectedSessionId,
    transcriptContentSignal,
  );
  /* once the full header scrolls out, a condensed one sticks to the top */
  const headerEnd = useRef<HTMLDivElement>(null);
  const [condensed, setCondensed] = useState(false);
  useEffect(() => {
    const el = headerEnd.current;
    const root = viewport();
    if (!el || !root) return;
    const io = new IntersectionObserver(([e]) => setCondensed(!e!.isIntersecting), {
      root,
    });
    io.observe(el);
    return () => io.disconnect();
  }, [shell.selectedSessionId]);
  const wide = useMediaQuery(ROOMY_WINDOW);
  const traceOpen = wide ? traceOpenPref : mobileTrace;
  const setTraceOpen = (open: boolean) =>
    wide ? setTracePref(open) : setMobileTrace(open);
  const trace = useResizableWidth({
    key: "gents-prototype-trace-width",
    initial: 520,
    min: 320,
    max: (container) => container * 0.6,
  });
  const choice = useBehaviorChoice(shell);
  const deployment = shell.selectedDeployment;
  const workers = useWorkers(shell);
  const parentWork = useParentWork(shell);
  /* the composer mounts with the session, not with the screen, so this
     measures from a callback ref rather than an effect that would run once
     while it was still absent. The height goes on the column, not the
     composer: a custom property inherits down, and the blocks that need to
     clear it are the composer's siblings. */
  const composerCleanup = useRef<(() => void) | null>(null);
  const composer = useCallback((el: HTMLDivElement | null) => {
    composerCleanup.current?.();
    composerCleanup.current = null;
    if (!el) return;
    /* what a block sticking to the foot needs is not the composer's height
       but how far its top sits above the scrollport's bottom edge. The two
       coincide only when the scroller ends where the window does, which is
       not true once the app is drawn inside a window frame. */
    const publish = () => {
      const scroller = scrollParent(el);
      const floor = scroller
        ? scroller.getBoundingClientRect().bottom
        : window.innerHeight;
      const gap = Math.max(0, Math.round(floor - el.getBoundingClientRect().top));
      /* the transcript pins itself to the foot when a session opens, and
         this measurement arrives after that: the room it reserves appears
         underneath a view that has already stopped, leaving it exactly a
         composer short of the end. A reader at the foot stays at the foot. */
      const was =
        scroller && scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
      column.current?.style.setProperty("--composer-h", `${gap}px`);
      if (scroller && was !== null && was < 4)
        requestAnimationFrame(() => {
          scroller.scrollTop = scroller.scrollHeight;
        });
    };
    publish();
    const size = new ResizeObserver(publish);
    size.observe(el);
    /* the frame around the app resizes without the composer changing size */
    window.addEventListener("resize", publish);
    composerCleanup.current = () => {
      size.disconnect();
      window.removeEventListener("resize", publish);
    };
  }, []);
  /* a person acting on a worker from here: cancel is the desktop's
     interrupt after the cascade preview, the same path as Stop */
  const workerActions = useMemo<WorkerActions>(
    () => ({
      parentRequestId: session?.latestRequestId ?? null,
      cancel: (requestId) => setCascadeFor(requestId),
    }),
    [session?.latestRequestId],
  );
  const agentName = deployment?.agentPrincipal.displayName ?? "the agent";
  /* the snapshot says what happened in a session; the summary says where it
     came from, which is the list's own view of it */
  const summary =
    deployment?.sessions.find((x) => x.sessionId === session?.sessionId) ?? null;

  const send = async (text: string) => {
    const pending = shell.sendMessage(text, session?.behaviorId ?? choice.behaviorId);
    const intentGeneration = shell.captureComposeIntent();
    const result = await pending;
    if (result) setDraft((current) => (current === text ? "" : current));
    if (!shell.acceptsComposeIntent(intentGeneration)) return;
    if (result && result.sessionId !== shell.selectedSessionId) {
      navigate({ name: "session", sessionId: result.sessionId });
    }
  };

  const contextFor = (behaviorId?: string | null) => {
    const b = deployment?.behaviors.find((x) => x.behaviorId === behaviorId);
    return deployment?.contexts.find((c) => c.context_id === b?.contextId);
  };
  const startSlash = useSlashSkills(
    draft,
    setDraft,
    deployment?.skills ?? [],
    contextFor(choice.behaviorId),
  );
  const slash = useSlashSkills(
    draft,
    setDraft,
    deployment?.skills ?? [],
    contextFor(session?.behaviorId),
  );

  /* ---- start a new session ---- */
  if (!shell.selectedSessionId) {
    const env = deployment?.behaviorEnvironments.find(
      (e) => e.behaviorId === choice.behaviorId,
    );
    const chosenName = behaviorName(choice.behaviorId, deployment);
    const startStatus = presentedComposerSendStatus(
      draft,
      shell.nonEmptyContentSendStatus,
    );
    return (
      <div
        key={shell.selectedAgentDid ?? "new"}
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
                ? "Answering a mailbox item; the first message starts its request"
                : "The first message creates the session automatically"}
            </p>
          </div>
        </div>
        <div data-testid="composer">
          <Composer
            value={draft}
            onChange={setDraft}
            onSend={send}
            models={[]}
            disabled={shell.nonEmptyContentSendStatus.kind === "disabled"}
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
            placeholder={
              startStatus.kind === "disabled" ? startStatus.hint : "Ask anything"
            }
          />
        </div>
        <SessionSubmissionStatus
          error={shell.error}
          activityStatus={shell.activityStatus}
        />
        <p className="text-xs text-muted-foreground">
          {chosenName} <strong className="font-medium text-foreground">can</strong>{" "}
          {env
            ? `${fileAccess(env.fileAccess)} files and ${bashAccess(env.bashAccess)} commands`
            : "…"}
          , and <strong className="font-medium text-foreground">has access</strong> to{" "}
          {network(env?.networkAccess)}.
          {deployment && choice.behaviorId && (
            <>
              {" "}
              <a
                href={href({
                  name: "agent",
                  agentDid: deployment.agentDid,
                  section: "behaviors",
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
    );
  }

  /* ---- an existing session ---- */
  const holdsHere = shell.holds.filter((h) => h.sessionId === session?.sessionId);
  const inFlight = shell.interruptVisible ?? Boolean(shell.selectedTrackedRequestId);

  /* stop: the desktop previews the cascade first; with no children it
     interrupts at once, otherwise it asks */
  const stoppableRequestId = shell.activeRequestId ?? session?.latestRequestId ?? null;
  const stopping = isStopping({
    inFlight,
    requestId: stoppableRequestId,
    latestRequestId: session?.latestRequestId ?? null,
    interruptObserved:
      session?.latestRequestOutcome?.cancelCause?.source === "requestInterrupt",
    requestedStop,
  });
  const stop = async () => {
    const requestId = stoppableRequestId;
    if (!requestId || stopping) return;
    setRequestedStop(requestId);
    const release = () =>
      setRequestedStop((current) => (current === requestId ? null : current));
    try {
      const preview = await shell.api.previewInterruptCascade({
        requestId,
        agentDid: shell.selectedAgentDid,
        includeTerminal: false,
      });
      const kids =
        preview.willInterrupt.length +
        preview.willDetach.length +
        preview.unknownPolicy.length;
      if (kids > 0) {
        release();
        return setCascadeFor(requestId);
      }
      const r = await shell.api.interruptRequest({
        requestId,
        agentDid: shell.selectedAgentDid,
        cause: "userCancelled",
        cascade: false,
        expectedPreviewSignature: null,
      });
      if (!r.accepted && !r.alreadyInterrupted) {
        release();
        toast("This response had already finished.");
      }
    } catch (e) {
      release();
      toast(`Couldn't stop: ${String(e)}`);
    }
  };

  /* Local text plus the canonical shell admission decision. */
  const status = presentedComposerSendStatus(draft, shell.nonEmptyContentSendStatus);

  /* PROTOTYPE ONLY: fork this session and open the copy */
  const fork = async () => {
    if (!session) return;
    try {
      const sessionId = await shell.forkSession(session.sessionId);
      /* the copy exists; moving to it is the person's call */
      setForked({ sessionId, title: `${session.title ?? "Session"} (fork)` });
    } catch (e) {
      toast(`Couldn't fork: ${String(e)}`);
    }
  };

  return (
    <div
      className="grid h-full min-h-0"
      data-testid="session-screen"
      style={{
        gridTemplateColumns: wide
          ? `minmax(0,1fr) ${traceOpen ? "auto" : "0px"} ${traceOpen ? trace.width : 0}px`
          : "minmax(0,1fr) 0px 0px",
        /* the columns ease when the panel opens or closes, not while it is dragged */
        transition: trace.isDragging
          ? undefined
          : "grid-template-columns 260ms cubic-bezier(0.22, 1, 0.36, 1)",
      }}
    >
      <div className="relative flex min-h-0 min-w-0 flex-col">
        {/* takes no space in the flow (negative margin), so nothing shifts when it appears */}
        <div
          className={`absolute inset-x-0 top-0 z-20 ${condensed ? "" : "pointer-events-none invisible"}`}
        >
          <div className="mx-auto flex h-12 w-full max-w-page items-center gap-3 border-b border-border/60 bg-background/95 px-6 backdrop-blur">
            <a
              href={href({ name: "sessions" })}
              aria-label="Sessions"
              className="text-muted-foreground hover:text-foreground"
            >
              <ArrowLeft className="size-4" />
            </a>
            <BehaviorHoverCard
              deployment={deployment}
              behaviorId={session?.behaviorId ?? null}
            >
              <button
                type="button"
                aria-label={`About ${behaviorName(session?.behaviorId ?? null, deployment)} behavior`}
                className="rounded-full focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2"
              >
                <BehaviorAvatar
                  name={behaviorName(session?.behaviorId ?? null, deployment)}
                  behaviorId={session?.behaviorId}
                  className="size-6 text-[10px]"
                />
              </button>
            </BehaviorHoverCard>
            <span className="min-w-0 flex-1 truncate font-heading text-sm font-medium text-heading">
              {session?.title}
            </span>
            {session?.context && <SessionContext context={session.context} compact />}
            <Hint label="Fork session">
              <Button
                variant="ghost"
                size="icon-xs"
                aria-label="Fork session"
                onClick={fork}
              >
                <Split />
              </Button>
            </Hint>
            <Hint label={traceOpen ? "Close side panel" : "Open side panel"}>
              <Button
                variant="ghost"
                size="icon-xs"
                aria-label={traceOpen ? "Close side panel" : "Open side panel"}
                className={traceOpen ? "bg-accent text-foreground" : undefined}
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
            {/* the right gutter is the parent marks' column, so it is only
                  spent where marks can appear: a session with a parent, on a
                  screen wide enough to give the width away. Elsewhere the
                  column keeps its even padding. */}
            <div
              className={cn(
                "mx-auto flex min-h-full w-full max-w-page flex-col px-6 pt-4",
                parentWork.parent && "sm:pr-14",
              )}
            >
              <a
                href={href({ name: "sessions" })}
                className="mb-4 inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
              >
                <ArrowLeft className="size-3.5" /> Sessions
              </a>
              <div className="flex items-start justify-between gap-4">
                <div>
                  {session ? (
                    <Title
                      key={session.sessionId + (session.title ?? "")}
                      title={session.title ?? "Untitled session"}
                      onRename={async (title) => {
                        await shell.api.renameSession({
                          agentDid: shell.selectedAgentDid ?? session.agentDid ?? "",
                          sessionId: session.sessionId,
                          title,
                        });
                        await shell.refreshSnapshot();
                        toast("Renamed");
                      }}
                    />
                  ) : (
                    <h1 className="font-heading text-lg font-medium text-heading">
                      {shell.sessionLoad.phase}
                    </h1>
                  )}
                  {parentWork.parent && (
                    <div className="mt-1 mb-1">
                      <ParentLine work={parentWork} />
                    </div>
                  )}
                  {summary &&
                    !parentWork.parent &&
                    (summary.taskId || summary.triggerId) && (
                      <div className="mt-1 mb-1">
                        <StartedByAutomation
                          summary={summary}
                          agentDid={deployment?.agentDid ?? null}
                        />
                      </div>
                    )}
                  <div className="mt-3 flex flex-wrap items-center gap-2">
                    <BehaviorChip
                      behaviorId={session?.behaviorId ?? null}
                      deployment={deployment}
                    />
                    {session?.context && <SessionContext context={session.context} />}
                  </div>
                </div>
                <div className="flex gap-1">
                  <Hint label="Fork session">
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label="Fork session"
                      onClick={fork}
                    >
                      <Split />
                    </Button>
                  </Hint>
                  <Hint label={traceOpen ? "Close side panel" : "Open side panel"}>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={traceOpen ? "Close side panel" : "Open side panel"}
                      aria-pressed={traceOpen}
                      className={traceOpen ? "bg-accent text-foreground" : undefined}
                      onClick={() => setTraceOpen(!traceOpen)}
                    >
                      <PanelRight />
                    </Button>
                  </Hint>
                </div>
              </div>

              {session?.goal && <Goal goal={session.goal} />}
              <div ref={headerEnd} aria-hidden="true" />
              <TranscriptPanel
                deployment={deployment}
                actionsRef={transcriptActions}
                holdsCount={holdsHere.length}
                inFlight={inFlight}
                stopping={stopping}
                ownerRef={column}
                session={session}
                workers={workers}
                parentWork={parentWork}
                workerActions={workerActions}
              />
              {/* its height is published as --composer-h, so a block that
                  sticks to the foot of the scroller can sit clear of it
                  rather than hard-coding what the composer happens to be */}
              {/* the composer owns the foot of the scroller: anything else
                  that sticks there passes behind it rather than over it,
                  however short the block it belongs to turns out to be */}
              <div
                ref={composer}
                className="sticky bottom-0 z-20 mt-auto bg-background pt-6 pb-6"
              >
                {/* conversation still running on under the composer: a short
                    fade on its top edge says the transcript has not ended,
                    where a hard edge reads as the end of it */}
                <div
                  aria-hidden
                  className={cn(
                    "pointer-events-none absolute inset-x-0 bottom-full h-8 bg-gradient-to-t from-background to-transparent transition-opacity duration-200",
                    atBottom ? "opacity-0" : "opacity-100",
                  )}
                />
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
                      detail={
                        <code className="font-mono text-xs">{holdsHere[0].args}</code>
                      }
                      onApprove={() =>
                        shell.resolveHold(holdsHere[0]!.toolCallId, true)
                      }
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
                    disabled={
                      shell.nonEmptyContentSendStatus.kind === "disabled" && !inFlight
                    }
                    above={
                      <SlashSkillMenu
                        items={slash.items}
                        active={slash.active}
                        onPick={slash.accept}
                      />
                    }
                    onKeyDown={slash.onKeyDown}
                    sending={shell.sending || inFlight}
                    onStop={inFlight && !stopping ? stop : undefined}
                    placeholder={
                      status.kind === "disabled" && !inFlight
                        ? status.hint
                        : "Ask anything"
                    }
                  />
                </div>
                {status.kind === "disabled" &&
                  !shell.activityStatus &&
                  !shell.error &&
                  !inFlight && (
                    <p className="mt-2 px-1 text-xs text-muted-foreground">
                      {status.hint}
                    </p>
                  )}
                <SessionSubmissionStatus
                  error={shell.error}
                  activityStatus={shell.activityStatus}
                />
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
        /* a closed panel's handle takes no width, or it overflows its 0px track */
        className={cn(
          "group flex cursor-col-resize items-center justify-center overflow-hidden outline-none focus-visible:bg-accent",
          traceOpen && wide ? "w-3" : "w-0",
        )}
      >
        <div className="h-10 w-0.5 rounded-full bg-border transition-colors group-hover:bg-muted-foreground group-focus-visible:bg-ring" />
      </div>
      {/* the panel keeps its width inside a clipping cell, so it slides rather than squashes */}
      <div
        className="min-h-0 min-w-0 overflow-hidden py-4"
        aria-hidden={!traceOpen || !wide}
      >
        <div
          className="h-full pr-4 transition-transform duration-[260ms] ease-[cubic-bezier(0.22,1,0.36,1)]"
          style={{
            width: trace.width,
            transform: traceOpen ? "translateX(0)" : "translateX(24px)",
          }}
        >
          {wide && <TracePanel shell={shell} onClose={() => setTraceOpen(false)} />}
        </div>
      </div>
      {/* in a narrow window the panel is a sheet over the transcript */}
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
        <AlertDialogContent aria-modal="true">
          <AlertDialogHeader>
            <AlertDialogTitle>Forked</AlertDialogTitle>
            <AlertDialogDescription>
              A copy of this transcript is now its own session, "{forked?.title}". This
              one stays as it is. Open the fork, or stay here and find it later in the
              sessions list.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Stay here</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                const target = forked;
                setForked(null);
                if (target) navigate({ name: "session", sessionId: target.sessionId });
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
  );
}

/* the session's title, renamed in place the way the desktop's chat
   header does: a pencil beside it, Enter or blur saves, Escape reverts */
function Title({
  title,
  onRename,
}: {
  title: string;
  onRename: (title: string) => Promise<void>;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(title);
  const submit = async () => {
    const next = draft.trim();
    setEditing(false);
    if (!next || next === title) {
      setDraft(title);
      return;
    }
    try {
      await onRename(next);
    } catch (e) {
      toast(`Couldn't rename: ${String(e)}`);
      setDraft(title);
    }
  };
  if (editing)
    return (
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <Input
          autoFocus
          aria-label={`Rename ${title}`}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={() => void submit()}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              setDraft(title);
              setEditing(false);
            }
          }}
          className="h-8 w-[28rem] max-w-full font-heading text-lg font-medium text-heading"
        />
      </form>
    );
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
  );
}

/* the goal a session runs under: what a task or trigger set as its
   objective, with the budget it has used */
function Goal({ goal }: { goal: GoalView }) {
  const used = goal.tokenBudget
    ? Math.round((goal.tokensUsed / goal.tokenBudget) * 100)
    : null;
  return (
    <div className="mt-4 rounded-2xl border border-dashed border-border px-4 py-3">
      <div className="flex items-center gap-2 text-xs text-muted-foreground">
        <Target className="size-3.5" />
        <span className="font-mono uppercase tracking-wide">Goal</span>
        <span>· {goal.status ?? "active"}</span>
        {goal.wrapupRequested && <span>· wrapping up</span>}
        {used !== null && (
          <span className="ml-auto font-mono">
            {used}% of {Math.round(goal.tokenBudget! / 1000)}k tokens
          </span>
        )}
      </div>
      {goal.objective && <p className="mt-1 text-sm">{goal.objective}</p>}
      {goal.lastBlockedReason && (
        <p className="mt-1 text-xs text-destructive">
          Blocked: {goal.lastBlockedReason}
        </p>
      )}
    </div>
  );
}

/* The model's reasoning, folded under the answer the way the desktop does.
   It runs to thousands of words, so opening it shows a screenful and says
   how much more there is. A tool's contents scroll inside their frame,
   because they are reference to dip into; reasoning is prose read from the
   top, and a scroller inside a transcript traps the wheel and stops a
   reader scanning past it. It stays quieter than the answer it explains. */
function Reasoning({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  const [all, setAll] = useState(false);
  /* whether there is anything behind the fade: a short reasoning needs no
     way to expand it, and offering one says there is more to read */
  const [clipped, setClipped] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const body = useRef<HTMLDivElement>(null);
  const words = text.trim().split(/\s+/).length;
  useEffect(() => {
    const el = body.current;
    if (!open || !el) return;
    const measure = () => setClipped(el.scrollHeight > el.clientHeight + 1);
    measure();
    /* prose reflows as fonts land and the column resizes */
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, [open, all, text]);
  /* folding thousands of words away moves everything under them; hold the
     block where the reader left it rather than dropping them elsewhere */
  const fold = (next: () => void) => {
    const restore = anchor(root.current);
    next();
    restore();
  };
  return (
    <Collapsible
      ref={root}
      open={open}
      onOpenChange={(next) => (next ? setOpen(true) : fold(() => setOpen(false)))}
      className="mb-2"
    >
      {/* the controls at the foot are the way out, so the trigger stays put:
          two sticky things for one block meet in the middle as soon as the
          block is short, and then neither is where it was reached for */}
      <CollapsibleTrigger className="-mx-2 -my-1 flex cursor-pointer items-center gap-1.5 rounded-lg px-2 py-1 text-xs text-muted-foreground transition-colors hover:bg-accent/30 hover:text-foreground">
        <ChevronDown
          className={cn("size-3.5 transition-transform", open && "rotate-180")}
        />
        Thinking
        <span className="tabular-nums opacity-70">
          · {words.toLocaleString()} words
        </span>
      </CollapsibleTrigger>
      <CollapsibleContent className="mt-1 border-l border-border pl-3">
        <div
          ref={body}
          className={cn(
            "relative text-muted-foreground",
            !all && "max-h-80 overflow-hidden",
          )}
        >
          <Markdown>{text}</Markdown>
          {!all && clipped && (
            <div className="pointer-events-none absolute inset-x-0 bottom-0 h-16 bg-gradient-to-t from-background to-transparent" />
          )}
        </div>
        {/* the way out sits where the reading ends, not back up at the top,
            and it is there whether or not the rest was ever unfolded; a
            reasoning that fits on a screen needs neither */}
        {(all || clipped) && (
          /* at the foot of the view while the block is in it, clear of the
             composer, so a long reasoning can be left without reading to
             the end of it */
          <div
            className={cn(
              "mt-1 flex w-fit items-center gap-1 rounded-lg p-0.5",
              /* only a block taller than the view has anywhere to stick:
                 clipped, it is a screenful and its foot is already in
                 reach, and sticky inside a short box just parks the
                 controls at its end, which may be under the composer */
              /* the composer floats over the foot of the scroller, and
                 padding the transcript does not move a sticky element:
                 it pins against the scrollport, so the offset is still
                 the room the composer leaves */
              all &&
                "sticky bottom-[calc(var(--composer-h,0px)+0.75rem)] z-10 border border-border bg-raised/95 shadow-sm backdrop-blur",
            )}
          >
            {all ? (
              <Button
                variant="quiet"
                size="xs"
                onClick={() => fold(() => setAll(false))}
              >
                Show less
              </Button>
            ) : (
              <Button variant="quiet" size="xs" onClick={() => setAll(true)}>
                Show all {words.toLocaleString()} words
              </Button>
            )}
            <Button
              variant="quiet"
              size="xs"
              onClick={() => fold(() => setOpen(false))}
            >
              Close
            </Button>
          </div>
        )}
      </CollapsibleContent>
    </Collapsible>
  );
}
