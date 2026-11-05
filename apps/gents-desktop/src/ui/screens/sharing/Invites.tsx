import { useState } from "react";
import {
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
} from "@gents/ui/components/dropdown-menu";
import type { Invite } from "../../../hooks/sharing";
import { useSharingActions } from "@/hooks/useSharing";
import { fullTime } from "../TranscriptMarker";
import { span, useMinute } from "../time";
import { InviteDetails, usesText } from "./InviteDialog";
import { errorMessage, ItemRow, kindText, MoreMenu, scopeText } from "./parts";

/** An open invite: who it's for, what it shares, how long and how often it
    still works. Its link, code and QR code show again on request; cancelling
    stops it working for anyone who hasn't used it yet. */
export function InviteRow({ nodeDid, invite }: { nodeDid: string; invite: Invite }) {
  const sharing = useSharingActions();
  const now = useMinute();
  const [shown, setShown] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const what = scopeText(invite.scope);
  const left = Date.parse(invite.expiresAt) - now;

  async function cancel() {
    setBusy(true);
    setError(null);
    try {
      await sharing.cancelInvite(nodeDid, invite.inviteId);
    } catch (cause) {
      setError(errorMessage(cause));
      setBusy(false);
    }
  }

  return (
    <ItemRow
      label={`Invite: ${what}`}
      title={
        <span className="truncate">
          {what} · {invite.profile.name}
        </span>
      }
      description={
        <>
          <span>{kindText(invite.kind)}</span>
          <span title={fullTime(invite.expiresAt)}>
            {left > 0 ? `Expires in ${span(left)}` : "Expired"}
          </span>
          <span>{usesText(invite.usesLeft)}</span>
          {invite.preApproved && <span>Pre-approved</span>}
        </>
      }
      actions={
        busy ? (
          <span className="text-xs text-muted-foreground">Cancelling…</span>
        ) : (
          <MoreMenu label={`More for the invite: ${what}`}>
            <DropdownMenuGroup>
              <DropdownMenuItem onClick={() => setShown((s) => !s)}>
                {shown ? "Hide link and code" : "Show link and code"}
              </DropdownMenuItem>
            </DropdownMenuGroup>
            <DropdownMenuSeparator />
            <DropdownMenuGroup>
              <DropdownMenuItem variant="destructive" onClick={() => void cancel()}>
                Cancel invite
              </DropdownMenuItem>
            </DropdownMenuGroup>
          </MoreMenu>
        )
      }
      below={shown && <InviteDetails invite={invite} />}
      error={error}
      inline
    />
  );
}
