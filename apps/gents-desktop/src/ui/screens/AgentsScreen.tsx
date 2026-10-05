/* Agents, the fleet: every deployment this desktop knows. A card per
   agent from the Fleet design: picture with a status dot, name, how many
   runs are live, and an overflow menu with what the desktop's fleet row
   offers (rename the saved label, check the peer, remove). Add agent is
   the desktop's status enrolment: a server address, a request the
   server's admin approves, then the peer joins. */
import { useEffect, useState } from "react";
import { EllipsisVertical, Inbox, Plus, SlidersHorizontal } from "lucide-react";
import { toast } from "sonner";
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
import type { Shell } from "@/hooks/useShell";
import { inferenceIsConfigured, isLocalAgent } from "@/lib/firstRun";
import { href } from "@/lib/router";
import { isLive } from "@/lib/live";
import { AgentAvatar } from "./AgentAvatar";
import { AgentHoverCard } from "./HoverCards";
import { isWorkingNode } from "@/lib/nodes";

export function AgentsScreen({ shell }: { shell: Shell }) {
  const [adding, setAdding] = useState(false);
  const [renaming, setRenaming] = useState<{
    peerId: string;
    label: string;
  } | null>(null);
  const [removing, setRemoving] = useState<{
    agentDid: string;
    peerId: string;
    label: string;
  } | null>(null);
  const [removingBusy, setRemovingBusy] = useState(false);
  const pending = shell.snapshot?.client?.enrollmentRequests;
  /* the node this machine runs, then the paired ones with the reachable
     first: the dot is the first thing read on a row */
  const groups = [
    {
      key: "local",
      label: "Local node",
      nodes: shell.deployments.filter(isWorkingNode),
      count: false,
    },
    {
      key: "remote",
      label: "Remote nodes",
      nodes: shell.deployments
        .filter((d) => !isWorkingNode(d))
        .sort((a, b) => Number(b.dialSucceeded) - Number(a.dialSucceeded)),
      count: true,
    },
  ];
  return (
    <ScrollArea className="h-full" data-testid="agents-screen">
      <div className="mx-auto max-w-page px-6 py-8">
        <div className="flex h-10 items-center justify-between">
          <h1 className="font-heading text-lg font-medium text-heading">Agents</h1>
          <Button variant="secondary" size="sm" onClick={() => setAdding(true)}>
            <Plus /> Add agent
          </Button>
        </div>
        {pending === null && (
          <p className="mt-4 rounded-2xl border border-border/60 bg-raised px-5 py-4 text-sm text-muted-foreground">
            Waiting for the signed enrolment state. New enrolment is disabled until the
            database can be read.
          </p>
        )}
        {pending && pending.length > 0 && (
          <ul className="mt-4 grid grid-cols-[minmax(0,1fr)] gap-3">
            {pending.map((r) => (
              <li
                key={r.requestId}
                className="flex items-center gap-3 rounded-2xl border border-dashed border-border px-5 py-4"
              >
                <Spinner className="text-foreground" />
                <div className="min-w-0">
                  <p className="text-sm font-medium">
                    {r.state === "approved"
                      ? "Approval received · finishing secure route"
                      : `Waiting for ${r.serverLabel ?? r.serverPeer} to accept`}
                  </p>
                  <p className="truncate font-mono text-[11px] text-muted-foreground">
                    {r.requestId} · expires {new Date(r.expiresAt).toLocaleTimeString()}
                  </p>
                </div>
              </li>
            ))}
          </ul>
        )}
        {groups.map((g) => (
          <section key={g.key} className="mt-6">
            <h2 className="flex items-baseline gap-2 font-mono text-[11px] tracking-wide text-muted-foreground uppercase">
              {g.label}
              {g.count && (
                <span className="text-muted-foreground/70">{g.nodes.length}</span>
              )}
            </h2>
            {g.nodes.length === 0 && (
              <p className="mt-3 rounded-2xl border border-dashed border-border px-5 py-4 text-sm text-muted-foreground">
                {g.key === "local"
                  ? "No node runs on this machine; work happens on the remote nodes."
                  : "No remote nodes paired."}
              </p>
            )}
            <ul className="mt-3 grid grid-cols-[minmax(0,1fr)] gap-3">
              {g.nodes.map((d) => {
                const name = d.agentPrincipal.displayName ?? d.label;
                const online = d.dialSucceeded;
                const live = d.sessions.filter((c) => isLive(c.turnState)).length;
                const waiting = d.mailboxItems.filter(
                  (m) => m.status === "open",
                ).length;
                const config = href({
                  name: "agent",
                  agentDid: d.agentDid,
                  section: "agent",
                });
                const check = async () => {
                  try {
                    const r = (await shell.api.fetchPeerStatus(d.peerId)) as {
                      reachable?: boolean;
                    };
                    toast(
                      r?.reachable
                        ? `${d.label} is reachable`
                        : `${d.label} is not reachable`,
                    );
                  } catch (e) {
                    toast(`Status check failed: ${String(e)}`);
                  }
                };
                return (
                  <li
                    key={d.agentDid}
                    className="flex items-center gap-4 rounded-2xl border border-border/60 bg-raised px-5 py-4 transition-colors hover:border-border hover:bg-accent"
                  >
                    {/* the row lights up whole, so it is clickable whole: the
                    link reaches back through the padding it sits in rather
                    than ending where its text does, which left the top and
                    bottom of every row looking live and doing nothing */}
                    <a
                      href={href({ name: "sessions", nodeDid: d.agentDid })}
                      onClick={() => shell.selectAgent(d.agentDid)}
                      aria-label={`${name} sessions`}
                      className="-my-4 -ml-5 flex min-w-0 flex-1 items-center gap-3 overflow-hidden py-4 pl-5"
                    >
                      <AgentHoverCard
                        deployment={d}
                        root={shell.snapshot?.bootstrap.initToolRoot}
                        ceiling={shell.snapshot?.bootstrap.initToolCeiling}
                      >
                        <span className="block shrink-0">
                          <AgentAvatar name={name} className="size-8" />
                        </span>
                      </AgentHoverCard>
                      <span
                        className={cn(
                          "size-2 shrink-0 rounded-full",
                          online ? "bg-brand" : "bg-destructive",
                        )}
                        aria-label={online ? "online" : "offline"}
                        role="img"
                      />
                      <span className="grid min-w-0 flex-1 auto-cols-[minmax(0,max-content)] grid-flow-col items-baseline justify-start gap-3">
                        {/* name and label are columns that each take their full text when it
                      fits; the short one always does, and the long one truncates
                      into what is left (both split the row when both are long) */}
                        <span className="truncate font-heading text-base font-medium text-heading">
                          {name}
                        </span>
                        {/* the pairing label names a remote node when its principal
                        does not; the local node's pill already says it */}
                        {!isWorkingNode(d) && d.label !== name && (
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
                    {isLocalAgent(d, shell.snapshot?.bootstrap.initAgentDid) &&
                      !inferenceIsConfigured(d) && (
                        <Button
                          size="sm"
                          variant="outline"
                          nativeButton={false}
                          render={
                            <a
                              href={href({
                                name: "agent",
                                agentDid: d.agentDid,
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
                        href={href({ name: "mailbox", nodeDid: d.agentDid })}
                        onClick={() => shell.selectAgent(d.agentDid)}
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
                    <Button
                      variant="quiet"
                      size="icon-sm"
                      aria-label={`Configure ${name}`}
                      title="Configure"
                      nativeButton={false}
                      render={
                        <a
                          href={config}
                          onClick={() => shell.selectAgent(d.agentDid)}
                        />
                      }
                    >
                      <SlidersHorizontal />
                    </Button>
                    <DropdownMenu>
                      <DropdownMenuTrigger
                        render={
                          <Button
                            variant="quiet"
                            size="icon-sm"
                            aria-label={`${name} actions`}
                          />
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
                                  nodeDid: d.agentDid,
                                })}
                              />
                            }
                            onClick={() => shell.selectAgent(d.agentDid)}
                          >
                            Open sessions
                          </DropdownMenuItem>
                          <DropdownMenuItem
                            nativeButton={false}
                            render={<a href={config} />}
                            onClick={() => shell.selectAgent(d.agentDid)}
                          >
                            Configure
                          </DropdownMenuItem>
                          <DropdownMenuItem
                            onClick={() =>
                              setRenaming({ peerId: d.peerId, label: d.label })
                            }
                          >
                            Rename
                          </DropdownMenuItem>
                          <DropdownMenuItem onClick={() => void check()}>
                            Check peer
                          </DropdownMenuItem>
                        </DropdownMenuGroup>
                        {!isLocalAgent(d, shell.snapshot?.bootstrap.initAgentDid) && (
                          <>
                            <DropdownMenuSeparator />
                            <DropdownMenuGroup>
                              <DropdownMenuItem
                                variant="destructive"
                                onClick={() =>
                                  setRemoving({
                                    agentDid: d.agentDid,
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
              })}
            </ul>
          </section>
        ))}
      </div>
      <AddAgentDialog shell={shell} open={adding} onClose={() => setAdding(false)} />
      <RenameDialog
        key={renaming?.peerId ?? "none"}
        title="Rename deployment"
        description="The saved label on this desktop; the agent's own name does not change."
        value={renaming?.label ?? null}
        onSave={async (next) => {
          if (!renaming) return;
          await shell.renamePeer(renaming.peerId, next);
          toast("Renamed");
        }}
        onClose={() => setRenaming(null)}
      />
      <AlertDialog
        open={removing !== null}
        onOpenChange={(open) => !open && !removingBusy && setRemoving(null)}
      >
        <AlertDialogContent aria-modal="true">
          <AlertDialogHeader>
            <AlertDialogTitle>Remove {removing?.label}?</AlertDialogTitle>
            <AlertDialogDescription>
              This forgets the peer and its synchronization route on this desktop. It
              does not delete the remote agent.
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
                  await shell.removePeer(removing.peerId, removing.agentDid);
                  toast("Peer removed");
                  setRemoving(null);
                } catch (error) {
                  toast(
                    `Remove failed: ${error instanceof Error ? error.message : String(error)}`,
                  );
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

function AddAgentDialog({
  shell,
  open,
  onClose,
}: {
  shell: Shell;
  open: boolean;
  onClose: () => void;
}) {
  const [address, setAddress] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (open) setError(null);
  }, [open]);
  const ready = /\S/.test(address);
  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await shell.api.requestStatusEnrollment(address.trim());
      await shell.refreshSnapshot();
      toast(`Enrolment request ${r.requestId} sent · waiting for acceptance`);
      setAddress("");
      onClose();
    } catch (e) {
      const reason = e instanceof Error ? e.message : String(e);
      setError(`Couldn't request enrolment: ${reason}`);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent aria-modal="true">
        <DialogHeader>
          <DialogTitle>Add agent</DialogTitle>
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
            Agent server
          </label>
          <Input
            id="enrol-address"
            value={address}
            onChange={(e) => setAddress(e.target.value)}
            placeholder="100.69.4.79:9191"
            className="font-mono"
            disabled={busy}
            autoFocus
            data-testid="fleet-add-server-address"
          />
        </form>
        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : null}
        <DialogFooter>
          <Button variant="outline" onClick={onClose} disabled={busy}>
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

/* one label edited in place: a deployment's saved label or an account's */
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
      setError(e instanceof Error ? e.message : String(e));
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
