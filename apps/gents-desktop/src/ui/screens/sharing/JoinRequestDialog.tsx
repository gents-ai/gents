import { useState } from "react";
import { useStore } from "zustand";
import { createStore } from "zustand/vanilla";
import { Button } from "@gents/ui/components/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@gents/ui/components/dialog";
import { DEFAULT_LEASE_SECONDS, type JoinRequest } from "../../../hooks/sharing";
import { useSharingActions } from "@/hooks/useSharing";
import { SHARING_SECTION } from "@/lib/joinInvite";
import { navigate } from "@/lib/router";
import { fullTime } from "../TranscriptMarker";
import { CopyButton, errorMessage, LeaseSelect, partyName, shortDid } from "./parts";
import { MonitorSmartphone } from "lucide-react";
import { cn } from "@gents/ui/lib/utils";
import { useMinute } from "../time";
import { sayApproved, sayDenied } from "./decided";

const reviewing = createStore<{
  nodeDid: string;
  nodeName: string;
  request: JoinRequest;
} | null>(() => null);

/* which requests the node still has waiting, as last read: a request can
   expire or be decided elsewhere while its dialog is open */
const waitingNow = createStore<{ nodeDid: string; ids: ReadonlySet<string> } | null>(
  () => null,
);
export const noteWaiting = (nodeDid: string, requests: readonly JoinRequest[]) =>
  waitingNow.setState(
    { nodeDid, ids: new Set(requests.map((r) => r.requestId)) },
    true,
  );

/** Opens the quick decision on one request to join, over whatever the
    person is doing. */
export const reviewJoinRequest = (
  nodeDid: string,
  nodeName: string,
  request: JoinRequest,
) => reviewing.setState({ nodeDid, nodeName, request }, true);

const closeReview = () => reviewing.setState(null, true);

/** test seam */
export const resetJoinRequestReview = () => {
  closeReview();
  waitingNow.setState(null, true);
};

/** One request to join, decided where the person is: who asks, what they'd
    get and for how long, with Approve and Deny. The Sharing section has the
    whole picture. */
export function JoinRequestDialog() {
  const open = useStore(reviewing);
  const waiting = useStore(waitingNow);
  const sharing = useSharingActions();
  const [lease, setLease] = useState(String(DEFAULT_LEASE_SECONDS));
  const [busy, setBusy] = useState<"approve" | "deny" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const minute = useMinute();
  if (!open) return <Dialog open={false} />;
  const { nodeDid, nodeName, request } = open;
  /* named honestly until a request carries its device's own name */
  const who = request.from.name ?? "A device";
  const gone =
    waiting !== null &&
    waiting.nodeDid === nodeDid &&
    !waiting.ids.has(request.requestId);
  const run = async (kind: "approve" | "deny") => {
    setBusy(kind);
    setError(null);
    try {
      if (kind === "approve") {
        await sharing.approve(nodeDid, request.requestId, Number(lease));
        sayApproved(nodeName, Number(lease), () =>
          sharing.revoke(nodeDid, request.requestId),
        );
      } else {
        await sharing.deny(nodeDid, request.requestId);
        sayDenied();
      }
      closeReview();
      setLease(String(DEFAULT_LEASE_SECONDS));
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setBusy(null);
    }
  };
  const left = Date.parse(request.expiresAt) - minute;
  const expires = gone
    ? null
    : left < 60_000
      ? "Expires in under a minute"
      : `Expires in ${Math.round(left / 60_000)} min`;
  return (
    <Dialog
      open
      onOpenChange={(next) => {
        if (!next && busy === null) closeReview();
      }}
    >
      <DialogContent className="sm:max-w-lg" data-testid="join-request-dialog">
        <DialogHeader>
          <div className="flex items-center gap-3">
            <span
              aria-hidden
              className="grid size-10 shrink-0 place-items-center rounded-full bg-muted text-muted-foreground"
            >
              <MonitorSmartphone className="size-5" />
            </span>
            <div className="grid min-w-0 gap-0.5">
              <DialogTitle>
                {who} asks to join {nodeName}
              </DialogTitle>
              <DialogDescription className="flex flex-wrap items-center gap-x-1 text-xs">
                <span
                  className={cn(left < 60_000 && !gone && "text-warning")}
                  title={fullTime(request.expiresAt)}
                >
                  {gone ? "No longer waiting" : expires}
                </span>
                <span aria-hidden>·</span>
                <span className="flex min-w-0 items-center gap-0.5">
                  from <span className="font-mono">{shortDid(request.from.did)}</span>
                  <CopyButton
                    text={request.from.did}
                    label={`Copy ${request.from.did}`}
                    title="Copy DID"
                  />
                </span>
              </DialogDescription>
            </div>
          </div>
        </DialogHeader>

        <div className="grid gap-1.5 rounded-lg border border-border bg-muted/40 px-3.5 py-3 text-sm">
          <b className="font-medium">They'd be able to</b>
          <span>{ableTo(request, nodeName)}</span>
          <b className="mt-1 font-medium">They'd see</b>
          <span>{seeing(request, nodeName)}</span>
          <span className="mt-1 flex flex-wrap items-center gap-2 text-muted-foreground">
            For
            <LeaseSelect
              value={lease}
              onChange={setLease}
              label={`Access for ${partyName(request.from)} lasts`}
              disabled={busy !== null || gone}
            />
            {request.inviteId ? "· with an invite you made" : "· no invite"}
          </span>
        </div>

        {request.scope.kind === "node" && !gone && (
          <p className="text-sm text-warning">
            That's the whole node. Approve it only for a device of your own.
          </p>
        )}
        {gone && (
          <p role="status" className="text-sm text-muted-foreground">
            This request isn't waiting any more: it expired, or was decided elsewhere.
          </p>
        )}
        {error && !gone && (
          <p role="alert" className="text-sm break-words text-destructive">
            {error}
          </p>
        )}
        <p className="text-xs text-muted-foreground">
          Nothing is shared until you approve.{" "}
          <button
            type="button"
            className="cursor-pointer underline-offset-2 hover:text-foreground hover:underline"
            onClick={() => {
              closeReview();
              navigate({ name: "agent", nodeDid, section: SHARING_SECTION });
            }}
          >
            Open Sharing
          </button>
        </p>

        <DialogFooter className="flex-row items-center justify-between sm:justify-between">
          {gone ? (
            <>
              <span />
              <Button onClick={closeReview}>Close</Button>
            </>
          ) : (
            <>
              <Button
                variant="outline"
                disabled={busy !== null}
                onClick={() => void run("deny")}
              >
                {busy === "deny" ? "Denying…" : "Deny"}
              </Button>
              <Button disabled={busy !== null} onClick={() => void run("approve")}>
                {busy === "approve" ? "Approving…" : "Approve"}
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/* what approving would let them do, in words */
function ableTo(request: JoinRequest, nodeName: string) {
  const { scope } = request;
  if (scope.kind === "node")
    return `Use all of ${nodeName}: its agents, settings and sessions, and send requests to any agent.`;
  if (scope.kind === "pack")
    return `Use the ${scope.name} pack on ${nodeName}, and nothing else.`;
  return `Start sessions with the ${scope.name} agent on ${nodeName}, and nothing else.`;
}

/* what of this node they'd see */
function seeing(request: JoinRequest, nodeName: string) {
  const { scope } = request;
  if (scope.kind === "node")
    return `Everything on ${nodeName}, including every session.`;
  if (scope.kind === "pack")
    return `The ${scope.name} pack's data, and nothing else of ${nodeName}.`;
  return `The sessions they start with the ${scope.name} agent.`;
}
