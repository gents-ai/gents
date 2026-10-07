/* The answer surface for an `ask` item whose payload is a typed question:
   one button per option (a click sends it), or toggles plus Send when the
   question takes several, and an "Other" field when it accepts free text.
   Sending is the item's ordinary reply request; the bridge renders its
   content from the question. */
import { Check } from "lucide-react";
import type {
  MailboxItemView,
  MailboxQuestion,
  MailboxQuestionAnswer,
  MailboxQuestionOption,
} from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import { useState } from "react";
import { toastFailure } from "@/lib/failure";

/* the question's shape, checked in full: any ask may carry an arbitrary
   payload, and only a well-formed question gets the answer surface */
const text = (value: unknown): value is string =>
  typeof value === "string" && value.trim() !== "";
const flag = (value: unknown) => value === undefined || typeof value === "boolean";
/* the runtime's types deny unknown fields; a payload it could not decode
   must not offer answers the bridge would refuse */
const only = (value: object, keys: string[]) =>
  Object.keys(value).every((key) => keys.includes(key));

function option(value: unknown): MailboxQuestionOption | null {
  if (typeof value !== "object" || value === null) return null;
  if (!only(value, ["id", "label", "description"])) return null;
  const { id, label, description } = value as Record<string, unknown>;
  if (!text(id) || !text(label)) return null;
  if (
    description !== undefined &&
    description !== null &&
    typeof description !== "string"
  ) {
    return null;
  }
  return { id, label, description: description ?? null };
}

export function parseQuestion(item: MailboxItemView): MailboxQuestion | null {
  if (item.kind !== "ask" || item.action !== "start_request" || !item.payload) {
    return null;
  }
  let value: unknown;
  try {
    value = JSON.parse(item.payload);
  } catch {
    return null;
  }
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  if (
    !only(value, ["version", "prompt", "options", "multi_select", "allow_free_text"])
  ) {
    return null;
  }
  const { version, prompt, options, multi_select, allow_free_text } = value as Record<
    string,
    unknown
  >;
  if (version !== 1 || !text(prompt) || !Array.isArray(options)) return null;
  if (options.length < 2 || options.length > 4) return null;
  if (!flag(multi_select) || !flag(allow_free_text)) return null;
  const parsed = options.map(option);
  if (parsed.some((entry) => entry === null)) return null;
  const valid = parsed as MailboxQuestionOption[];
  if (new Set(valid.map((entry) => entry.id.trim())).size !== valid.length) return null;
  return {
    version,
    prompt,
    options: valid,
    multi_select: multi_select === true,
    allow_free_text: allow_free_text === true,
  };
}

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
  /* a sent answer may wait behind the asking turn while the item stays
     open; only a failed send offers the controls again */
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
  /* a single choice sends on click unless a note is being written with it */
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
