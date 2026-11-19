import { useEffect, useState } from "react";
import type {
  ClientAccessRequestView,
  RuntimeAccessDecisionRequest,
} from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { useApp } from "@/app/AppContext";

export function ClientAccess({ nodeDid }: { nodeDid: string }) {
  const { actions } = useApp();
  const [rows, setRows] = useState<ClientAccessRequestView[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [revision, refresh] = useState(0);
  useEffect(() => {
    let current = true;
    setRows(null);
    setError(null);
    actions.listRuntimeAccess(nodeDid).then(
      (next) => {
        if (current) setRows(next);
      },
      (cause) => {
        if (current) setError(String(cause));
      },
    );
    return () => {
      current = false;
    };
  }, [actions, nodeDid, revision]);
  async function decide(
    row: ClientAccessRequestView,
    decision: RuntimeAccessDecisionRequest["decision"],
  ) {
    setBusy(row.requestId);
    setError(null);
    try {
      const next = await actions.decideRuntimeAccess({
        nodeDid,
        requestId: row.requestId,
        decision,
      });
      setRows(
        (before) =>
          before?.map((entry) => (entry.requestId === next.requestId ? next : entry)) ??
          null,
      );
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(null);
    }
  }
  return (
    <section aria-label="Device access" className="mt-4 grid min-w-0 gap-3">
      <div className="flex items-center justify-between gap-2">
        <h3 className="text-sm font-medium">Device access</h3>
        <Button
          variant="quiet"
          size="sm"
          disabled={busy !== null}
          onClick={() => refresh((n) => n + 1)}
        >
          Refresh
        </Button>
      </div>
      <p className="text-xs text-muted-foreground">
        Approved devices can administer this node. Revoking access stops their future
        requests.
      </p>
      {error && (
        <p role="alert" className="break-words text-sm text-destructive">
          {error}
        </p>
      )}
      {rows === null && !error && (
        <p role="status" className="text-sm text-muted-foreground">
          Loading device access…
        </p>
      )}
      {rows?.length === 0 && (
        <p className="text-sm text-muted-foreground">No device access requests.</p>
      )}
      {rows?.map((row) => (
        <div
          key={row.requestId}
          className="flex min-w-0 flex-wrap items-center gap-2 border-t border-border/60 pt-3"
        >
          <div className="min-w-0 flex-1">
            <p className="truncate font-mono text-xs" title={row.clientDid}>
              {row.clientDid}
            </p>
            <p className="text-xs text-muted-foreground">{row.state}</p>
          </div>
          {row.state === "pending" && (
            <>
              <Button
                size="sm"
                disabled={busy !== null}
                onClick={() => void decide(row, "approve")}
              >
                Approve
              </Button>
              <Button
                variant="outline"
                size="sm"
                disabled={busy !== null}
                onClick={() => void decide(row, "reject")}
              >
                Deny
              </Button>
            </>
          )}
          {row.state === "approved" && (
            <Button
              variant="outline"
              size="sm"
              disabled={busy !== null}
              onClick={() => void decide(row, "revoke")}
            >
              Revoke
            </Button>
          )}
        </div>
      ))}
    </section>
  );
}
