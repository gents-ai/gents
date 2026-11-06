import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { Check, ChevronLeft, ChevronRight, Pencil, X } from "lucide-react";
import type {
  MailboxItemView,
  MailboxQuestion,
  MailboxQuestionAnswer,
} from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { Kbd } from "@gents/ui/components/kbd";
import { cn } from "@gents/ui/lib/utils";
import { useApp } from "@/app/AppContext";
import { toastFailure } from "@/lib/failure";
import { nodeOf } from "../../../hooks/fleetStore";
import { useFleet } from "../../hooks/useFleet";
import { agentName } from "../behavior";
import { parseQuestion } from "../MailboxQuestion";
import { Markdown } from "../Markdown";
import { useMorph } from "@/lib/morph";

/* a field the panel's composer hides as it opens is not one being written in */
const writingElsewhere = (element: Element | null) =>
  !!element?.matches(
    "input, textarea, select, [contenteditable]:not([contenteditable='false'])",
  ) && !element.closest("[hidden]");

/* each question's answer in progress, kept while the person writes a message
   instead or looks at another question, for as long as the app runs */
type Draft = { selected: string[]; other: string };
const drafts = new Map<string, Draft>();

/** The question an agent is waiting on, in the composer's place: the prompt
    as its title, its choices as a marked list with a faint key each, and a
    last row to write another answer, with Send, or Skip for an optional
    question while nothing is chosen. Where one answer is allowed, picking a
    choice sends it; a choice's pencil copies it into the field to change. One of several open questions shows at
    a time, with arrows between them. A long question scrolls between its
    header and last row. */
export function QuestionPanel({
  items,
  onTypeInstead,
  className,
  where,
  icon,
  aside,
  kind,
}: {
  items: readonly MailboxItemView[];
  /** the person would rather write a message; the question stays open */
  onTypeInstead?: () => void;
  className?: string;
  /** where and when it was asked, for a question shown outside its session */
  where?: ReactNode;
  /** what kind of item this is, where questions sit among other kinds */
  icon?: ReactNode;
  /** at the header's end, before its controls: when it was asked */
  aside?: ReactNode;
  /** a first header line, ahead of who asks: what kind of item this is */
  kind?: ReactNode;
}) {
  const { morph } = useMorph();
  const [at, setAt] = useState(0);
  const shown = Math.min(at, items.length - 1);
  const item = items[shown];
  const question = item ? parseQuestion(item) : null;
  if (!item || !question) return null;
  const step = (by: number) =>
    morph(() => setAt((items.length + shown + by) % items.length));
  return (
    <QuestionForm
      key={item.itemId}
      item={item}
      question={question}
      position={items.length > 1 ? { at: shown, of: items.length, step } : null}
      onTypeInstead={onTypeInstead}
      className={className}
      where={where}
      icon={icon}
      aside={aside}
      kind={kind}
    />
  );
}

