import { useState } from "react";
import { Button } from "@gents/ui/components/button";
import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";
import { navigate } from "@/lib/router";
import { toastFailure } from "@/lib/failure";
import { useApp, useView } from "@/app/AppContext";
import { useSelectedAgentDid } from "@/hooks/useClient";

const LABEL = {
  retryLocal: "Try again",
  retryHydration: "Try again",
  configureInference: "Configure inference",
};
const BUSY = {
  retryLocal: "Retrying…",
  retryHydration: "Retrying…",
  configureInference: "Opening…",
};

export function LoadingStatus() {
  const status = useView((view) => view.loadingStatus);
  const {
    stores,
    actions: { refreshSnapshot, retrySessionHydration },
  } = useApp();
  const agentDid = useSelectedAgentDid();
  const selectedSessionId = stores.selection.use.sessionId();
  const [busy, setBusy] = useState(false);
  if (!status) return null;
  const act = async () => {
    const action = status.action;
    if (!action) return;
    if (action === "configureInference") {
      navigate(
        agentDid
          ? { name: "agent", agentDid, section: "inference" }
          : { name: "agents" },
      );
      return;
    }
    setBusy(true);
    try {
      if (action === "retryHydration") await retrySessionHydration(selectedSessionId);
      else await refreshSnapshot();
    } catch (e) {
      toastFailure("load the session", e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div
      role={status.phase === "failed" ? "alert" : "status"}
      className={cn(
        "mb-3 flex items-center gap-3 rounded-2xl border px-4 py-3",
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
}
