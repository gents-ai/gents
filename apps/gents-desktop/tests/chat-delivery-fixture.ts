import type { ChatSendRequest, ChatSendResult } from "@source-inc/gents-desktop-client";

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
