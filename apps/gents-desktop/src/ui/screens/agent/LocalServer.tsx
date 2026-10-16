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
import { authoritiesEqual, authorityForSelection } from "@/lib/managedRuntimeAuthority";
import {
  LOGIN_ITEMS_PATH,
  describeManagedServerWait,
  managedServerWaitKind,
} from "../../../lib/managedServerStartup";
import { Fact, Group, PathFact, Row, StackedRow } from "./rows";
import { useApp } from "@/app/AppContext";
import { ManagedRuntimeUnavailableError } from "../../../lib/managedRuntimeReadiness";
import { useStartup } from "@/hooks/useClient";
import { errorText } from "../../../lib/errorText";

export function LocalServer() {
  const { stores, actions } = useApp();
  const { refreshSnapshot } = actions;
  const { incompatibleHome } = useStartup();
  const snapshot = stores.client.use.snapshot();
  const status = stores.localServer.use.status();
  const readFailure = stores.localServer.use.readFailure();
  const wait = stores.localServer.use.wait();
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
  useEffect(() => actions.watchLocalServer(), [actions]);
  if (!actions.localServerOffers.status) return null;
  const act = async (label: string, run: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await run();
      await refreshSnapshot();
      toast(label);
    } catch (e) {
      if (!(await incompatibleHome?.adopt(e))) {
        toast(`${label} failed: ${String(e)}`);
      }
    } finally {
      setBusy(false);
    }
  };
  const running = status?.state === "running" || status?.state === "external";
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
    setEditingAuthority(true);
  };
  const restartWithAuthority = async () => {
    if (!authority || !actions.localServerOffers.restart) return;
    setBusy(true);
    setAuthorityError(null);
    try {
      let next = await actions.restartLocalServer(name, authority);
      if (!next.pairingReady) {
        const paired = await actions
          .awaitLocalServerPairing()
          .catch((cause: unknown) => {
            throw cause instanceof ManagedRuntimeUnavailableError
              ? new Error("The runtime restarted, but background pairing is not ready.")
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
    <Group
      title="Local server"
      action={
        running ? (
          <Button
            size="sm"
            variant="outline"
            disabled={busy || status?.state === "external"}
            onClick={() => void act("Node stopped", () => actions.stopLocalServer())}
          >
            {busy ? <Spinner /> : null} Stop node
          </Button>
        ) : (
          <Button
            size="sm"
            variant="brand"
            disabled={busy || status?.state === "starting"}
            onClick={() =>
              void act("Node started", () => actions.startLocalServer(name))
            }
          >
            {busy || status?.state === "starting" ? <Spinner /> : null} Start node
          </Button>
        )
      }
    >
      <Row
        label="State"
        description="Node readiness is checked against the OS service and runtime endpoint. Pairing and desktop connectivity are observed separately."
      >
        <span className="flex items-center gap-2">
          {(statusError || status?.error) && (
            <span role="alert" className="text-xs text-destructive">
              {statusError || status?.error}
            </span>
          )}
          {status?.state === "running" && status.approvalRequired && (
            <span role="alert" className="text-xs text-destructive">
              macOS no longer allows Gents in the background, so the node will not start
              again after it stops. Turn on Gents under {LOGIN_ITEMS_PATH}.
            </span>
          )}
          {wait && (
            <span role="status" className="text-xs text-muted-foreground">
              {describeManagedServerWait(wait, Date.now()).label}
            </span>
          )}
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
      <Row
        label="Desktop app"
        description="Closing the window keeps controls in the menu bar. Quitting stops the local node unless background running is enabled."
      >
        <Fact>{status?.autoStart ? "Runs in background" : "Stops on quit"}</Fact>
      </Row>
      <Row
        label="Keep running after quit"
        description="Keep triggers, schedules and sync running after quitting, and start the node when you sign in. Stop node keeps this setting."
      >
        <Switch
          aria-label="Keep running after quit"
          checked={status?.autoStart ?? false}
          disabled={busy || !status}
          onCheckedChange={(on) =>
            void act(on ? "Auto-start on" : "Auto-start off", () =>
              actions.setLocalServerAutoStart(on),
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
            onCeilingChange={setToolCeiling}
            onRootChange={setSelectedDirectory}
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
