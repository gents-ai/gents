import { useEffect, useMemo, useState } from "react";
import { Button } from "@gents/ui/components/button";
import { FieldRow, FieldRows } from "@gents/ui/components/field";
import { Skeleton } from "@gents/ui/components/skeleton";
import type { SharingState } from "../../../hooks/sharing";
import type { NodeView } from "../../../hooks/fleetStore";
import { useApp } from "@/app/AppContext";
import { approvedHere, isApproval, lapsed, useApprovedHere } from "@/app/approvedHere";
import { isLocalNode } from "@/lib/firstRun";
import { startJoin } from "@/lib/joinInvite";
import { useHomeDid } from "@/hooks/useClient";
import { useSharing } from "@/hooks/useSharing";
import { readInstalledPacks, shortName, useInstalledPacks } from "../packs/model";
import { InviteDialog, scopeKey, type ScopeOption } from "../sharing/InviteDialog";
import { JoinDialog, JoinGroup, useJoining } from "../sharing/Join";
import { RequestsStrip } from "../sharing/Requests";
import { chooseSharingTab, missingText, SharingTabs } from "../sharing/SharingTabs";
import { useMinute } from "../time";

/** Who may use the node, and the ways in: requests waiting for a decision
    at the top, then who has access, open invites and what was decided, one
    at a time. The desktop decides for the node it runs; what the node's
    runtime doesn't offer yet is named in one line rather than shown empty.
    On the desktop's own node it also joins other nodes from their invites,
    since that is the node that joins. */
export function SharingPanel({ deployment }: { deployment: NodeView }) {
  const homeDid = useHomeDid();
  const local = isLocalNode(deployment, homeDid);
  const name = deployment.node.displayName ?? deployment.label;
  const view = useSharing(deployment.nodeDid);
  const [inviting, setInviting] = useState(false);
  const scopes = useScopes(deployment, local);
  const joining = useJoining();
  const canInvite = view.status === "ready" && view.state.available.invites;
  /* the invite made shows in its list once the dialog closes */
  const invite = () => {
    chooseSharingTab("invites");
    setInviting(true);
  };
  return (
    <div data-testid="sharing-panel">
      <header className="mb-6 flex items-start justify-between gap-4 max-sm:flex-col max-sm:gap-3">
        <div>
          <h2 className="font-heading text-lg text-heading">Sharing</h2>
          <p className="mt-1 text-sm text-muted-foreground">
            Who can use {name}, and how they got in.
          </p>
        </div>
        {(local || canInvite) && (
          <div className="flex shrink-0 flex-wrap justify-end gap-2">
            {local && (
              <Button size="sm" variant="outline" onClick={() => startJoin()}>
                Join with an invite…
              </Button>
            )}
            {canInvite && (
              <Button size="sm" onClick={invite}>
                Invite…
              </Button>
            )}
          </div>
        )}
      </header>
      {local && joining && <JoinGroup />}
      {view.status === "loading" ? (
        <SharingLoading />
      ) : view.status === "error" ? (
        <p role="alert" className="text-sm break-words text-destructive">
          Couldn't read who may use {name}: {view.message}
        </p>
      ) : view.status === "unavailable" ? (
        <Quiet>
          {`Sharing isn't available for ${name} from this desktop. Requests to join are decided on the node they ask for.`}
        </Quiet>
      ) : (
        <Shared
          nodeDid={deployment.nodeDid}
          name={name}
          state={view.state}
          onInvite={canInvite ? invite : null}
          onJoin={local ? () => startJoin() : null}
        />
      )}
      {local && <JoinDialog node={deployment} />}
      {canInvite && (
        <InviteDialog
          nodeDid={deployment.nodeDid}
          name={name}
          open={inviting}
          scopes={scopes}
          onClose={() => setInviting(false)}
        />
      )}
    </div>
  );
}

