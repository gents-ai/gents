import type { NodeView } from "../../../hooks/fleetStore";
import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import { ArrowLeft } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@gents/ui/components/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@gents/ui/components/dialog";
import { Input } from "@gents/ui/components/input";
import { Spinner } from "@gents/ui/components/spinner";
import type { InstalledPack } from "@source-inc/gents-desktop-client";
import type { InvitePreview } from "../../../hooks/sharing";
import { useApp } from "@/app/AppContext";
import { useSharingActions } from "@/hooks/useSharing";
import {
  closeJoin,
  enterInvite,
  forgetJoin,
  joinSent,
  showInvite,
  useJoinPanel,
  useSentJoins,
  type SentJoin,
} from "@/lib/joinInvite";
import { fullTime } from "../TranscriptMarker";
import { span, useMinute } from "../time";
import { usePackActions } from "../agent/usePackActions";
import { PackInstallReview } from "../packs/PackInstallReview";
import { readInstalledPacks, useInstalledPacks } from "../packs/model";
import { Grants } from "./InviteDialog";
import {
  CopyButton,
  errorMessage,
  ItemRow,
  ListGroup,
  partyName,
  scopeText,
  shortDid,
} from "./parts";

/** the requests this desktop sent from invites, on the page once the join
    dialog closes */
export function JoinGroup() {
  const sent = useSentJoins();
  return (
    <ListGroup title="Requests you sent" label="Joining another node">
      {sent.map((join) => (
        <SentRow key={join.acceptance.requestId} join={join} />
      ))}
    </ListGroup>
  );
}

/** whether a request sent from an invite is there to show */
export function useJoining() {
  return useSentJoins().length > 0;
}

/** Joining another node from an invite, in steps: a field for its link or
    code, then what the invite says before anything is sent. The inviter's
    name is only what their invite says, so the DID leads. A pack the invite
    brings installs through the same review as any other, in this dialog,
    pinned to the version (and digest, when the invite names one) the
    inviter runs, before Join sends the request. Closing forgets the draft. */
export function JoinDialog({ node }: { node: NodeView }) {
  const panel = useJoinPanel();
  const [busy, setBusy] = useState(false);
  /* the step closed from stays on show while the dialog fades out */
  const [shown, setShown] = useState(panel);
  if (panel && panel !== shown) setShown(panel);
  return (
    <Dialog
      open={panel !== null}
      onOpenChange={(open) => {
        if (!open && !busy) closeJoin();
      }}
    >
      <DialogContent className="sm:max-w-xl" data-testid="join-dialog">
        {shown?.step === "enter" && <EnterStep text={shown.text} error={shown.error} />}
        {shown?.step === "preview" && (
          <InviteSteps
            key={shown.invite}
            invite={shown.invite}
            read={shown.preview}
            node={node}
            onBusy={setBusy}
          />
        )}
      </DialogContent>
    </Dialog>
  );
}

function JoinHeader({ line }: { line: string }) {
  return (
    <DialogHeader>
      <DialogTitle>Join with an invite</DialogTitle>
      <DialogDescription>{line}</DialogDescription>
    </DialogHeader>
  );
}

function EnterStep({
  text: given,
  error: refused,
}: {
  text: string;
  error: string | null;
}) {
  const sharing = useSharingActions();
  const [text, setText] = useState(given);
  const [error, setError] = useState(refused);
  const [reading, setReading] = useState(false);
  /* a read that settles after the dialog closed must not open it again */
  const live = useRef(true);
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);
  const invite = text.trim();
  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!invite || reading) return;
    setReading(true);
    setError(null);
    try {
      const preview = await sharing.readInvite(invite);
      if (live.current) showInvite(invite, preview);
    } catch (cause) {
      if (!live.current) return;
      setError(errorMessage(cause));
      setReading(false);
    }
  }
  return (
    <form onSubmit={submit} className="contents" aria-label="Enter an invite">
      <JoinHeader line="Paste the link or the code you were sent. Nothing is sent until you choose Join." />
      <div className="grid gap-1.5">
        <Input
          autoFocus
          value={text}
          onChange={(e) => {
            setText(e.target.value);
            setError(null);
          }}
          placeholder="Paste an invite link or code"
          aria-label="Invite link or code"
          aria-invalid={error ? true : undefined}
          spellCheck={false}
          autoComplete="off"
        />
        {error && (
          <p role="alert" className="text-sm break-words text-destructive">
            Couldn't read this invite: {error}
          </p>
        )}
      </div>
      <DialogFooter>
        <Button type="button" variant="outline" onClick={closeJoin}>
          Cancel
        </Button>
        <Button type="submit" disabled={!invite || reading}>
          {reading ? "Reading…" : "Continue"}
        </Button>
      </DialogFooter>
    </form>
  );
}

