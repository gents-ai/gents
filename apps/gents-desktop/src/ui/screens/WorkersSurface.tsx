import { useCallback, useMemo, useState } from "react";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import type { SurfaceContext } from "@/app/surfaces";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { href } from "@/lib/router";
import { nodeOfSession } from "@/lib/nodes";
import { stateOf } from "@/lib/live";
import { NodeAgentStack } from "./NodeBehaviorStack";
import { SessionStatus } from "./SessionStatus";
import { StartedCount } from "./StartedCount";
import { span, useMinute } from "./time";
import { sessionViews } from "@/app/sessionViews";
import { sessionKeyOf } from "../../hooks/fleetStore";
import {
  failedUnder,
  runningUnder,
  stoppedUnder,
  useFleet,
  workersOf,
  workersOfId,
} from "../hooks/useFleet";

/* newest started first; a row keeps its place as its state changes */
const ordered = (workers: readonly SessionSummary[]) =>
  [...workers].sort(
    (a, b) => Date.parse(b.createdAt ?? "") - Date.parse(a.createdAt ?? "") || 0,
  );

/* how long it has run, or ran; the icon says which */
function duration(s: SessionSummary, now: number) {
  const started = s.createdAt ? Date.parse(s.createdAt) : null;
  if (!started) return null;
  const state = stateOf(s.turnState);
  if (state === "working" || state === "waiting") return span(now - started);
  const last = s.updatedAt ? Date.parse(s.updatedAt) : null;
  return last && last > started ? span(last - started) : null;
}

type Opened = { has: (key: string) => boolean; toggle: (key: string) => void };

function WorkerRow({
  worker,
  opened,
  above,
}: {
  worker: SessionSummary;
  opened: Opened;
  /* the sessions this row is nested under, so a cycle cannot recurse */
  above: ReadonlySet<string>;
}) {
  const now = useMinute();
  const key = sessionKeyOf(worker);
  const children = useFleet((s) => workersOf(s, worker));
  const running = useFleet((s) => runningUnder(s, worker));
  const failed = useFleet((s) => failedUnder(s, worker));
  const stopped = useFleet((s) => stoppedUnder(s, worker));
  const shown = useMemo(
    () => ordered(children.filter((c) => !above.has(sessionKeyOf(c)))),
    [children, above],
  );
  const open = opened.has(key) && shown.length > 0;
  const path = useMemo(() => new Set(above).add(key), [above, key]);
  const took = duration(worker, now);
  return (
    <li>
      <div className="relative grid grid-cols-[auto_1fr_auto_auto] items-start gap-x-2.5 rounded-lg bg-accent/40 px-3 py-2 text-sm hover:bg-accent">
        <SessionStatus turnState={worker.turnState} className="mt-0.5" />
        <div className="min-w-0">
          <a
            href={href({ name: "session", sessionId: worker.sessionId })}
            className="block truncate outline-none after:absolute after:inset-0 after:rounded-lg focus-visible:after:ring-2 focus-visible:after:ring-ring"
          >
            {worker.title ?? "Untitled"}
          </a>
          {took && <p className="truncate text-xs text-muted-foreground">{took}</p>}
        </div>
        <span className="flex justify-end">
          <StartedCount
            session={worker}
            started={shown.length}
            running={running}
            failed={failed}
            stopped={stopped}
            open={open}
            onToggle={() => opened.toggle(key)}
          />
        </span>
        <span className="relative z-10">
          <NodeAgentStack
            nodeDid={nodeOfSession(worker)}
            agentId={worker.agentId}
            size="sm"
          />
        </span>
      </div>
      {open && (
        <ul className="mt-1.5 ml-3 space-y-1.5 border-l border-border/50 pl-3">
          {shown.map((w) => (
            <WorkerRow key={sessionKeyOf(w)} worker={w} opened={opened} above={path} />
          ))}
        </ul>
      )}
    </li>
  );
}

export function WorkersSurface({ sessionId }: SurfaceContext) {
  const workers = useFleet((s) => workersOfId(s, sessionId));
  const shown = useMemo(() => ordered(workers), [workers]);
  /* which rows show their started sessions, kept for the next visit; the
     dock can stay up while another session opens */
  const [held, setHeld] = useState(() => ({
    session: sessionId,
    open: new Set(sessionViews.openedOf(sessionId)) as ReadonlySet<string>,
  }));
  if (held.session !== sessionId)
    setHeld({ session: sessionId, open: new Set(sessionViews.openedOf(sessionId)) });
  const open = held.open;
  const toggle = useCallback(
    (key: string) =>
      setHeld((was) => {
        const next = new Set(was.open);
        if (!next.delete(key)) next.add(key);
        if (was.session) sessionViews.setOpened(was.session, [...next]);
        return { session: was.session, open: next };
      }),
    [],
  );
  const opened = useMemo<Opened>(
    () => ({ has: (k) => open.has(k), toggle }),
    [open, toggle],
  );
  const lead = useFleet((s) => (sessionId ? s.bySessionId[sessionId] : undefined));
  const top = useMemo(() => new Set(lead ? [sessionKeyOf(lead)] : []), [lead]);
  return (
    <ScrollArea className="h-full">
      <div className="px-3 py-2">
        {shown.length === 0 && (
          <p className="py-6 text-center text-sm text-muted-foreground">
            This session has not handed out any work.
          </p>
        )}
        <ul className="space-y-1.5">
          {shown.map((w) => (
            <WorkerRow key={sessionKeyOf(w)} worker={w} opened={opened} above={top} />
          ))}
        </ul>
      </div>
    </ScrollArea>
  );
}
