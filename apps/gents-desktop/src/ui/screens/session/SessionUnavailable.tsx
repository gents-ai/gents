import { useEffect, useId, useState, type ReactNode } from "react";
import { ArrowLeft, ChevronRight } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { cn } from "@gents/ui/lib/utils";
import { useApp, useView } from "@/app/AppContext";
import { useSelectedNodeDid, useSessionLoad } from "@/hooks/useClient";
import { toastFailure } from "@/lib/failure";
import { href } from "@/lib/router";
import { type FleetState, nodeOf } from "../../../hooks/fleetStore";
import { nameOf, OPEN_ON_WAIT_MS, reachOf } from "../../../hooks/selectionReconcile";
import { CopyButton } from "../Markdown";
import { useFleet } from "../../hooks/useFleet";

/** How a look for the selected session ended, when nothing came back: gone
    from what this app can see, or too early to say because its node isn't
    connected yet. A node that dropped looks the same as one not yet dialled,
    so that wait is bounded. A first send adopts its session before any
    node has it, so a session this app just sent into is never missing, and
    neither is one a node lists. */
export function useSessionAbsence(
  nothingLoaded: boolean,
): "missing" | "waiting" | null {
  const sessionId = useApp().stores.selection.use.sessionId();
  const nodeDid = useSelectedNodeDid();
  const load = useSessionLoad();
  const justSent = useView(
    (view) =>
      view.pendingTurn !== null ||
      view.sentTurns.some((turn) => turn.sessionId === sessionId),
  );
  const listed = useFleet((fleet) =>
    fleet.nodeKeys.some((key) =>
      fleet.sessionsOf[key]?.some((session) => session.sessionId === sessionId),
    ),
  );
  const unconnected = useFleet((fleet) => {
    const node = nodeOf(fleet, nodeDid);
    return node !== null && reachOf(node) === "unknown";
  });
  const absent =
    nothingLoaded &&
    load.found === false &&
    load.sessionId === sessionId &&
    !justSent &&
    !listed;
  const waiting = absent && unconnected;
  const [waited, setWaited] = useState(false);
  useEffect(() => {
    setWaited(false);
    if (!waiting) return;
    const timer = setTimeout(() => setWaited(true), OPEN_ON_WAIT_MS);
    return () => clearTimeout(timer);
  }, [waiting, sessionId, nodeDid]);
  if (!absent) return null;
  return waiting && !waited ? "waiting" : "missing";
}

/** Says what is known. A node can't yet tell a client that a session doesn't
    exist from one it may not read, and this app sees only the nodes it lists,
    so none of that is claimed. */
export function missingSessionText(
  fleet: FleetState,
  nodeDid: string | null,
): { title: string; detail: string } {
  const node = nodeOf(fleet, nodeDid);
  if (node && !node.dialSucceeded) {
    return {
      title: `Can't reach ${nameOf(node)}`,
      detail: "This session may be there. Try again once the node is back.",
    };
  }
  const where = node ? `${nameOf(node)} or your other nodes` : "your nodes";
  return {
    title: "Session not found",
    detail: `This app can't find it on ${where}. It may have been removed, may not have synced yet, or may not be shared with you.`,
  };
}

export function SessionUnavailable() {
  const {
    stores,
    actions: { refreshSession, refreshSnapshot },
  } = useApp();
  const sessionId = stores.selection.use.sessionId();
  const nodeDid = useSelectedNodeDid();
  const fleet = useFleet((state) => state);
  const load = useSessionLoad();
  const { title, detail } = missingSessionText(fleet, nodeDid);
  const [busy, setBusy] = useState(false);
  const retry = async () => {
    setBusy(true);
    try {
      await Promise.all([refreshSnapshot(), refreshSession(sessionId)]);
    } catch (e) {
      toastFailure("load the session", e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Unavailable
      title={title}
      detail={detail}
      report={sessionReport(sessionId, fleet, nodeDid, load.error)}
    >
      <Button size="sm" variant="outline" disabled={busy} onClick={() => void retry()}>
        {busy ? "Trying…" : "Try again"}
      </Button>
    </Unavailable>
  );
}

/** A link can name a node this app isn't connected to. The app stays on the
    node it's working with, so there's nothing here to retry. */
export function SessionOnUnknownNode({
  sessionId,
  nodeDid,
}: {
  sessionId: string | null;
  nodeDid: string;
}) {
  const fleet = useFleet((state) => state);
  return (
    <Unavailable
      report={sessionReport(sessionId, fleet, nodeDid, null)}
      title="This link names a node you aren't connected to"
      detail="You may not have added it, it may have been removed, or the link may be wrong."
    />
  );
}

/** What to pass on when reporting a link: the page's own words can't say
    which session or what the node answered. */
export function sessionReport(
  sessionId: string | null,
  fleet: FleetState,
  nodeDid: string | null,
  answer: string | null,
) {
  const node = nodeOf(fleet, nodeDid);
  return [
    `Session: ${sessionId ?? "none"}`,
    `Node: ${node ? `${nameOf(node)} (${nodeDid})` : (nodeDid ?? "none")}${node ? "" : " (not on this app's list)"}`,
    ...(answer ? [`Answer: ${answer.replace(/^BridgeInvokeError: /, "")}`] : []),
  ].join("\n");
}

function Unavailable({
  title,
  detail,
  report,
  children,
}: {
  title: string;
  detail: string;
  report: string;
  children?: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const reportId = useId();
  return (
    <div
      role="alert"
      data-testid="session-unavailable"
      className="grid h-full place-items-center px-6"
    >
      <div className="flex w-fit max-w-prose flex-col items-center gap-4">
        <div className="flex flex-col items-center gap-4 text-center">
          <div className="flex flex-col gap-1">
            <h1 className="text-base font-medium">{title}</h1>
            <p className="text-sm text-muted-foreground">{detail}</p>
          </div>
          <div className="flex items-center gap-3">
            {children}
            <a
              href={href({ name: "sessions" })}
              className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
            >
              <ArrowLeft className="size-3.5" /> Sessions
            </a>
          </div>
        </div>
        <div className="flex w-full flex-col items-center gap-2 border-t border-border/60 pt-3">
          <button
            type="button"
            aria-expanded={open}
            aria-controls={reportId}
            onClick={() => setOpen((was) => !was)}
            className="inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
          >
            Details
            <ChevronRight
              aria-hidden
              className={cn("size-3.5 transition-transform", open && "rotate-90")}
            />
          </button>
          <div id={reportId} hidden={!open} className="relative w-full">
            <pre className="overflow-auto rounded-lg bg-raised p-3 pr-9 text-left font-mono text-[11px] whitespace-pre-wrap break-words text-muted-foreground">
              {report}
            </pre>
            <CopyButton className="absolute top-1 right-1" getText={() => report} />
          </div>
        </div>
      </div>
    </div>
  );
}
