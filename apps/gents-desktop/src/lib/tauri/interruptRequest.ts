import {
  interruptRequest as _interruptRequest,
  previewInterruptCascade as _previewInterruptCascade,
} from "../desktop-api";
import type { CascadeCancelPreview, InterruptRequestResult } from "../types/operations";

// Request shapes mirror Rust bridge requests (see apps/gents-desktop/src-tauri/src/bridge/types/requests.rs).
// Tauri serializes the params object directly, and tauri::command expects an
// argument named `request` matching the Rust handler signature, so each call
// wraps its body under { request }.

export type DesktopPreviewInterruptCascadeArgs = {
  requestId: string;
  agentDid?: string | null;
  includeTerminal?: boolean;
};

export type DesktopInterruptRequestArgs = {
  requestId: string;
  agentDid?: string | null;
  cause: "userCancelled"; // operator-authentic only
  cascade: boolean;
  expectedPreviewSignature?: string;
};

export async function previewInterruptCascade(
  request: DesktopPreviewInterruptCascadeArgs,
): Promise<CascadeCancelPreview> {
  return _previewInterruptCascade(request);
}

export async function interruptRequest(
  request: DesktopInterruptRequestArgs,
): Promise<InterruptRequestResult> {
  return _interruptRequest(request);
}
