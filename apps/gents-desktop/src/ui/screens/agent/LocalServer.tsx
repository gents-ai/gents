/* The OS-managed local agent service. The desktop observes and controls it,
   but does not own its process lifetime. */
import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import type {
  ManagedServerStatus,
  ManagedServerAuthorityInput,
} from "@source-inc/gents-desktop-client";
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
  observeManagedServerOperation,
  type ManagedServerWait,
} from "../../../lib/managedServerStartup";
import { Fact, Group, Row } from "./rows";
import { useApp } from "@/app/AppContext";
import { useStartup } from "@/hooks/useClient";

export function LocalServer() {
  const {
    api,
    stores,
    actions: { refreshSnapshot },
  } = useApp();
  const { incompatibleHome } = useStartup();
  const snapshot = stores.client.use.snapshot();
  const [status, setStatus] = useState<ManagedServerStatus | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [editingAuthority, setEditingAuthority] = useState(false);
  const [toolCeiling, setToolCeiling] =
    useState<ManagedServerAuthorityInput["toolCeiling"]>("readwrite");
  const [selectedDirectory, setSelectedDirectory] = useState<string | null>(null);
  const [authorityError, setAuthorityError] = useState<string | null>(null);
  const [wait, setWait] = useState<ManagedServerWait | null>(null);
  /* only the newest status asked for is shown: a read begun before a start,
     stop or restart may answer after it with what it replaced */
  const asked = useRef(0);
  const ask = () => {
    const read = ++asked.current;
    return () => read === asked.current;
  };
  const load = () => {
    const current = ask();
    return api.managedServerStatus?.().then(
      (next) => {
        if (!current()) return;
        setStatus(next);
        setStatusError(null);
      },
      (error: unknown) => {
        if (!current()) return;
        setStatus(null);
        setStatusError(`Could not check the background agent: ${String(error)}`);
      },
    );
  };
  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [snapshot]);
  if (!api.managedServerStatus) return null;
  const act = async (
    label: string,
    run: () => Promise<ManagedServerStatus> | undefined,
  ) => {
    setBusy(true);
    const current = ask();
    try {
      const next = await run();
      if (next && current()) setStatus(next);
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
  const name = status?.agentName ?? "gents";
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
    if (!authority || !api.restartManagedServer) return;
    setBusy(true);
    setAuthorityError(null);
    const current = ask();
    try {
      const restartManagedServer = api.restartManagedServer;
      let next = await observeManagedServerOperation(
        api,
        () => restartManagedServer(name, authority),
        setWait,
      );
      if (current()) setStatus(next);
      const deadline = Date.now() + 30_000;
      while (!next.pairingReady && Date.now() < deadline) {
        await new Promise((resolve) => window.setTimeout(resolve, 250));
        next = (await api.managedServerStatus?.()) ?? next;
        if (current()) setStatus(next);
      }
      if (!next.pairingReady) {
        throw new Error("The runtime restarted, but background pairing is not ready.");
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
      setAuthorityError(cause instanceof Error ? cause.message : String(cause));
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
            onClick={() =>
              void act("Agent stopped", () => api.stopManagedServer?.(false))
            }
          >
            {busy ? <Spinner /> : null} Stop agent
          </Button>
        ) : (
          <Button
            size="sm"
            variant="brand"
            disabled={busy || status?.state === "starting"}
            onClick={() =>
              void act("Agent started", () => {
                const startManagedServer = api.startManagedServer;
                return startManagedServer
                  ? observeManagedServerOperation(
                      api,
                      () => startManagedServer(name),
                      setWait,
                    )
                  : undefined;
              })
            }
          >
            {busy || status?.state === "starting" ? <Spinner /> : null} Start agent
          </Button>
        )
      }
    >
      <Row
        label="State"
        description="Agent readiness is checked against the OS service and runtime endpoint. Pairing and desktop connectivity are observed separately."
      >
        <span className="flex items-center gap-2">
          {(statusError || status?.error) && (
            <span role="alert" className="text-xs text-destructive">
              {statusError || status?.error}
            </span>
          )}
          {status?.state === "running" && status.approvalRequired && (
            <span role="alert" className="text-xs text-destructive">
              macOS no longer allows Gents in the background, so the agent will not
              start again after it stops. Turn on Gents under {LOGIN_ITEMS_PATH}.
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
        description="Closing the window keeps controls in the menu bar. Quit Desktop closes only this frontend; the agent keeps running."
      >
        <Fact>Independent</Fact>
      </Row>
      <Row
        label="Start at login"
        description="Let your operating system—not the desktop app—start the agent when you sign in. Stop agent keeps this preference, so it may start again at your next login."
      >
        <Switch
          aria-label="Start at login"
          checked={status?.autoStart ?? false}
          disabled={busy || !status}
          onCheckedChange={(on) =>
            void act(on ? "Auto-start on" : "Auto-start off", () =>
              api.setManagedServerAutoStart?.(on),
            )
          }
        />
      </Row>
      <Row
        label="Native logs"
        description="Agent runtime diagnostics are separate from desktop connectivity and pairing observations."
      >
        <Fact mono>{snapshot?.bootstrap?.diagnosticsHint ?? "System logs"}</Fact>
      </Row>
      <Row
        label="Host access"
        description="Reviewed host access, confirmed by the runtime while running. Changing access stops the OS service, saves your choices, and starts it again."
      >
        <div className="grid justify-items-end gap-1 text-right">
          <Fact>{status?.effectiveToolCeiling ?? "—"}</Fact>
          <Fact mono>{status?.effectiveToolRoot ?? "No host path"}</Fact>
          {running && status?.state !== "external" ? (
            <Button size="sm" variant="outline" onClick={beginAuthorityEdit}>
              Change access…
            </Button>
          ) : null}
        </div>
      </Row>
      {editingAuthority && home ? (
        <div className="grid gap-4 border-t border-border/60 pt-4">
          <div>
            <p className="text-sm font-medium">Restart-required access change</p>
            <p className="text-xs text-muted-foreground">
              The agent keeps its current access unless the native service stops
              successfully. It restarts only after the reviewed root and ceiling are
              saved.
            </p>
          </div>
          <ManagedRuntimeAuthorityPicker
            home={home}
            toolCeiling={toolCeiling}
            toolRoot={selectedDirectory}
            onCeilingChange={setToolCeiling}
            onRootChange={setSelectedDirectory}
            validateRoot={api.validateManagedServerRoot}
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
        </div>
      ) : null}
      <Row label="GraphQL">
        <Fact mono>{status?.graphql ?? "—"}</Fact>
      </Row>
    </Group>
  );
}
