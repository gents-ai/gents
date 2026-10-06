/* A call that started or messaged another session — a subagent — or ran a
   background process, as a row in the transcript's activity block: what the
   agent asked, the receipt, and where that work is now. Replaces the plain
   tool step for those calls. The words come from the contract's own
   states. */
import { type ReactNode } from "react";
import { ArrowUpRight, Ban, CircleCheck, CircleX } from "lucide-react";
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import { Spinner } from "@gents/ui/components/spinner";
import { BehaviorAvatar } from "./parts";
import { behaviorName } from "./behavior";
import { useExclusiveStep } from "@gents/ui/conversation";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@gents/ui/components/collapsible";
import { cn } from "@gents/ui/lib/utils";
import { href } from "@/lib/router";
import { useSelectedNode } from "@/hooks/useClient";
import { ToolBody } from "./tool-views";
import { duration } from "./tool-summary";
import { useMinute, when } from "./time";
import { isLive } from "@/lib/live";
import { scopeKey, type Reached, type Subagent, type Workers } from "./workers";
import { RequestStop } from "./WorkerActions";

type Tone = "running" | "done" | "failed" | "stopped" | "unknown";

const firstLine = (s: string | null | undefined) => s?.trim().split("\n")[0] ?? null;
const since = (iso: string | null | undefined) =>
  iso ? duration(Math.max(0, Date.now() - Date.parse(iso))) : null;

/* where this row's call got to: the lifecycle of the one request it caused.
   Other requests in the same session are other rows' business. Nothing here
   guesses at replication: a request the lineage has not delivered yet reads
   as the call's own status says. */
const WAITING = new Set(["waitingforclaim", "pending", "claimed"]);

export function workerNow(
  tool: RenderedToolCallView,
  reached: Reached | null,
  now: number,
): { tone: Tone; text: string; detail?: string | null } {
  const failure = firstLine(
    tool.presentation.kind === "subagent" ? tool.presentation.output : null,
  );
  const state = reached?.request.lifecycleState ?? null;
  if (!state) {
    /* no lineage fact: only the call speaks, and its status
       (tool_status_kind: success, error, running, unknown) is about the
       call, so it never makes a request look live on its own */
    switch (tool.statusKind) {
      case "running":
        return { tone: "running", text: "starting" };
      case "success":
        return { tone: "done", text: "sent" };
      case "error":
        return { tone: "failed", text: "failed", detail: failure };
      default:
        return { tone: "unknown", text: "state unknown" };
    }
  }
  if (WAITING.has(state.toLowerCase()))
    return { tone: "running", text: "waiting for the agent to pick it up" };
  if (isLive(state)) {
    const s = when(reached?.summary?.updatedAt ?? null, now);
    return {
      tone: "running",
      text: s && s !== "now" ? `working · last change ${s}` : "working",
    };
  }
  switch (state) {
    case "completed":
      return { tone: "done", text: "finished" };
    case "failed":
    case "error":
    case "dead":
      return { tone: "failed", text: "failed", detail: failure };
    case "cancelled":
    case "interrupted":
    case "superseded":
      return { tone: "stopped", text: state };
    default:
      return { tone: "unknown", text: state };
  }
}

function Icon({ tone }: { tone: Tone }) {
  return (
    <span className="grid size-4 shrink-0 place-items-center">
      {tone === "running" ? (
        <Spinner className="text-foreground" />
      ) : tone === "done" ? (
        <CircleCheck className="size-4 text-muted-foreground" />
      ) : tone === "failed" ? (
        <CircleX className="size-4 text-destructive" />
      ) : tone === "stopped" ? (
        <Ban className="size-4 text-muted-foreground" />
      ) : (
        <span className="size-1.5 rounded-full bg-border" />
      )}
    </span>
  );
}

