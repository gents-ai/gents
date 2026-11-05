import { useState } from "react";
import { ChevronDown, UserPlus } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { FieldRows } from "@gents/ui/components/field";
import { cn } from "@gents/ui/lib/utils";
import { DEFAULT_LEASE_SECONDS, type JoinRequest } from "../../../hooks/sharing";
import { approvedHere } from "@/app/approvedHere";
import { useSharingActions } from "@/hooks/useSharing";
import { fullTime, shortTime } from "../TranscriptMarker";
import {
  errorMessage,
  ItemRow,
  LeaseSelect,
  Party,
  partyName,
  scopeText,
} from "./parts";

/** The requests waiting for a decision, as one strip at the top of the
    section: its count, opening to each request with its lease, Approve and
    Deny. Nothing shows while nobody is waiting. */
export function RequestsStrip({
  nodeDid,
  requests,
}: {
  nodeDid: string;
  requests: JoinRequest[];
}) {
  const [open, setOpen] = useState(true);
  if (requests.length === 0) return null;
  const count =
    requests.length === 1 ? "1 request to join" : `${requests.length} requests to join`;
  return (
    <section aria-label="Requests to join" className="mb-6">
      <FieldRows>
        <button
          type="button"
          aria-expanded={open}
          onClick={() => setOpen((was) => !was)}
          className="flex w-full items-center gap-2.5 px-5 py-3 text-left text-sm font-medium outline-none hover:bg-accent/50 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
        >
          <UserPlus aria-hidden className="size-4 shrink-0 text-muted-foreground" />
          <span className="flex-1">{count}</span>
          <ChevronDown
            aria-hidden
            className={cn(
              "size-4 shrink-0 text-muted-foreground transition-transform",
              !open && "-rotate-90",
            )}
          />
        </button>
        {open && (
          <div role="list" aria-label="Requests" className="divide-y divide-border/60">
            {requests.map((request) => (
              <RequestRow key={request.requestId} nodeDid={nodeDid} request={request} />
            ))}
          </div>
        )}
      </FieldRows>
    </section>
  );
}

function RequestRow({ nodeDid, request }: { nodeDid: string; request: JoinRequest }) {
  const sharing = useSharingActions();
  const [lease, setLease] = useState(String(DEFAULT_LEASE_SECONDS));
  const [busy, setBusy] = useState<"approve" | "deny" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const { from, profile, scope } = request;
  const who = partyName(from);
  const run = async (kind: "approve" | "deny") => {
    setBusy(kind);
    setError(null);
    try {
      if (kind === "approve") {
        const seconds = Number(lease);
        await sharing.approve(nodeDid, request.requestId, seconds);
        const now = Date.now();
        approvedHere.add(nodeDid, {
          requestId: request.requestId,
          party: from,
          scope,
          profile,
          leaseExpiresAt: new Date(now + seconds * 1000).toISOString(),
          approvedAt: new Date(now).toISOString(),
        });
      } else await sharing.deny(nodeDid, request.requestId);
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setBusy(null);
    }
  };
  const asked = request.requestedAt ? shortTime(request.requestedAt) : null;
  const expires = shortTime(request.expiresAt);
  return (
    <ItemRow
      label={`Request from ${who}`}
      title={<Party party={from} />}
      description={
        <>
          <span>{scopeText(scope)}</span>
          {profile.name && <span>{profile.name}</span>}
          {asked && request.requestedAt && (
            <span title={fullTime(request.requestedAt)}>Asked {asked}</span>
          )}
          {expires && (
            <span title={fullTime(request.expiresAt)}>Expires {expires}</span>
          )}
        </>
      }
      actions={
        <>
          <LeaseSelect
            value={lease}
            onChange={setLease}
            label={`Access for ${who} lasts`}
            disabled={busy !== null}
          />
          <Button
            size="sm"
            disabled={busy !== null}
            onClick={() => void run("approve")}
          >
            {busy === "approve" ? "Approving…" : "Approve"}
          </Button>
          <Button
            size="sm"
            variant="outline"
            disabled={busy !== null}
            onClick={() => void run("deny")}
          >
            {busy === "deny" ? "Denying…" : "Deny"}
          </Button>
        </>
      }
      error={error}
    />
  );
}
