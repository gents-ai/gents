import type { EnrollmentRequestView } from "@source-inc/gents-desktop-client";
import { StatusEnrollmentForm } from "./addPeer/StatusEnrollmentForm.js";
import { useStatusEnrollment } from "./addPeer/useStatusEnrollment.js";

export type AddPeerFormProps = {
  addingPeer: boolean;
  disabled: boolean;
  localError: string | null;
  onRequestStatusEnrollment: (
    serverAddress: string,
  ) => Promise<EnrollmentRequestView>;
};

export function AddPeerForm({
  addingPeer,
  disabled,
  localError,
  onRequestStatusEnrollment,
}: AddPeerFormProps) {
  const discovery = useStatusEnrollment({
    onRequestStatusEnrollment,
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
