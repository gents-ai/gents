import { useContext, useRef, useState, type ReactNode } from "react";
import {
  ArrowUpRight,
  Bot,
  ChevronDown,
  Clock,
  FileText,
  Play,
  SquareTerminal,
  TextSelect,
  Timer,
  TriangleAlert,
  Workflow,
  Zap,
} from "lucide-react";
import type {
  CapturedContext,
  RenderedTimelineItem,
  RequestLifecycleState,
  RequestOriginView,
  RenderedToolCallView,
  SessionSummary,
} from "@source-inc/gents-desktop-client";
import {
  listedSession,
  nodeOf,
  type FleetState,
  type NodeView,
} from "../../hooks/fleetStore";
import { useSelectedNode } from "@/hooks/useClient";
import { useFleet } from "../hooks/useFleet";

type Reconstruction = Extract<
  RenderedTimelineItem,
  { kind: "automatedInput" }
>["reconstruction"];
import { Badge } from "@gents/ui/components/badge";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@gents/ui/components/collapsible";
import { UserMessage } from "@gents/ui/conversation";
import { cn } from "@gents/ui/lib/utils";
import { href } from "@/lib/router";
import { holdRow } from "@/lib/scroll";
import { GroupStateContext } from "./ActivityGroup";
import { agentName } from "./behavior";
import { Markdown } from "./Markdown";
import { AgentInitials } from "./parts";
import { ToolIcon } from "./tool-icon";
import { spoken, toolSummary } from "./tool-summary";
import { isWorkerStep } from "./WorkerStep";
import { NO_WORKERS, type Reached, type Workers } from "./workers";
import { completionOf, type BackgroundResult } from "./transcript-groups";

export function pendingInputState(
  lifecycleState: RequestLifecycleState | null | undefined,
  foldedIntoRequestId?: string | null,
): string | null {
  if (foldedIntoRequestId || !lifecycleState) return null;
  switch (lifecycleState) {
    case "interrupted":
      return "Not sent · interrupted";
    case "failed":
      return "Not sent · failed";
    case "dead":
      return "Not sent · expired";
    case "workspaceBindingPending":
    case "pending":
    case "claimed":
    case "processing":
    case "completed":
    case "superseded":
      return null;
  }
}

function triggerName(triggerId: string, node: NodeView | null) {
  const trigger = node?.triggers?.find((t) => t.config.trigger_id === triggerId);
  const task = node?.tasks?.find((t) => t.taskId === trigger?.config.task_id);
  return trigger?.config.display_name ?? task?.name ?? triggerId;
}

type SessionMessageOrigin = Extract<RequestOriginView, { kind: "sessionMessage" }>;

const ROUTING_HEADER =
  /^\[message agent:(\S+) session:(\S+) node:(\S+) · at:(\S+) · cause:(\S+)\]$/;

/* the runtime shortens a long node DID in the routing line */
const shortDid = (did: string) =>
  did.length > 20 ? `${did.slice(0, 16)}..${did.slice(-4)}` : did;

/** The sender's session as the fleet lists it. A routing line is the
    sender's own text, so without a sender session on the origin it only
    names a session the fleet lists under the authenticated sender node,
    and only when its node and cause agree with the origin. */
export function senderSession(
  fleet: FleetState,
  origin: SessionMessageOrigin,
  content: string | null | undefined,
): SessionSummary | null {
  const node = origin.senderNodeDid;
  if (origin.senderSessionId)
    return (
      listedSession(fleet, node, origin.senderSessionId) ??
      fleet.bySessionId[origin.senderSessionId] ??
      null
    );
  const header = ROUTING_HEADER.exec(content?.split("\n", 1)[0]?.trimEnd() ?? "");
  if (!header || !node || header[3] !== shortDid(node)) return null;
  if (origin.senderRequestId && header[5] !== origin.senderRequestId) return null;
  return listedSession(fleet, node, header[2]);
}

