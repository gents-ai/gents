import { useEffect, useState } from "react";
import { Check, Copy, Link2 } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { FieldRows } from "@gents/ui/components/field";
import type { SharingInvitation } from "../../../hooks/sharing";
import { useSharingActions } from "@/hooks/useSharing";
import { useCopied } from "@/lib/clipboard";
import { fullTime, shortTime } from "../TranscriptMarker";
import { errorMessage } from "./parts";

type Making =
  | { status: "idle" }
  | { status: "making" }
  | { status: "made"; invitation: SharingInvitation }
  | { status: "error"; message: string };

/** Create invitation, and the invitation it made: its link to copy, when it
    expires, and another once it has. Each one admits one node's request to
    join; approving it stays here. */
export function InvitationAction({ nodeDid, name }: { nodeDid: string; name: string }) {
  const sharing = useSharingActions();
  const [making, setMaking] = useState<Making>({ status: "idle" });
  const make = async () => {
    setMaking({ status: "making" });
    try {
      setMaking({ status: "made", invitation: await sharing.invite(nodeDid) });
    } catch (cause) {
      setMaking({ status: "error", message: errorMessage(cause) });
    }
  };
  return (
    <section aria-label="Invitation" className="mb-6">
      {making.status === "made" ? (
        <InvitationCard
          name={name}
          invitation={making.invitation}
          onAnother={() => void make()}
          onDone={() => setMaking({ status: "idle" })}
        />
      ) : (
        <div className="flex flex-wrap items-center gap-3">
          <Button
            size="sm"
            variant="outline"
            disabled={making.status === "making"}
            onClick={() => void make()}
          >
            <Link2 aria-hidden />
            {making.status === "making" ? "Creating…" : "Create invitation"}
          </Button>
          {making.status === "error" ? (
            <p role="alert" className="text-xs break-words text-destructive">
              Couldn't create an invitation: {making.message}
            </p>
          ) : (
            <p className="text-xs text-muted-foreground">
              A link another node uses to ask to join {name}.
            </p>
          )}
        </div>
      )}
    </section>
  );
}

function InvitationCard({
  name,
  invitation,
  onAnother,
  onDone,
}: {
  name: string;
  invitation: SharingInvitation;
  onAnother: () => void;
  onDone: () => void;
}) {
  const expired = useExpired(invitation.expiresAt);
  const { copied, copy } = useCopied();
  const until = shortTime(invitation.expiresAt);
  return (
    <FieldRows>
      <div className="grid gap-3 px-5 py-4">
        <p className="text-sm font-medium">Invitation to {name}</p>
        <div className="flex items-center gap-2">
          <code
            aria-label="Invitation link"
            className="min-w-0 flex-1 truncate rounded-md bg-muted px-2.5 py-1.5 font-mono text-xs select-all"
            title={invitation.link}
          >
            {invitation.link}
          </code>
          <Button size="sm" disabled={expired} onClick={() => copy(invitation.link)}>
            {copied ? <Check aria-hidden /> : <Copy aria-hidden />}
            {copied ? "Copied" : "Copy link"}
          </Button>
        </div>
        <p className="text-xs text-muted-foreground">
          {expired ? (
            <>
              This invitation expired
              {until && <span title={fullTime(invitation.expiresAt)}> at {until}</span>}
              . Create another to send.
            </>
          ) : (
            <>
              The node that opens it can ask to join {name}
              {until && (
                <span title={fullTime(invitation.expiresAt)}> until {until}</span>
              )}
              . It works for one node; you approve its request here.
            </>
          )}
        </p>
        <div className="flex justify-end gap-2">
          <Button size="sm" variant="outline" onClick={onAnother}>
            Create another
          </Button>
          <Button size="sm" variant="ghost" onClick={onDone}>
            Done
          </Button>
        </div>
      </div>
    </FieldRows>
  );
}

/* true once `expiresAt` has passed, turning over at that moment */
function useExpired(expiresAt: string) {
  const at = Date.parse(expiresAt);
  const [expired, setExpired] = useState(() => !(at > Date.now()));
  useEffect(() => {
    const left = at - Date.now();
    setExpired(!(left > 0));
    if (!(left > 0)) return;
    const timer = setTimeout(() => setExpired(true), left);
    return () => clearTimeout(timer);
  }, [at]);
  return expired;
}
