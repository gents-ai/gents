/* The desktop observes and controls the OS-managed local node service, but
   does not own its process lifetime. */
import { useEffect, useState } from "react";
import { toast } from "sonner";
import type { ManagedServerAuthorityInput } from "@source-inc/gents-desktop-client";
import { Badge } from "@gents/ui/components/badge";
import { Button } from "@gents/ui/components/button";
import { Spinner } from "@gents/ui/components/spinner";
import { Switch } from "@gents/ui/components/switch";
import {
  ManagedRuntimeAuthorityPicker,
  ManagedRuntimeAuthorityReview,
} from "@/components/ManagedRuntimeAuthority";
import {
  authoritiesEqual,
  authorityForSelection,
  authorityRefusalText,
  authorityRestartPlan,
  type AuthorityRestartPlan,
} from "@/lib/managedRuntimeAuthority";
import {
  LOGIN_ITEMS_PATH,
  describeManagedServerWait,
  managedServerWaitKind,
} from "../../../lib/managedServerStartup";
import { Fact, Group, PathFact, Row, StackedRow } from "./rows";
import { useApp } from "@/app/AppContext";
import { useFleet } from "@/hooks/useFleet";
import { nodeOf, type NodeView } from "../../../hooks/fleetStore";
import { engineerAgentOf } from "@/lib/engineer";
import { ManagedRuntimeUnavailableError } from "../../../lib/managedRuntimeReadiness";
import { useHomeDid, useStartup } from "@/hooks/useClient";
import { isLocalNode } from "@/lib/firstRun";
import { errorText } from "../../../lib/errorText";

/** Starting and stopping the local node, for the node page's header. */
export function useLocalServerRun() {
  const { stores, actions } = useApp();
  const { incompatibleHome } = useStartup();
  const status = stores.localServer.use.status();
  const operating = stores.localServer.use.operation() !== null;
  const [acting, setActing] = useState(false);
  const run = async (done: string, failed: string, op: () => Promise<unknown>) => {
    setActing(true);
    try {
      await op();
      await actions.refreshSnapshot();
      toast(done);
    } catch (e) {
      if (!(await incompatibleHome?.adopt(e)))
        toast.error(`${failed}: ${errorText(e)}`);
    } finally {
      setActing(false);
    }
  };
  return {
    offered: actions.localServerOffers.status,
    /** whether the node's state has been read yet */
    known: status != null,
    running: status?.state === "running" || status?.state === "external",
    external: status?.state === "external",
    starting: status?.state === "starting",
    busy: acting || operating,
    start: () =>
      void run("Node started", "Couldn’t start the node", () =>
        actions.startLocalServer(status?.nodeName ?? "gents"),
      ),
    stop: () =>
      void run("Node stopped", "Couldn’t stop the node", () =>
        actions.stopLocalServer(),
      ),
  };
}

export function RunNodeButton() {
  const node = useLocalServerRun();
  if (!node.offered) return null;
  return node.running ? (
    <Button
      size="sm"
      variant="outline"
      disabled={node.busy || node.external}
      onClick={node.stop}
    >
      {node.busy ? <Spinner /> : null} Stop node
    </Button>
  ) : (
    <Button
      size="sm"
      variant="brand"
      disabled={node.busy || node.starting}
      onClick={node.start}
    >
      {node.busy || node.starting ? <Spinner /> : null} Start node
    </Button>
  );
}

