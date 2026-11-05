import { type ReactNode, useEffect, useRef, useState } from "react";
import { Button } from "@gents/ui/components/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@gents/ui/components/dialog";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@gents/ui/components/select";
import { Spinner } from "@gents/ui/components/spinner";
import { Switch } from "@gents/ui/components/switch";
import { ToggleGroup, ToggleGroupItem } from "@gents/ui/components/toggle-group";
import {
  sameScope,
  type Invite,
  type InviteKind,
  type ShareProfile,
  type ShareScope,
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

export function scopeKey(scope: ShareScope): string {
  switch (scope.kind) {
    case "node":
      return "node";
    case "pack":
      return `pack:${scope.pack}`;
    case "agent":
      return `agent:${scope.agent}`;
    case "session":
      return `session:${scope.session}`;
  }
}

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
  scopes: listed,
  initial,
  startKind = "person",
  onClose,
}: {
  nodeDid: string;
  name: string;
  open: boolean;
  /** what can be shared: the whole node first, then packs and agents */
  scopes: readonly ScopeOption[];
  /** what it opens on, as from a pack's or a session's own Share…; added to
      the choices when they don't list it */
  initial?: ShareScope;
  /** who it opens for: one's own device, or a person */
  startKind?: InviteKind;
  onClose: () => void;
}) {
  const sharing = useSharingActions();
  const scopes: readonly ScopeOption[] =
    initial && !listed.some((s) => sameScope(s.scope, initial))
      ? [...listed, { key: scopeKey(initial), scope: initial }]
      : listed;
  const [kind, setKind] = useState<InviteKind>(startKind);
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
    setKind(startKind);
    setKey(initial ? scopeKey(initial) : startFor(startKind));
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

  /* a person starts on something narrower than the whole node, where there is
     something; one's own device starts on the node */
  const startFor = (next: InviteKind) =>
    next === "device"
      ? "node"
      : (scopes.find((s) => s.scope.kind !== "node")?.key ?? "node");
  function choose(next: InviteKind) {
    setKind(next);
    setKey(startFor(next));
  }

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
      <DialogContent className="sm:max-w-xl" data-testid="invite-dialog">
        <DialogHeader>
          <DialogTitle>{made ? "Invite ready" : `Invite to ${name}`}</DialogTitle>
          <DialogDescription>
            {made
              ? "Send the link or the code, or let them scan the QR code."
              : "Nothing is shared until they join."}
          </DialogDescription>
        </DialogHeader>

        {made ? (
          <InviteDetails invite={made} />
        ) : (
          <div className="grid text-sm">
            <ul className="divide-y divide-border/60 border-y border-border/60">
              <InviteRow label="Who it's for">
                <ToggleGroup
                  aria-label="Who it's for"
                  variant="outline"
                  size="sm"
                  className="w-full"
                  value={[kind]}
                  onValueChange={(next: string[]) =>
                    next[0] && choose(next[0] as InviteKind)
                  }
                >
                  <ToggleGroupItem
                    value="person"
                    className="flex-1"
                    disabled={creating}
                  >
                    A person
                  </ToggleGroupItem>
                  <ToggleGroupItem
                    value="device"
                    className="flex-1"
                    disabled={creating}
                  >
                    My device
                  </ToggleGroupItem>
                </ToggleGroup>
              </InviteRow>

              <InviteRow
                label="What they get"
                hint={
                  kind === "person" && scope.kind === "node"
                    ? "They'd have this whole node, as you. Share a pack or an agent if that's enough."
                    : undefined
                }
              >
                <InviteSelect
                  label="What they get"
                  items={scopeItems}
                  value={key}
                  disabled={creating}
                  onChange={setKey}
                />
              </InviteRow>

              <InviteRow
                label="What they can do"
                hint={profile?.grants.length ? <Grants profile={profile} /> : undefined}
              >
                {profiles.state === "reading" ? (
                  <span className="flex items-center gap-2 text-muted-foreground">
                    <Spinner /> Reading
                  </span>
                ) : profiles.state === "read" && offered.length > 0 ? (
                  <InviteSelect
                    label="What they can do"
                    items={profileItems}
                    value={profileId}
                    disabled={creating}
                    onChange={setProfileId}
                  />
                ) : (
                  <span className="text-muted-foreground">Not available</span>
                )}
              </InviteRow>

              <InviteRow label="Expires after">
                <InviteSelect
                  label="Expires after"
                  items={EXPIRIES}
                  value={expires}
                  disabled={creating}
                  onChange={setExpires}
                />
              </InviteRow>

              <InviteRow label="Can be used">
                <InviteSelect
                  label="Can be used"
                  items={USES}
                  value={uses}
                  disabled={creating}
                  onChange={setUses}
                />
              </InviteRow>

              <InviteRow
                label="Let them in without asking me"
                hint="Anyone with this invite joins as soon as they use it."
                end
              >
                <Switch
                  aria-label="Let them in without asking me"
                  checked={preApproved}
                  disabled={creating}
                  onCheckedChange={setPreApproved}
                />
              </InviteRow>
            </ul>

            {profiles.state === "unread" && (
              <p role="alert" className="mt-3 text-destructive">
                Couldn't read what they can do: {profiles.error}
              </p>
            )}
            {profiles.state === "read" && offered.length === 0 && (
              <p className="mt-3 text-muted-foreground">
                Nothing is offered for this. Choose something else to share.
              </p>
            )}
            {error && (
              <p role="alert" className="mt-3 break-words text-destructive">
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
function InviteRow({
  label,
  hint,
  end = false,
  children,
}: {
  label: string;
  hint?: ReactNode;
  /** the control keeps its own size, at the row's end */
  end?: boolean;
  children: ReactNode;
}) {
  return (
    <li className="flex items-center gap-6 py-3">
      <div className="grid min-w-0 flex-1 gap-0.5">
        <span>{label}</span>
        {hint && <div className="text-xs text-muted-foreground">{hint}</div>}
      </div>
      <div className={end ? "flex shrink-0 justify-end" : "w-52 shrink-0"}>
        {children}
      </div>
    </li>
  );
}

function InviteSelect({
  label,
  items,
  value,
  disabled,
  onChange,
}: {
  label: string;
  items: readonly { value: string; label: string }[];
  value: string | null;
  disabled: boolean;
  onChange: (value: string) => void;
}) {
  return (
    <Select
      items={[...items]}
      value={value}
      onValueChange={(next) => next && onChange(next as string)}
    >
      <SelectTrigger aria-label={label} disabled={disabled} className="w-full">
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {items.map((option) => (
          <SelectItem key={option.value} value={option.value}>
            {option.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

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
