import { useEffect, useRef, useState } from "react";
import {
  asBridgeErrorPayload,
  type ManagedServerStatus,
} from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@gents/ui/components/alert-dialog";
import { useApp } from "@/app/AppContext";
import { ManagedRuntimeAuthorityReview } from "@/components/ManagedRuntimeAuthority";
import { authoritiesEqual } from "@/lib/managedRuntimeAuthority";
import { errorText } from "../../../lib/errorText";

const restartable = (status: ManagedServerStatus | null, nodeDid: string) =>
  status?.nodeDid === nodeDid &&
  Boolean(status.nodeName && status.effectiveToolCeiling) &&
  ["running", "stopped", "failed"].includes(status.state);

export function StorageFullRecovery({
  nodeDid,
  error,
}: {
  nodeDid: string;
  error: unknown;
}) {
  const app = useApp();
  const status = app.stores.localServer.use.status();
  const operation = app.stores.localServer.use.operation();
  const [review, setReview] = useState<{
    status: ManagedServerStatus;
    intent: number;
    clientDid: string | undefined;
  } | null>(null);
  const [reviewOpen, setReviewOpen] = useState(false);
  const [busy, setBusy] = useState<"checking" | "restarting" | null>(null);
  const [recoveredError, setRecoveredError] = useState<unknown>();
  const recovered = recoveredError === error;
  const [failure, setFailure] = useState("");
  const running = useRef(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  if (asBridgeErrorPayload(error)?.code !== "storageFull") return null;

  const clientDid = () => app.stores.client.getState().snapshot?.client?.clientDid;
  const current = (intent: number, actor: string | undefined) =>
    mounted.current &&
    actor === clientDid() &&
    app.actions.acceptsComposeIntent(intent);
  const unavailable =
    "Restart this node on its host after freeing disk space, then check the saved message's delivery. Its original identity is retained.";

  async function inspect() {
    if (running.current || operation) return;
    const intent = app.actions.captureComposeIntent();
    const actor = clientDid();
    running.current = true;
    setBusy("checking");
    setFailure("");
    try {
      const next = await app.actions.refreshLocalServer();
      if (!current(intent, actor)) return;
      if (!restartable(next, nodeDid)) throw new Error(unavailable);
      setReview({ status: next!, intent, clientDid: actor });
      setReviewOpen(true);
    } catch (cause) {
      if (current(intent, actor)) setFailure(errorText(cause));
    } finally {
      running.current = false;
      if (mounted.current) setBusy(null);
    }
  }

  async function restart(confirmed: NonNullable<typeof review>) {
    if (running.current || !current(confirmed.intent, confirmed.clientDid)) return;
    running.current = true;
    setBusy("restarting");
    setFailure("");
    setReviewOpen(false);
    try {
      const next = await app.actions.refreshLocalServer();
      if (!current(confirmed.intent, confirmed.clientDid)) return;
      if (!restartable(next, nodeDid) || app.stores.localServer.getState().operation)
        throw new Error(unavailable);
      const before = confirmed.status;
      const authority = {
        toolCeiling: before.effectiveToolCeiling!,
        toolRoot: before.effectiveToolRoot,
      };
      if (
        next!.nodeName !== before.nodeName ||
        !authoritiesEqual(authority, {
          toolCeiling: next!.effectiveToolCeiling!,
          toolRoot: next!.effectiveToolRoot,
        })
      )
        throw new Error(
          "The runtime or its access changed. Review restart options again.",
        );
      const restarted = await app.actions.restartLocalServer(
        before.nodeName!,
        authority,
      );
      if (!current(confirmed.intent, confirmed.clientDid)) return;
      if (
        restarted.nodeDid !== nodeDid ||
        restarted.state !== "running" ||
        Boolean(restarted.error) ||
        !restarted.effectiveToolCeiling ||
        !authoritiesEqual(authority, {
          toolCeiling: restarted.effectiveToolCeiling,
          toolRoot: restarted.effectiveToolRoot,
        })
      )
        throw new Error(
          "The restart did not confirm the same node and access. Check runtime settings before continuing.",
        );
      setRecoveredError(error);
      await app.actions.refreshSnapshot();
    } catch (cause) {
      if (current(confirmed.intent, confirmed.clientDid)) setFailure(errorText(cause));
    } finally {
      running.current = false;
      if (mounted.current) setBusy(null);
    }
  }

  const canInspect =
    app.actions.localServerOffers.status &&
    app.actions.localServerOffers.restart &&
    (!status || status.nodeDid === nodeDid) &&
    status?.state !== "external";
  return (
    <div className="mt-2 border-t border-border/60 pt-2" aria-label="Storage recovery">
      <p className="text-muted-foreground">
        {recovered
          ? "The runtime restarted. Check delivery to recover the original message; restarting did not resend it."
          : "This node ran out of disk space. Free space on its host, then restart its runtime to reopen the same history. Message delivery is still unconfirmed."}
      </p>
      {!recovered &&
        (canInspect ? (
          <Button
            variant="ghost"
            size="sm"
            className="mt-1"
            disabled={Boolean(busy) || operation !== null}
            onClick={() => void inspect()}
          >
            {busy === "restarting"
              ? "Restarting runtime…"
              : busy
                ? "Checking runtime…"
                : "Review runtime restart…"}
          </Button>
        ) : (
          <p className="mt-1 text-muted-foreground">{unavailable}</p>
        ))}
      {busy && (
        <p role="status" className="mt-1 text-muted-foreground">
          Recovering the affected runtime…
        </p>
      )}
      {failure && (
        <p role="alert" className="mt-1 text-destructive">
          {failure}
        </p>
      )}
      <AlertDialog open={reviewOpen} onOpenChange={setReviewOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Restart the affected runtime?</AlertDialogTitle>
            <AlertDialogDescription>
              After freeing disk space, restart {review?.status.nodeName} with its
              existing access. This can interrupt work on that node. Its history and the
              saved message remain intact; restart does not resend the message.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <p className="break-all font-mono text-xs text-muted-foreground">{nodeDid}</p>
          {review?.status.effectiveToolCeiling && (
            <ManagedRuntimeAuthorityReview
              authority={{
                toolCeiling: review.status.effectiveToolCeiling,
                toolRoot: review.status.effectiveToolRoot,
              }}
            />
          )}
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                if (review) void restart(review);
              }}
            >
              Restart runtime
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}