function Row({
  tone,
  verb,
  name,
  state,
  detail,
  sessionId,
  mark,
  menu,
  children,
}: {
  tone: Tone;
  /* a worker's own mark, where the parent knows whose work it is */
  mark?: ReactNode;
  verb: string;
  name: string;
  state: string;
  detail?: ReactNode;
  sessionId: string | null;
  /* the row's actions, on the rows that are a worker */
  menu?: ReactNode;
  children?: ReactNode;
}) {
  const { open, setOpen, ref } = useExclusiveStep(undefined, false);
  const head = (
    <span className="flex min-w-0 flex-1 items-center gap-3 text-sm">
      {mark ?? <Icon tone={tone} />}
      <span className="min-w-0 truncate">
        <span className="text-muted-foreground">{verb} </span>
        <span className={cn(tone === "running" && "font-medium")}>{name}</span>
      </span>
      {/* beside the name when there is room; under it on a phone */}
      <span
        className={cn(
          "min-w-0 max-w-[55%] shrink truncate text-xs max-sm:hidden",
          tone === "failed" ? "text-destructive" : "text-muted-foreground",
        )}
      >
        {state}
      </span>
    </span>
  );
  return (
    /* the hover surface is the whole step, expanded detail included, and
       faint enough to read as a hit area rather than a card: -mx-2 px-2
       lets it reach past the text column */
    <Collapsible
      ref={ref}
      open={open}
      onOpenChange={setOpen}
      /* an open step takes a little more air inside and pushes its
         neighbors off a touch, so it reads as lifted out of the run
         without taking a surface; eased so nothing jumps as it unfolds.
         An open step keeps the tint at rest, not only under the
         pointer: it is what bounds the output while you read it. */
      className="group/row -mx-2 rounded-lg px-2 py-1.5 transition-[background-color] duration-200 ease-out hover:bg-accent/30 motion-reduce:transition-none data-open:bg-accent/30 data-open:my-3 data-open:py-2.5"
    >
      <div className="flex items-center gap-2">
        {/* the whole row is the expander, the small line under the name
            included, so the target is as tall as the row looks; the arrow
            is the one control. Everything inside is phrasing content: the
            trigger is a button, so no divs may nest in it. */}
        <CollapsibleTrigger className="-my-1.5 -ml-2 min-w-0 flex-1 cursor-pointer py-1.5 pl-2 text-left">
          {head}
          <span className="mt-0.5 block pl-[28px] text-xs text-muted-foreground">
            <span
              className={cn(
                "block truncate sm:hidden",
                tone === "failed" && "text-destructive",
              )}
            >
              {state}
            </span>
            {detail && (
              <span className="block truncate" title={String(detail)}>
                {detail}
              </span>
            )}
          </span>
        </CollapsibleTrigger>
        {menu}
        {sessionId && (
          <a
            href={href({ name: "session", sessionId })}
            aria-label={`Open ${name}`}
            title={`Open ${name}`}
            /* where there is a pointer the arrow waits for it, holding its
               place so the row does not reflow; a touch screen has no
               hover to wait for, so it stays out. visibility rides along
               with the fade so the hidden arrow is neither clickable nor
               tabbable: it flips in at the start of the way in, and at the
               end of the way out */
            className="grid size-6 shrink-0 place-items-center rounded-md text-muted-foreground transition-[opacity,visibility,translate] duration-150 ease-out hover:bg-accent hover:text-foreground focus-visible:visible focus-visible:translate-x-0 focus-visible:opacity-100 motion-reduce:transition-none sm:invisible sm:translate-x-1 sm:opacity-0 sm:group-hover/row:visible sm:group-hover/row:translate-x-0 sm:group-hover/row:opacity-100 sm:group-focus-within/row:visible sm:group-focus-within/row:translate-x-0 sm:group-focus-within/row:opacity-100"
          >
            <ArrowUpRight className="size-4" />
          </a>
        )}
      </div>
      <CollapsibleContent className="pt-2 pl-[28px]">{children}</CollapsibleContent>
    </Collapsible>
  );
}

/* agent_new, agent_message and agent_interrupt name a session and read as
   work on it; agent_list is a plain step */
export function isWorkerStep(tool: RenderedToolCallView) {
  const p = tool.presentation;
  return (
    (p.kind === "subagent" && p.action !== "list") ||
    (p.kind === "process" && tool.awaitMode === "background")
  );
}

