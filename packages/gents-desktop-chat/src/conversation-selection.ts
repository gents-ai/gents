import type { ConversationSummary } from "@source-inc/gents-desktop-client";

export function conversationBelongsToBehavior(
  conversation: ConversationSummary,
  selectedBehaviorId: string | null,
  defaultBehaviorId: string | null,
) {
  if (!selectedBehaviorId) {
    return true;
  }
  if (conversation.behaviorId) {
    return conversation.behaviorId === selectedBehaviorId;
  }

  return selectedBehaviorId === defaultBehaviorId;
}
