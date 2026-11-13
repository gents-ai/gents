import { useEffect, useState } from "react";
import { Check, Copy, Link2 } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { Popover, PopoverContent, PopoverTrigger } from "@gents/ui/components/popover";
import type { SharingInvitation } from "../../../hooks/sharing";
import { useExclusivePopover } from "@/hooks/useExclusivePopover";
import { useSharingActions } from "@/hooks/useSharing";
import { useCopied } from "@/lib/clipboard";
import { fullTime, shortTime } from "../TranscriptMarker";
import { errorMessage } from "./parts";

type Making =
  | { status: "idle" }
  | { status: "making" }
  | { status: "made"; invitation: SharingInvitation }
  | { status: "error"; message: string };

/** Create invitation, opening onto the invitation it made: its link to copy,
    when it expires, and another. Opening it again keeps an invitation that
    still works. Each one admits one node's request to join; approving it
    stays here. */
export function InvitationButton({ nodeDid, name }: { nodeDid: string; name: string }) {
  const sharing = useSharingActions();
  const popover = useExclusivePopover();
  const [making, setMaking] = useState<Making>({ status: "idle" });
  const make = async () => {
    setMaking({ status: "making" });
    try {
      setMaking({ status: "made", invitation: await sharing.invite(nodeDid) });
    } catch (cause) {
      setMaking({ status: "error", message: errorMessage(cause) });
    }
  };
  const onOpenChange = (open: boolean) => {
    popover.onOpenChange(open);
    const working =
      making.status === "making" ||
      (making.status === "made" &&
        Date.parse(making.invitation.expiresAt) > Date.now());
    if (open && !working) void make();
  };
  return (
    <Popover
      open={popover.open}
      onOpenChange={onOpenChange}
      onOpenChangeComplete={popover.onOpenChangeComplete}
    >
      <PopoverTrigger
        render={
          <Button size="sm" variant="outline" className="shrink-0">
            <Link2 aria-hidden />
            Create invitation
          </Button>
        }
      />
      <PopoverContent
        ref={popover.popupRef}
        aria-label="Invitation"
        align="end"
        className="w-96 max-w-[calc(100vw-2rem)] gap-3"
      >
        <p className="font-medium">Invitation to {name}</p>
        {making.status === "made" ? (
          <Made
            name={name}
            invitation={making.invitation}
            onAnother={() => void make()}
          />
        ) : making.status === "error" ? (
          <>
            <p role="alert" className="text-xs break-words text-destructive">
              Couldn't create an invitation: {making.message}
            </p>
            <Button
              size="sm"
              variant="outline"
              className="w-fit"
              onClick={() => void make()}
            >
              Try again
            </Button>
          </>
        ) : (
          <p className="text-xs text-muted-foreground">Creating…</p>
        )}
      </PopoverContent>
    </Popover>
  );
}

function Made({
  name,
  invitation,
  onAnother,
}: {
  name: string;
  invitation: SharingInvitation;
  onAnother: () => void;
}) {
  const expired = useExpired(invitation.expiresAt);
  const { copied, copy } = useCopied();
  const until = shortTime(invitation.expiresAt);
  return (
    <>
      <div className="flex min-w-0 items-center gap-2">
        <code
          aria-label="Invitation link"
          className="min-w-0 flex-1 truncate rounded-md bg-muted px-2.5 py-1.5 font-mono text-xs select-all"
          title={invitation.link}
        >
          {invitation.link}
        </code>
        <Button
          size="sm"
          className="shrink-0"
          disabled={expired}
          onClick={() => copy(invitation.link)}
        >
          {copied ? <Check aria-hidden /> : <Copy aria-hidden />}
          {copied ? "Copied" : "Copy link"}
        </Button>
      </div>
      <p className="text-xs text-muted-foreground">
        {expired ? (
          <>
            This invitation expired
            {until && <span title={fullTime(invitation.expiresAt)}> at {until}</span>}.
            Create another to send.
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
      <Button size="sm" variant="outline" className="w-fit" onClick={onAnother}>
        Create another
      </Button>
    </>
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