export function WorkerStep({
  tool,
  workers,
}: {
  tool: RenderedToolCallView;
  workers: Workers;
}) {
  const deployment = useSelectedNode();
  const minute = useMinute();
  const p = tool.presentation;
  if (p.kind === "process") {
    const bg = workers.background(tool);
    const running = tool.statusKind === "running";
    const overdue = bg?.deadlineExpired
      ? `past its deadline by ${since(bg.deadlineAt) ?? "?"}`
      : null;
    const tone: Tone = running
      ? "running"
      : tool.statusKind === "error"
        ? "failed"
        : "done";
    return (
      <Row
        tone={tone}
        verb="In the background"
        name={p.target ?? tool.toolName}
        state={
          running
            ? (overdue ?? `running${bg ? ` · ${since(bg.startedAt)}` : ""}`)
            : tool.statusKind
        }
        detail={bg?.nativeExecutor ? `pid ${bg.nativeExecutor.pid}` : null}
        sessionId={null}
      >
        <ToolBody tool={tool} />
      </Row>
    );
  }
  if (p.kind !== "subagent") return null;
  const reached = workers.byToolCall(tool);
  const summary = reached?.summary ?? null;
  const name = summary?.title ?? p.name ?? "a session";
  const sessionId = reached?.request.sessionId ?? p.sessionId ?? null;
  /* the same mark the session list uses, so a session looks like itself
     wherever it appears. One with no summary has no behavior to wear, and
     keeps the tone's glyph. */
  const behaviorId = summary?.behaviorId ?? null;
  const mark = behaviorId ? (
    <BehaviorAvatar
      name={behaviorName(behaviorId, deployment)}
      behaviorId={behaviorId}
      className="size-4 text-[8px]"
    />
  ) : undefined;
  if (p.action === "interrupt") {
    /* agent_interrupt causes no request: the call's own status is the fact */
    const tone: Tone =
      tool.statusKind === "running"
        ? "running"
        : tool.statusKind === "error"
          ? "failed"
          : "stopped";
    return (
      <Row
        tone={tone}
        verb="Interrupted"
        mark={mark}
        name={summary?.title ?? p.name ?? p.sessionId ?? "a session"}
        state={tool.statusKind === "success" ? "interrupted" : tool.statusKind}
        detail={firstLine(p.output)}
        sessionId={p.sessionId}
      >
        <ToolBody tool={tool} />
      </Row>
    );
  }
  const now = workerNow(tool, reached, minute);
  return (
    <Row
      tone={now.tone}
      /* an agent_new whose session this one began is a subagent; any other
         call only sent a message */
      verb={
        p.action === "start" && (!reached || reached.subagent) ? "Started" : "Messaged"
      }
      mark={mark}
      name={name}
      state={now.text}
      detail={now.detail ?? firstLine(p.description)}
      sessionId={sessionId}
      /* agent_interrupt's rule (gents::session_message::agent_interrupt_allowed):
         only the session that started a session may stop it, so only a
         subagent's row offers Stop */
      menu={
        <RequestStop name={name} request={reached?.subagent ? reached.request : null} />
      }
    >
      <ToolBody tool={tool} />
    </Row>
  );
}

export const subagentName = (subagent: Subagent | null, target?: string | null) =>
  subagent?.summary?.title ?? target ?? "a subagent";

/* The sessions this one started, each as the session it is: where it got
   to and a way in. Stopping is a row's business: it names the call. */
export function SubagentList({ workers }: { workers: Workers }) {
  const deployment = useSelectedNode();
  if (workers.all.length === 0) return null;
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
      <span>Subagents</span>
      {workers.all.map((subagent) => {
        const name = subagentName(subagent);
        const behaviorId = subagent.summary?.behaviorId ?? null;
        const state = subagent.summary?.turnState ?? null;
        return (
          <span
            key={scopeKey(subagent.link)}
            className="flex min-w-0 items-center gap-1"
          >
            <a
              href={href({ name: "session", sessionId: subagent.sessionId })}
              className="flex min-w-0 items-center gap-1 hover:text-foreground hover:underline"
            >
              {behaviorId && (
                <BehaviorAvatar
                  name={behaviorName(behaviorId, deployment)}
                  behaviorId={behaviorId}
                  className="size-4 text-[8px]"
                />
              )}
              <span className="truncate">{name}</span>
              {state && <span className="shrink-0">· {state}</span>}
            </a>
          </span>
        );
      })}
    </div>
  );
}