const installedAs = (installed: readonly InstalledPack[] | null, pack: string) =>
  installed?.find((p) => p.pack === pack)?.installed ?? null;

/* the invite's preview, and the install review of a pack it brings in its
   place until that's installed or left */
function InviteSteps({
  invite,
  read,
  node,
  onBusy,
}: {
  invite: string;
  read: InvitePreview | null;
  node: NodeView;
  onBusy: (busy: boolean) => void;
}) {
  const sharing = useSharingActions();
  const { actions } = useApp();
  const [preview, setPreview] = useState(read);
  const [joining, setJoining] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const refresh = useCallback(() => readInstalledPacks(actions), [actions]);
  const packs = usePackActions(refresh);
  const { packs: installed } = useInstalledPacks();
  const now = useMinute();

  const working = joining || packs.busy !== null;
  useEffect(() => {
    onBusy(working);
    return () => onBusy(false);
  }, [onBusy, working]);

  /* a link opens here before the invite is read; one that can't be read
     goes back to the field, saying why */
  const unread = preview === null;
  useEffect(() => {
    if (!unread) return;
    let current = true;
    sharing.readInvite(invite).then(
      (preview) => current && setPreview(preview),
      (error) => current && enterInvite(invite, errorMessage(error)),
    );
    return () => {
      current = false;
    };
  }, [sharing, invite, unread]);

  const brings = preview !== null && preview.installs.length > 0;
  useEffect(() => {
    if (brings) void refresh();
  }, [brings, refresh]);

  if (!preview)
    return (
      <>
        <JoinHeader line="Nothing is sent until you choose Join." />
        <p className="flex items-center gap-2 text-sm text-muted-foreground">
          <Spinner /> Reading the invite
        </p>
      </>
    );

  if (packs.review)
    return (
      <>
        <Button
          variant="ghost"
          size="sm"
          autoFocus
          className="-mb-3 -ml-2 justify-self-start"
          disabled={packs.busy !== null}
          onClick={packs.closeReview}
        >
          <ArrowLeft /> Back to invite
        </Button>
        <PackInstallReview
          inline
          request={packs.review}
          node={node}
          installed={installed}
          onCancel={packs.closeReview}
          onInstall={packs.installReviewed}
        />
      </>
    );

  const { from } = preview;
  const missing = preview.installs.filter(
    (p) => installedAs(installed, p.pack) !== p.version,
  );
  const left = Date.parse(preview.expiresAt) - now;
  const expired = left <= 0;
  const reason = expired
    ? "Ask for a new one."
    : missing.length > 0 && installed !== null
      ? `Install ${missing.length === 1 ? "the pack" : "the packs"} first.`
      : null;

  async function join() {
    if (!preview) return;
    setJoining(true);
    setError(null);
    try {
      const acceptance = await sharing.acceptInvite(invite);
      joinSent({ invite, preview, acceptance });
    } catch (cause) {
      setError(errorMessage(cause));
      setJoining(false);
    }
  }

  return (
    <>
      <JoinHeader line="Check who it's from before you join. Nothing is sent until you choose Join." />
      <dl
        className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-6 gap-y-2 text-sm"
        data-testid="invite-preview"
      >
        <dt className="text-muted-foreground">From</dt>
        <dd className="grid min-w-0 gap-0.5">
          <span className="flex min-w-0 items-center gap-1">
            <span className="truncate font-mono text-xs" title={from.did}>
              {from.did}
            </span>
            <CopyButton text={from.did} label={`Copy ${from.did}`} title="Copy DID" />
          </span>
          <span className="text-xs text-muted-foreground">
            {from.name
              ? `Says it's ${from.name}. Check the DID with them before you join.`
              : "Check the DID with them before you join."}
          </span>
        </dd>
        <dt className="text-muted-foreground">For</dt>
        <dd>{preview.kind === "device" ? "One of their own devices" : "A person"}</dd>
        <dt className="text-muted-foreground">Shares</dt>
        <dd>{scopeText(preview.scope)}</dd>
        <dt className="text-muted-foreground">Profile</dt>
        <dd className="grid gap-1">
          <span>{preview.profile.name}</span>
          <Grants profile={preview.profile} />
        </dd>
        {preview.installs.length > 0 && (
          <>
            <dt className="text-muted-foreground">Installs</dt>
            <dd>
              <ul aria-label="Packs joining installs" className="grid gap-1.5">
                {preview.installs.map((p) => {
                  const have = installedAs(installed, p.pack);
                  const ready = have === p.version;
                  return (
                    <li
                      key={p.pack}
                      className="flex flex-wrap items-center gap-x-3 gap-y-1"
                    >
                      <span className="min-w-0 truncate">
                        {p.pack}{" "}
                        <span className="text-muted-foreground tabular-nums">
                          {p.version}
                        </span>
                      </span>
                      <span className="text-xs text-muted-foreground">
                        {installed === null
                          ? "Checking…"
                          : ready
                            ? "Installed"
                            : have
                              ? `You have ${have}`
                              : "Not installed"}
                      </span>
                      {installed !== null && !ready && (
                        <Button
                          size="xs"
                          variant="outline"
                          disabled={working}
                          onClick={() =>
                            packs.reviewInstall({
                              package: `${p.pack}@${p.version}`,
                              source: "invite",
                              ...(p.digest ? { digest: p.digest } : {}),
                            })
                          }
                        >
                          {have ? "Review update" : "Review install"}
                        </Button>
                      )}
                    </li>
                  );
                })}
              </ul>
            </dd>
          </>
        )}
        <dt className="text-muted-foreground">Expires</dt>
        <dd title={fullTime(preview.expiresAt)}>
          {expired ? "Expired" : `In ${span(left)}`}
        </dd>
        <dt className="text-muted-foreground">Approval</dt>
        <dd>
          {preview.preApproved
            ? "Not needed: you're in once you join"
            : "The inviter approves your request"}
        </dd>
      </dl>

      {error && (
        <p role="alert" className="text-sm break-words text-destructive">
          Couldn't join: {error}
        </p>
      )}
      <DialogFooter className="sm:items-center">
        <Button
          variant="ghost"
          className="sm:mr-auto"
          disabled={joining}
          onClick={() => enterInvite(invite)}
        >
          Back
        </Button>
        {reason && <span className="text-xs text-muted-foreground">{reason}</span>}
        <Button variant="outline" disabled={joining} onClick={closeJoin}>
          Cancel
        </Button>
        <Button
          variant="brand"
          disabled={joining || expired || missing.length > 0}
          onClick={() => void join()}
        >
          {joining ? "Joining…" : "Join"}
        </Button>
      </DialogFooter>
    </>
  );
}

