import { CircleHelp } from "lucide-react";
import type { MailboxItemView } from "@source-inc/gents-desktop-client";
import { nodeOf } from "../../../hooks/fleetStore";
import { useFleet } from "../../hooks/useFleet";
import { agentName } from "../behavior";

/** While the person writes instead of answering, a line above the composer
    says the agent is still asking, and brings the question back. */
export function AskedChip({
  item,
  onAnswer,
}: {
  item: MailboxItemView;
  onAnswer: () => void;
}) {
  const filer = useFleet((state) => nodeOf(state, item.nodeDid));
  const agent = agentName(item.targetAgentId, filer);
  return (
    <p className="mb-2 flex items-center gap-1.5 text-xs text-muted-foreground">
      <CircleHelp className="size-3.5 shrink-0" />
      <span className="min-w-0 truncate">{agent} is waiting for your answer</span>
      <button
        type="button"
        onClick={onAnswer}
        className="font-medium text-foreground underline-offset-4 hover:underline"
      >
        Answer
      </button>
    </p>
  );
}
