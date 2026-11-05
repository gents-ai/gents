import { useEffect, useRef, useState } from "react";
import { Button } from "@gents/ui/components/button";
import { Checkbox } from "@gents/ui/components/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@gents/ui/components/dialog";
import { RadioGroup, RadioGroupItem } from "@gents/ui/components/radio-group";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@gents/ui/components/select";
import { Spinner } from "@gents/ui/components/spinner";
import type {
  Invite,
  InviteKind,
  ShareProfile,
  ShareScope,
} from "../../../hooks/sharing";
import { useSharingActions } from "@/hooks/useSharing";
import { fullTime } from "../TranscriptMarker";
import { span, useMinute } from "../time";
import { InviteQr } from "./InviteQr";
import { CopyButton, errorMessage, scopeText } from "./parts";

const HOUR = 60 * 60;

const EXPIRIES = [
  { value: String(HOUR), label: "1 hour" },
  { value: String(24 * HOUR), label: "1 day" },
  { value: String(3 * 24 * HOUR), label: "3 days" },
  { value: String(7 * 24 * HOUR), label: "7 days" },
];

const USES = [
  { value: "1", label: "Once" },
  { value: "5", label: "5 times" },
  { value: "any", label: "Any number of times" },
];

/** something an invite can share, keyed for a select */
export type ScopeOption = { key: string; scope: ShareScope };

export const scopeKey = (scope: ShareScope) =>
  scope.kind === "node"
    ? "node"
    : scope.kind === "pack"
      ? `pack:${scope.pack}`
      : `agent:${scope.agent}`;

type Profiles =
  | { state: "reading" }
  | { state: "read"; profiles: ShareProfile[] }
  | { state: "unread"; error: string };

/** Makes an invite to the node: who it's for, what it shares and with which
    profile, how long it lasts, how often it can be used and whether joining
    needs approval. Once made, it shows the link, the code and the QR code
    to send. */