/* a request this desktop sent from an invite: waiting for approval, which
   can be withdrawn, or approved */
function SentRow({ join }: { join: SentJoin }) {
  const sharing = useSharingActions();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { preview, acceptance } = join;
  const who = partyName(preview.from);
  const approved = acceptance.state === "approved";
  async function withdraw() {
    setBusy(true);
    setError(null);
    try {
      await sharing.cancelRequest(acceptance.requestId);
      forgetJoin(acceptance.requestId);
      toast.success("Request withdrawn");
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setBusy(false);
    }
  }
  return (
    <ItemRow
      label={`Request to ${who}`}
      title={
        <span className="truncate">
          {scopeText(preview.scope)} · {preview.profile.name}
        </span>
      }
      description={
        <span className="truncate">
          From{" "}
          <span className="font-mono" title={preview.from.did}>
            {shortDid(preview.from.did)}
          </span>
          {preview.from.name && <> · says it's {preview.from.name}</>}
        </span>
      }
      actions={
        <>
          <span
            className={
              approved ? "text-xs text-success" : "text-xs text-muted-foreground"
            }
          >
            {approved ? "Approved: you're in" : "Waiting for approval"}
          </span>
          {approved ? (
            <Button
              size="sm"
              variant="ghost"
              onClick={() => forgetJoin(acceptance.requestId)}
            >
              Dismiss
            </Button>
          ) : (
            <Button
              size="sm"
              variant="outline"
              disabled={busy}
              onClick={() => void withdraw()}
            >
              {busy ? "Withdrawing…" : "Withdraw"}
            </Button>
          )}
        </>
      }
      error={error && `Couldn't withdraw: ${error}`}
    />
  );
}
