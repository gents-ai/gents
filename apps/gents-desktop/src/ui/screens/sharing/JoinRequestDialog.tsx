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
import { fullTime, shortTime } from "../TranscriptMarker";
import { errorMessage, LeaseSelect, Party, partyName, scopeInSentence } from "./parts";

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
  if (!open) return <Dialog open={false} />;
  const { nodeDid, nodeName, request } = open;
  const who = partyName(request.from);
  const gone =
    waiting !== null &&
    waiting.nodeDid === nodeDid &&
    !waiting.ids.has(request.requestId);
  const run = async (kind: "approve" | "deny") => {
    setBusy(kind);
    setError(null);
    try {
      if (kind === "approve")
        await sharing.approve(nodeDid, request.requestId, Number(lease));
      else await sharing.deny(nodeDid, request.requestId);
      closeReview();
      setLease(String(DEFAULT_LEASE_SECONDS));
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setBusy(null);
    }
  };
  return (
    <Dialog
      open
      onOpenChange={(next) => {
        if (!next && busy === null) closeReview();
      }}
    >
      <DialogContent className="sm:max-w-lg" data-testid="join-request-dialog">
        <DialogHeader>
          <DialogTitle>
            {who} asks to join {nodeName}
          </DialogTitle>
          <DialogDescription>
            Nothing is shared until you approve. A request lasts only a few minutes.
          </DialogDescription>
        </DialogHeader>
        <dl className="grid grid-cols-[auto_minmax(0,1fr)] items-center gap-x-6 gap-y-2 text-sm">
          <dt className="text-muted-foreground">From</dt>
          <dd className="min-w-0">
            <Party party={request.from} />
          </dd>
          <dt className="text-muted-foreground">They'd get</dt>
          <dd>
            {scopeInSentence(request.scope).replace(/^the /, "The ")}
            {request.profile.name ? `, as ${request.profile.name}` : ""}
          </dd>
          <dt className="text-muted-foreground">How</dt>
          <dd>{request.inviteId ? "With an invite you made" : "No invite"}</dd>
          {request.requestedAt && (
            <>
              <dt className="text-muted-foreground">Asked</dt>
              <dd title={fullTime(request.requestedAt)}>
                {shortTime(request.requestedAt)}
              </dd>
            </>
          )}
          <dt className="text-muted-foreground">Expires</dt>
          <dd title={fullTime(request.expiresAt)}>{shortTime(request.expiresAt)}</dd>
          <dt className="text-muted-foreground">Access lasts</dt>
          <dd>
            <LeaseSelect
              value={lease}
              onChange={setLease}
              label={`Access for ${who} lasts`}
              disabled={busy !== null || gone}
            />
          </dd>
        </dl>
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
        <DialogFooter className="sm:justify-between">
          <Button
            variant="ghost"
            disabled={busy !== null}
            onClick={() => {
              closeReview();
              navigate({ name: "agent", nodeDid, section: SHARING_SECTION });
            }}
          >
            Open Sharing
          </Button>
          {gone ? (
            <Button onClick={closeReview}>Close</Button>
          ) : (
            <span className="flex gap-2">
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
            </span>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
