import { type ReactNode } from "react";
import { ArrowUpRight, Ban, CircleCheck, CircleX } from "lucide-react";
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import { Spinner } from "@gents/ui/components/spinner";
import { AgentInitials } from "./parts";
import { agentName } from "./behavior";
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
import { type Reached, type Workers } from "./workers";
import { RequestStop } from "./WorkerActions";

type Tone = "running" | "done" | "failed" | "stopped" | "unknown";

const firstLine = (s: string | null | undefined) => s?.trim().split("\n")[0] ?? null;

export function failureLine(output: string | null | undefined): string | null {
  const text = output?.trim();
  if (!text) return null;
  if (!text.startsWith("{")) return firstLine(text);
  try {
    const value: unknown = JSON.parse(text);
    if (value && typeof value === "object") {
      for (const key of ["error", "message", "reason", "detail"]) {
        const field = (value as Record<string, unknown>)[key];
        if (typeof field === "string" && field.trim()) return firstLine(field);
        if (field && typeof field === "object") {
          const nested = (field as Record<string, unknown>).message;
          if (typeof nested === "string" && nested.trim()) return firstLine(nested);
        }
      }
    }
  } catch {}
  return null;
}
const since = (iso: string | null | undefined) =>
  iso ? duration(Math.max(0, Date.now() - Date.parse(iso))) : null;

const WAITING = new Set(["waitingforclaim", "pending", "claimed"]);

export function workerNow(
  tool: RenderedToolCallView,
  reached: Reached | null,
  now: number,
): { tone: Tone; text: string; detail?: string | null } {
  const failure = failureLine(
    tool.presentation.kind === "agent" ? tool.presentation.output : null,
  );
  const state = reached?.request.lifecycleState ?? null;
  if (!state) {
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
  mark?: ReactNode;
  verb: string;
  name: string;
  state: string;
  detail?: ReactNode;
  sessionId: string | null;
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
    <Collapsible
      ref={ref}
      open={open}
      onOpenChange={setOpen}
      className="group/row -mx-2 rounded-lg px-2 py-1.5 transition-[background-color] duration-200 ease-out hover:bg-accent/30 motion-reduce:transition-none data-open:bg-accent/30 data-open:my-3 data-open:py-2.5"
    >
      <div className="flex items-center gap-2">
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
    (p.kind === "agent" &&
      (p.action === "start" || p.action === "message" || p.action === "interrupt")) ||
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
    const ws = bg?.waitStatus ?? null;
    // Panel scope (accepted F2): this is the ACTIVE-worker panel. Terminal
    // rows are excluded upstream (bridge projection filters lifecycle
    // terminal + superseded from backgroundedTools), so terminal elapsed
    // rendering here is unreachable today; the shared protocol projection
    // and model tools (list_processes / wait_process envelopes) are the
    // consumers of stable terminal elapsed. Do not broaden this panel into
    // history without reworking the bridge filter.
    // Truthful lifecycle from the shared projection: unknown stays unknown;
    // pending is never relabeled running.
    const active = ws
      ? ws.lifecycle === "running" || ws.lifecycle === "pending"
      : tool.statusKind === "running";
    const pending = ws?.lifecycle === "pending";
    const terminal = ws?.lifecycle ?? null;
    const stableElapsed =
      ws?.terminalElapsedSeconds != null ? `${ws.terminalElapsedSeconds}s` : null;
    const runningAge =
      ws?.runningElapsedSeconds != null ? `${ws.runningElapsedSeconds}s` : null;
    const overdue = bg?.deadlineExpired
      ? `past its deadline by ${since(bg.deadlineAt) ?? "?"}`
      : null;
    // Tone and text answer different questions: tone tracks the rendered
    // tool stream (running/failed/done), while "state unknown" text must
    // survive even under a failed tone — an unparseable lifecycle stays
    // unknown regardless of stream error state.
    const tone: Tone = active
      ? "running"
      : tool.statusKind === "error"
        ? "failed"
        : terminal === "unknown"
          ? "unknown"
          : "done";
    return (
      <Row
        tone={tone}
        verb="In the background"
        name={p.target ?? tool.toolName}
        state={
          active
            ? (overdue ??
              `${pending ? "pending" : "running"}${runningAge ? ` · ${runningAge}` : since(bg?.startedAt ?? null) ? ` · ${since(bg?.startedAt ?? null)}` : ""}`)
            : terminal === "unknown"
              ? "state unknown"
              : (stableElapsed ?? tool.statusKind)
        }
        detail={bg?.nativeExecutor ? `pid ${bg.nativeExecutor.pid}` : null}
        sessionId={null}
      >
        <ToolBody tool={tool} />
      </Row>
    );
  }
  if (p.kind !== "agent") return null;
  const reached = workers.byToolCall(tool);
  const summary = reached?.summary ?? null;
  const name = summary?.title ?? p.name ?? "a session";
  const sessionId = reached?.request.sessionId ?? p.sessionId ?? null;
  const agentId = summary?.agentId ?? null;
  const mark = agentId ? (
    <AgentInitials
      name={agentName(agentId, deployment)}
      className="size-4 text-[8px]"
    />
  ) : undefined;
  if (p.action === "interrupt") {
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
        detail={failureLine(p.output)}
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
      verb={
        p.action === "start" && (!reached || reached.worker) ? "Started" : "Messaged"
      }
      mark={mark}
      name={name}
      state={now.text}
      detail={now.detail ?? firstLine(p.description)}
      sessionId={sessionId}
      menu={
        <RequestStop name={name} request={reached?.worker ? reached.request : null} />
      }
    >
      <ToolBody tool={tool} />
    </Row>
  );
}
