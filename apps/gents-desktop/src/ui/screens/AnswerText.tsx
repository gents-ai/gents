/* A person's message that answers an agent's question, drawn as the answer
   it is: the question it answers and the choices made, from the question as
   the transcript holds it where it was asked. The message's text is what the
   agent reads; it shows only when the question isn't among the loaded rows. */

import { useMemo, type ReactNode } from "react";
import type { QuestionReplyView } from "@source-inc/gents-desktop-client";
import { useSessionFacts } from "@/hooks/useSelectedSession";
import { filedQuestion } from "./FiledQuestion";

export function AnswerText({
  reply,
  fallback,
}: {
  reply: QuestionReplyView;
  /** the message's own text, for an answer whose question isn't loaded */
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
    <span data-testid="question-answer" className="grid gap-1.5">
      <span className="text-xs text-muted-foreground">
        Answered{" "}
        <span className="text-foreground/80" title={question.prompt}>
          “{question.prompt}”
        </span>
      </span>
      {answer.skip ? (
        <span className="text-muted-foreground">Skipped</span>
      ) : (
        <>
          {chosen.length > 0 && (
            <span className="flex flex-wrap gap-1">
              {chosen.map((label, i) => (
                <span
                  key={i}
                  data-testid="question-answer-choice"
                  className="rounded-md border border-border bg-background/60 px-2 py-0.5 text-sm"
                >
                  {label}
                </span>
              ))}
            </span>
          )}
          {text && <span className="whitespace-pre-wrap">{text}</span>}
        </>
      )}
    </span>
  );
}
