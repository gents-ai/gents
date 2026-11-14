import { useEffect, useState, type ReactNode } from "react";
import { Button } from "@gents/ui/components/button";
import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";
import { navigate } from "@/lib/router";
import { toastFailure } from "@/lib/failure";
import { useApp, useView } from "@/app/AppContext";
import { Notice } from "@/components/Notice";
import { useBootstrap, useSelectedNodeDid, useWorkingNode } from "@/hooks/useClient";

const LABEL = {
  retryLocal: "Try again",
  reloadSession: "Try again",
  configureInference: "Configure inference",
};
const BUSY = {
  retryLocal: "Retrying…",
  reloadSession: "Retrying…",
  configureInference: "Opening…",
};

export const DELETED_HOME_RECHECK_MS = 3_000;

export function LoadingStatus({ overlay = false }: { overlay?: boolean }) {
  const status = useView((view) => view.loadingStatus);
  const {
    stores,
    lifecycle,
    actions: {
      reconnectNode,
      refreshLocalServer,
      refreshSession,
      refreshSnapshot,
      reloadSession,
      watchLocalServer,
    },
  } = useApp();
  const nodeDid = useSelectedNodeDid();
  const selectedSessionId = stores.selection.use.sessionId();
  const [busy, setBusy] = useState(false);
  const savedPeers = useBootstrap()?.savedPeers;
  /* once its folder is deleted, init.json no longer names the local node;
     its saved connection still does */
  const local =
    useWorkingNode()?.nodeDid === nodeDid ||
    Boolean(savedPeers?.some((peer) => peer.local && peer.nodeDid === nodeDid));
  const managed = stores.localServer.use.status();
  const deletedHome = local ? managed?.deletedHome : null;
  /* read only while something is shown: each read asks the OS service manager.
     A dead node changes no snapshot, so a failure keeps reading until the
     deleted folder is found. */
  const watch = local && status !== null;
  useEffect(() => (watch ? watchLocalServer() : undefined), [watch, watchLocalServer]);
  const recheck = watch && status?.phase === "failed" && !deletedHome;
  useEffect(() => {
    if (!recheck) return undefined;
    const timer = setInterval(() => void refreshLocalServer(), DELETED_HOME_RECHECK_MS);
    return () => clearInterval(timer);
  }, [recheck, refreshLocalServer]);
  const placed = (card: ReactNode) =>
    overlay ? (
      <div className="[grid-area:1/1]" data-testid="composer-blocker">
        {card}
      </div>
    ) : (
      card
    );
  if (deletedHome)
    return placed(
      <Notice
        tone="warning"
        className={overlay ? "h-full" : "mb-3"}
        title="Your local node’s folder was deleted"
        description={`${deletedHome} is gone, so ${managed?.nodeName || "your local node"} stopped. Its sessions can’t be loaded.`}
        actions={
          <Button
            size="sm"
            variant="outline"
            onClick={() => void lifecycle.home.continueFresh()}
          >
            Set up a new node
          </Button>
        }
      />,
    );
  if (!status || status.quiet) return null;
  const act = async () => {
    const action = status.action;
    if (!action) return;
    if (action === "configureInference") {
      navigate(
        nodeDid ? { name: "agent", nodeDid, section: "inference" } : { name: "agents" },
      );
      return;
    }
    setBusy(true);
    try {
      if (nodeDid) await reconnectNode(nodeDid);
      if (action === "reloadSession") await reloadSession(selectedSessionId);
      else await Promise.all([refreshSnapshot(), refreshSession(selectedSessionId)]);
    } catch (e) {
      toastFailure("load the session", e);
    } finally {
      setBusy(false);
    }
  };
  const card = (
    <div
      role={status.phase === "failed" ? "alert" : "status"}
      className={cn(
        "flex items-center gap-3 rounded-2xl border px-4 py-3",
        overlay ? "h-full" : "mb-3",
        status.phase === "failed"
          ? "border-destructive/30 bg-destructive/5"
          : "border-border/60 bg-raised",
      )}
    >
      {status.phase === "loading" && <Spinner className="text-foreground" />}
      <div className="min-w-0 flex-1">
        <p className="text-sm font-medium">{status.title}</p>
        <p className="text-xs text-muted-foreground">{status.detail}</p>
      </div>
      {status.action && (
        <Button size="sm" variant="outline" disabled={busy} onClick={() => void act()}>
          {busy
            ? BUSY[status.action as keyof typeof BUSY]
            : LABEL[status.action as keyof typeof LABEL]}
        </Button>
      )}
    </div>
  );
  return placed(card);
}
