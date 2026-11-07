import { memo, useCallback, useState } from "react";
import { useStore } from "zustand";
import {
  ChevronDown,
  EllipsisVertical,
  Inbox,
  Plus,
  SlidersHorizontal,
} from "lucide-react";
import { toast } from "sonner";
import {
  enrollmentActivationMessage,
  type EnrollmentRequestView,
} from "@source-inc/gents-desktop-client";
import { Badge } from "@gents/ui/components/badge";
import { Button } from "@gents/ui/components/button";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@gents/ui/components/alert-dialog";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@gents/ui/components/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import { Input } from "@gents/ui/components/input";
import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { configIsRead, inferenceIsConfigured, isLocalNode } from "@/lib/firstRun";
import { href } from "@/lib/router";
import { NodeAvatar, nodeAvatarOf } from "./AgentAvatar";
import { NodeHoverCard } from "./HoverCards";
import { isWorkingNode } from "@/lib/nodes";
import { toastFailure } from "@/lib/failure";
import { useApp } from "@/app/AppContext";
import {
  useBootstrap,
  useEnrollmentRequests,
  useHomeDid,
  useSelectedNode,
} from "@/hooks/useClient";
import { supportsLocalManagedServer } from "../../lib/shellPlatform";
import { SetupScreen } from "./setup/SetupScreen";
import { activityOf, useFleet } from "@/hooks/useFleet";
import { fleetNodes } from "@/lib/scope";
import type { NodeView } from "../../hooks/fleetStore";
import { useShallow } from "zustand/react/shallow";
import { errorText } from "../../lib/errorText";
import { useHidden } from "@/lib/hidden";
import { usePackGroups } from "./agent/ListDetail";
import { preferences, useOpenOn } from "@/preferences";

