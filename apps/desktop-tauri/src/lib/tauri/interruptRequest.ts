import { invoke } from "@tauri-apps/api/core";

import type {
  CascadeCancelPreview,
  InterruptRequestResult,
} from "../types/operations";

// Request shapes mirror Rust bridge requests (see apps/desktop-tauri/src-tauri/src/bridge/types/requests.rs).
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
  cause: "userCancelled"; // operator-authentic only
  cascade: boolean;
  expectedPreviewSignature?: string;
};

export async function previewInterruptCascade(
  request: DesktopPreviewInterruptCascadeArgs,
): Promise<CascadeCancelPreview> {
  return invoke<CascadeCancelPreview>("desktop_preview_interrupt_cascade", { request });
}

export async function interruptRequest(
  request: DesktopInterruptRequestArgs,
): Promise<InterruptRequestResult> {
  return invoke<InterruptRequestResult>("desktop_interrupt_request", { request });
}