function senderName(
  origin: SessionMessageOrigin,
  node: NodeView | null,
  senderTitle: string | null,
  sessionId = origin.senderSessionId,
): string {
  if (senderTitle) return senderTitle;
  if (sessionId || origin.senderNodeDid === node?.nodeDid) return "another session";
  return "another node";
}

export function originLabel(
  origin: RequestOriginView,
  node: NodeView | null,
  senderTitle: string | null,
): string {
  switch (origin.kind) {
    case "sessionMessage":
      return `From ${senderName(origin, node, senderTitle)}`;
    case "trigger": {
      const name = triggerName(origin.triggerId, node);
      return origin.triggerKind === "schedule"
        ? `Schedule · ${name}`
        : origin.triggerKind === "event"
          ? `Event · ${name}`
          : `Task run · ${name}`;
    }
    case "goalContinuation":
      return origin.sequence != null
        ? `Goal continuation · ${origin.sequence}`
        : "Goal continuation";
    case "backgroundCompletion":
      return "Background result";
  }
}

/** inputs with nothing for a person to read: one line, their model input
    behind it */
export const oneLine = (origin: RequestOriginView) =>
  origin.kind === "backgroundCompletion" || origin.kind === "goalContinuation";

/** a message's text without the routing line the runtime writes above it
    for the model */
export function withoutRoutingHeader(content: string): string {
  const end = content.indexOf("\n");
  const first = (end === -1 ? content : content.slice(0, end)).trimEnd();
  if (!first.startsWith("[message ") || !first.endsWith("]")) return content;
  return end === -1 ? "" : content.slice(end + 1).replace(/^(?:[ \t]*\r?\n)+/, "");
}

const OUTCOMES: Record<string, string> = {
  failed: "failed",
  error: "failed",
  cancelled: "cancelled",
  canceled: "cancelled",
  timed_out: "timed out",
};

type BackgroundName = {
  subject: string;
  outcome: string;
  /** the session a worker step reached, named as that step names it */
  worker?: { sessionId: string | null; agentId: string | null };
};

/** the call a background result reports on, by what the step that started
    it shows when that step is drawn, and how it ended */
export function backgroundName(
  content: string | null | undefined,
  call: RenderedToolCallView | null,
  reached: Reached | null = null,
): BackgroundName | null {
  const completion = completionOf(content);
  if (!completion) return null;
  const outcome = OUTCOMES[completion.status] ?? "finished";
  const p = call?.presentation;
  if (call && p?.kind === "agent" && isWorkerStep(call))
    return {
      subject: reached?.summary?.title ?? p.name ?? "a session",
      outcome,
      worker: {
        sessionId: reached?.request.sessionId ?? p.sessionId ?? null,
        agentId: reached?.summary?.agentId ?? null,
      },
    };
  const subject = !call
    ? completion.toolName
    : p?.kind === "command"
      ? spoken(p.command)
      : toolSummary(call).primary;
  return { subject: subject || "call", outcome };
}

export const backgroundText = (named: BackgroundName | null) =>
  named ? `${named.subject} ${named.outcome}` : "Background result";

/** the line a run of background results folds into */
export function backgroundRunLabel(
  results: readonly BackgroundResult[],
  workers: Workers = NO_WORKERS,
): string {
  const named = results.flatMap(
    (r) =>
      backgroundName(r.item.content, r.call, r.call && workers.byToolCall(r.call)) ??
      [],
  );
  const count = named.length || results.length;
  return count === 1 ? backgroundText(named[0] ?? null) : `${count} background results`;
}

function OriginIcon({ origin }: { origin: RequestOriginView }) {
  const className = "size-3.5 shrink-0";
  switch (origin.kind) {
    case "sessionMessage":
      return <Bot className={className} />;
    case "trigger":
      return origin.triggerKind === "schedule" ? (
        <Timer className={className} />
      ) : origin.triggerKind === "event" ? (
        <Zap className={className} />
      ) : (
        <Play className={className} />
      );
    case "goalContinuation":
      return <Workflow className={className} />;
    case "backgroundCompletion":
      return <Clock className={className} />;
  }
}

