import type { SavedRuntimeView } from "@source-inc/gents-desktop-client/generated/SavedRuntimeView";

import { isSavedRemote } from "@source-inc/gents-desktop-client";

export function continueLabel(peers: SavedRuntimeView[]): string | null {
  const remotes = peers.filter((peer) => isSavedRemote(peer));
  if (remotes.length === 0) return null;
  return remotes.length === 1
    ? `Continue with ${remotes[0].label}`
    : "Continue with your other nodes";
}