export function AgentsScreen() {
  const {
    actions: { removePeer, renamePeer },
  } = useApp();
  const nodes = useFleet(useShallow(fleetNodes));
  const [adding, setAdding] = useState(false);
  const [addingLocal, setAddingLocal] = useState(false);
  const [renaming, setRenaming] = useState<{
    peerId: string;
    label: string;
  } | null>(null);
  const [removing, setRemoving] = useState<{
    nodeDid: string;
    peerId: string;
    label: string;
  } | null>(null);
  const [removingBusy, setRemovingBusy] = useState(false);
  const pending = useEnrollmentRequests();
  const homeDid = useHomeDid();
  const bootstrap = useBootstrap();
  const hidden = useHidden();
  const openOn = useOpenOn();
  const selected = useSelectedNode()?.nodeDid ?? null;
  const [hiding, setHiding] = useState<{
    nodeDid: string;
    name: string;
    why: "active" | "openOn";
  } | null>(null);
  /* hiding the node in use, or the one the app opens on, asks first, since
     neither then shows in the menus that would lead back to it */
  const hide = useCallback(
    (node: NodeView, hide: boolean) => {
      const why =
        node.nodeDid === selected
          ? "active"
          : node.nodeDid === openOn
            ? "openOn"
            : null;
      if (hide && why)
        return setHiding({
          nodeDid: node.nodeDid,
          name: node.node.displayName ?? node.label,
          why,
        });
      preferences.setNodeHidden(node.nodeDid, hide);
    },
    [selected, openOn],
  );
  const shown = nodes.filter((d) => !hidden.node(d.nodeDid));
  const groupChoices = usePackGroups("nodes");
  const hiddenOpen = groupChoices.isOpen("hidden", false);
  if (addingLocal) {
    return (
      <SetupScreen
        localOnly
        onCancel={() => setAddingLocal(false)}
        onDone={() => setAddingLocal(false)}
      />
    );
  }
  /* a request waits among the remote nodes until its node is listed; the
     node's own row then takes its place */
  const waiting = (pending ?? []).filter(
    (r) => !nodes.some((node) => node.nodeDid === r.ownerNode),
  );
  const groups = [
    {
      key: "local",
      label: "Local node",
      nodes: shown.filter((d) => isWorkingNode(d, homeDid)),
      count: false,
    },
    {
      key: "remote",
      label: "Remote nodes",
      nodes: shown
        .filter((d) => !isWorkingNode(d, homeDid))
        .sort((a, b) => Number(b.dialSucceeded) - Number(a.dialSucceeded)),
      count: true,
    },
    /* out of the menus on this device, and only here when there are any */
    ...(shown.length < nodes.length
      ? [
          {
            key: "hidden",
            label: "Hidden",
            nodes: nodes.filter((d) => hidden.node(d.nodeDid)),
            count: true,
          },
        ]
      : []),
  ];
  return (
    <ScrollArea className="h-full" data-testid="agents-screen">
      <div className="mx-auto max-w-page px-6 py-8">
        <div className="flex h-10 items-center justify-between">
          <h1 className="font-heading text-lg font-medium text-heading">Nodes</h1>
          <Button variant="secondary" size="sm" onClick={() => setAdding(true)}>
            <Plus /> Add node
          </Button>
        </div>
        {pending === null && (
          <p className="mt-4 rounded-2xl border border-border/60 bg-raised px-5 py-4 text-sm text-muted-foreground">
            Waiting for the signed enrolment state. New enrolment is disabled until the
            database can be read.
          </p>
        )}
        {groups.map((g) => (
          <section key={g.key} className="mt-6">
            {g.key === "hidden" ? (
              /* folded unless opened, since these were put out of the way */
              <h2>
                <button
                  type="button"
                  aria-expanded={hiddenOpen}
                  onClick={() => groupChoices.toggle("hidden", false)}
                  className="flex items-center gap-2 font-mono text-[11px] tracking-wide text-muted-foreground uppercase hover:text-foreground"
                >
                  <ChevronDown
                    aria-hidden
                    className={cn(
                      "size-3.5 transition-transform",
                      !hiddenOpen && "-rotate-90",
                    )}
                  />
                  {g.label}
                  <span className="text-muted-foreground/70">{g.nodes.length}</span>
                </button>
              </h2>
            ) : (
              <h2 className="flex items-baseline gap-2 font-mono text-[11px] tracking-wide text-muted-foreground uppercase">
                {g.label}
                {g.count && (
                  <span className="text-muted-foreground/70">{g.nodes.length}</span>
                )}
              </h2>
            )}
            {g.nodes.length === 0 && !(g.key === "remote" && waiting.length > 0) && (
              <div className="mt-3 rounded-2xl border border-dashed border-border px-5 py-4 text-sm text-muted-foreground">
                <p>
                  {g.key === "local"
                    ? "No node runs on this machine; work happens on the remote nodes."
                    : "No remote nodes paired."}
                </p>
                {g.key === "local" &&
                  bootstrap?.initConfigStatus === "absent" &&
                  supportsLocalManagedServer() && (
                    <Button
                      className="mt-3"
                      variant="secondary"
                      size="sm"
                      onClick={() => setAddingLocal(true)}
                    >
                      Set up local node
                    </Button>
                  )}
              </div>
            )}
            {(g.key !== "hidden" || hiddenOpen) && (
              <ul className="mt-3 grid grid-cols-[minmax(0,1fr)] gap-3">
                {g.key === "remote" &&
                  waiting.map((r) => <PendingNodeRow key={r.requestId} request={r} />)}
                {g.nodes.map((d) => (
                  <AgentRow
                    key={d.nodeDid}
                    node={d}
                    homeDid={homeDid}
                    hidden={g.key === "hidden"}
                    onHide={hide}
                    onRename={setRenaming}
                    onRemove={setRemoving}
                  />
                ))}
              </ul>
            )}
          </section>
        ))}
      </div>
      <AddAgentDialog open={adding} onClose={() => setAdding(false)} />
      <RenameDialog
        key={renaming?.peerId ?? "none"}
        title="Rename node"
        description="The saved label on this desktop; the node's own name does not change."
        value={renaming?.label ?? null}
        onSave={async (next) => {
          if (!renaming) return;
          await renamePeer(renaming.peerId, next);
          toast("Renamed");
        }}
        onClose={() => setRenaming(null)}
      />
      <AlertDialog
        open={hiding !== null}
        onOpenChange={(open) => !open && setHiding(null)}
      >
        <AlertDialogContent aria-modal="true">
          <AlertDialogHeader>
            <AlertDialogTitle>Hide {hiding?.name}?</AlertDialogTitle>
            <AlertDialogDescription>
              {hiding?.why === "active"
                ? "It's the node in use. It stays in use until you switch, but the node switcher, the sidebar and search won't offer it again."
                : "It's the startup node, and the app still starts on it, but the node switcher, the sidebar and search won't offer it."}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Keep it shown</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                if (hiding) preferences.setNodeHidden(hiding.nodeDid, true);
                setHiding(null);
              }}
            >
              Hide
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      <AlertDialog
        open={removing !== null}
        onOpenChange={(open) => !open && !removingBusy && setRemoving(null)}
      >
        <AlertDialogContent aria-modal="true">
          <AlertDialogHeader>
            <AlertDialogTitle>Remove {removing?.label}?</AlertDialogTitle>
            <AlertDialogDescription>
              This forgets the peer and its synchronization route on this desktop. It
              does not delete the remote node.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={removingBusy}>Keep peer</AlertDialogCancel>
            <AlertDialogAction
              disabled={removingBusy}
              onClick={async (event) => {
                event.preventDefault();
                if (!removing) return;
                setRemovingBusy(true);
                try {
                  await removePeer(removing.peerId, removing.nodeDid);
                  toast("Peer removed");
                  setRemoving(null);
                } catch (error) {
                  toastFailure("remove the peer", error);
                } finally {
                  setRemovingBusy(false);
                }
              }}
            >
              {removingBusy ? "Removing…" : "Remove peer"}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </ScrollArea>
  );
}

function AddAgentDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const {
    actions: { requestStatusEnrollment },
  } = useApp();
  const [address, setAddress] = useState("");
  const [candidateName, setCandidateName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const close = () => {
    setError(null);
    onClose();
  };
  const ready = /\S/.test(address);
  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await requestStatusEnrollment(
        address.trim(),
        candidateName.trim() || undefined,
      );
      toast(`Enrolment request ${r.requestId} sent · waiting for acceptance`);
      setAddress("");
      setCandidateName("");
      close();
    } catch (e) {
      const reason = errorText(e);
      setError(`Couldn't request enrolment: ${reason}`);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={(o) => !o && close()}>
      <DialogContent aria-modal="true">
        <DialogHeader>
          <DialogTitle>Add node</DialogTitle>
          <DialogDescription>
            Connect to a Gents server. Its admin approves the enrolment.
          </DialogDescription>
        </DialogHeader>
        <form
          className="grid gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            if (ready && !busy) void submit();
          }}
        >
          <label htmlFor="enrol-address" className="text-sm">
            Node server
          </label>
          <Input
            id="enrol-address"
            inputMode="url"
            autoCapitalize="none"
            autoCorrect="off"
            value={address}
            onChange={(e) => setAddress(e.target.value)}
            placeholder="100.69.4.79:9191"
            className="font-mono"
            disabled={busy}
            autoFocus
            data-testid="fleet-add-server-address"
          />
          <label htmlFor="enrol-name" className="text-sm">
            Your name (optional)
          </label>
          <Input
            id="enrol-name"
            value={candidateName}
            onChange={(e) => setCandidateName(e.target.value)}
            disabled={busy}
          />
        </form>
        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : null}
        <DialogFooter>
          <Button variant="outline" onClick={close} disabled={busy}>
            Cancel
          </Button>
          <Button
            variant="brand"
            disabled={!ready || busy}
            onClick={() => void submit()}
            data-testid="fleet-fetch-status"
          >
            {busy ? <Spinner /> : null} {busy ? "Working…" : "Request enrolment"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function RenameDialog({
  title,
  description,
  value,
  onSave,
  onClose,
}: {
  title: string;
  description: string;
  value: string | null;
  onSave: (next: string) => Promise<void>;
  onClose: () => void;
}) {
  const [label, setLabel] = useState(value ?? "");
  const [error, setError] = useState<string | null>(null);
  const save = async () => {
    if (value === null) return;
    const next = label.trim();
    try {
      if (next && next !== value) await onSave(next);
      onClose();
    } catch (e) {
      setError(errorText(e));
    }
  };
  return (
    <Dialog open={value !== null} onOpenChange={(o) => !o && onClose()}>
      <DialogContent aria-modal="true">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{description}</DialogDescription>
        </DialogHeader>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void save();
          }}
        >
          <Input value={label} onChange={(e) => setLabel(e.target.value)} autoFocus />
        </form>
        {error && (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        )}
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="brand" onClick={() => void save()}>
            Save
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

const AgentRow = memo(function AgentRow({
  node: d,
  homeDid,
  hidden,
  onHide,
  onRename,
  onRemove,
}: {
  node: NodeView;
  homeDid: string | null;
  /** left out of the menus on this device */
  hidden: boolean;
  onHide: (node: NodeView, hide: boolean) => void;
  onRename: (peer: { peerId: string; label: string }) => void;
  onRemove: (peer: { nodeDid: string; peerId: string; label: string }) => void;
}) {
  const {
    stores,
    actions: { fetchPeerStatus },
  } = useApp();
  const configRead = useStore(stores.client, (state) =>
    state.snapshot ? configIsRead(state.snapshot, d.nodeDid) : false,
  );
  const { running: live, waiting } = useFleet(
    useShallow((s) => activityOf(s, d.nodeDid)),
  );
  const name = d.node.displayName ?? d.label;
  const online = d.dialSucceeded;
  /* the node the app is working with, ringed as the rail's avatar is */
  const active = useSelectedNode()?.nodeDid === d.nodeDid;
  const config = href({
    name: "agent",
    nodeDid: d.nodeDid,
    section: "agent",
  });
  const check = async () => {
    try {
      const r = (await fetchPeerStatus(d.peerId)) as {
        reachable?: boolean;
      };
      toast(r?.reachable ? `${d.label} is reachable` : `${d.label} is not reachable`);
    } catch (e) {
      toast(`Status check failed: ${errorText(e)}`);
    }
  };
  return (
    <li
      key={d.nodeDid}
      className={cn(
        "flex items-center gap-4 rounded-2xl border border-border/60 bg-raised px-5 py-4 transition-colors hover:border-border hover:bg-accent",
        hidden && "opacity-75",
      )}
    >
      <a
        href={href({ name: "sessions", nodeDid: d.nodeDid })}

        aria-label={`${name} sessions`}
        aria-current={active ? "true" : undefined}
        className="-my-4 -ml-5 flex min-w-0 flex-1 items-center gap-3 overflow-hidden py-4 pl-5"
      >
        <NodeHoverCard deployment={d}>
          <span
            className={cn(
              "block shrink-0 rounded-full",
              active && "ring-2 ring-ink ring-offset-2 ring-offset-raised",
            )}
          >
            <NodeAvatar name={name} {...nodeAvatarOf(d)} className="size-8" />
          </span>
        </NodeHoverCard>
        <span
          className={cn(
            "size-2 shrink-0 rounded-full",
            online ? "bg-brand" : "bg-destructive",
          )}
          aria-label={online ? "online" : "offline"}
          role="img"
        />
        <span className="grid min-w-0 flex-1 auto-cols-[minmax(0,max-content)] grid-flow-col items-baseline justify-start gap-3">
          <span className="truncate font-heading text-base font-medium text-heading">
            {name}
          </span>
          {d.node.enabled === false && <Badge variant="outline">Disabled</Badge>}
          {!isWorkingNode(d, homeDid) && d.label !== name && (
            <span className="min-w-0 truncate text-xs text-muted-foreground">
              {d.label}
            </span>
          )}
          {d.lastError && (
            <span className="min-w-0 truncate text-xs text-muted-foreground">
              {d.lastError}
            </span>
          )}
        </span>
      </a>
      {isLocalNode(d, homeDid) && configRead && !inferenceIsConfigured(d) && (
        <Button
          size="sm"
          variant="outline"
          nativeButton={false}
          render={
            <a
              href={href({
                name: "agent",
                nodeDid: d.nodeDid,
                section: "inference",
              })}
            />
          }
          title={`Configure inference for ${d.label}`}
        >
          Setup needed
        </Button>
      )}
      {waiting > 0 && (
        <a
          href={href({ name: "mailbox", nodeDid: d.nodeDid })}

          className="flex items-center gap-1.5 rounded-full px-2 py-1 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          title={`${waiting} item${waiting === 1 ? "" : "s"} need${waiting === 1 ? "s" : ""} your attention`}
        >
          <Inbox className="size-4" /> {waiting}
        </a>
      )}
      {live > 0 && (
        <span
          className="flex items-center gap-2 text-sm text-muted-foreground"
          title={`${live} running`}
        >
          <Spinner className="text-foreground" /> {live}
        </span>
      )}
      {hidden && (
        <Button variant="outline" size="sm" onClick={() => onHide(d, false)}>
          Show
        </Button>
      )}
      <Button
        variant="quiet"
        size="icon-sm"
        aria-label={`Configure ${name}`}
        title="Configure"
        nativeButton={false}
        render={<a href={config} />}
      >
        <SlidersHorizontal />
      </Button>
      <DropdownMenu>
        <DropdownMenuTrigger
          render={
            <Button variant="quiet" size="icon-sm" aria-label={`${name} actions`} />
          }
        >
          <EllipsisVertical />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuGroup>
            <DropdownMenuItem
              nativeButton={false}
              render={
                <a
                  href={href({
                    name: "sessions",
                    nodeDid: d.nodeDid,
                  })}
                />
              }
            >
              Open sessions
            </DropdownMenuItem>
            <DropdownMenuItem nativeButton={false} render={<a href={config} />}>
              Configure
            </DropdownMenuItem>
            <DropdownMenuItem
              onClick={() => onRename({ peerId: d.peerId, label: d.label })}
            >
              Rename
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => void check()}>Check peer</DropdownMenuItem>
            <DropdownMenuItem onClick={() => onHide(d, !hidden)}>
              {hidden ? "Show in menus" : "Hide from menus"}
            </DropdownMenuItem>
          </DropdownMenuGroup>
          {!isLocalNode(d, homeDid) && (
            <>
              <DropdownMenuSeparator />
              <DropdownMenuGroup>
                <DropdownMenuItem
                  variant="destructive"
                  onClick={() =>
                    onRemove({
                      nodeDid: d.nodeDid,
                      peerId: d.peerId,
                      label: d.label,
                    })
                  }
                >
                  Remove peer
                </DropdownMenuItem>
              </DropdownMenuGroup>
            </>
          )}
        </DropdownMenuContent>
      </DropdownMenu>
    </li>
  );
});