const fileName = (uri: string) => {
  const last = uri.split(/[\\/]/).filter(Boolean).pop() ?? uri;
  try {
    return decodeURIComponent(last);
  } catch {
    return last;
  }
};

/* positions are zero-based; people count lines from one */
const lines = ({ range }: CapturedContext) => {
  if (!range) return "";
  const [from, to] = [range.start.line + 1, range.end.line + 1];
  return from === to ? `:${from}` : `:${from}–${to}`;
};

const CONTEXT_ICON = {
  selection: TextSelect,
  file: FileText,
  diagnostics: TriangleAlert,
  terminal: SquareTerminal,
} as const;

/** The context an input captured, such as an editor selection or a
    terminal's output: what each is, with its text on hover. */
export function SentContext({
  items,
}: {
  items: readonly CapturedContext[] | null | undefined;
}) {
  if (!items?.length) return null;
  return (
    <span
      className="mt-1.5 flex flex-wrap gap-1.5 first:mt-0"
      data-testid="sent-context"
    >
      {items.map((item, index) => {
        const Icon = CONTEXT_ICON[item.kind];
        const label =
          item.kind === "terminal"
            ? (item.terminalName ?? "Terminal")
            : item.uri
              ? `${fileName(item.uri)}${lines(item)}`
              : item.kind;
        return (
          <span
            key={index}
            title={[item.uri ?? item.terminalName, item.content.slice(0, 400)]
              .filter(Boolean)
              .join("\n\n")}
            className="inline-flex max-w-full items-center gap-1 rounded-md border border-border/60 px-1.5 py-0.5 text-xs text-muted-foreground"
          >
            <Icon className="size-3.5 shrink-0" />
            <span className="min-w-0 truncate">{label}</span>
          </span>
        );
      })}
    </span>
  );
}

/** why an input's text can't be shown, when its reconstruction isn't ready */
export function unavailableReason(reconstruction: Reconstruction | undefined) {
  switch (reconstruction?.state) {
    case "loading":
      return "Still syncing this input.";
    case "denied":
      return "This input is not shared with this device.";
    case "invalid":
      return "This input could not be read.";
    default:
      return null;
  }
}

/* kept with the transcript's row state when there is one, so a row the
   list stops drawing comes back as the reader left it */
function useKeptOpen(key: string | null): [boolean, (open: boolean) => void] {
  const rows = useContext(GroupStateContext);
  const [local, setLocal] = useState(false);
  if (!rows || !key) return [local, setLocal];
  return [rows.folded[key] === false, (open) => rows.setFolded(key, !open)];
}

const LINE =
  "flex min-w-0 items-center gap-3 px-2 py-1 text-left text-sm text-muted-foreground";

function Chevron({ open }: { open: boolean }) {
  return (
    <ChevronDown
      aria-hidden
      className={cn(
        "size-3.5 transition-transform duration-150 motion-reduce:transition-none",
        !open && "-rotate-90",
      )}
    />
  );
}

function Title({ icon, children }: { icon: ReactNode; children: ReactNode }) {
  return (
    <span className="flex min-w-0 flex-1 items-center gap-1.5">
      {icon}
      <span className="min-w-0 truncate">{children}</span>
    </span>
  );
}

function SessionLink({ sessionId, name }: { sessionId: string; name: string }) {
  return (
    <a
      href={href({ name: "session", sessionId })}
      className="underline decoration-border underline-offset-4 hover:text-foreground"
    >
      {name}
      <ArrowUpRight aria-hidden className="ml-0.5 inline size-3 align-[-1px]" />
    </a>
  );
}

