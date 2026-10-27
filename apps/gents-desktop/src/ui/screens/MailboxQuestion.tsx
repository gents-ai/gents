import { parseMailboxQuestion } from "@source-inc/gents-desktop-client";
export const parseQuestion = parseMailboxQuestion;
import { Check } from "lucide-react";
import type {
  MailboxQuestion,
  MailboxQuestionAnswer,
} from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import { useEffect, useRef, useState } from "react";
import { toastFailure } from "@/lib/failure";

export function QuestionAnswer({
  question,
  questionHash,
  onAnswer,
  pending = false,
  confirmed = false,
}: {
  question: MailboxQuestion;
  questionHash?: string | null;
  onAnswer: (
    answer: MailboxQuestionAnswer,
    acceptsView: () => boolean,
  ) => Promise<boolean>;
  pending?: boolean;
  confirmed?: boolean;
}) {
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const [unconfirmed, setUnconfirmed] = useState(false);
  const [selected, setSelected] = useState<string[]>([]);
  const [other, setOther] = useState("");
  const [sending, setSending] = useState(false);
  const [sent, setSent] = useState<"answer" | "skip" | null>(null);
  const unavailable = question.version === 2 && !questionHash;
  const send = async (answer: MailboxQuestionAnswer) => {
    if (unavailable || sending || pending || confirmed) return;
    setSending(true);
    try {
      const applied = await onAnswer(
        {
          ...answer,
          ...(question.version === 2 ? { question_hash: questionHash } : {}),
        },
        () => mounted.current,
      );
      if (mounted.current) {
        setSent(applied ? (answer.skip ? "skip" : "answer") : null);
        setUnconfirmed(!applied);
      }
    } catch (error) {
      if (mounted.current) toastFailure("send the answer", error);
    } finally {
      if (mounted.current) setSending(false);
    }
  };
  const note = other.trim() || null;
  const deferred =
    question.version === 2 ||
    question.multi_select ||
    (question.allow_free_text && note !== null);
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
  if (sent || confirmed) {
    return (
      <div className="mt-3 max-w-prose" data-testid="mailbox-question">
        <p className="text-sm text-foreground">{question.prompt}</p>
        <p className="mt-2 text-xs text-muted-foreground">
          {sent === "skip" ? "Question skipped." : "Answer sent to the agent."}
        </p>
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
              disabled={sending || unavailable || pending}
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
      {unavailable && (
        <p role="status">Reconnect to load this question before answering.</p>
      )}
      {unconfirmed && pending && (
        <p role="alert" className="mt-2 text-xs text-muted-foreground">
          Your answer is not confirmed. Use Check answer delivery or Resume saved answer
          below.
        </p>
      )}
      {(question.version === 2 ||
        question.allow_free_text ||
        question.multi_select) && (
        <form
          className="mt-2 flex items-center gap-2"
          onSubmit={(event) => {
            event.preventDefault();
            if (canSend) void send({ option_ids: selected, free_text: note });
          }}
        >
          {question.allow_free_text && (
            <Input
              aria-label={question.options.length ? "Other" : "Your answer"}
              placeholder={question.options.length ? "Other…" : "Your answer…"}
              value={other}
              disabled={sending || unavailable || pending}
              onChange={(event) => setOther(event.target.value)}
              className="h-8 max-w-xs text-sm"
            />
          )}
          <Button
            type="submit"
            size="sm"
            variant="brand"
            disabled={!canSend || sending || unavailable || pending}
          >
            Send
          </Button>
          {question.version === 2 && question.optional && (
            <Button
              type="button"
              size="sm"
              variant="raised"
              disabled={sending || unavailable || pending}
              onClick={() => void send({ option_ids: [], free_text: null, skip: true })}
            >
              Skip
            </Button>
          )}
        </form>
      )}
    </div>
  );
}