export function LocalServer() {
  const { stores, actions } = useApp();
  const { refreshSnapshot } = actions;
  const { incompatibleHome } = useStartup();
  const snapshot = stores.client.use.snapshot();
  const status = stores.localServer.use.status();
  const readFailure = stores.localServer.use.readFailure();
  const wait = stores.localServer.use.wait();
  const localNode = useFleet((fleet) => nodeOf(fleet, status?.nodeDid));
  const operating = stores.localServer.use.operation() !== null;
  const statusError = readFailure
    ? `Could not check the background node: ${readFailure}`
    : null;
  const [acting, setBusy] = useState(false);
  const busy = acting || operating;
  const [editingAuthority, setEditingAuthority] = useState(false);
  const [toolCeiling, setToolCeiling] =
    useState<ManagedServerAuthorityInput["toolCeiling"]>("readwrite");
  const [selectedDirectory, setSelectedDirectory] = useState<string | null>(null);
  const [authorityError, setAuthorityError] = useState<string | null>(null);
  const [refusal, setRefusal] = useState<Extract<
    AuthorityRestartPlan,
    { kind: "refused" }
  > | null>(null);
  useEffect(() => actions.watchLocalServer(), [actions]);
  if (!actions.localServerOffers.status) return null;
  const act = async (done: string, failed: string, run: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await run();
      await refreshSnapshot();
      toast(done);
    } catch (e) {
      if (!(await incompatibleHome?.adopt(e))) {
        toast.error(`${failed}: ${errorText(e)}`);
      }
    } finally {
      setBusy(false);
    }
  };
  const running = status?.state === "running" || status?.state === "external";
  const failure = statusError || status?.error;
  const backgroundRefused = status?.state === "running" && status.approvalRequired;
  const updating =
    wait?.kind === "updating" ||
    (status ? managedServerWaitKind(status) === "updating" : false);
  const name = status?.nodeName ?? "gents";
  const home = status?.suggestedToolRoot ?? status?.effectiveToolRoot ?? "";
  const authority = authorityForSelection(toolCeiling, selectedDirectory);
  const beginAuthorityEdit = () => {
    if (!status || !home) return;
    setToolCeiling(status.effectiveToolCeiling ?? "readwrite");
    setSelectedDirectory(status.effectiveToolRoot ?? home);
    setAuthorityError(null);
    setRefusal(null);
    setEditingAuthority(true);
  };
  const keepCurrentFolder = () => {
    const current = status?.effectiveToolRoot ?? null;
    if (current) setSelectedDirectory(current);
    else setToolCeiling("meta-only");
    setAuthorityError(null);
    setRefusal(null);
  };
  const restartWithAuthority = async () => {
    if (!authority || !actions.localServerOffers.restart) return;
    const engineer = localNode ? engineerAgentOf(localNode) : undefined;
    const plan = authorityRestartPlan({
      rootChanged:
        !!authority.toolRoot && authority.toolRoot !== status?.effectiveToolRoot,
      nodeObserved: !!status?.nodeDid && !!localNode,
      agentPresent: !!engineer,
    });
    if (plan.kind === "refused") {
      setRefusal(plan);
      setAuthorityError(authorityRefusalText(plan));
      return;
    }
    setBusy(true);
    setAuthorityError(null);
    setRefusal(null);
    try {
      if (plan.kind === "publishThenRestart") {
        await actions.selectAgentWorkspaceRoot(
          status!.nodeDid!,
          engineer!.agentDid,
          authority.toolRoot!,
        );
      }
      let next = await actions.restartLocalServer(name, authority);
      if (!next.pairingReady) {
        const paired = await actions
          .awaitLocalServerPairing()
          .catch((cause: unknown) => {
            throw cause instanceof ManagedRuntimeUnavailableError
              ? new Error(
                  "The runtime restarted, but this app's enrollment with it isn't ready yet.",
                )
              : cause;
          });
        if (paired) next = paired;
      }
      const confirmed = next.effectiveToolCeiling
        ? {
            toolCeiling: next.effectiveToolCeiling,
            toolRoot: next.effectiveToolRoot,
          }
        : null;
      if (!confirmed || !authoritiesEqual(confirmed, authority)) {
        throw new Error(
          "The managed runtime restarted with different authority than the reviewed settings.",
        );
      }
      setEditingAuthority(false);
      await refreshSnapshot();
      toast("Managed runtime restarted with the reviewed access");
    } catch (cause) {
      setAuthorityError(errorText(cause));
      await incompatibleHome?.adopt(cause);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Group title="Local server">
      <Row
        label="State"
        description={
          <>
            Node readiness is checked against the OS service and runtime endpoint.
            Pairing and desktop connectivity are observed separately.
            {wait && (
              <span role="status" className="mt-1 block">
                {describeManagedServerWait(wait, Date.now()).label}
              </span>
            )}
          </>
        }
        error={
          (failure || backgroundRefused) && (
            <>
              {failure && <span className="block wrap-break-word">{failure}</span>}
              {backgroundRefused && (
                <span className="block">
                  macOS no longer allows Gents in the background, so the node will not
                  start again after it stops. Turn on Gents under {LOGIN_ITEMS_PATH}.
                </span>
              )}
            </>
          )
        }
      >
        <span className="flex items-center gap-2">
          <Badge
            variant={
              running
                ? "secondary"
                : status?.state === "failed"
                  ? "destructive"
                  : "outline"
            }
          >
            {updating ? "updating data" : (status?.state ?? "unknown")}
          </Badge>
        </span>
      </Row>
      {running && !status?.pairingReady && (
        <Row
          label="Desktop connection"
          description="Connect this desktop to the local node with the node owner's approval."
        >
          <Button
            variant="outline"
            disabled={busy}
            onClick={() =>
              void act("Desktop connected", "Couldn’t connect this desktop", () =>
                actions.enrollLocalServer(),
              )
            }
          >
            Connect this desktop
          </Button>
        </Row>
      )}
      <Row
        label="Desktop app"
        description="Closing the window keeps controls in the menu bar. Quitting stops the local node unless background running is enabled."
      >
        <Fact>{status?.autoStart ? "Runs in background" : "Stops on quit"}</Fact>
      </Row>
      <Row
        label="Keep running after quit"
        description="Keep triggers and schedules running after quitting, and start the node when you sign in. Stop node keeps this setting."
      >
        <Switch
          aria-label="Keep running after quit"
          checked={status?.autoStart ?? false}
          disabled={busy || !status}
          onCheckedChange={(on) =>
            void act(
              on ? "Auto-start on" : "Auto-start off",
              "Couldn’t change auto-start",
              () => actions.setLocalServerAutoStart(on),
            )
          }
        />
      </Row>
      <Row
        label="Native logs"
        description="Node runtime diagnostics are separate from desktop connectivity and pairing observations."
      >
        <Fact mono>{snapshot?.bootstrap?.diagnosticsHint ?? "System logs"}</Fact>
      </Row>
      <Row
        label="Host access"
        description="Reviewed host access, confirmed by the runtime while running. Changing access stops the OS service, saves your choices, and starts it again."
      >
        <div className="grid justify-items-end gap-1 text-right *:max-w-full max-md:justify-items-start max-md:text-left md:max-w-1/2 md:shrink-0">
          <Fact>{status?.effectiveToolCeiling ?? "—"}</Fact>
          {status?.effectiveToolRoot ? (
            <PathFact path={status.effectiveToolRoot} local />
          ) : (
            <Fact mono>No host path</Fact>
          )}
          {running && status?.state !== "external" ? (
            <Button size="sm" variant="outline" onClick={beginAuthorityEdit}>
              Change access…
            </Button>
          ) : null}
        </div>
      </Row>
      {editingAuthority && home ? (
        <StackedRow
          label="Restart-required access change"
          description="The node keeps its current access unless the native service stops successfully. It restarts only after the reviewed root and ceiling are saved."
        >
          <ManagedRuntimeAuthorityPicker
            home={home}
            toolCeiling={toolCeiling}
            toolRoot={selectedDirectory}
            onCeilingChange={(ceiling) => {
              setRefusal(null);
              setToolCeiling(ceiling);
            }}
            onRootChange={(root) => {
              setRefusal(null);
              setSelectedDirectory(root);
            }}
            validateRoot={actions.validateLocalServerRoot}
            error={authorityError}
            onError={setAuthorityError}
          />
          {authority ? <ManagedRuntimeAuthorityReview authority={authority} /> : null}
          <div className="flex justify-end gap-2">
            <Button
              variant="outline"
              disabled={busy}
              onClick={() => setEditingAuthority(false)}
            >
              Cancel
            </Button>
            {refusal?.nextActions.includes("keepCurrentFolder") ? (
              <Button variant="outline" disabled={busy} onClick={keepCurrentFolder}>
                Keep current folder
              </Button>
            ) : null}
            <Button
              variant="brand"
              disabled={busy || !authority}
              onClick={() => void restartWithAuthority()}
            >
              {busy ? <Spinner /> : null} Review complete — restart
            </Button>
          </div>
        </StackedRow>
      ) : null}
      <Row label="GraphQL">
        <Fact mono>{status?.graphql ?? "—"}</Fact>
      </Row>
    </Group>
  );
}

export function useStoppedLocalNode(deployment: NodeView | null) {
  const { actions } = useApp();
  const homeDid = useHomeDid();
  const local = useLocalServerRun();
  const watched =
    deployment !== null && isLocalNode(deployment, homeDid) && local.offered;
  useEffect(
    () => (watched ? actions.watchLocalServer() : undefined),
    [actions, watched],
  );
  return watched && local.known && !local.running ? local : null;
}