function BackgroundTitle({ named }: { named: BackgroundName | null }) {
  if (!named) return <>Background result</>;
  const sessionId = named.worker?.sessionId;
  return (
    <>
      {sessionId ? (
        <SessionLink sessionId={sessionId} name={named.subject} />
      ) : named.worker ? (
        named.subject
      ) : (
        <span className="font-mono text-xs">{named.subject}</span>
      )}{" "}
      {named.outcome}
    </>
  );
}

function ModelInput({ content }: { content: string | null | undefined }) {
  return (
    <CollapsibleContent className="mt-1 ml-4 border-l border-border py-1 pr-2 pl-4">
      <pre
        className="max-h-96 overflow-auto font-mono text-xs leading-relaxed break-words whitespace-pre-wrap text-muted-foreground"
        data-testid="automated-input-content"
      >
        {content ?? ""}
      </pre>
    </CollapsibleContent>
  );
}

const avatar = (name: string) => (
  <AgentInitials name={name} className="size-4 text-[8px]" />
);

export function AutomatedInput({
  origin,
  content,
  reconstruction,
  state = null,
  stateKey = null,
  call = null,
  reached = null,
}: {
  origin: RequestOriginView;
  content: string | null | undefined;
  reconstruction?: Reconstruction;
  state?: string | null;
  /** the row its open model input is kept under */
  stateKey?: string | null;
  /** the background call a result reports on, when drawn */
  call?: RenderedToolCallView | null;
  /** the session that call reached, for a worker step */
  reached?: Reached | null;
}) {
  const unavailable = unavailableReason(reconstruction);
  const node = useSelectedNode();
  const sender = useFleet((fleet) =>
    origin.kind === "sessionMessage" ? senderSession(fleet, origin, content) : null,
  );
  const senderNode = useFleet((fleet) => nodeOf(fleet, sender?.nodeDid));
  const [open, setOpen] = useKeptOpen(stateKey && `input:${stateKey}`);
  const root = useRef<HTMLElement>(null);
  const toggle = (next: boolean) => {
    holdRow(root.current);
    setOpen(next);
  };
  const named =
    origin.kind === "backgroundCompletion"
      ? backgroundName(content, call, reached)
      : null;
  let label: string;
  let title: ReactNode;
  let icon: ReactNode;
  if (origin.kind === "sessionMessage") {
    const sessionId = origin.senderSessionId ?? sender?.sessionId ?? null;
    const name = senderName(origin, node, sender?.title ?? null, sessionId);
    label = `From ${name}`;
    title = sessionId ? (
      <>
        From <SessionLink sessionId={sessionId} name={name} />
      </>
    ) : (
      label
    );
    icon = sender ? (
      avatar(agentName(sender.agentId, senderNode ?? node))
    ) : (
      <OriginIcon origin={origin} />
    );
  } else if (origin.kind === "backgroundCompletion") {
    label = backgroundText(named);
    title = <BackgroundTitle named={named} />;
    const agentId = named?.worker?.agentId;
    icon = agentId ? (
      avatar(agentName(agentId, node))
    ) : call ? (
      <ToolIcon tool={call} className="size-3.5 shrink-0" />
    ) : (
      <OriginIcon origin={origin} />
    );
  } else {
    label = originLabel(origin, node, null);
    title = label;
    icon = <OriginIcon origin={origin} />;
  }
  const body = oneLine(origin)
    ? null
    : origin.kind === "sessionMessage"
      ? (origin.displayBody ?? withoutRoutingHeader(content ?? ""))
      : content;
  const badge = state && (
    <Badge variant="outline" className="shrink-0 font-normal">
      {state}
    </Badge>
  );
  const action = `${open ? "Hide" : "Show"} model input`;
  return (
    <section
      ref={root}
      data-testid="automated-input"
      aria-label={label}
      className="min-w-0"
    >
      <Collapsible open={open && !unavailable} onOpenChange={toggle}>
        {unavailable ? (
          <div className={LINE}>
            <span className="size-4 shrink-0" />
            <Title icon={icon}>{title}</Title>
            {badge}
          </div>
        ) : (
          /* the chevron is the control; the rest of the line is a larger
             target for a pointer, except its links */
          <div
            className={cn(LINE, "cursor-pointer hover:text-foreground")}
            onClick={(event) => {
              if ((event.target as Element).closest("a, button")) return;
              toggle(!open);
            }}
          >
            <CollapsibleTrigger
              aria-label={oneLine(origin) ? `${action}: ${label}` : action}
              className="grid size-4 shrink-0 cursor-pointer place-items-center rounded-sm hover:text-foreground"
            >
              <Chevron open={open} />
            </CollapsibleTrigger>
            <Title icon={icon}>{title}</Title>
            {badge}
          </div>
        )}
        {unavailable ? (
          <p
            className="pr-2 pl-9 text-sm text-muted-foreground"
            data-testid="automated-input-unavailable"
          >
            {unavailable}
          </p>
        ) : (
          <>
            {body && (
              <div
                className="prose-app pr-2 pl-9 break-words"
                data-testid="automated-input-body"
              >
                <Markdown>{body}</Markdown>
              </div>
            )}
            <ModelInput content={content} />
          </>
        )}
      </Collapsible>
    </section>
  );
}

