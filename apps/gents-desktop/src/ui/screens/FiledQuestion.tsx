import {
  parseMailboxQuestion,
  type MailboxItemView,
  type MailboxQuestion,
  type RenderedToolCallView,
} from "@source-inc/gents-desktop-client";
import { useFleet } from "../hooks/useFleet";

/** The question a mailbox filing stored, read from the item in the tool's
    result rather than its arguments, so the transcript shows what the
    person was actually asked. */
export function filedQuestion(
  tool: RenderedToolCallView,
): { itemId: string; question: MailboxQuestion } | null {
  const p = tool.presentation;
  if (tool.toolName !== "file_mailbox_item" || p.kind !== "generic") return null;
  try {
    const { item } = JSON.parse(p.output ?? "") as {
      item?: { _docID?: unknown; kind?: unknown; action?: unknown; payload?: unknown };
    };
    if (!item || typeof item._docID !== "string" || typeof item.payload !== "string")
      return null;
    const question = parseMailboxQuestion({
      kind: item.kind,
      action: item.action,
      payload: item.payload,
    } as MailboxItemView);
    return question ? { itemId: item._docID, question } : null;
  } catch {
    return null;
  }
}

/** A question in the transcript where the agent asked it: the prompt, its
    choices, and whether it is still waiting for the person. */
export function FiledQuestion({
  itemId,
  question,
}: {
  itemId: string;
  question: MailboxQuestion;
}) {
  const waiting = useFleet((state) =>
    Object.values(state.mailboxOf).some((items) =>
      items.some((m) => m.itemId === itemId && m.status === "open"),
    ),
  );
  return (
    <div className="grid gap-1.5 pb-1 text-sm" data-testid="filed-question">
      <p className="break-words text-foreground">{question.prompt}</p>
      {question.options.length > 0 && (
        <ul className="grid gap-1">
          {question.options.map((option) => (
            <li key={option.id} className="flex items-start gap-2.5">
              <span
                aria-hidden
                className="mt-2 size-1.5 shrink-0 rounded-full bg-muted-foreground/50"
              />
              <span className="min-w-0 break-words">
                <span className="text-foreground">{option.label}</span>
                {option.description && (
                  <span className="block text-xs text-muted-foreground">
                    {option.description}
                  </span>
                )}
              </span>
            </li>
          ))}
        </ul>
      )}
      <p className="text-xs text-muted-foreground">
        {[
          question.multi_select && "Any that apply",
          question.allow_free_text &&
            (question.options.length ? "Or a written answer" : "A written answer"),
          question.version >= 2 && question.optional && "Optional",
          waiting && "Waiting for your answer",
        ]
          .filter(Boolean)
          .join(" · ")}
      </p>
    </div>
  );
}
