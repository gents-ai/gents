import { useState } from "react";
import { Badge } from "@gents/ui/components/badge";
import { Button } from "@gents/ui/components/button";
import {
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
} from "@gents/ui/components/dropdown-menu";
import { cn } from "@gents/ui/lib/utils";
import { DEFAULT_LEASE_SECONDS, type SharingMember } from "../../../hooks/sharing";
import { approvedHere, type ApprovedHere } from "@/app/approvedHere";
import { useSharingActions } from "@/hooks/useSharing";
import { Hint } from "../Hint";
import { fullTime } from "../TranscriptMarker";
import { span, useMinute } from "../time";
import {
  DAY_SECONDS,
  errorMessage,
  ItemRow,
  LeaseSelect,
  MoreMenu,
  Party,
  partyName,
  scopeText,
} from "./parts";

/* access that ends within this long is worth a look */
const SOON_MS = 7 * DAY_SECONDS * 1000;

export const APPROVED_HERE_HINT =
  "This runtime can't list who has access yet; this device remembers the ones you approved.";

/** One grant someone holds: who, what it covers, with which profile and
    until when. Renewing restarts the lease from now; revoking asks here
    first, then ends the grant at once. */
export function MemberRow({
  nodeDid,
  member,
}: {
  nodeDid: string;
  member: SharingMember;
}) {
  const sharing = useSharingActions();
  const [asking, setAsking] = useState<"renew" | "revoke" | null>(null);
  const [lease, setLease] = useState(String(DEFAULT_LEASE_SECONDS));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { party, scope, profile } = member;
  const who = partyName(party);
  const what = scopeText(scope);

  async function run(kind: "renew" | "revoke") {
    setBusy(true);
    setError(null);
    try {
      if (kind === "renew")
        await sharing.renew(nodeDid, party.did, scope, Number(lease));
      else await sharing.revoke(nodeDid, party.did, scope);
      setAsking(null);
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setBusy(false);
    }
  }

  return (
    <ItemRow
      label={`${who}: ${what}`}
      title={<Party party={party} />}
      description={
        <>
          <span>{what}</span>
          {profile.name && <span>{profile.name}</span>}
          <Lease ends={member.leaseExpiresAt} />
        </>
      }
      actions={
        asking === null && (
          <MoreMenu label={`More for ${who}: ${what}`}>
            {member.renewable && (
              <>
                <DropdownMenuGroup>
                  <DropdownMenuItem onClick={() => setAsking("renew")}>
                    Renew…
                  </DropdownMenuItem>
                </DropdownMenuGroup>
                <DropdownMenuSeparator />
              </>
            )}
            <DropdownMenuGroup>
              <DropdownMenuItem
                variant="destructive"
                onClick={() => setAsking("revoke")}
              >
                Revoke…
              </DropdownMenuItem>
            </DropdownMenuGroup>
          </MoreMenu>
        )
      }
      below={
        asking === "renew" ? (
          <div className="flex flex-wrap items-center gap-2 text-sm">
            <span className="text-muted-foreground">Renew for</span>
            <LeaseSelect
              value={lease}
              onChange={setLease}
              label={`Renew ${who}'s access for`}
              disabled={busy}
            />
            <span className="text-muted-foreground">from now</span>
            <Button size="sm" disabled={busy} onClick={() => void run("renew")}>
              {busy ? "Renewing…" : "Renew"}
            </Button>
            <Button
              size="sm"
              variant="ghost"
              disabled={busy}
              onClick={() => setAsking(null)}
            >
              Cancel
            </Button>
          </div>
        ) : asking === "revoke" ? (
          <RevokeAsk
            who={who}
            what={what}
            busy={busy}
            onRevoke={() => void run("revoke")}
            onKeep={() => setAsking(null)}
          />
        ) : null
      }
      error={error}
      inline
    />
  );
}

/** An approval this desktop made that the runtime can't list yet: shown as
    access, marked as only this device's memory of it. It can be revoked by
    its request where the runtime allows that. */
export function ApprovedHereRow({
  nodeDid,
  entry,
}: {
  nodeDid: string;
  entry: ApprovedHere;
}) {
  const sharing = useSharingActions();
  const [asking, setAsking] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { party, scope, profile } = entry;
  const who = partyName(party);
  const what = scopeText(scope);
  const revocable = Boolean(sharing.revokeApproval);

  async function revoke() {
    setBusy(true);
    setError(null);
    try {
      await sharing.revokeApproval?.(nodeDid, entry.requestId);
      approvedHere.forget(nodeDid, entry.requestId);
    } catch (cause) {
      setError(errorMessage(cause));
      setBusy(false);
    }
  }

  return (
    <ItemRow
      label={`${who}: ${what}`}
      title={
        <span className="flex min-w-0 items-center gap-2">
          <Party party={party} />
          <Hint label={APPROVED_HERE_HINT}>
            <Badge variant="outline" tabIndex={0} className="shrink-0">
              Approved here
            </Badge>
          </Hint>
        </span>
      }
      description={
        <>
          <span>{what}</span>
          {profile.name && <span>{profile.name}</span>}
          <Lease ends={entry.leaseExpiresAt} />
        </>
      }
      actions={
        revocable &&
        !asking && (
          <MoreMenu label={`More for ${who}: ${what}`}>
            <DropdownMenuGroup>
              <DropdownMenuItem variant="destructive" onClick={() => setAsking(true)}>
                Revoke…
              </DropdownMenuItem>
            </DropdownMenuGroup>
          </MoreMenu>
        )
      }
      below={
        asking && (
          <RevokeAsk
            who={who}
            what={what}
            busy={busy}
            onRevoke={() => void revoke()}
            onKeep={() => setAsking(false)}
          />
        )
      }
      error={error}
      inline
    />
  );
}

/* until when access lasts, in warning colour once it ends within a week */
function Lease({ ends }: { ends: string | null }) {
  const now = useMinute();
  if (ends === null) return <span>Doesn't lapse</span>;
  const left = Date.parse(ends) - now;
  return (
    <span title={fullTime(ends)} className={cn(left < SOON_MS && "text-warning")}>
      {left > 0 ? `Ends in ${span(left)}` : "Ended"}
    </span>
  );
}

function RevokeAsk({
  who,
  what,
  busy,
  onRevoke,
  onKeep,
}: {
  who: string;
  what: string;
  busy: boolean;
  onRevoke: () => void;
  onKeep: () => void;
}) {
  return (
    <div
      role="group"
      aria-label={`Revoke ${who}'s access`}
      className="flex flex-wrap items-center gap-2 text-sm"
    >
      <span>
        Revoke {who}'s access to {what}? It ends right away.
      </span>
      <Button size="sm" variant="destructive" disabled={busy} onClick={onRevoke}>
        {busy ? "Revoking…" : "Revoke"}
      </Button>
      <Button size="sm" variant="ghost" disabled={busy} onClick={onKeep}>
        Keep
      </Button>
    </div>
  );
}
