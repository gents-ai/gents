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
  onStartServer?: (nodeName: string) => Promise<unknown>;
  onCommitServerAutoStart?: (nodeName: string) => Promise<unknown>;
};

export function LocalRuntimeConnect({
  bootstrap,
  busy,
  loading = false,
  copy,
  onConnect,
  onStartServer,
  onCommitServerAutoStart,
}: LocalRuntimeConnectProps) {
  const [error, setError] = useState<string | null>(null);
  const [newNodeName, setNewNodeName] = useState("Local Node");
  const nodeName =
    bootstrap?.initNodeName?.trim() || newNodeName.trim() || "Local Node";
  const identity =
    bootstrap?.initNodeDid?.trim() || bootstrap?.defaultNodeHome || "";

  async function connect() {
    setError(null);
    try {
      await onStartServer?.(nodeName);
      await onConnect(nodeName);
      await onCommitServerAutoStart?.(nodeName);
    } catch (connectError) {
      setError(formatPeerConnectionError(connectError, "local-runtime", copy));
    }
  }

  return (
    <section className="fleet-local-runtime">
      <div className="fleet-local-runtime-copy">
        <span className="eyebrow">Local runtime</span>
        {onStartServer && !bootstrap?.nodeHomeExists ? (
          <label>
            <span>Node name</span>
            <input
              data-testid="fleet-local-node-name"
              value={newNodeName}
              onChange={(event) => setNewNodeName(event.target.value)}
            />
          </label>
        ) : (
          <strong>{nodeName}</strong>
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
            ? bootstrap?.nodeHomeExists
              ? "Start Local Node"
              : "Create & Start Local Node"
            : "Connect Local Node"}
      </button>
      {error ? <p className="fleet-local-runtime-error">{error}</p> : null}
    </section>
  );
}
