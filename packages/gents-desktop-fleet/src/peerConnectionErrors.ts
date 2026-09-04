export type PeerConnectionAction =
  | "add-peer"
  | "local-runtime"
  | "peer-status"
  | "remove-peer"
  | "rename-peer"
  | "repair-p2p";

export function formatPeerConnectionError(
  error: unknown,
  action: PeerConnectionAction,
  copy: Pick<FleetCopy, "runtimeProductName" | "cliBinaryName"> = {},
): string {
  const message = errorMessage(error);
  // Prefer the bridge's structured endpoint (BridgeErrorCode.endpointUnreachable,
  // #1339) when the error carries one; only a command not yet wired to emit a
  // typed BridgeError falls back to parsing the underlying transport message.
  const url = structuredEndpointFromError(error) ?? requestUrlFromMessage(message);
  if (!url) {
    return message;
  }

  const endpoint = endpointLabel(url);
  if (action === "local-runtime") {
    const productName =
      copy.runtimeProductName?.trim() || DEFAULT_RUNTIME_PRODUCT_NAME;
    const cliBinaryName = copy.cliBinaryName?.trim() || DEFAULT_CLI_BINARY_NAME;
    return `Could not reach the local ${productName} runtime at ${endpoint}. Start \`${cliBinaryName} server\` and try again.`;
  }

  if (action === "peer-status") {
    return `Could not fetch runtime connection details from ${endpoint}. Check that the runtime is running and the address is reachable.`;
  }

  return message;
}

function structuredEndpointFromError(error: unknown): string | null {
  const payload = asBridgeErrorPayload(error);
  return payload?.code === "endpointUnreachable" ? payload.endpoint : null;
}

function errorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }
  return String(error);
}

function requestUrlFromMessage(message: string): string | null {
  const match = message.match(
    /\b(?:sending|reading) GET request to (https?:\/\/\S+)/i,
  );
  if (!match) {
    return null;
  }
  return stripTrailingPunctuation(match[1]);
}

function stripTrailingPunctuation(value: string): string {
  return value.replace(/[),.;]+$/g, "");
}

function endpointLabel(url: string): string {
  try {
    const parsed = new URL(url);
    return parsed.origin;
  } catch {
    return url;
  }
}
import { asBridgeErrorPayload } from "@source-inc/gents-desktop-client";

import {
  DEFAULT_CLI_BINARY_NAME,
  DEFAULT_RUNTIME_PRODUCT_NAME,
  type FleetCopy,
} from "./copy.js";
