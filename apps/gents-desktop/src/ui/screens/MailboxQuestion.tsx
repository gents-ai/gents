import { parseMailboxQuestion } from "@source-inc/gents-desktop-client";
export const parseQuestion = parseMailboxQuestion;
import { Check } from "lucide-react";
import type {
  MailboxQuestion,
  MailboxQuestionAnswer,
} from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import { useState } from "react";
import { toastFailure } from "@/lib/failure";

export function QuestionAnswer({
  question,
  onAnswer,
}: {
  question: MailboxQuestion;
  onAnswer: (answer: MailboxQuestionAnswer) => Promise<void>;
}) {
  const [selected, setSelected] = useState<string[]>([]);
  const [other, setOther] = useState("");
  const [sending, setSending] = useState(false);
  const [sent, setSent] = useState(false);
  const send = async (answer: MailboxQuestionAnswer) => {
    setSending(true);
    try {
      await onAnswer(answer);
      setSent(true);
    } catch (error) {
      toastFailure("send the answer", error);
    } finally {
      setSending(false);
    }
  };
  const note = other.trim() || null;
  const deferred = question.multi_select || (question.allow_free_text && note !== null);
  const choose = (id: string) => {
    if (!deferred) {
      void send({ option_ids: [id], free_text: null });
      return;
    }
    setSelected((current) =>
      current.includes(id)
        ? current.filter((value) => value !== id)
        : question.multi_select
          ? [...current, id]
          : [id],
    );
  };
  const canSend = selected.length > 0 || note !== null;
  if (sent) {
    return (
      <div className="mt-3 max-w-prose" data-testid="mailbox-question">
        <p className="text-sm text-foreground">{question.prompt}</p>
        <p className="mt-2 text-xs text-muted-foreground">Answer sent to the agent.</p>
      </div>
    );
  }
  return (
    <div className="mt-3 max-w-prose" data-testid="mailbox-question">
      <p className="text-sm text-foreground">{question.prompt}</p>
      <div
        className="mt-2 flex flex-wrap gap-2"
        role="group"
        aria-label={question.prompt}
      >
        {question.options.map((option) => {
          const on = selected.includes(option.id);
          return (
            <Button
              key={option.id}
              size="sm"
              variant={on ? "brand" : "raised"}
              aria-pressed={deferred ? on : undefined}
              title={option.description ?? undefined}
              disabled={sending}
              onClick={() => choose(option.id)}
            >
              {deferred && on && <Check />}
              {option.label}
            </Button>
          );
        })}
      </div>
      {question.options.some((option) => option.description) && (
        <dl className="mt-2 grid grid-cols-[auto_minmax(0,1fr)] gap-x-2 gap-y-0.5 text-xs text-muted-foreground">
          {question.options
            .filter((option) => option.description)
            .map((option) => (
              <div key={option.id} className="contents">
                <dt className="text-foreground">{option.label}</dt>
                <dd>{option.description}</dd>
              </div>
            ))}
        </dl>
      )}
      {(question.allow_free_text || question.multi_select) && (
        <form
          className="mt-2 flex items-center gap-2"
          onSubmit={(event) => {
            event.preventDefault();
            if (canSend) void send({ option_ids: selected, free_text: note });
          }}
        >
          {question.allow_free_text && (
            <Input
              aria-label="Other"
              placeholder="Other…"
              value={other}
              disabled={sending}
              onChange={(event) => setOther(event.target.value)}
              className="h-8 max-w-xs text-sm"
            />
          )}
          <Button
            type="submit"
            size="sm"
            variant="brand"
            disabled={!canSend || sending}
          >
            Send
          </Button>
        </form>
      )}
    </div>
  );
}