/* What an invite can share: the whole node, each pack the desktop's own
   node has installed (another node's packs aren't listed here), and each
   of the node's agents, granted on its own. */
function useScopes(node: NodeView, local: boolean): ScopeOption[] {
  const { actions } = useApp();
  const { packs } = useInstalledPacks();
  useEffect(() => {
    if (local) void readInstalledPacks(actions);
  }, [actions, local]);
  const agents = node.agents;
  return useMemo(() => {
    const scopes: ScopeOption["scope"][] = [
      { kind: "node" },
      ...(local ? (packs ?? []) : []).map((p) => ({
        kind: "pack" as const,
        pack: p.pack,
        name: shortName(p.pack),
      })),
      ...agents.map((a) => ({
        kind: "agent" as const,
        agent: a.agentId,
        name: a.displayName || a.agentId,
      })),
    ];
    return scopes.map((scope) => ({ key: scopeKey(scope), scope }));
  }, [agents, local, packs]);
}

function Shared({
  nodeDid,
  name,
  state,
  onInvite,
  onJoin,
}: {
  nodeDid: string;
  name: string;
  state: SharingState;
  onInvite: (() => void) | null;
  onJoin: (() => void) | null;
}) {
  const { available, requests, members, invites, history } = state;
  const now = useMinute();
  const remembered = useApprovedHere(nodeDid);
  /* an approval made here is kept until the runtime lists that member, or
     its lease passes */
  useEffect(() => {
    approvedHere.prune(nodeDid, members, now);
  }, [nodeDid, members, now]);
  const approved = remembered.filter(
    (e) => !lapsed(e, now) && !members.some((m) => isApproval(e, m)),
  );
  const missing = missingText(available, name);
  const note = missing && <Quiet>{missing}</Quiet>;
  const nothing =
    requests.length + members.length + approved.length + invites.length === 0 &&
    history.length === 0;
  return (
    <>
      <RequestsStrip nodeDid={nodeDid} requests={requests} />
      {nothing ? (
        <>
          <Nothing
            line={
              available.members
                ? "Nothing is shared yet."
                : `Nobody is waiting to join ${name}.`
            }
            onInvite={onInvite}
            onJoin={onJoin}
          />
          {note && <div className="mt-3">{note}</div>}
        </>
      ) : (
        <SharingTabs
          nodeDid={nodeDid}
          name={name}
          available={available}
          members={members}
          approved={approved}
          invites={invites}
          history={history}
          note={note}
        />
      )}
    </>
  );
}

/* the one empty state, with the ways to start sharing */
function Nothing({
  line,
  onInvite,
  onJoin,
}: {
  line: string;
  onInvite: (() => void) | null;
  onJoin: (() => void) | null;
}) {
  return (
    <div
      data-testid="sharing-empty"
      className="grid justify-items-center gap-3 rounded-2xl border border-border/60 bg-raised px-4 py-10 text-center"
    >
      <p className="text-sm text-muted-foreground">{line}</p>
      {(onInvite || onJoin) && (
        <div className="flex flex-wrap justify-center gap-2">
          {onInvite && (
            <Button size="sm" onClick={onInvite}>
              Invite someone
            </Button>
          )}
          {onJoin && (
            <Button size="sm" variant="outline" onClick={onJoin}>
              Join with an invite
            </Button>
          )}
        </div>
      )}
    </div>
  );
}

function Quiet({ children }: { children: string }) {
  return <p className="text-xs text-muted-foreground">{children}</p>;
}

function SharingLoading() {
  return (
    <FieldRows aria-label="Loading sharing" aria-busy="true" className="mb-8">
      {[0, 1].map((i) => (
        <FieldRow key={i} className="flex-col items-stretch gap-1.5">
          <Skeleton className="h-3.5 w-48 max-w-full" />
          <Skeleton className="h-3 w-72 max-w-full" />
        </FieldRow>
      ))}
    </FieldRows>
  );
}
