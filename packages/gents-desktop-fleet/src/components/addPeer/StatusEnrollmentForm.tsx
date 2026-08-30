import type { StatusEnrollmentController } from "./useStatusEnrollment.js";

export type StatusEnrollmentFormProps = {
  addingPeer: boolean;
  busy: boolean;
  disabled: boolean;
  localError: string | null;
  discovery: StatusEnrollmentController;
};

export function StatusEnrollmentForm({
  addingPeer,
  busy,
  disabled,
  localError,
  discovery,
}: StatusEnrollmentFormProps) {
  return (
    <form
      className="fleet-status-form"
      data-testid="fleet-status-form"
      onSubmit={(event) => {
        event.preventDefault();
        void discovery.connectFromStatus();
      }}
    >
      <div className="fleet-pairing-copy">
        <p className="eyebrow">Recommended</p>
        <h3>Connect by server address</h3>
        <p className="muted">
          Enter an agent&apos;s IP address, hostname, or URL. Gents reads its
          <code> /status</code> offer, authenticates the server, and requests
          enrollment. The server must approve the request before chat opens.
        </p>
      </div>
      <div className="fleet-discovery-row">
        <label className="field">
          <span>Agent server</span>
          <input
            className="mono"
            data-testid="fleet-add-server-address"
            disabled={busy}
            onChange={(event) =>
              discovery.updateServerAddress(event.currentTarget.value)
            }
            placeholder="100.69.4.79:9191"
            value={discovery.serverAddress}
          />
        </label>
        <button
          className="primary-button"
          data-testid="fleet-fetch-status"
          disabled={busy || !discovery.serverAddressReady}
          type="submit"
        >
          {discovery.fetchingStatus
            ? "Connecting..."
            : addingPeer
              ? "Adding..."
              : disabled
                ? "Preparing..."
                : "Request enrollment"}
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
      {localError ? <p className="fleet-inline-error">{localError}</p> : null}
    </form>
  );
}