/** A node asked to accept this desktop, in the place its row will take: a
    dashed outline where the node's portrait goes, its name, and where the
    request stands. */
function PendingNodeRow({ request: r }: { request: EnrollmentRequestView }) {
  const failure = enrollmentActivationMessage(r);
  const name = r.serverLabel ?? r.serverPeer;
  return (
    <li
      data-testid="pending-node"
      className="flex items-center gap-4 rounded-2xl border border-dashed border-border px-5 py-4"
    >
      <span className="grid size-8 shrink-0 place-items-center rounded-full ring-1 ring-border ring-dashed">
        <Spinner className="size-3.5 text-muted-foreground" />
      </span>
      <div className="min-w-0 flex-1">
        <p className="flex min-w-0 items-baseline gap-3">
          <span className="truncate font-heading text-base font-medium text-heading">
            {name}
          </span>
          <span className="min-w-0 truncate text-sm text-muted-foreground">
            {failure
              ? "Access approved · connection retrying"
              : r.state === "approved"
                ? "Approval received · finishing secure route"
                : `Waiting for ${name} to accept`}
          </span>
        </p>
        {failure && (
          <p role="alert" className="break-words text-sm text-destructive">
            {failure}
          </p>
        )}
        <p className="truncate font-mono text-[11px] text-muted-foreground">
          {r.requestId}
          {r.state !== "approved" && (
            <> · expires {new Date(r.expiresAt).toLocaleTimeString()}</>
          )}
        </p>
      </div>
    </li>
  );
}
