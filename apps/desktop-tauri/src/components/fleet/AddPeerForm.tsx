import { useState, type FormEvent } from "react";

import type { PeerAddRequest } from "../../lib/types";
import { parsePeerConnectionJson } from "./peerConnectionImport";

export type AddPeerFormProps = {
  addingPeer: boolean;
  disabled: boolean;
  localError: string | null;
  peerForm: PeerAddRequest;
  onPeerFormChange: (value: PeerAddRequest) => void;
  onFetchPeerStatus: (serverAddress: string) => Promise<unknown>;
  onSubmit: (request: PeerAddRequest) => Promise<void>;
};

export function AddPeerForm({
  addingPeer,
  disabled,
  localError,
  peerForm,
  onPeerFormChange,
  onFetchPeerStatus,
  onSubmit,
}: AddPeerFormProps) {
  const [connectionJson, setConnectionJson] = useState("");
  const [importStatus, setImportStatus] = useState<string | null>(null);
  const [serverAddress, setServerAddress] = useState("");
  const [fetchingStatus, setFetchingStatus] = useState(false);
  const manualPeerReady =
    Boolean(peerForm.label.trim()) &&
    Boolean(peerForm.agentDid.trim()) &&
    Boolean(peerForm.addr.trim());
  const serverAddressReady = Boolean(serverAddress.trim());
  const busy = disabled || addingPeer || fetchingStatus;

  function updateServerAddress(value: string) {
    setServerAddress(value);
    if (looksLikeGraphqlEndpoint(value) && !peerForm.graphql?.trim()) {
      onPeerFormChange({ ...peerForm, graphql: value.trim() });
    }
  }

  function updateConnectionJson(value: string) {
    setConnectionJson(value);
    if (!value.trim()) {
      setImportStatus(null);
      return;
    }

    try {
      onPeerFormChange(parsePeerConnectionJson(value));
      setImportStatus("Imported connection JSON");
    } catch (error) {
      setImportStatus(String(error));
    }
  }

  async function fetchServerStatus() {
    const trimmed = serverAddress.trim();
    if (!trimmed) {
      throw new Error("Server address is required");
    }

    setFetchingStatus(true);
    setImportStatus(null);
    try {
      const status = await onFetchPeerStatus(trimmed);
      const request = parsePeerConnectionJson(JSON.stringify(status));
      onPeerFormChange(request);
      setImportStatus("Fetched /status");
      return request;
    } catch (error) {
      setImportStatus(String(error));
      throw error;
    } finally {
      setFetchingStatus(false);
    }
  }

  async function handleSubmit(event: FormEvent) {
    event.preventDefault();
    try {
      const request = manualPeerReady
        ? withGraphqlFallback(peerForm, serverAddress)
        : await fetchServerStatus();
      await onSubmit(request);
    } catch {
      // Field-level and parent errors are rendered in the form.
    }
  }

  async function handleFetchClick() {
    try {
      await fetchServerStatus();
    } catch {
      // The status line renders the discovery error.
    }
  }

  return (
    <form className="fleet-add-form" onSubmit={(event) => void handleSubmit(event)}>
      <div className="fleet-discovery-row">
        <label className="field">
          <span>Server address</span>
          <input
            className="mono"
            data-testid="fleet-add-server-address"
            disabled={busy}
            onChange={(event) => updateServerAddress(event.currentTarget.value)}
            placeholder="http://127.0.0.1:9181/api/v0/graphql"
            value={serverAddress}
          />
        </label>
        <button
          className="ghost-button"
          data-testid="fleet-fetch-status"
          disabled={busy || !serverAddressReady}
          onClick={() => void handleFetchClick()}
          type="button"
        >
          {fetchingStatus ? "Fetching..." : "Fetch /status"}
        </button>
      </div>
      <label className="field fleet-import-field">
        <span>Connection JSON</span>
        <textarea
          className="mono"
          data-testid="fleet-add-connection-json"
          disabled={busy}
          onChange={(event) => updateConnectionJson(event.currentTarget.value)}
          placeholder='{"label":"api-gateway","agentDid":"did:key:z6Mk...","addr":"/ip4/100.73.235.38/tcp/9161/p2p/12D3Koo..."}'
          value={connectionJson}
        />
        {importStatus ? (
          <span className="fleet-import-status muted">{importStatus}</span>
        ) : null}
      </label>
      <label className="field">
        <span>Agent label</span>
        <input
          data-testid="fleet-add-label"
          disabled={busy}
          onChange={(event) =>
            onPeerFormChange({ ...peerForm, label: event.currentTarget.value })
          }
          placeholder="api-gateway"
          value={peerForm.label}
        />
      </label>
      <label className="field">
        <span>Agent DID</span>
        <input
          className="mono"
          data-testid="fleet-add-agent-did"
          disabled={busy}
          onChange={(event) =>
            onPeerFormChange({
              ...peerForm,
              agentDid: event.currentTarget.value,
            })
          }
          placeholder="did:key:z6Mk..."
          value={peerForm.agentDid}
        />
      </label>
      <label className="field">
        <span>P2P address</span>
        <input
          className="mono"
          data-testid="fleet-add-addr"
          disabled={busy}
          onChange={(event) =>
            onPeerFormChange({ ...peerForm, addr: event.currentTarget.value })
          }
          placeholder="/ip4/100.73.235.38/tcp/9161/p2p/12D3Koo..."
          value={peerForm.addr}
        />
      </label>
      <label className="field">
        <span>GraphQL endpoint</span>
        <input
          className="mono"
          data-testid="fleet-add-graphql"
          disabled={busy}
          onChange={(event) =>
            onPeerFormChange({
              ...peerForm,
              graphql: event.currentTarget.value,
            })
          }
          placeholder="http://127.0.0.1:9181/api/v0/graphql"
          value={peerForm.graphql ?? ""}
        />
      </label>
      {localError ? <p className="fleet-inline-error">{localError}</p> : null}
      <div className="fleet-add-actions">
        <button
          className="primary-button"
          data-testid="fleet-add-submit"
          disabled={
            disabled ||
            addingPeer ||
            fetchingStatus ||
            (!manualPeerReady && !serverAddressReady)
          }
          type="submit"
        >
          {fetchingStatus
            ? "Fetching..."
            : addingPeer
              ? "Adding..."
              : disabled
                ? "Preparing..."
                : "Add Agent Connection"}
        </button>
      </div>
    </form>
  );
}

function looksLikeGraphqlEndpoint(value: string) {
  const trimmed = value.trim();
  return /\/graphql\/?$/i.test(trimmed.split(/[?#]/, 1)[0] ?? "");
}

function withGraphqlFallback(
  request: PeerAddRequest,
  serverAddress: string,
): PeerAddRequest {
  if (request.graphql?.trim()) {
    return request;
  }
  if (!looksLikeGraphqlEndpoint(serverAddress)) {
    return request;
  }
  return { ...request, graphql: serverAddress.trim() };
}
