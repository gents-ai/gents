import { useState, type FormEvent, type ReactNode } from "react";

import { formatPeerConnectionError } from "../peerConnectionErrors.js";
import type {
  BearerPairingRequest,
  BearerPairingResponse,
  PeerAddRequest,
} from "@source-inc/gents-desktop-client";
import { parsePeerConnectionJson } from "../peerConnectionImport.js";
import { QrScannerDialog } from "./QrScannerDialog.js";

export type AddPeerFormProps = {
  addingPeer: boolean;
  disabled: boolean;
  localError: string | null;
  peerForm: PeerAddRequest;
  onPeerFormChange: (value: PeerAddRequest) => void;
  /** Fleet-admin address probe (not the saved-peer-id read path). */
  onProbePeerAddress: (serverAddress: string) => Promise<unknown>;
  onPairBearer: (
    request: BearerPairingRequest,
  ) => Promise<BearerPairingResponse>;
  onSubmit: (request: PeerAddRequest) => Promise<void>;
  pairingQrHint?: ReactNode;
};

export function AddPeerForm({
  addingPeer,
  disabled,
  localError,
  peerForm,
  onPeerFormChange,
  onProbePeerAddress,
  onPairBearer,
  onSubmit,
  pairingQrHint,
}: AddPeerFormProps) {
  const [bearerToken, setBearerToken] = useState("");
  const [pairLabel, setPairLabel] = useState("");
  const [pairingStatus, setPairingStatus] = useState<string | null>(null);
  const [pairingError, setPairingError] = useState(false);
  const [scannerOpen, setScannerOpen] = useState(false);
  const [connectionJson, setConnectionJson] = useState("");
  const [importStatus, setImportStatus] = useState<string | null>(null);
  const [importError, setImportError] = useState(false);
  const [serverAddress, setServerAddress] = useState("");
  const [fetchingStatus, setFetchingStatus] = useState(false);
  const manualPeerReady =
    Boolean(peerForm.label.trim()) &&
    Boolean(peerForm.agentDid.trim()) &&
    Boolean(peerForm.addr.trim());
  const serverAddressReady = Boolean(serverAddress.trim());
  const busy = disabled || addingPeer || fetchingStatus;
  const bearerReady = bearerToken.trim().startsWith("dabear1-");

  function updateBearerToken(value: string) {
    setBearerToken(value);
    setPairingStatus(null);
    setPairingError(false);
  }

  async function handlePairSubmit(event: FormEvent) {
    event.preventDefault();
    const token = bearerToken.trim();
    if (!token.startsWith("dabear1-")) {
      setPairingStatus("Paste or scan a valid dabear1- pairing invite.");
      setPairingError(true);
      return;
    }

    setPairingStatus(
      "Pairing phases: 1 verify invite → 2 connect → 3 submit claim → 4 verify signed membership and reciprocal replication. This can take up to 60 seconds…",
    );
    setPairingError(false);
    try {
      const response = await onPairBearer({
        token,
        label: pairLabel.trim() || null,
      });
      setBearerToken("");
      setPairingStatus(
        `${response.pairing.label} is ready. Signed membership and bidirectional replication were observed.`,
      );
    } catch (error) {
      setPairingStatus(formatPeerConnectionError(error, "add-peer"));
      setPairingError(true);
    }
  }

  function updateServerAddress(value: string) {
    setServerAddress(value);
    setImportStatus(null);
    setImportError(false);
    if (looksLikeGraphqlEndpoint(value) && !peerForm.graphql?.trim()) {
      onPeerFormChange({ ...peerForm, graphql: value.trim() });
    }
  }

  function updateConnectionJson(value: string) {
    setConnectionJson(value);
    if (!value.trim()) {
      setImportStatus(null);
      setImportError(false);
      return;
    }

    try {
      onPeerFormChange(parsePeerConnectionJson(value));
      setImportStatus("Imported connection JSON");
      setImportError(false);
    } catch (error) {
      setImportStatus(String(error));
      setImportError(true);
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
      const status = await onProbePeerAddress(trimmed);
      const request = parsePeerConnectionJson(JSON.stringify(status));
      onPeerFormChange(request);
      setConnectionJson(JSON.stringify(status, null, 2));
      setImportStatus("Fetched /status");
      setImportError(false);
      return request;
    } catch (error) {
      setImportStatus(formatPeerConnectionError(error, "peer-status"));
      setImportError(true);
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
    <div className="fleet-pairing">
      <form
        className="fleet-bearer-form"
        onSubmit={(event) => void handlePairSubmit(event)}
      >
        <div className="fleet-pairing-copy">
          <p className="eyebrow">Recommended</p>
          <h3>Pair with a signed invite</h3>
          <p className="muted">
            Scan the QR code on your agent or paste its one-time invite. The app
            verifies the agent, submits this phone's claim, and configures
            least-privilege conversation replication.
          </p>
        </div>
        <label className="field">
          <span>
            Agent label <span className="muted">(optional)</span>
          </span>
          <input
            aria-label="Pairing agent label"
            data-testid="fleet-pair-label"
            disabled={busy}
            onChange={(event) => setPairLabel(event.currentTarget.value)}
            placeholder="My agent"
            value={pairLabel}
          />
        </label>
        <label className="field fleet-bearer-token-field">
          <span>Pairing invite</span>
          <textarea
            aria-label="Pairing invite"
            className="mono"
            data-testid="fleet-pair-token"
            disabled={busy}
            onChange={(event) => updateBearerToken(event.currentTarget.value)}
            placeholder="dabear1-…"
            value={bearerToken}
          />
        </label>
        <div className="fleet-pair-actions">
          <button
            className="ghost-button"
            data-testid="fleet-pair-scan"
            disabled={busy}
            onClick={() => setScannerOpen(true)}
            type="button"
          >
            Scan QR
          </button>
          <button
            className="primary-button"
            data-testid="fleet-pair-submit"
            disabled={busy || !bearerReady}
            type="submit"
          >
            {addingPeer ? "Pairing…" : "Pair securely"}
          </button>
        </div>
        {pairingStatus ? (
          <p
            aria-live="polite"
            className={
              pairingError ? "fleet-inline-error" : "fleet-pairing-success"
            }
            data-testid="fleet-pair-status"
          >
            {pairingStatus}
          </p>
        ) : null}
      </form>

      <details className="fleet-manual-disclosure">
        <summary>Advanced manual discovery</summary>
        <p className="muted">
          Diagnostic fallback only. Manual <code>/status</code> imports do not
          exchange signed membership claims.
        </p>
        <form
          className="fleet-add-form"
          onSubmit={(event) => void handleSubmit(event)}
        >
          <div className="fleet-discovery-row">
            <label className="field">
              <span>Server address</span>
              <input
                className="mono"
                data-testid="fleet-add-server-address"
                disabled={busy}
                onChange={(event) =>
                  updateServerAddress(event.currentTarget.value)
                }
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
          {importStatus ? (
            <p
              aria-live="polite"
              className={`fleet-import-status ${importError ? "fleet-inline-error" : "muted"}`}
              data-testid="fleet-import-status"
            >
              {importStatus}
            </p>
          ) : null}
          <label className="field fleet-import-field">
            <span>Connection JSON</span>
            <textarea
              className="mono"
              data-testid="fleet-add-connection-json"
              disabled={busy}
              onChange={(event) =>
                updateConnectionJson(event.currentTarget.value)
              }
              placeholder='{"label":"api-gateway","agentDid":"did:key:z6Mk...","addr":"/ip4/100.73.235.38/tcp/9161/p2p/12D3Koo..."}'
              value={connectionJson}
            />
          </label>
          <label className="field">
            <span>Agent label</span>
            <input
              data-testid="fleet-add-label"
              disabled={busy}
              onChange={(event) =>
                onPeerFormChange({
                  ...peerForm,
                  label: event.currentTarget.value,
                })
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
                onPeerFormChange({
                  ...peerForm,
                  addr: event.currentTarget.value,
                })
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
          {localError ? (
            <p className="fleet-inline-error">{localError}</p>
          ) : null}
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
      </details>
      {scannerOpen ? (
        <QrScannerDialog
          onClose={() => setScannerOpen(false)}
          onScan={(token) => updateBearerToken(token)}
          pairingHint={pairingQrHint}
        />
      ) : null}
    </div>
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
