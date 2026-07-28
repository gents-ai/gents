import { useCallback, useEffect, useRef, useState } from "react";

import { fetchNetworkStatus } from "@source-inc/gents-desktop-client";
import type { NetworkStatusView } from "@source-inc/gents-desktop-client";
import { CopyButton } from "@source-inc/gents-desktop-ui";
import { formatRelativeTime } from "../fleetMetrics.js";

/// Live P2P state for this desktop node: own addresses, connected peers
/// matched against saved deployments, and replicator collection sets.
/// Collapsed by default; fetched on expand and on explicit refresh only.
export function NetworkPanel() {
  const [open, setOpen] = useState(false);
  const [status, setStatus] = useState<NetworkStatusView | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const generationRef = useRef(0);

  const load = useCallback(async () => {
    const generation = ++generationRef.current;
    setLoading(true);
    setError(null);
    try {
      const next = await fetchNetworkStatus();
      if (generationRef.current === generation) {
        setStatus(next);
      }
    } catch (err) {
      if (generationRef.current === generation) {
        setError(err instanceof Error ? err.message : String(err));
      }
    } finally {
      if (generationRef.current === generation) {
        setLoading(false);
      }
    }
  }, []);

  useEffect(() => {
    if (open && !status && !loading) {
      void load();
    }
  }, [open, status, loading, load]);

  const peerLabel = (peerId: string) => {
    const saved = status?.savedPeers.find(
      (peer) => peerId.includes(peer.peerId) || peer.addr.includes(peerId),
    );
    return saved?.label ?? null;
  };

  return (
    <section className="network-panel" data-testid="network-panel">
      <button
        aria-expanded={open}
        className="ghost-button network-toggle"
        data-testid="network-toggle"
        onClick={() => setOpen((value) => !value)}
        type="button"
      >
        {open ? "Hide network" : "Network"}
      </button>

      {open ? (
        <div className="network-body">
          <div className="network-toolbar">
            <p className="eyebrow">This node</p>
            <button
              className="ghost-button"
              data-testid="network-refresh"
              disabled={loading}
              onClick={() => void load()}
              type="button"
            >
              {loading ? "Loading..." : "Refresh"}
            </button>
          </div>

          {error ? (
            <p
              className="network-error"
              data-testid="network-error"
              role="alert"
            >
              Network status failed: {error}
            </p>
          ) : null}

          {status ? (
            <>
              <div className="network-facts">
                <div>
                  <dt>Peer ID</dt>
                  <dd className="mono">
                    {status.localPeerId ?? status.localPeerIdError ?? "unknown"}
                    {status.localPeerId ? (
                      <CopyButton
                        label="Copy"
                        getText={() => status.localPeerId ?? ""}
                      />
                    ) : null}
                  </dd>
                </div>
                <div>
                  <dt>Listen addresses</dt>
                  <dd className="mono">
                    {status.listenAddressesError ??
                      (status.listenAddresses.length
                        ? status.listenAddresses.join("\n")
                        : "none")}
                    {status.listenAddresses.length ? (
                      <CopyButton
                        label="Copy"
                        getText={() => status.listenAddresses.join("\n")}
                      />
                    ) : null}
                  </dd>
                </div>
                <div>
                  <dt>Connected peers</dt>
                  <dd data-testid="network-connected">
                    {status.connectedPeersError ??
                      (status.connectedPeers.length
                        ? status.connectedPeers
                            .map((peer) => peerLabel(peer) ?? peer)
                            .join(", ")
                        : "none")}
                  </dd>
                </div>
              </div>

              <p className="eyebrow">Replicators</p>
              {status.replicatorsError ? (
                <p className="network-error" role="alert">
                  {status.replicatorsError}
                </p>
              ) : !status.replicators.length ? (
                <p className="muted">No replicators configured.</p>
              ) : (
                <table className="network-replicators">
                  <thead>
                    <tr>
                      <th>Peer</th>
                      <th>Collections</th>
                      <th>Status</th>
                      <th>Last change</th>
                    </tr>
                  </thead>
                  <tbody>
                    {status.replicators.map((replicator, index) => {
                      const id = replicator.peerId ?? replicator.address ?? "";
                      return (
                        <tr key={`${id}-${index}`}>
                          <td
                            className="mono"
                            title={replicator.address ?? undefined}
                          >
                            {(id && peerLabel(id)) ?? id ?? "unknown"}
                          </td>
                          <td
                            title={replicator.collections.join(", ")}
                          >{`${replicator.collections.length} collections`}</td>
                          <td>{replicator.status ?? "?"}</td>
                          <td>
                            {replicator.lastStatusChange
                              ? formatRelativeTime(replicator.lastStatusChange)
                              : "—"}
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              )}
            </>
          ) : null}
        </div>
      ) : null}
    </section>
  );
}