export function InviteDialog({
  nodeDid,
  name,
  open,
  scopes,
  onClose,
}: {
  nodeDid: string;
  name: string;
  open: boolean;
  /** what can be shared: the whole node first, then packs and agents */
  scopes: readonly ScopeOption[];
  onClose: () => void;
}) {
  const sharing = useSharingActions();
  const [kind, setKind] = useState<InviteKind>("person");
  const [key, setKey] = useState("node");
  const [profileId, setProfileId] = useState<string | null>(null);
  const [expires, setExpires] = useState(EXPIRIES[1]!.value);
  const [uses, setUses] = useState("1");
  const [preApproved, setPreApproved] = useState(false);
  const [profiles, setProfiles] = useState<Profiles>({ state: "reading" });
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [made, setMade] = useState<Invite | null>(null);
  const scope: ShareScope = scopes.find((s) => s.key === key)?.scope ?? {
    kind: "node",
  };
  /* the profiles are read again when the choice changes, not each time the
     options are listed afresh */
  const chosen = useRef(scope);
  chosen.current = scope;

  useEffect(() => {
    if (!open) return;
    setKind("person");
    setKey("node");
    setExpires(EXPIRIES[1]!.value);
    setUses("1");
    setPreApproved(false);
    setError(null);
    setMade(null);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    let current = true;
    setProfiles({ state: "reading" });
    sharing.profiles(nodeDid, chosen.current).then(
      (profiles) => {
        if (!current) return;
        setProfiles({ state: "read", profiles });
        setProfileId((id) =>
          profiles.some((p) => p.id === id) ? id : (profiles[0]?.id ?? null),
        );
      },
      (error) =>
        current && setProfiles({ state: "unread", error: errorMessage(error) }),
    );
    return () => {
      current = false;
    };
  }, [sharing, nodeDid, key, open]);

  const offered = profiles.state === "read" ? profiles.profiles : [];
  const profile = offered.find((p) => p.id === profileId) ?? null;

  async function create() {
    if (!profile) return;
    setCreating(true);
    setError(null);
    try {
      setMade(
        await sharing.createInvite(nodeDid, {
          kind,
          scope,
          profileId: profile.id,
          expiresInSeconds: Number(expires),
          uses: uses === "any" ? null : Number(uses),
          preApproved,
        }),
      );
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setCreating(false);
    }
  }

  const scopeItems = scopes.map((s) => ({ value: s.key, label: scopeText(s.scope) }));
  const profileItems = offered.map((p) => ({ value: p.id, label: p.name }));

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !creating) onClose();
      }}
    >
      <DialogContent className="sm:max-w-lg" data-testid="invite-dialog">
        <DialogHeader>
          <DialogTitle>{made ? "Invite ready" : `Invite to ${name}`}</DialogTitle>
          <DialogDescription>
            {made
              ? "Send the link or the code, or let them scan the QR code."
              : "Choose who it's for and what they get. Nothing is shared until they join."}
          </DialogDescription>
        </DialogHeader>

        {made ? (
          <InviteDetails invite={made} />
        ) : (
          <div className="grid gap-4 text-sm">
            <div className="grid gap-1.5">
              <span className="text-muted-foreground">Who it's for</span>
              <RadioGroup
                value={kind}
                onValueChange={(value) => setKind(value as InviteKind)}
                aria-label="Who it's for"
                className="flex flex-wrap gap-4"
              >
                <label className="flex items-center gap-2">
                  <RadioGroupItem value="person" disabled={creating} /> A person
                </label>
                <label className="flex items-center gap-2">
                  <RadioGroupItem value="device" disabled={creating} /> One of your
                  devices
                </label>
              </RadioGroup>
            </div>

            <div className="grid gap-1.5">
              <span className="text-muted-foreground">What they get</span>
              <Select
                items={scopeItems}
                value={key}
                onValueChange={(next) => next && setKey(next as string)}
              >
                <SelectTrigger aria-label="What they get" disabled={creating}>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {scopeItems.map((option) => (
                    <SelectItem key={option.value} value={option.value}>
                      {option.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>

            <div className="grid gap-1.5">
              <span className="text-muted-foreground">Profile</span>
              {profiles.state === "reading" ? (
                <span className="flex items-center gap-2 text-muted-foreground">
                  <Spinner /> Reading the profiles
                </span>
              ) : profiles.state === "unread" ? (
                <p role="alert" className="text-destructive">
                  Couldn't read the profiles: {profiles.error}
                </p>
              ) : offered.length === 0 ? (
                <p className="text-muted-foreground">
                  No profile is offered for this. Choose something else to share.
                </p>
              ) : (
                <>
                  <Select
                    items={profileItems}
                    value={profileId}
                    onValueChange={(next) => next && setProfileId(next as string)}
                  >
                    <SelectTrigger aria-label="Profile" disabled={creating}>
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {profileItems.map((option) => (
                        <SelectItem key={option.value} value={option.value}>
                          {option.label}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  {profile && <Grants profile={profile} />}
                </>
              )}
            </div>

            <div className="flex flex-wrap gap-4">
              <div className="grid gap-1.5">
                <span className="text-muted-foreground">Expires after</span>
                <Select
                  items={EXPIRIES}
                  value={expires}
                  onValueChange={(next) => next && setExpires(next as string)}
                >
                  <SelectTrigger
                    className="w-36"
                    aria-label="Expires after"
                    disabled={creating}
                  >
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {EXPIRIES.map((option) => (
                      <SelectItem key={option.value} value={option.value}>
                        {option.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
              <div className="grid gap-1.5">
                <span className="text-muted-foreground">Can be used</span>
                <Select
                  items={USES}
                  value={uses}
                  onValueChange={(next) => next && setUses(next as string)}
                >
                  <SelectTrigger
                    className="w-48"
                    aria-label="Can be used"
                    disabled={creating}
                  >
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {USES.map((option) => (
                      <SelectItem key={option.value} value={option.value}>
                        {option.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            </div>

            <label className="flex items-start gap-2">
              <Checkbox
                className="mt-0.5"
                checked={preApproved}
                disabled={creating}
                onCheckedChange={(checked) => setPreApproved(checked === true)}
              />
              <span className="grid gap-0.5">
                Let them in without asking me
                <span className="text-xs text-muted-foreground">
                  Anyone with this invite joins as soon as they use it.
                </span>
              </span>
            </label>

            {error && (
              <p role="alert" className="break-words text-destructive">
                Couldn't make the invite: {error}
              </p>
            )}
          </div>
        )}

        <DialogFooter>
          {made ? (
            <Button onClick={onClose}>Done</Button>
          ) : (
            <>
              <Button variant="outline" disabled={creating} onClick={onClose}>
                Cancel
              </Button>
              <Button
                variant="brand"
                disabled={!profile || creating}
                onClick={() => void create()}
              >
                {creating ? "Making…" : "Make invite"}
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** what a profile lets the other side do, as the runtime words it */
export function Grants({ profile }: { profile: ShareProfile }) {
  if (profile.grants.length === 0) return null;
  return (
    <ul aria-label={`What ${profile.name} allows`} className="grid gap-0.5 text-xs">
      {profile.grants.map((grant) => (
        <li key={grant} className="text-muted-foreground">
          {grant}
        </li>
      ))}
    </ul>
  );
}

/** an invite's link, code and QR code, to send or show */
export function InviteDetails({ invite }: { invite: Invite }) {
  const now = useMinute();
  const left = Date.parse(invite.expiresAt) - now;
  return (
    <div className="flex flex-wrap items-start gap-5">
      <InviteQr text={invite.qr} label="QR code for the invite" />
      <dl className="grid min-w-0 flex-1 grid-cols-[auto_minmax(0,1fr)] items-center gap-x-4 gap-y-2 text-sm">
        <dt className="text-muted-foreground">Link</dt>
        <dd className="flex min-w-0 items-center gap-1">
          <span className="truncate font-mono text-xs" title={invite.link}>
            {invite.link}
          </span>
          <CopyButton text={invite.link} label="Copy the link" />
        </dd>
        <dt className="text-muted-foreground">Code</dt>
        <dd className="flex min-w-0 items-center gap-1">
          <span className="truncate font-mono text-sm tracking-wider">
            {invite.code}
          </span>
          <CopyButton text={invite.code} label="Copy the code" />
        </dd>
        <dt className="text-muted-foreground">Shares</dt>
        <dd className="truncate">
          {scopeText(invite.scope)} · {invite.profile.name}
        </dd>
        <dt className="text-muted-foreground">Expires</dt>
        <dd title={fullTime(invite.expiresAt)}>
          {left > 0 ? `In ${span(left)}` : "Expired"}
        </dd>
        <dt className="text-muted-foreground">Uses</dt>
        <dd>{usesText(invite.usesLeft)}</dd>
        <dt className="text-muted-foreground">Approval</dt>
        <dd>{invite.preApproved ? "Not needed" : "You approve each request"}</dd>
      </dl>
    </div>
  );
}

export const usesText = (usesLeft: number | null) =>
  usesLeft === null
    ? "Any number of times"
    : usesLeft === 1
      ? "1 use left"
      : `${usesLeft} uses left`;
