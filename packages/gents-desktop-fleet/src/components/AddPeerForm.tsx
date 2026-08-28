import type { ReactNode } from "react";

import type {
  BearerPairingRequest,
  BearerPairingResponse,
  PeerAddRequest,
} from "@source-inc/gents-desktop-client";
import { BearerPairingForm } from "./addPeer/BearerPairingForm.js";
import { ManualPeerDiscoveryForm } from "./addPeer/ManualPeerDiscoveryForm.js";
import { useManualPeerDiscovery } from "./addPeer/useManualPeerDiscovery.js";

export type AddPeerFormProps = {
  addingPeer: boolean;
  disabled: boolean;
  localError: string | null;
  peerForm: PeerAddRequest;
  onPeerFormChange: (value: PeerAddRequest) => void;
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
  const discovery = useManualPeerDiscovery({
    peerForm,
    onPeerFormChange,
    onProbePeerAddress,
    onSubmit,
  });
  const busy = disabled || addingPeer || discovery.fetchingStatus;

  return (
    <div className="fleet-pairing">
      <ManualPeerDiscoveryForm
        addingPeer={addingPeer}
        busy={busy}
        disabled={disabled}
        discovery={discovery}
        localError={localError}
        peerForm={peerForm}
        onPeerFormChange={onPeerFormChange}
      />
      <details className="fleet-alternative-disclosure">
        <summary>Use a signed pairing invite</summary>
        <BearerPairingForm
          addingPeer={addingPeer}
          busy={busy}
          onPairBearer={onPairBearer}
          pairingQrHint={pairingQrHint}
        />
      </details>
    </div>
  );
}
