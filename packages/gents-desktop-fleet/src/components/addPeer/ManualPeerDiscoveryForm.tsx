import type { PeerAddRequest } from "@source-inc/gents-desktop-client";
import type { ManualPeerDiscoveryController } from "./useManualPeerDiscovery.js";

export type ManualPeerDiscoveryFormProps = {
  addingPeer: boolean;
  busy: boolean;
  disabled: boolean;
  localError: string | null;
  peerForm: PeerAddRequest;
  discovery: ManualPeerDiscoveryController;
  onPeerFormChange: (value: PeerAddRequest) => void;
};

export function ManualPeerDiscoveryForm({
  addingPeer,
  busy,
  disabled,
  localError,
  peerForm,
  discovery,
  onPeerFormChange,
}: ManualPeerDiscoveryFormProps) {
  return (
    <details className="fleet-manual-disclosure">
      <summary>Advanced manual discovery</summary>
      <p className="muted">
        Diagnostic fallback only. Manual <code>/status</code> imports do not
        exchange signed membership claims.
      </p>
      <form
        className="fleet-add-form"
        onSubmit={(event) => {
          event.preventDefault();
          void discovery.submit();
        }}
      >
        <div className="fleet-discovery-row">
          <label className="field">
            <span>Server address</span>
            <input
              className="mono"
              data-testid="fleet-add-server-address"
              disabled={busy}
              onChange={(event) =>
                discovery.updateServerAddress(event.currentTarget.value)
              }
              placeholder="http://127.0.0.1:9181/api/v0/graphql"
              value={discovery.serverAddress}
            />
          </label>
          <button
            className="ghost-button"
            data-testid="fleet-fetch-status"
            disabled={busy || !discovery.serverAddressReady}
            onClick={() => void discovery.fetchStatus()}
            type="button"
          >
            {discovery.fetchingStatus ? "Fetching..." : "Fetch /status"}
          </button>
        </div>
        {discovery.importStatus ? (
          <p
            aria-live="polite"
            className={`fleet-import-status ${discovery.importError ? "fleet-inline-error" : "muted"}`}
            data-testid="fleet-import-status"
          >
            {discovery.importStatus}
          </p>
        ) : null}
        <label className="field fleet-import-field">
          <span>Connection JSON</span>
          <textarea
            className="mono"
            data-testid="fleet-add-connection-json"
            disabled={busy}
            onChange={(event) =>
              discovery.updateConnectionJson(event.currentTarget.value)
            }
            placeholder='{"label":"api-gateway","agentDid":"did:key:z6Mk...","addr":"/ip4/100.73.235.38/tcp/9161/p2p/12D3Koo..."}'
            value={discovery.connectionJson}
          />
        </label>
        <PeerField
          label="Agent label"
          testId="fleet-add-label"
          value={peerForm.label}
          placeholder="api-gateway"
          disabled={busy}
          onChange={(label) => onPeerFormChange({ ...peerForm, label })}
        />
        <PeerField
          mono
          label="Agent DID"
          testId="fleet-add-agent-did"
          value={peerForm.agentDid}
          placeholder="did:key:z6Mk..."
          disabled={busy}
          onChange={(agentDid) => onPeerFormChange({ ...peerForm, agentDid })}
        />
        <PeerField
          mono
          label="P2P address"
          testId="fleet-add-addr"
          value={peerForm.addr}
          placeholder="/ip4/100.73.235.38/tcp/9161/p2p/12D3Koo..."
          disabled={busy}
          onChange={(addr) => onPeerFormChange({ ...peerForm, addr })}
        />
        <PeerField
          mono
          label="GraphQL endpoint"
          testId="fleet-add-graphql"
          value={peerForm.graphql ?? ""}
          placeholder="http://127.0.0.1:9181/api/v0/graphql"
          disabled={busy}
          onChange={(graphql) => onPeerFormChange({ ...peerForm, graphql })}
        />
        {localError ? <p className="fleet-inline-error">{localError}</p> : null}
        <div className="fleet-add-actions">
          <button
            className="primary-button"
            data-testid="fleet-add-submit"
            disabled={
              disabled ||
              addingPeer ||
              discovery.fetchingStatus ||
              (!discovery.manualPeerReady && !discovery.serverAddressReady)
            }
            type="submit"
          >
            {discovery.fetchingStatus
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
  );
}

function PeerField({
  disabled,
  label,
  mono = false,
  placeholder,
  testId,
  value,
  onChange,
}: {
  disabled: boolean;
  label: string;
  mono?: boolean;
  placeholder: string;
  testId: string;
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <label className="field">
      <span>{label}</span>
      <input
        className={mono ? "mono" : undefined}
        data-testid={testId}
        disabled={disabled}
        onChange={(event) => onChange(event.currentTarget.value)}
        placeholder={placeholder}
        value={value}
      />
    </label>
  );
}