/** Background results that arrived with nothing between them: one line,
    opening to each with its own model input. */
export function BackgroundResults({
  results,
  workers = NO_WORKERS,
}: {
  results: BackgroundResult[];
  workers?: Workers;
}) {
  const first = results[0]!;
  const [open, setOpen] = useKeptOpen(`background:${first.item.itemKey}`);
  const root = useRef<HTMLElement>(null);
  const row = ({ item, call }: BackgroundResult) => (
    <AutomatedInput
      key={item.itemKey}
      origin={item.origin}
      content={item.content}
      reconstruction={item.reconstruction}
      stateKey={item.itemKey}
      call={call}
      reached={call && workers.byToolCall(call)}
    />
  );
  if (results.length === 1) return row(first);
  const label = backgroundRunLabel(results, workers);
  return (
    <section
      ref={root}
      data-testid="background-results"
      aria-label={label}
      className="min-w-0"
    >
      <Collapsible
        open={open}
        onOpenChange={(next) => {
          holdRow(root.current);
          setOpen(next);
        }}
      >
        <CollapsibleTrigger
          className={cn(LINE, "w-full cursor-pointer hover:text-foreground")}
        >
          <span className="grid size-4 shrink-0 place-items-center">
            <Chevron open={open} />
          </span>
          <Title icon={<Clock className="size-3.5 shrink-0" />}>{label}</Title>
        </CollapsibleTrigger>
        <CollapsibleContent className="ml-4 border-l border-border pl-1">
          {results.map(row)}
        </CollapsibleContent>
      </Collapsible>
    </section>
  );
}

export function UserInputWithState({
  content,
  state,
  actions,
  meta,
}: {
  content: ReactNode;
  state: string | null;
  actions?: Parameters<typeof UserMessage>[0]["actions"];
  meta?: ReactNode;
}) {
  if (!state)
    return (
      <UserMessage actions={actions} meta={meta}>
        {content}
      </UserMessage>
    );
  return (
    <div
      className={cn(
        "relative mb-2",
        (state === "Queued" || state === "Pending") && "opacity-70",
      )}
    >
      <UserMessage actions={actions} meta={meta}>
        {content}
      </UserMessage>
      <Badge
        variant="outline"
        title={state === "Pending" ? "Waiting for the next safe step" : undefined}
        className={cn(
          "absolute right-4 translate-y-1/2 border-border/60 bg-background text-[10px] font-normal text-muted-foreground",
          /* on the bubble's lower edge, above the row of actions under it */
          actions || meta != null ? "bottom-8" : "bottom-0",
        )}
      >
        {state}
      </Badge>
    </div>
  );
}
