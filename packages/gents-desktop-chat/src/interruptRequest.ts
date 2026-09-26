import type { InterruptRequestResult } from "@source-inc/gents-desktop-client";

// Request shapes mirror the plugin's Rust requests (see
// crates/gents-desktop-bridge/src/types/requests.rs).
// Tauri serializes the params object directly, and tauri::command expects an
// argument named `request` matching the Rust handler signature, so each call
// wraps its body under { request }.

/** Interrupts exactly `requestId`; requests it caused keep running. */
export type DesktopInterruptRequestArgs = {
  requestId: string;
  agentDid?: string | null;
  cause: "userCancelled";
};

export type InterruptChatRequest = (
  request: DesktopInterruptRequestArgs,
) => Promise<InterruptRequestResult>;
