import { invoke } from "@tauri-apps/api/core";

import type {
  ChatSendResult,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
  InitSummary,
} from "./types";

export type DesktopApiAdapter = {
  fetchDesktopSnapshot: () => Promise<DesktopClientSnapshot>;
  initLocalStandardRuntime: (request: {
    label: string;
    dangerouslyOverwrite: boolean;
    reset: boolean;
  }) => Promise<InitSummary>;
  startDesktopClient: () => Promise<DesktopClientSnapshot>;
  shutdownDesktopClient: () => Promise<DesktopClientSnapshot>;
  fetchSessionSnapshot: (
    sessionId: string,
    requestId?: string | null,
  ) => Promise<DesktopSessionSnapshot | null>;
  sendChatMessage: (request: {
    agentDid: string;
    behaviorId?: string | null;
    sessionId?: string | null;
    content: string;
  }) => Promise<ChatSendResult>;
  renameConversation: (request: {
    sessionId: string;
    title: string;
  }) => Promise<void>;
};

const defaultDesktopApiAdapter: DesktopApiAdapter = {
  fetchDesktopSnapshot() {
    return invoke<DesktopClientSnapshot>("desktop_client_snapshot");
  },
  initLocalStandardRuntime(request) {
    return invoke<InitSummary>("desktop_init_local_standard", { request });
  },
  startDesktopClient() {
    return invoke<DesktopClientSnapshot>("desktop_client_start");
  },
  shutdownDesktopClient() {
    return invoke<DesktopClientSnapshot>("desktop_client_shutdown");
  },
  fetchSessionSnapshot(sessionId, requestId) {
    return invoke<DesktopSessionSnapshot | null>("desktop_session_snapshot", {
      sessionId,
      requestId,
    });
  },
  sendChatMessage(request) {
    return invoke<ChatSendResult>("desktop_chat_send", { request });
  },
  renameConversation(request) {
    return invoke<void>("desktop_conversation_rename", { request });
  },
};

let desktopApiAdapterOverride: DesktopApiAdapter | null = null;

function desktopApiAdapter() {
  return desktopApiAdapterOverride ?? defaultDesktopApiAdapter;
}

export function setDesktopApiAdapterForTests(adapter: DesktopApiAdapter | null) {
  desktopApiAdapterOverride = adapter;
}

export async function fetchDesktopSnapshot() {
  return desktopApiAdapter().fetchDesktopSnapshot();
}

export async function initLocalStandardRuntime(request: {
  label: string;
  dangerouslyOverwrite: boolean;
  reset: boolean;
}) {
  return desktopApiAdapter().initLocalStandardRuntime(request);
}

export async function startDesktopClient() {
  return desktopApiAdapter().startDesktopClient();
}

export async function shutdownDesktopClient() {
  return desktopApiAdapter().shutdownDesktopClient();
}

export async function fetchSessionSnapshot(
  sessionId: string,
  requestId?: string | null,
) {
  return desktopApiAdapter().fetchSessionSnapshot(sessionId, requestId);
}

export async function sendChatMessage(request: {
  agentDid: string;
  behaviorId?: string | null;
  sessionId?: string | null;
  content: string;
}) {
  return desktopApiAdapter().sendChatMessage(request);
}

export async function renameConversation(request: {
  sessionId: string;
  title: string;
}) {
  return desktopApiAdapter().renameConversation(request);
}
