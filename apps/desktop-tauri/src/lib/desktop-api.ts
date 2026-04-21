import { invoke } from "@tauri-apps/api/core";

import type {
  ChatSendResult,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
  InitSummary,
} from "./types";

export async function fetchDesktopSnapshot() {
  return invoke<DesktopClientSnapshot>("desktop_client_snapshot");
}

export async function initLocalStandardRuntime(request: {
  label: string;
  dangerouslyOverwrite: boolean;
  reset: boolean;
}) {
  return invoke<InitSummary>("desktop_init_local_standard", { request });
}

export async function startDesktopClient() {
  return invoke<DesktopClientSnapshot>("desktop_client_start");
}

export async function shutdownDesktopClient() {
  return invoke<DesktopClientSnapshot>("desktop_client_shutdown");
}

export async function fetchSessionSnapshot(sessionId: string) {
  return invoke<DesktopSessionSnapshot | null>("desktop_session_snapshot", {
    sessionId,
  });
}

export async function sendChatMessage(request: {
  agentDid: string;
  behaviorId?: string | null;
  sessionId?: string | null;
  content: string;
}) {
  return invoke<ChatSendResult>("desktop_chat_send", { request });
}
