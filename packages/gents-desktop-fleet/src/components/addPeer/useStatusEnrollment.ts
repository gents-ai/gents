import { useState } from "react";

import type { EnrollmentRequestView } from "@source-inc/gents-desktop-client";
import { formatPeerConnectionError } from "../../peerConnectionErrors.js";

export type StatusEnrollmentOptions = {
  onRequestStatusEnrollment: (
    serverAddress: string,
  ) => Promise<EnrollmentRequestView>;
};

export function useStatusEnrollment({
  onRequestStatusEnrollment,
}: StatusEnrollmentOptions) {
  const [importStatus, setImportStatus] = useState<string | null>(null);
  const [importError, setImportError] = useState(false);
  const [serverAddress, setServerAddress] = useState("");
  const [fetchingStatus, setFetchingStatus] = useState(false);
  const serverAddressReady = Boolean(serverAddress.trim());

  function updateServerAddress(value: string) {
    setServerAddress(value);
    setImportStatus(null);
    setImportError(false);
  }

  async function connectFromStatus() {
    const trimmed = serverAddress.trim();
    if (!trimmed) throw new Error("Server address is required");

    setFetchingStatus(true);
    setImportStatus(null);
    try {
      const enrollment = await onRequestStatusEnrollment(trimmed);
      setImportStatus(
        `Enrollment request ${enrollment.requestId} sent · awaiting server approval`,
      );
      setImportError(false);
      return enrollment;
    } catch (error) {
      setImportStatus(formatPeerConnectionError(error, "peer-status"));
      setImportError(true);
      return null;
    } finally {
      setFetchingStatus(false);
    }
  }

  return {
    fetchingStatus,
    importError,
    importStatus,
    serverAddress,
    serverAddressReady,
    connectFromStatus,
    updateServerAddress,
  };
}

export type StatusEnrollmentController = ReturnType<typeof useStatusEnrollment>;
