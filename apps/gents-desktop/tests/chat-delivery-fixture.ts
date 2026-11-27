import { createChatDelivery } from "../src/hooks/chatDelivery";
import { shellStores } from "./shell-fixture";
import { memorySendAttachments } from "./send-attachments-fixture";
import type {
  ChatSendRequest,
  ChatSendResult,
  DesktopApiAdapter,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";

export function acceptedChat(request: ChatSendRequest): ChatSendResult {
  return {
    publication: "delivered",
    nodeDid: request.nodeDid,
    requestId: request.requestId!,
    sessionId: request.sessionId!,
    agentDid: request.agentDid ?? null,
  };
}

export async function acceptedChatReply(request: ChatSendRequest) {
  return acceptedChat(request);
}

export function chatDeliveryFixture(
  sendChatMessage: DesktopApiAdapter["sendChatMessage"],
) {
  const stores = shellStores({
    deployments: [{ nodeDid: "node", sessions: [], mailboxItems: [] }],
    selection: { nodeDid: "node", sessionId: "session" },
  });
  stores.client.setState({
    snapshot: { client: { clientDid: "client" } } as DesktopClientSnapshot,
  });
  const api = { sendChatMessage } as DesktopApiAdapter;
  return { stores, delivery: createChatDelivery(api, stores, memorySendAttachments()) };
}
