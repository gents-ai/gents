export type PeerConnectionAction =
  | "add-peer"
  | "local-runtime"
  | "peer-status"
  | "repair-p2p";

export function formatPeerConnectionError(
  error: unknown,
  action: PeerConnectionAction,
): string {
  const message = errorMessage(error);
  const url = requestUrlFromMessage(message);
  if (!url) {
    return message;
  }

  const endpoint = endpointLabel(url);
  if (action === "local-runtime") {
    return `Could not reach the local defra-agent runtime at ${endpoint}. Start \`defra-agent server\` and try again.`;
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

function requestUrlFromMessage(message: string): string | null {
  const match = message.match(/\b(?:sending|reading) GET request to (https?:\/\/\S+)/i);
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
