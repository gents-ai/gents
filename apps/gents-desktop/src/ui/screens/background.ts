/* What a session left running in the background: its backgrounded tool
   calls, read from the session's own rows and joined with the operations
   snapshot, which knows only the calls still running and adds whether one
   stopped responding or ran past its deadline. */
import { useEffect, useMemo, useState } from "react";
import { createStore, useStore } from "zustand";
import type {
  BackgroundedToolView,
  DesktopOperationsSnapshot,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";
import { useApp } from "@/app/AppContext";
import { dockScopeOf, workspace } from "@/app/workspace";
import {
  useSelectedSessionFields,
  useSelectedSessionValue,
  useSessionFacts,
} from "../hooks/useSelectedSession";

export type BackgroundCall = {
  key: string;
  requestId: string;
  toolCallId: string;
  /* the call's row in the session, when that part of it is loaded */
  tool: RenderedToolCallView | null;
  /* the snapshot's view of it, while it runs */
  view: BackgroundedToolView | null;
  /* the command line, or the tool's name when it is not a command */
  runs: string;
};

export type Tone = "running" | "stuck" | "overdue" | "done" | "failed" | "stopped";

const keyOf = (requestId: string, toolCallId: string) =>
  `${requestId}\u0000${toolCallId}`;
const physicalKey = (requestId: string, toolCallId: string, physical: string) =>
  `${keyOf(requestId, toolCallId)}\u0000${physical}`;

/* the runtime names the call that spawn_process starts after the spawning
   call's document: `spawned:<doc id>` */
const spawnedFrom = (child: string, tools: readonly RenderedToolCallView[]) =>
  child.startsWith("spawned:")
    ? (tools.find(
        (t) =>
          t.presentation.kind === "process" &&
          t.toolCallDocId &&
          child === `spawned:${t.toolCallDocId}`,
      ) ?? null)
    : null;

function spawnedCommand(spawn: RenderedToolCallView | null): string | null {
  if (spawn?.presentation.kind !== "process") return null;
  const { description, target } = spawn.presentation;
  try {
    const args: unknown = JSON.parse(description ?? "");
    const command = (args as Record<string, unknown> | null)?.command;
    if (typeof command === "string" && command.trim()) return command.trim();
  } catch {}
  return target;
}

function runsOf(
  tool: RenderedToolCallView | null,
  view: BackgroundedToolView | null,
  tools: readonly RenderedToolCallView[],
): string {
  const name = tool?.toolName ?? view?.toolName ?? "tool";
  const p = tool?.presentation;
  if (p?.kind === "command" && p.command && p.command !== name) return p.command;
  if (p?.kind === "process") return spawnedCommand(tool) ?? name;
  const id = tool?.toolCallId ?? view?.toolCallId ?? "";
  return spawnedCommand(spawnedFrom(id, tools)) ?? name;
}

export function backgroundCalls(
  tools: readonly RenderedToolCallView[],
  views: readonly BackgroundedToolView[],
  requestIds: ReadonlySet<string>,
): BackgroundCall[] {
  const viewOf = new Map(
    views
      .filter((v) => v.toolCallDocId)
      .map((v) => [physicalKey(v.requestId, v.toolCallId, v.toolCallDocId!), v]),
  );
  const backgrounded = tools.filter(
    (t): t is RenderedToolCallView & { requestId: string; toolCallId: string } =>
      t.awaitMode === "background" &&
      Boolean(t.requestId && t.toolCallId) &&
      t.presentation.kind !== "agent" &&
      (t.presentation.kind !== "process" || t.presentation.action === "spawn"),
  );
  /* a spawn whose started call is listed is that call */
  const spawns = new Set(
    backgrounded.flatMap((t) => {
      const spawn = spawnedFrom(t.toolCallId, tools);
      return spawn ? [spawn] : [];
    }),
  );
  const calls = new Map<string, BackgroundCall>();
  for (const tool of backgrounded) {
    if (spawns.has(tool)) continue;
    const key = physicalKey(
      tool.requestId,
      tool.toolCallId,
      tool.toolCallDocId ?? tool.itemKey,
    );
    const view = tool.toolCallDocId ? (viewOf.get(key) ?? null) : null;
    if (view?.startsSession) continue;
    calls.set(key, {
      key,
      requestId: tool.requestId,
      toolCallId: tool.toolCallId,
      tool,
      view,
      runs: runsOf(tool, view, tools),
    });
  }
  /* a call the session's rows hold but leave out is not listed from the
     snapshot either */
  const loaded = new Set(
    tools.map((t) =>
      t.requestId && t.toolCallId ? keyOf(t.requestId, t.toolCallId) : "",
    ),
  );
  const loadedPhysical = new Set(
    tools.flatMap((t) =>
      t.requestId && t.toolCallId && t.toolCallDocId
        ? [physicalKey(t.requestId, t.toolCallId, t.toolCallDocId)]
        : [],
    ),
  );
  for (const view of views) {
    const logical = keyOf(view.requestId, view.toolCallId);
    const key = view.toolCallDocId
      ? physicalKey(view.requestId, view.toolCallId, view.toolCallDocId)
      : logical;
    if (
      (view.toolCallDocId ? loadedPhysical.has(key) : loaded.has(logical)) ||
      view.startsSession ||
      !requestIds.has(view.requestId)
    )
      continue;
    calls.set(key, {
      key,
      requestId: view.requestId,
      toolCallId: view.toolCallId,
      tool: null,
      view,
      runs: runsOf(null, view, tools),
    });
  }
  const started = (c: BackgroundCall) => Date.parse(startedAtOf(c) ?? "") || 0;
  return [...calls.values()].sort(
    (a, b) => Number(isActive(b)) - Number(isActive(a)) || started(b) - started(a),
  );
}

/* the session's row is newer than the snapshot once the call has ended */
export const isActive = (call: BackgroundCall) =>
  call.tool ? call.tool.statusKind === "running" : call.view !== null;

const startedAtOf = (call: BackgroundCall) =>
  call.view?.waitStatus?.startedAt ??
  call.view?.startedAt ??
  call.tool?.startedAt ??
  null;

/* "12 min", "2 h 5 min", "3 days" */
function lasted(ms: number): string {
  const mins = Math.floor(Math.max(0, ms) / 60_000);
  if (mins < 1) return "less than a minute";
  if (mins < 60) return `${mins} min`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return mins % 60 ? `${hours} h ${mins % 60} min` : `${hours} h`;
  const days = Math.floor(hours / 24);
  return `${days} ${days === 1 ? "day" : "days"}`;
}

const ago = (ms: number) => (ms < 60_000 ? "just now" : `${lasted(ms)} ago`);

/** A state short enough to sit at the end of a one-line row; the row's
    title and the log carry the whole of it. */
export function briefStateOf(call: BackgroundCall, now: number): string {
  const { tone } = stateOf(call, now);
  const { tool, view } = call;
  if (tone === "stuck") return "stuck";
  if (tone === "overdue") return "overdue";
  if (tone === "running") {
    if (view?.waitStatus?.lifecycle === "pending") return "waiting";
    const started = at(startedAtOf(call));
    return started ? brief(now - started) : "running";
  }
  const p = tool?.presentation;
  const command = p?.kind === "command" ? p : null;
  if (tone === "failed") {
    if (command?.timedOut) return "timed out";
    return command?.exitCode != null ? `exit ${command.exitCode}` : "failed";
  }
  if (tone === "stopped") return "stopped";
  const started = at(tool?.startedAt);
  const ended = at(tool?.completedAt);
  const took =
    command?.durationMs ??
    (started && ended && ended > started ? ended - started : null);
  return took != null ? brief(took) : "done";
}

function brief(ms: number): string {
  const mins = Math.floor(Math.max(0, ms) / 60_000);
  if (mins < 1) return "<1 min";
  if (mins < 60) return `${mins} min`;
  const hours = Math.floor(mins / 60);
  return hours < 24 ? `${hours} h` : `${Math.floor(hours / 24)} d`;
}

const at = (iso: string | null | undefined) => (iso ? Date.parse(iso) || null : null);

export function stateOf(
  call: BackgroundCall,
  now: number,
): { tone: Tone; text: string } {
  const { tool, view } = call;
  if (isActive(call)) {
    const stuck = at(view?.stuckSince);
    if (stuck) return { tone: "stuck", text: `Stopped responding ${ago(now - stuck)}` };
    const deadline = at(view?.deadlineAt ?? tool?.deadlineAt);
    if (view?.deadlineExpired || (deadline && deadline <= now))
      return {
        tone: "overdue",
        text: deadline
          ? `Past its deadline by ${lasted(now - deadline)}`
          : "Past its deadline",
      };
    if (view?.waitStatus?.lifecycle === "pending")
      return { tone: "running", text: "Waiting to start" };
    const started = at(startedAtOf(call));
    return {
      tone: "running",
      text: started ? `Running for ${lasted(now - started)}` : "Running",
    };
  }
  const p = tool?.presentation;
  const command = p?.kind === "command" ? p : null;
  const started = at(tool?.startedAt);
  const ended = at(tool?.completedAt);
  const took =
    command?.durationMs ??
    (started && ended && ended > started ? ended - started : null);
  const exit = command?.exitCode != null ? ` · exit code ${command.exitCode}` : "";
  switch (tool?.statusKind) {
    case "success":
      if (command?.timedOut) return { tone: "failed", text: "Timed out" };
      if (command?.failed) return { tone: "failed", text: `Failed${exit}` };
      return {
        tone: "done",
        text: took != null ? `Finished after ${lasted(took)}` : "Finished",
      };
    case "error":
    case "failed":
      if (command?.timedOut) return { tone: "failed", text: "Timed out" };
      return { tone: "failed", text: `Failed${exit}` };
    case "cancelled":
      return { tone: "stopped", text: "Stopped" };
    default:
      return { tone: "stopped", text: tool?.statusKind ?? "State unknown" };
  }
}

const RED = "\x1b[31m";
const PLAIN = "\x1b[0m";

/* the call's output as the session's rows carry it: the live tail while it
   runs, its result once it ends; stderr follows stdout in red */
export function outputOf(tool: RenderedToolCallView | null): string {
  if (!tool) return "";
  if (tool.statusKind === "running" && tool.partialOutputTail)
    return tool.partialOutputTail;
  const p = tool.presentation;
  switch (p.kind) {
    case "command":
      return [p.stdout, p.stderr && `${RED}${p.stderr}${PLAIN}`, p.fallbackOutput]
        .filter(Boolean)
        .join("\n");
    case "fileRead":
      return p.body || (p.fallbackOutput ?? "");
    case "fileEdit":
      return p.fallbackOutput ?? "";
    default:
      return p.output ?? "";
  }
}

/* xterm moves down a line on \n without returning to the line's start */
export function crlf(text: string, after = ""): string {
  const converted = text.replace(/(?<!\r)\n/g, "\r\n");
  return after.endsWith("\r") && text.startsWith("\n") ? converted.slice(1) : converted;
}

/* what to write to show `next` where `shown` is: only the new part when
   `next` extends it, else all of `next` after a reset */
export function nextWrite(
  shown: string,
  next: string,
): { reset: boolean; data: string } | null {
  if (next === shown) return null;
  if (shown && next.startsWith(shown))
    return { reset: false, data: crlf(next.slice(shown.length), shown) };
  return { reset: true, data: crlf(next) };
}

const POLL_MS = 10_000;

type Held = { scope: string; value: DesktopOperationsSnapshot };

/** The open session's background calls; empty for any other session. */
export function useBackgroundCalls(sessionId: string | null): BackgroundCall[] {
  const {
    stores,
    actions: { fetchOperationsSnapshot },
  } = useApp();
  const session = useSelectedSessionFields((s) =>
    s && s.sessionId === sessionId
      ? { nodeDid: s.nodeDid ?? null, latestRequestId: s.latestRequestId ?? null }
      : null,
  );
  const selectedNode = stores.selection.use.nodeDid();
  const facts = useSessionFacts();
  const tools = session ? facts?.tools : undefined;
  const nodeDid = session ? (session.nodeDid ?? selectedNode) : null;
  const fromRows = useMemo(() => backgroundCalls(tools ?? [], [], new Set()), [tools]);
  const watching = fromRows.length > 0 && nodeDid !== null;
  const running = fromRows.some(isActive);
  const [held, setHeld] = useState<Held | null>(null);
  const [tick, setTick] = useState(0);
  useEffect(() => {
    if (!watching || !running) return;
    const timer = setInterval(() => setTick((n) => n + 1), POLL_MS);
    return () => clearInterval(timer);
  }, [watching, running]);
  /* read again as calls start or end, not on every line they write */
  const shape = fromRows.map((c) => `${c.key}\u0000${c.tool?.statusKind}`).join("\n");
  useEffect(() => {
    if (!watching || !nodeDid) return;
    let live = true;
    void fetchOperationsSnapshot({ nodeDid }).then(
      (value) => live && setHeld({ scope: nodeDid, value }),
      () => live && setHeld(null),
    );
    return () => {
      live = false;
    };
  }, [fetchOperationsSnapshot, watching, nodeDid, shape, tick]);
  const views =
    watching && held?.scope === nodeDid ? held.value.backgroundedTools : null;
  const latestRequestId = session?.latestRequestId ?? null;
  const userRequestIds = facts?.userRequestIds;
  return useMemo(() => {
    if (!tools || !views) return fromRows;
    const requests = new Set(userRequestIds);
    for (const t of tools) if (t.requestId) requests.add(t.requestId);
    if (latestRequestId) requests.add(latestRequestId);
    return backgroundCalls(tools, views, requests);
  }, [tools, views, fromRows, userRequestIds, latestRequestId]);
}

/** How many background calls the open session's rows list, and how many of
    them are running. */
export function useBackgroundCount(sessionId: string | null): {
  listed: number;
  running: number;
} {
  const open = useSelectedSessionValue((s) => s?.sessionId === sessionId);
  const tools = useSessionFacts()?.tools;
  return useMemo(() => {
    const calls = open && tools ? backgroundCalls(tools, [], new Set()) : [];
    return { listed: calls.length, running: calls.filter(isActive).length };
  }, [open, tools]);
}

/* a log asked for from outside the Background tab, such as a command's row
   in the transcript, waiting for the tab to open it */
const asked = createStore<{ sessionId: string; key: string } | null>(() => null);

export function openBackgroundLog(
  sessionId: string,
  requestId: string,
  toolCallId: string,
  physical: string,
) {
  asked.setState(
    { sessionId, key: physicalKey(requestId, toolCallId, physical) },
    true,
  );
  workspace.openSurface(dockScopeOf("session", sessionId), "background");
}

/** The log asked for in this session, taken once. */
export function useAskedLog(sessionId: string | null, take: (key: string) => void) {
  const wanted = useStore(asked, (a) => (a?.sessionId === sessionId ? a.key : null));
  useEffect(() => {
    if (!wanted) return;
    asked.setState(null, true);
    take(wanted);
  }, [wanted, take]);
}

/* where the reader was in each session's Background tab, while the app runs:
   the log they had open and how far above its newest line */
const places = new Map<string, { log: string | null; above: number }>();

export const backgroundPlace = (sessionId: string | null) =>
  (sessionId && places.get(sessionId)) || { log: null, above: 0 };

export function rememberBackgroundPlace(
  sessionId: string | null,
  place: { log: string | null; above: number },
) {
  if (sessionId) places.set(sessionId, place);
}

export const resetBackgroundPlaces = () => places.clear();
