import type { ClientAccessRequestView } from "@source-inc/gents-desktop-client";
import { StatusEnrollmentForm } from "./addPeer/StatusEnrollmentForm.js";
import { useStatusEnrollment } from "./addPeer/useStatusEnrollment.js";

export type AddPeerFormProps = {
  addingPeer: boolean;
  disabled: boolean;
  localError: string | null;
  onRequestRuntimeAccess: (
    serverAddress: string,
  ) => Promise<ClientAccessRequestView>;
};

export function AddPeerForm({
  addingPeer,
  disabled,
  localError,
  onRequestRuntimeAccess,
}: AddPeerFormProps) {
  const discovery = useStatusEnrollment({
    onRequestRuntimeAccess,
  });
  const busy = disabled || addingPeer || discovery.fetchingStatus;

  return (
    <div className="fleet-enrollment">
      <StatusEnrollmentForm
        addingPeer={addingPeer}
        busy={busy}
        disabled={disabled}
        discovery={discovery}
        localError={localError}
      />
    </div>
  );
}
