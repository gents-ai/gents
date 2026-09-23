/* A delegated piece of work as a row in the transcript's activity block:
   what the agent asked of a worker, the receipt, and where that worker is
   now. Replaces the plain tool step for subagent and background-process
   calls. The words come from the contract's own states. */
import { type ReactNode } from "react";
import { ArrowUpRight, Ban, CircleCheck, CircleX } from "lucide-react";
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import { Spinner } from "@gents/ui/components/spinner";
import { BehaviorAvatar } from "./parts";
import { behaviorName } from "./behavior";
import { useDeployment } from "./deployment-context";
import { useExclusiveStep } from "@gents/ui/conversation";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@gents/ui/components/collapsible";
import { cn } from "@gents/ui/lib/utils";
import { href } from "@/lib/router";
import { ToolBody } from "./tool-views";
import { duration } from "./tool-summary";
import { when } from "./time";
import { isLive } from "@/lib/live";
import type { WorkerState, Workers } from "./workers";
import { WorkerStop } from "./WorkerActions";

type Tone = "running" | "done" | "failed" | "stopped" | "unknown";

const firstLine = (s: string | null | undefined) => s?.trim().split("\n")[0] ?? null;
const since = (iso: string | null | undefined) =>
  iso ? duration(Math.max(0, Date.now() - Date.parse(iso))) : null;

/* the worker's state, from the bridge's own facts in order of freshness:
   the child session's turn (turn_state_label), then the lineage node's
   request lifecycle, then the edge's, then the tool call. Nothing here
   guesses at replication: a worker with no summary yet reads as its
   lifecycle says. */
const WAITING = new Set([
  "waitingforclaim",
  "pending",
  "claimed",
  "workspace_binding_pending",
  "workspacebindingpending",
]);

export function workerNow(
  tool: RenderedToolCallView,
  w: WorkerState | null,
): { tone: Tone; text: string; detail?: string | null } {
  const turn = w?.summary?.turnState ?? null;
  const node = w?.node?.lifecycleState ?? null;
  const edge = w?.edge?.lifecycleState ?? null;
  const state = turn ?? node ?? edge ?? tool.statusKind ?? null;
  const failure = firstLine(
    tool.presentation.kind === "subagent" ? tool.presentation.output : null,
  );
  if (state && WAITING.has(state.toLowerCase()))
    return { tone: "running", text: "waiting for the agent to pick it up" };
  if (isLive(state)) {
    /* the request the parent spawned ended, yet the session works on: both
       facts, side by side (gents#1541) */
    if (edge === "failed")
      return {
        tone: "running",
        text: "request failed · session still working",
        detail: failure,
      };
    const s = when(w?.summary?.updatedAt ?? null);
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
      return { tone: "unknown", text: state ?? "unknown" };
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

export function isWorkerStep(tool: RenderedToolCallView) {
  const p = tool.presentation;
  return (
    p.kind === "subagent" || (p.kind === "process" && tool.awaitMode === "background")
  );
}

export function WorkerStep({
  tool,
  workers,
}: {
  tool: RenderedToolCallView;
  workers: Workers;
}) {
  const deployment = useDeployment();
  const p = tool.presentation;
  if (p.kind === "process") {
    const bg = workers.byToolCall(tool);
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
        detail={
          bg?.nativeExecutor
            ? `pid ${bg.nativeExecutor.pid}${bg.cancelPolicy ? ` · on cancel: ${bg.cancelPolicy}` : ""}`
            : null
        }
        sessionId={null}
      >
        <ToolBody tool={tool} />
      </Row>
    );
  }
  if (p.kind !== "subagent") return null;
  const w = p.childRequestId ? workers.byChildRequest(p.childRequestId) : null;
  const name = w?.summary?.title ?? p.name ?? p.childRequestId ?? "a worker";
  const sessionId = w?.sessionId ?? null;
  /* the same mark the gathered rows and the session list use, so a worker
     looks like itself wherever it appears. A worker with no summary has no
     behavior to wear, and keeps the tone's glyph. */
  const behaviorId = w?.summary?.behaviorId ?? null;
  const mark = behaviorId ? (
    <BehaviorAvatar
      name={behaviorName(behaviorId, deployment)}
      behaviorId={behaviorId}
      className="size-4 text-[8px]"
    />
  ) : undefined;
  const now = workerNow(tool, w);
  switch (p.action) {
    case "spawn":
      return (
        <Row
          tone={now.tone}
          verb="Started"
          mark={mark}
          name={name}
          state={now.text}
          detail={now.detail}
          sessionId={sessionId}
          menu={
            <WorkerStop
              name={name}
              currentRequestId={w?.summary?.latestRequestId ?? p.childRequestId ?? null}
              running={now.tone === "running"}
            />
          }
        >
          <ToolBody tool={tool} />
        </Row>
      );
    case "steer": {
      const interrupt = p.description?.startsWith("[interrupt]");
      return (
        <Row
          tone={
            tool.statusKind === "running"
              ? "running"
              : tool.statusKind === "error"
                ? "failed"
                : "done"
          }
          verb={interrupt ? "Interrupted and told" : "Told"}
          name={name}
          state={
            firstLine(p.output) ??
            (tool.statusKind === "completed" ? "delivered" : tool.statusKind)
          }
          detail={p.description?.replace(/^\[interrupt\]\s*/, "")}
          sessionId={sessionId}
        >
          <ToolBody tool={tool} />
        </Row>
      );
    }
    case "wait": {
      const out = firstLine(p.output) ?? "";
      const timedOut = /timed out/i.test(out);
      return (
        <Row
          tone={
            tool.statusKind === "running" ? "running" : timedOut ? "stopped" : "done"
          }
          verb="Waited for"
          name={name}
          state={
            tool.statusKind === "running"
              ? "waiting"
              : timedOut
                ? `timed out · worker ${now.tone === "running" ? "still working" : now.text}`
                : out || "returned"
          }
          detail={firstLine(p.description)}
          sessionId={sessionId}
        >
          <ToolBody tool={tool} />
        </Row>
      );
    }
    case "cancel":
      return (
        <Row
          tone={tool.statusKind === "running" ? "running" : "stopped"}
          verb="Cancelled"
          name={name}
          state={now.tone === "running" ? `still ${now.text}` : now.text}
          detail={firstLine(p.output) ?? firstLine(p.description)}
          sessionId={sessionId}
        >
          <ToolBody tool={tool} />
        </Row>
      );
    default:
      return (
        <Row
          tone={tool.statusKind === "running" ? "running" : "done"}
          verb={p.action}
          name={name}
          state={now.text}
          detail={firstLine(p.description)}
          sessionId={sessionId}
        >
          <ToolBody tool={tool} />
        </Row>
      );
  }
}
