import { useState } from "react";

import type { BootstrapSummary } from "@source-inc/gents-desktop-client";

import type { FleetCopy } from "./copy.js";
import { formatPeerConnectionError } from "./peerConnectionErrors.js";

export type LocalRuntimeConnectProps = {
  bootstrap: BootstrapSummary | null;
  busy: boolean;
  loading?: boolean;
  copy?: Pick<FleetCopy, "runtimeProductName" | "cliBinaryName">;
  onConnect: (label?: string | null) => Promise<unknown>;
  onStartServer?: (agentName: string) => Promise<unknown>;
};

export function LocalRuntimeConnect({
  bootstrap,
  busy,
  loading = false,
  copy,
  onConnect,
  onStartServer,
}: LocalRuntimeConnectProps) {
  const [error, setError] = useState<string | null>(null);
  const [newAgentName, setNewAgentName] = useState("Local Agent");
  const agentName =
    bootstrap?.initAgentName?.trim() || newAgentName.trim() || "Local Agent";
  const identity =
    bootstrap?.initAgentDid?.trim() || bootstrap?.defaultAgentHome || "";

  async function connect() {
    setError(null);
    try {
      await onStartServer?.(agentName);
      await onConnect(agentName);
      // The second call commits launch restoration only after the client has
      // successfully provisioned and connected the local peer.
      await onStartServer?.(agentName);
    } catch (connectError) {
      setError(formatPeerConnectionError(connectError, "local-runtime", copy));
    }
  }

  return (
    <section className="fleet-local-runtime">
      <div className="fleet-local-runtime-copy">
        <span className="eyebrow">Local runtime</span>
        {onStartServer && !bootstrap?.agentHomeExists ? (
          <label>
            <span>Agent name</span>
            <input
              data-testid="fleet-local-agent-name"
              value={newAgentName}
              onChange={(event) => setNewAgentName(event.target.value)}
            />
          </label>
        ) : (
          <strong>{agentName}</strong>
        )}
        {identity ? (
          <span className="muted mono" title={identity}>
            {identity}
          </span>
        ) : null}
      </div>
      <button
        className="primary-button"
        data-testid="fleet-connect-local"
        disabled={busy || loading}
        onClick={() => void connect()}
        type="button"
      >
        {busy
          ? onStartServer
            ? "Starting..."
            : "Connecting..."
          : onStartServer
            ? bootstrap?.agentHomeExists
              ? "Start Local Agent"
              : "Create & Start Local Agent"
            : "Connect Local Agent"}
      </button>
      {error ? <p className="fleet-local-runtime-error">{error}</p> : null}
    </section>
  );
}
