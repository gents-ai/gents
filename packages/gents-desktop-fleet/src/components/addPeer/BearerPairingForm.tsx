import { useState, type FormEvent, type ReactNode } from "react";

import type {
  BearerPairingRequest,
  BearerPairingResponse,
} from "@source-inc/gents-desktop-client";
import { formatPeerConnectionError } from "../../peerConnectionErrors.js";
import { QrScannerDialog } from "../QrScannerDialog.js";

export type BearerPairingFormProps = {
  addingPeer: boolean;
  busy: boolean;
  onPairBearer: (
    request: BearerPairingRequest,
  ) => Promise<BearerPairingResponse>;
  pairingQrHint?: ReactNode;
};

export function BearerPairingForm({
  addingPeer,
  busy,
  onPairBearer,
  pairingQrHint,
}: BearerPairingFormProps) {
  const [bearerToken, setBearerToken] = useState("");
  const [pairLabel, setPairLabel] = useState("");
  const [pairingStatus, setPairingStatus] = useState<string | null>(null);
  const [pairingError, setPairingError] = useState(false);
  const [scannerOpen, setScannerOpen] = useState(false);
  const bearerReady = bearerToken.trim().startsWith("dabear1-");

  function updateBearerToken(value: string) {
    setBearerToken(value);
    setPairingStatus(null);
    setPairingError(false);
  }

  async function handleSubmit(event: FormEvent) {
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

  return (
    <>
      <form
        className="fleet-bearer-form"
        onSubmit={(event) => void handleSubmit(event)}
      >
        <div className="fleet-pairing-copy">
          <h3>Pair with a signed invite</h3>
          <p className="muted">
            Scan the QR code on your agent or paste its one-time invite. The app
            verifies the agent, submits this phone&apos;s claim, and configures
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
      {scannerOpen ? (
        <QrScannerDialog
          onClose={() => setScannerOpen(false)}
          onScan={updateBearerToken}
          pairingHint={pairingQrHint}
        />
      ) : null}
    </>
  );
}
