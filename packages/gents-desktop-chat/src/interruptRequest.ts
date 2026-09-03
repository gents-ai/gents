import type {
  CascadeCancelPreview,
  InterruptRequestResult,
} from "@source-inc/gents-desktop-client";

// Request shapes mirror the plugin's Rust requests (see
// crates/gents-desktop-bridge/src/types/requests.rs).
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
  cause: "userCancelled";
  cascade: boolean;
  expectedPreviewSignature?: string;
};

export type PreviewChatInterruptCascade = (
  request: DesktopPreviewInterruptCascadeArgs,
) => Promise<CascadeCancelPreview>;

export type InterruptChatRequest = (
  request: DesktopInterruptRequestArgs,
) => Promise<InterruptRequestResult>;
