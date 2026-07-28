import type { ConversationSummary } from "@source-inc/gents-desktop-client";

export function conversationStatusClass(conversation: ConversationSummary) {
  const state = (conversation.turnState ?? conversation.status ?? "").toLowerCase();

  switch (state) {
    case "completed":
      return "conversation-status-dot conversation-status-dot-success";
    case "failed":
    case "error":
    case "cancelled":
      return "conversation-status-dot conversation-status-dot-error";
    case "streaming":
    case "waitingforclaim":
    case "processing":
    case "active":
      return "conversation-status-dot conversation-status-dot-running";
    default:
      return "conversation-status-dot conversation-status-dot-idle";
  }
}

export function boolText(value?: boolean | null) {
  return value === false ? "disabled" : "enabled";
}
