import { useMemo, type ReactNode } from "react";
import { Check } from "lucide-react";
import type { QuestionReplyView } from "@source-inc/gents-desktop-client";
import { useSessionFacts } from "@/hooks/useSelectedSession";
import { filedQuestion } from "./FiledQuestion";

export function AnswerText({
  reply,
  fallback,
}: {
  reply: QuestionReplyView;
  fallback: ReactNode;
}) {
  const tools = useSessionFacts()?.tools;
  const question = useMemo(() => {
    for (const tool of tools ?? []) {
      const filed = filedQuestion(tool);
      if (filed?.itemId === reply.sourceDocId) return filed.question;
    }
    return null;
  }, [tools, reply.sourceDocId]);
  const answer = reply.answer;
  if (!question || !answer) return <>{fallback}</>;
  const chosen = answer.option_ids.map(
    (id) => question.options.find((option) => option.id === id)?.label ?? id,
  );
  const text = answer.free_text?.trim();
  return (
    <span data-testid="question-answer" className="grid gap-3 py-0.5">
      <span className="grid gap-1">
        <span className="text-xs font-medium text-muted-foreground">Answered</span>
        <span className="break-words text-sm text-muted-foreground">
          {question.prompt}
        </span>
      </span>
      {answer.skip ? (
        <span className="text-sm text-muted-foreground italic">Skipped</span>
      ) : (
        <span className="grid gap-2">
          {chosen.length > 0 && (
            <span className="flex flex-wrap gap-1.5">
              {chosen.map((label, i) => (
                <span
                  key={i}
                  data-testid="question-answer-choice"
                  className="inline-flex items-center gap-1.5 rounded-md border border-border bg-background/60 px-2.5 py-1 text-sm text-foreground"
                >
                  <Check aria-hidden className="size-3.5 text-brand" />
                  {label}
                </span>
              ))}
            </span>
          )}
          {text && <span className="whitespace-pre-wrap break-words">{text}</span>}
        </span>
      )}
    </span>
  );
}
