import { useState } from "react";

import type { BootstrapSummary } from "@source-inc/gents-desktop-client";

import type { FleetCopy } from "./copy.js";
import { formatPeerConnectionError } from "./peerConnectionErrors.js";

export type LocalRuntimeConnectProps = {
  bootstrap: BootstrapSummary | null;
  busy: boolean;
  copy?: Pick<FleetCopy, "runtimeProductName" | "cliBinaryName">;
  onConnect: (label?: string | null) => Promise<unknown>;
};

export function LocalRuntimeConnect({
  bootstrap,
  busy,
  copy,
  onConnect,
}: LocalRuntimeConnectProps) {
  const [error, setError] = useState<string | null>(null);
  const agentName = bootstrap?.initAgentName?.trim() || "Local Agent";
  const identity =
    bootstrap?.initAgentDid?.trim() || bootstrap?.defaultAgentHome || "";

  async function connect() {
    setError(null);
    try {
      await onConnect(agentName);
    } catch (connectError) {
      setError(formatPeerConnectionError(connectError, "local-runtime", copy));
    }
  }

  return (
    <section className="fleet-local-runtime">
      <div className="fleet-local-runtime-copy">
        <span className="eyebrow">Local runtime</span>
        <strong>{agentName}</strong>
        {identity ? (
          <span className="muted mono" title={identity}>
            {identity}
          </span>
        ) : null}
      </div>
      <button
        className="primary-button"
        data-testid="fleet-connect-local"
        disabled={busy}
        onClick={() => void connect()}
        type="button"
      >
        {busy ? "Connecting..." : "Connect Local Agent"}
      </button>
      {error ? <p className="fleet-local-runtime-error">{error}</p> : null}
    </section>
  );
}
