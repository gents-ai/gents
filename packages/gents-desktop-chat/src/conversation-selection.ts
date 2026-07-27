import type { ConversationSummary } from "./types.js";

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

  // Conversations created before behavior routing was persisted belonged to
  // the principal's default behavior. Keep that history visible without
  // mixing it into every other behavior.
  return selectedBehaviorId === defaultBehaviorId;
}