function QuestionForm({
  item,
  question,
  position,
  onTypeInstead,
  className,
  where,
  icon,
  aside,
  kind,
}: {
  item: MailboxItemView;
  question: MailboxQuestion;
  position: { at: number; of: number; step: (by: number) => void } | null;
  onTypeInstead?: () => void;
  className?: string;
  where?: ReactNode;
  icon?: ReactNode;
  aside?: ReactNode;
  kind?: ReactNode;
}) {
  const {
    actions: { answerMailboxQuestion },
  } = useApp();
  const filer = useFleet((state) => nodeOf(state, item.nodeDid));
  const agent = agentName(item.targetAgentDid, filer);
  const root = useRef<HTMLElement>(null);
  const field = useRef<HTMLInputElement>(null);
  const mounted = useRef(true);
  const textOnly = question.options.length === 0;
  useEffect(() => {
    mounted.current = true;
    /* keys typed into another field must not become picks here */
    if (!writingElsewhere(document.activeElement)) {
      if (textOnly) field.current?.focus();
      else root.current?.focus();
    }
    return () => {
      mounted.current = false;
    };
  }, [textOnly]);
  const kept = drafts.get(item.itemId);
  const [selected, setSelected] = useState<string[]>(kept?.selected ?? []);
  const [other, setOther] = useState(kept?.other ?? "");
  useEffect(() => {
    drafts.set(item.itemId, { selected, other });
  }, [item.itemId, selected, other]);
  const [sending, setSending] = useState(false);
  const note = question.allow_free_text ? other.trim() || null : null;
  const unavailable = question.version >= 2 && !item.questionHash;
  const chosen = selected.length > 0 || note !== null;
  const skippable = question.version >= 2 && Boolean(question.optional);
  const busy = sending || unavailable;

  /* one answer allowed: picking a row is the answer, so it goes at once */
  const choose = (id: string) => {
    if (busy) return;
    if (!question.multi_select) {
      setOther("");
      setSelected([id]);
      void send({ option_ids: [id], free_text: null });
      return;
    }
    setSelected((current) =>
      current.includes(id) ? current.filter((value) => value !== id) : [...current, id],
    );
  };
  const write = (text: string) => {
    if (!question.multi_select && text.trim()) setSelected([]);
    setOther(text);
  };
  /* a choice that is nearly right becomes written text to change */
  const edit = (label: string) => {
    if (busy) return;
    write(label);
    requestAnimationFrame(() => {
      field.current?.focus();
      field.current?.setSelectionRange(label.length, label.length);
    });
  };
  const send = async (answer: MailboxQuestionAnswer) => {
    if (busy) return;
    setSending(true);
    try {
      const applied = await answerMailboxQuestion(
        item,
        {
          ...answer,
          ...(question.version >= 2 ? { question_hash: item.questionHash } : {}),
        },
        () => mounted.current,
      );
      if (applied) drafts.delete(item.itemId);
    } catch (error) {
      if (mounted.current) toastFailure("send the answer", error);
    } finally {
      if (mounted.current) setSending(false);
    }
  };
  const submit = () => {
    if (chosen && !busy) void send({ option_ids: selected, free_text: note });
  };
  const skip = () => void send({ option_ids: [], free_text: null, skip: true });

  const onKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    const typing = event.target instanceof HTMLInputElement;
    /* an empty field has no caret to move; ← and → step through the questions */
    const caret = event.target instanceof HTMLInputElement && event.target.value !== "";
    if (event.key === "Escape" && onTypeInstead) {
      event.preventDefault();
      onTypeInstead();
    } else if (
      event.key === "Enter" &&
      !event.shiftKey &&
      /* a focused button's Enter is its own click */
      (typing || event.target === root.current)
    ) {
      event.preventDefault();
      submit();
    } else if (!typing && /^[1-9]$/.test(event.key)) {
      /* the key picks a row; it must not also type into the field it opens */
      event.preventDefault();
      const n = Number(event.key) - 1;
      if (n < question.options.length) choose(question.options[n]!.id);
      else if (n === question.options.length && question.allow_free_text)
        field.current?.focus();
    } else if (!caret && position && event.key === "ArrowRight") position.step(1);
    else if (!caret && position && event.key === "ArrowLeft") position.step(-1);
  };

  const action =
    !chosen && skippable ? (
      <Button size="sm" variant="outline" disabled={busy} onClick={skip}>
        Skip
      </Button>
    ) : (
      <Button
        size="sm"
        variant={chosen ? "brand" : "outline"}
        disabled={!chosen || busy}
        onClick={submit}
      >
        Send
      </Button>
    );

  const controls = (
    <>
      {aside}
      {position && (
        <span className="flex items-center gap-0.5 tabular-nums">
          <Button
            variant="quiet"
            size="icon-xs"
            aria-label="Previous question"
            onClick={() => position.step(-1)}
          >
            <ChevronLeft />
          </Button>
          {position.at + 1} of {position.of}
          <Button
            variant="quiet"
            size="icon-xs"
            aria-label="Next question"
            onClick={() => position.step(1)}
          >
            <ChevronRight />
          </Button>
        </span>
      )}
      {onTypeInstead && (
        <Button
          variant="quiet"
          size="icon-xs"
          aria-label="Type a message instead"
          title="Type a message instead (Esc)"
          onClick={onTypeInstead}
        >
          <X />
        </Button>
      )}
    </>
  );

  return (
    <section
      ref={root}
      tabIndex={-1}
      aria-label={`${agent} is asking`}
      data-testid="question-panel"
      onKeyDown={onKeyDown}
      className={cn(
        "flex min-h-0 max-h-[60vh] flex-col rounded-2xl border border-border bg-raised shadow-xs outline-none",
        className,
      )}
    >
      <header className="flex shrink-0 flex-col gap-2 px-4 pt-3 text-xs text-muted-foreground">
        {/* with a kind line, the controls end it and who asks gets a line of
            its own; without, one line holds both */}
        {kind && (
          <div className="flex items-center gap-2">
            <span className="min-w-0 flex-1">{kind}</span>
            {controls}
          </div>
        )}
        <div className="flex items-center gap-2">
          {icon}
          <span className="min-w-0 flex-1 truncate">
            {/* a kind line already says it is a question */}
            {agent}
            {kind ? null : " is asking"}
            {where}
          </span>
          {!kind && controls}
        </div>
      </header>
      <div
        className={cn(
          "min-h-0 overflow-y-auto px-4",
          question.allow_free_text || question.multi_select || skippable
            ? "pb-1"
            : "pb-3",
        )}
      >
        <h3 className="mt-1 mb-2 font-heading text-base font-normal break-words text-foreground">
          {question.prompt}
        </h3>
        {/* the agent's own account of why it asks, when it gave one */}
        {item.summary?.trim() && (
          <div className="prose-app mb-3 text-sm text-muted-foreground">
            <Markdown breaks>{item.summary}</Markdown>
          </div>
        )}
        {question.multi_select && (
          <p className="-mt-1 mb-2 text-xs text-muted-foreground">
            Choose any that apply
          </p>
        )}
        {!textOnly && (
          <div
            role={question.multi_select ? "group" : "radiogroup"}
            aria-label={question.prompt}
            className="grid divide-y divide-foreground/10"
          >
            {question.options.map((option, n) => {
              const on = selected.includes(option.id);
              return (
                <div
                  key={option.id}
                  className={cn(
                    "group/row relative -mx-2 flex items-start pr-2 hover:bg-accent has-focus-visible:bg-accent",
                    on && "bg-accent",
                  )}
                >
                  <button
                    type="button"
                    role={question.multi_select ? "checkbox" : "radio"}
                    aria-checked={on}
                    disabled={busy}
                    onClick={() => choose(option.id)}
                    className="flex min-w-0 flex-1 items-start gap-3 py-2.5 pr-2 pl-2 text-left outline-none after:absolute after:inset-0"
                  >
                    {/* the button covers the row, so its key and the space
                        around the pencil pick the choice too */}
                    {/* the mark leads the row, setting the choices in from the
                      prompt as a list */}
                    {question.multi_select ? (
                      <span
                        aria-hidden
                        className={cn(
                          "mt-0.5 grid size-4 shrink-0 place-items-center rounded border",
                          on
                            ? "border-foreground bg-foreground text-background"
                            : "border-muted-foreground/60",
                        )}
                      >
                        {on && <Check className="size-3" />}
                      </span>
                    ) : (
                      <span
                        aria-hidden
                        className={cn(
                          "mt-2 size-1.5 shrink-0 rounded-full",
                          on ? "bg-foreground" : "bg-muted-foreground/50",
                        )}
                      />
                    )}
                    <span className="min-w-0 flex-1 break-words">
                      <span className="block text-sm text-foreground">
                        {option.label}
                      </span>
                      {option.description && (
                        <span className="block text-xs text-muted-foreground">
                          {option.description}
                        </span>
                      )}
                    </span>
                  </button>
                  {question.allow_free_text && (
                    <Button
                      variant="quiet"
                      size="icon-xs"
                      aria-label={`Edit ${option.label}`}
                      title="Edit"
                      disabled={busy}
                      onClick={() => edit(option.label)}
                      className="relative my-2 mr-1 opacity-0 group-hover/row:opacity-100 focus-visible:opacity-100 [@media(hover:none)]:opacity-100"
                    >
                      <Pencil />
                    </Button>
                  )}
                  {on && !question.multi_select ? (
                    <Check
                      aria-hidden
                      className="pointer-events-none mt-3 size-4 shrink-0 text-foreground"
                    />
                  ) : (
                    /* the row's key; touch screens have none to press */
                    <Kbd
                      aria-hidden
                      className="pointer-events-none mt-3 shrink-0 opacity-50 [@media(hover:none)]:hidden"
                    >
                      {n + 1}
                    </Kbd>
                  )}
                </div>
              );
            })}
          </div>
        )}
        {unavailable && (
          <p role="status" className="mt-2 text-xs text-muted-foreground">
            Reconnect to load this question before answering.
          </p>
        )}
      </div>
      {/* with one choice to click and nothing to write or skip, a choice is
          the answer and the row would hold only a Send that does nothing */}
      {(question.allow_free_text || question.multi_select || skippable) && (
        <div className="m-1.5 mt-1 flex shrink-0 items-center gap-3 rounded-xl bg-background/60 py-1.5 pr-1.5 pl-3">
          {question.allow_free_text ? (
            <>
              <Pencil aria-hidden className="size-4 shrink-0 text-muted-foreground" />
              <input
                ref={field}
                aria-label={textOnly ? "Your answer" : "Something else"}
                placeholder={textOnly ? "Your answer" : "Something else"}
                value={other}
                disabled={busy}
                onChange={(event) => write(event.target.value)}
                className="min-w-0 flex-1 bg-transparent py-1 text-sm text-foreground outline-none placeholder:text-muted-foreground"
              />
            </>
          ) : (
            <span className="flex-1" />
          )}
          {action}
        </div>
      )}
    </section>
  );
}
