/* Stopping a request that has children: the desktop previews the cascade
   first and asks. Confirm sends the interrupt with the preview's
   signature; if the tree changed meanwhile the bridge hands back a new
   preview and the person confirms again. */
import { useEffect, useState } from "react";
import type { CascadeCancelPreview } from "@source-inc/gents-desktop-client";
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
import { Spinner } from "@gents/ui/components/spinner";
import type { Shell } from "@/hooks/useShell";

export function CascadeDialog({
  shell,
  requestId,
  onClose,
  onResult,
}: {
  shell: Shell;
  requestId: string | null;
  onClose: () => void;
  onResult: (text: string) => void;
}) {
  const [preview, setPreview] = useState<CascadeCancelPreview | null>(null);
  const [changed, setChanged] = useState(false);
  const [busy, setBusy] = useState(false);
  const agentDid = shell.selectedAgentDid;
  const api = shell.api;
  useEffect(() => {
    if (!requestId) return;
    let gone = false;
    api
      .previewInterruptCascade({ requestId, agentDid, includeTerminal: true })
      .then((p) => {
        if (!gone) setPreview(p);
      })
      .catch((e: unknown) => {
        onResult(`Couldn't interrupt: ${String(e)}`);
        onClose();
      });
    return () => {
      gone = true;
    };
  }, [api, requestId, agentDid, onClose, onResult]);

  const confirm = async () => {
    if (!preview || !requestId) return;
    setBusy(true);
    try {
      const r = await api.interruptRequest({
        requestId,
        agentDid,
        cause: "userCancelled",
        cascade: true,
        expectedPreviewSignature: preview.previewSignature,
      });
      if (r.stalePreview && r.preview) {
        setPreview(r.preview);
        setChanged(true);
        return;
      }
      onResult(r.alreadyInterrupted ? "Already interrupted" : "Interrupt requested");
      onClose();
    } catch (e) {
      onResult(`Couldn't interrupt: ${String(e)}`);
      onClose();
    } finally {
      setBusy(false);
    }
  };

  const count =
    (preview?.willInterrupt.length ?? 0) + (preview?.willDetach.length ?? 0);
  return (
    <AlertDialog open={requestId !== null} onOpenChange={(o) => !o && onClose()}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Stop this request and its children?</AlertDialogTitle>
          <AlertDialogDescription>
            {!preview ? (
              <span className="inline-flex items-center gap-2">
                <Spinner /> Looking at what is running…
              </span>
            ) : (
              <>
                {changed && (
                  <span className="block text-foreground">
                    The tree changed; check again.
                  </span>
                )}
                {count === 0
                  ? "Nothing else is running under it."
                  : `${preview.willInterrupt.length} running child request${preview.willInterrupt.length === 1 ? "" : "s"} will be interrupted${preview.willDetach.length ? `, ${preview.willDetach.length} detached` : ""}.`}
              </>
            )}
          </AlertDialogDescription>
        </AlertDialogHeader>
        {preview && preview.willInterrupt.length > 0 && (
          <ul className="grid gap-1 text-sm">
            {preview.willInterrupt.map((c) => (
              <li key={c.requestId} className="flex items-center justify-between gap-3">
                <span className="truncate">{c.toolName ?? c.requestId}</span>
                <span className="font-mono text-[11px] text-muted-foreground">
                  {c.requestId}
                </span>
              </li>
            ))}
          </ul>
        )}
        <AlertDialogFooter>
          <AlertDialogCancel disabled={busy}>Keep running</AlertDialogCancel>
          <AlertDialogAction disabled={!preview || busy} onClick={confirm}>
            {busy ? <Spinner /> : null} Stop all
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
