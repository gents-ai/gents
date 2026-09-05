import { asBridgeErrorPayload } from "@source-inc/gents-desktop-client";

import {
  DEFAULT_CLI_BINARY_NAME,
  DEFAULT_RUNTIME_PRODUCT_NAME,
  type FleetCopy,
} from "./copy.js";

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
  // The bridge classifies connectivity failures into
  // BridgeErrorCode.endpointUnreachable and attaches the endpoint as a
  // structured field (#1339) — no message parsing on the TS side. Prefer the
  // payload's own `message` (correctly extracted even for a plain object)
  // over the generic `errorMessage` fallback, which only handles `Error`/
  // string inputs.
  const payload = asBridgeErrorPayload(error);
  const message = payload?.message ?? errorMessage(error);
  if (payload?.code !== "endpointUnreachable" || !payload.endpoint) {
    return message;
  }

  const endpoint = endpointLabel(payload.endpoint);
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

function errorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message;
  }
  return String(error);
}

function endpointLabel(url: string): string {
  try {
    const parsed = new URL(url);
    return parsed.origin;
  } catch {
    return url;
  }
}
