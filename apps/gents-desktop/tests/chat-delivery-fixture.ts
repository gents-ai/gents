import type { ChatSendRequest, ChatSendResult } from "@source-inc/gents-desktop-client";

export function acceptedChat(request: ChatSendRequest): ChatSendResult {
  const result = {
    nodeDid: request.nodeDid,
    requestId: request.requestId!,
    sessionId: request.sessionId!,
    agentId: request.agentId ?? null,
  };
  return {
    ...result,
    submissionReceipt: {
      ...result,
      envelopeHash: "sha256:" + "a".repeat(64),
      signedPayloadHash: "sha256:" + "b".repeat(64),
    },
  };
}

export async function acceptedChatReply(request: ChatSendRequest) {
  return acceptedChat(request);
}
