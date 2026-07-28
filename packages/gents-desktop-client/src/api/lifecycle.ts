import { getDesktopApiAdapter, normalizeInitSummary } from "./adapter.js";
import type { DesktopApiAdapter } from "./types.js";

export function fetchDesktopSnapshot(api?: DesktopApiAdapter) {
  return getDesktopApiAdapter(api).fetchDesktopSnapshot();
}

export async function initLocalStandardRuntime(
  request: {
    label: string;
    dangerouslyOverwrite: boolean;
    reset: boolean;
  },
  api?: DesktopApiAdapter,
) {
  return normalizeInitSummary(
    await getDesktopApiAdapter(api).initLocalStandardRuntime(request),
  );
}

export function startDesktopClient(api?: DesktopApiAdapter) {
  return getDesktopApiAdapter(api).startDesktopClient();
}

export function shutdownDesktopClient(api?: DesktopApiAdapter) {
  return getDesktopApiAdapter(api).shutdownDesktopClient();
}

export function setSelectedAgent(
  agentDid: string | null,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).setSelectedAgent(agentDid);
}
