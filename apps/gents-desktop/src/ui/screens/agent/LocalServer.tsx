/* The OS-managed local agent service. The desktop observes and controls it,
   but does not own its process lifetime. */
import { useEffect, useState } from "react";
import { toast } from "sonner";
import type {
  ManagedServerStatus,
  ManagedServerAuthorityInput,
} from "@source-inc/gents-desktop-client";
import { Badge } from "@gents/ui/components/badge";
import { Button } from "@gents/ui/components/button";
import { Spinner } from "@gents/ui/components/spinner";
import { Switch } from "@gents/ui/components/switch";
import type { Shell } from "@/hooks/useShell";
import {
  ManagedRuntimeAuthorityPicker,
  ManagedRuntimeAuthorityReview,
} from "@/components/ManagedRuntimeAuthority";
import { authoritiesEqual, authorityForSelection } from "@/lib/managedRuntimeAuthority";
import { Fact, Group, Row } from "./rows";

export function LocalServer({ shell }: { shell: Shell }) {
  const api = shell.api;
  const [status, setStatus] = useState<ManagedServerStatus | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [editingAuthority, setEditingAuthority] = useState(false);
  const [toolCeiling, setToolCeiling] =
    useState<ManagedServerAuthorityInput["toolCeiling"]>("readwrite");
  const [selectedDirectory, setSelectedDirectory] = useState<string | null>(null);
  const [authorityError, setAuthorityError] = useState<string | null>(null);
  const load = () =>
    api.managedServerStatus?.().then(
      (next) => {
        setStatus(next);
        setStatusError(null);
      },
      (error: unknown) => {
        setStatus(null);
        setStatusError(`Could not check the background agent: ${String(error)}`);
      },
    );
  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [shell.snapshot]);
  if (!api.managedServerStatus) return null;
  const act = async (
    label: string,
    run: () => Promise<ManagedServerStatus> | undefined,
  ) => {
    setBusy(true);
    try {
      const next = await run();
      if (next) setStatus(next);
      await shell.refreshSnapshot();
      toast(label);
    } catch (e) {
      toast(`${label} failed: ${String(e)}`);
    } finally {
      setBusy(false);
    }
  };
  const running = status?.state === "running" || status?.state === "external";
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
    try {
      let next = await api.restartManagedServer(name, authority);
      setStatus(next);
      const deadline = Date.now() + 30_000;
      while (!next.pairingReady && Date.now() < deadline) {
        await new Promise((resolve) => window.setTimeout(resolve, 250));
        next = (await api.managedServerStatus?.()) ?? next;
        setStatus(next);
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
      await shell.refreshSnapshot();
      toast("Managed runtime restarted with the reviewed access");
    } catch (cause) {
      setAuthorityError(cause instanceof Error ? cause.message : String(cause));
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
              void act("Agent started", () => api.startManagedServer?.(name))
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
          <Badge
            variant={
              running
                ? "secondary"
                : status?.state === "failed"
                  ? "destructive"
                  : "outline"
            }
          >
            {status?.state ?? "unknown"}
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
        <Fact mono>{shell.snapshot?.bootstrap?.diagnosticsHint ?? "System logs"}</Fact>
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
