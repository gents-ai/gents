/* The local agent's server, as the desktop supervises it: its state,
   whether it starts with the app, and Start / Stop. Only the local
   deployment has one; peers run their own. */
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
  const [busy, setBusy] = useState(false);
  const [editingAuthority, setEditingAuthority] = useState(false);
  const [toolCeiling, setToolCeiling] =
    useState<ManagedServerAuthorityInput["toolCeiling"]>("readwrite");
  const [selectedDirectory, setSelectedDirectory] = useState<string | null>(null);
  const [authorityError, setAuthorityError] = useState<string | null>(null);
  const load = () => api.managedServerStatus?.().then(setStatus, () => setStatus(null));
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
              void act("Server stopped", () => api.stopManagedServer?.(false))
            }
          >
            {busy ? <Spinner /> : null} Stop
          </Button>
        ) : (
          <Button
            size="sm"
            variant="brand"
            disabled={busy || status?.state === "starting"}
            onClick={() =>
              void act("Server started", () => api.startManagedServer?.(name))
            }
          >
            {busy || status?.state === "starting" ? <Spinner /> : null} Start
          </Button>
        )
      }
    >
      <Row
        label="State"
        description="Supervised by the desktop; external means something else runs it."
      >
        <span className="flex items-center gap-2">
          {status?.error && (
            <span className="text-xs text-destructive">{status.error}</span>
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
        label="Start with the app"
        description="Auto-start the server when the desktop opens."
      >
        <Switch
          checked={status?.autoStart ?? false}
          disabled={busy || !status}
          onCheckedChange={(on) =>
            void act(on ? "Auto-start on" : "Auto-start off", () =>
              on
                ? api.commitManagedServerAutoStart?.(name)
                : api.stopManagedServer?.(true),
            )
          }
        />
      </Row>
      <Row
        label="Host access"
        description="Runtime-confirmed process ceiling. Changes require one managed restart."
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
          <p className="text-sm font-medium">Restart-required access change</p>
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
