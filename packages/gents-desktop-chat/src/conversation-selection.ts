import type { ConversationSummary } from "@source-inc/gents-desktop-client";

export function conversationBelongsToBehavior(
  conversation: ConversationSummary,
  selectedBehaviorId: string | null,
) {
  if (!selectedBehaviorId) {
    return true;
  }
  return conversation.behaviorId === selectedBehaviorId;
}
