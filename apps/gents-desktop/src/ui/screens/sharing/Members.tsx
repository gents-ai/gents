import { useThisAppDid } from "@/hooks/useClient";
import { useState } from "react";
import { Button } from "@gents/ui/components/button";
import {
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
} from "@gents/ui/components/dropdown-menu";
import { cn } from "@gents/ui/lib/utils";
import { DEFAULT_LEASE_SECONDS, type SharingMember } from "../../../hooks/sharing";
import { useSharingActions } from "@/hooks/useSharing";
import { fullTime } from "../TranscriptMarker";
import { span, useMinute } from "../time";
import {
  CopyButton,
  DAY_SECONDS,
  errorMessage,
  ItemRow,
  LeaseSelect,
  MoreMenu,
  Party,
  partyName,
  scopeInSentence,
  scopeText,
  shortDid,
} from "./parts";

/* access that ends within this long is worth a look */
const SOON_MS = 7 * DAY_SECONDS * 1000;

/** One grant someone holds: who, what it covers, with which profile and
    until when. Renewing, where the runtime can, restarts the lease from now;
    revoking asks here first, then ends the grant at once. */
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
  /* this app's own way into the node: revoking it cuts the app off */
  const thisApp = useThisAppDid() === party.did;
  const who = thisApp ? "This app" : partyName(party);
  const what = scopeText(scope);
  const renewable = sharing.renew && member.leaseExpiresAt !== null;

  async function run(kind: "renew" | "revoke") {
    setBusy(true);
    setError(null);
    try {
      if (kind === "renew")
        await sharing.renew?.(nodeDid, member.requestId, Number(lease));
      else await sharing.revoke(nodeDid, member.requestId);
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
      title={
        thisApp ? (
          <span
            className="truncate text-sm font-medium"
            title="How this app reaches the node. It can't be revoked from here."
          >
            This app
          </span>
        ) : (
          <Party party={party} />
        )
      }
      description={
        <>
          {thisApp && (
            <span className="inline-flex min-w-0 items-center gap-0.5">
              <span className="truncate font-mono" title={party.did}>
                {shortDid(party.did)}
              </span>
              <CopyButton
                text={party.did}
                label={`Copy ${party.did}`}
                title="Copy DID"
              />
            </span>
          )}
          <span>{what}</span>
          {profile.name && <span>{profile.name}</span>}
          <Lease ends={member.leaseExpiresAt} />
        </>
      }
      actions={
        asking === null &&
        !thisApp && (
          <MoreMenu label={`More for ${who}: ${what}`}>
            {renewable && (
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
            what={scopeInSentence(scope)}
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
      className="flex flex-wrap items-center gap-x-4 gap-y-2 text-sm"
    >
      <span className="min-w-0 flex-1">
        Revoke {who}'s access to {what}? It ends right away.
      </span>
      <span className="ml-auto flex shrink-0 gap-2">
        <Button size="sm" variant="outline" disabled={busy} onClick={onKeep}>
          Keep
        </Button>
        <Button size="sm" variant="destructive" disabled={busy} onClick={onRevoke}>
          {busy ? "Revoking…" : "Revoke"}
        </Button>
      </span>
    </div>
  );
}
