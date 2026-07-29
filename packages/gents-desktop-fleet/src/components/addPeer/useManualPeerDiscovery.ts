import { useState } from "react";

import type { PeerAddRequest } from "@source-inc/gents-desktop-client";
import { formatPeerConnectionError } from "../../peerConnectionErrors.js";
import { parsePeerConnectionJson } from "../../peerConnectionImport.js";

export type ManualPeerDiscoveryOptions = {
  peerForm: PeerAddRequest;
  onPeerFormChange: (value: PeerAddRequest) => void;
  onProbePeerAddress: (serverAddress: string) => Promise<unknown>;
  onSubmit: (request: PeerAddRequest) => Promise<void>;
};

export function useManualPeerDiscovery({
  peerForm,
  onPeerFormChange,
  onProbePeerAddress,
  onSubmit,
}: ManualPeerDiscoveryOptions) {
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
    if (!trimmed) throw new Error("Server address is required");

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

  async function submit() {
    try {
      const request = manualPeerReady
        ? withGraphqlFallback(peerForm, serverAddress)
        : await fetchServerStatus();
      await onSubmit(request);
    } catch {
      // Field-level and parent errors are rendered in the form.
    }
  }

  async function fetchStatus() {
    try {
      await fetchServerStatus();
    } catch {
      // The status line renders the discovery error.
    }
  }

  return {
    connectionJson,
    fetchingStatus,
    importError,
    importStatus,
    manualPeerReady,
    serverAddress,
    serverAddressReady,
    fetchStatus,
    submit,
    updateConnectionJson,
    updateServerAddress,
  };
}

export type ManualPeerDiscoveryController = ReturnType<
  typeof useManualPeerDiscovery
>;

function looksLikeGraphqlEndpoint(value: string) {
  const trimmed = value.trim();
  return /\/graphql\/?$/i.test(trimmed.split(/[?#]/, 1)[0] ?? "");
}

function withGraphqlFallback(
  request: PeerAddRequest,
  serverAddress: string,
): PeerAddRequest {
  if (request.graphql?.trim()) return request;
  if (!looksLikeGraphqlEndpoint(serverAddress)) return request;
  return { ...request, graphql: serverAddress.trim() };
}
