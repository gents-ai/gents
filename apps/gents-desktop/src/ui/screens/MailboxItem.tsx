import {
  ArrowUpRight,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  CircleCheck,
  Clock,
  CircleHelp,
  CircleX,
  Flag,
  OctagonPause,
  X,
} from "lucide-react";
import type { MailboxItemView } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { ButtonGroup } from "@gents/ui/components/button-group";
import { Dialog, DialogContent, DialogTitle } from "@gents/ui/components/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import { Kbd } from "@gents/ui/components/kbd";
import { cn } from "@gents/ui/lib/utils";
import { href, navigate } from "@/lib/router";
import { useEffect, useRef, useState } from "react";
import { agentName } from "./behavior";
import type { ShellActions } from "@/../hooks/shellActions";
import { NodeAgentStack } from "./NodeBehaviorStack";
import { deadlineOf, dueSoon } from "./mailbox-triage";
import { QuestionPanel } from "./session/QuestionPanel";
import { MorphProvider, useMorph, useMorphCard } from "@/lib/morph";
import { mailboxSeen, useNewItem } from "@/app/mailboxSeen";
import { ChatDeliveryFeedback } from "./session/ChatDeliveryFeedback";
import { parseQuestion, useAnswerDelivery } from "./MailboxQuestion";
import { Hint } from "./Hint";
import { Markdown } from "./Markdown";
import { span, when } from "./time";
import { useApp } from "@/app/AppContext";
import { toastFailure } from "@/lib/failure";
import { listedSession, nodeOf } from "../../hooks/fleetStore";
import { useFleet } from "@/hooks/useFleet";

export const KIND: Record<
  string,
  { icon: typeof CircleHelp; label: string; tone?: string }
> = {
  ask: { icon: CircleHelp, label: "Question" },
  gate: { icon: OctagonPause, label: "Gate" },
  finished: { icon: CircleCheck, label: "Finished" },
  failed: { icon: CircleX, label: "Failed", tone: "text-destructive" },
  flag: { icon: Flag, label: "Flag" },
};

/** One item of the mailbox: who sent it, its title, what kind it is and
    where it came from, and when it came or falls due. The whole row opens it. */
export function ItemRow({
  item: m,
  now,
  current,
  onOpen,
}: {
  item: MailboxItemView;
  now: number;
  /** the row open in the dialog, kept in view behind it */
  current: boolean;
  onOpen: () => void;
}) {
  const fresh = useNewItem(m);
  // The filer names the agent; a session may be listed by another node.
  const filer = useFleet((state) => nodeOf(state, m.nodeDid));
  const session = useFleet((state) => listedSession(state, m.nodeDid, m.sessionId));
  const kind = kindOf(m);
  const Icon = kind.icon;
  const dismissible = isDismissible(m);
  return (
    <li data-testid="mailbox-item" className="group/row relative">
      <button
        type="button"
        onClick={onOpen}
        aria-current={current || undefined}
        className={cn(
          "relative -mx-3 grid w-[calc(100%+1.5rem)] cursor-pointer grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-3 rounded-lg px-3 py-2.5 text-left outline-none hover:bg-accent focus-visible:bg-accent",
          current && "bg-accent",
        )}
      >
        {/* not yet opened here: a dot in the gutter, the title a step stronger */}
        {fresh && (
          <span
            role="img"
            aria-label="New"
            data-testid="mailbox-item-new"
            className="absolute top-1/2 left-0 size-1.5 -translate-x-1/2 -translate-y-1/2 rounded-full bg-brand"
          />
        )}
        <NodeAgentStack nodeDid={m.nodeDid} agentId={m.targetAgentId} />
        <span className="min-w-0">
          <span
            className={cn(
              "block truncate text-sm text-foreground",
              fresh && "font-medium",
            )}
          >
            {m.title}
          </span>
          <span className="mt-0.5 flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
            <Icon aria-hidden className={cn("size-3 shrink-0", kind.tone)} />
            <span className={cn("shrink-0", kind.tone)}>{kind.label}</span>
            <span aria-hidden>·</span>
            <span className="shrink-0">{agentName(m.targetAgentId, filer)}</span>
            {m.sessionId && (
              <>
                <span aria-hidden>·</span>
                <span className="truncate">{session?.title?.trim() || "Session"}</span>
              </>
            )}
          </span>
        </span>
        <span
          className={cn(
            "self-start pt-0.5 text-xs whitespace-nowrap text-muted-foreground",
            /* the time gives way to Dismiss under the pointer */
            dismissible &&
              "group-hover/row:invisible group-has-focus-visible/row:invisible [@media(hover:none)]:invisible",
          )}
        >
          <When item={m} now={now} />
        </span>
      </button>
      {dismissible && <RowDismiss item={m} />}
    </li>
  );
}

/** Dismiss without opening the item: shown over the row's time while the
    pointer is on the row, and always where there is no pointer to hover.
    Dismissing can't be undone, so it is this button only, with no key. */
function RowDismiss({ item }: { item: MailboxItemView }) {
  const { dismissMailboxItem } = useApp().actions;
  return (
    <Hint label="Dismiss">
      <Button
        size="icon-sm"
        variant="ghost"
        aria-label={`Dismiss ${item.title}`}
        className="absolute top-1/2 right-0 -translate-y-1/2 text-muted-foreground hover:text-foreground opacity-0 group-hover/row:opacity-100 group-has-focus-visible/row:opacity-100 [@media(hover:none)]:opacity-100"
        onClick={() =>
          dismissMailboxItem(item.itemId).catch((e: unknown) =>
            toastFailure("dismiss the item", e),
          )
        }
      >
        <X />
      </Button>
    </Hint>
  );
}

/* newer questions are answered or skipped, never dismissed */
const isDismissible = (m: MailboxItemView) => {
  const question = parseQuestion(m);
  return !question || question.version < 2;
};

const kindOf = (m: MailboxItemView) =>
  KIND[m.kind] ?? { icon: CircleHelp, label: m.kind };

function KindIcon({ item, className }: { item: MailboxItemView; className?: string }) {
  const kind = kindOf(item);
  const Icon = kind.icon;
  return (
    <Icon
      aria-label={kind.label}
      role="img"
      className={cn("shrink-0", kind.tone, className)}
    />
  );
}

/** when an item falls due, or else how long ago it came */
function When({ item, now }: { item: MailboxItemView; now: number }) {
  const deadline = deadlineOf(item);
  // A passed deadline stays overdue until the runtime expires the item.
  if (deadline !== null)
    return (
      <Due
        due={span(Math.max(0, deadline - now))}
        soon={dueSoon(deadline, now)}
        overdue={deadline < now}
      />
    );
  return (
    <span className="tabular-nums">
      <Age item={item} now={now} />
    </span>
  );
}

/* tints the dialog adds to a kind, seen one item at a time; in the list a
   green row for every finished item would only distract */
const OPENED_TONE: Record<string, string> = { finished: "text-success-foreground" };

/** The opened item's kind as its icon and name. */
function KindLabel({ item }: { item: MailboxItemView }) {
  const kind = kindOf(item);
  return (
    <span
      className={cn(
        "flex shrink-0 items-center gap-1.5",
        kind.tone ?? OPENED_TONE[item.kind],
      )}
    >
      <KindIcon item={item} className="size-3.5" />
      {kind.label}
    </span>
  );
}

function Age({ item, now }: { item: MailboxItemView; now: number }) {
  const created = Number.isNaN(Date.parse(item.createdAt))
    ? undefined
    : new Date(item.createdAt).toLocaleString();
  return (
    <time dateTime={item.createdAt} title={created}>
      {when(item.createdAt, now)}
    </time>
  );
}

/** The opened item, one of the list it was opened from, with the next ones
    stacked behind it. ← and → or the arrows move through the list; answering
    or dismissing moves on to the next, and past the last one it closes. A
    question an agent is waiting on is the question panel the session shows;
    anything else shows its details and its actions. */
export function ItemDialog({
  items,
  openId,
  lastAt,
  now,
  onOpen,
  onClose,
  toMailbox = false,
}: {
  items: readonly MailboxItemView[];
  openId: string | null;
  /** where the open item last was in `items` */
  lastAt: number;
  now: number;
  onOpen: (itemId: string, at: number) => void;
  onClose: () => void;
  /** a link to the whole Mailbox, from a page other than it */
  toMailbox?: boolean;
}) {
  const at = openId === null ? -1 : items.findIndex((m) => m.itemId === openId);
  /* the open item left the list, answered or dismissed here or elsewhere:
     its place goes to the one after it */
  useEffect(() => {
    if (openId === null || at >= 0) return;
    const place = Math.min(lastAt, items.length - 1);
    const next = items[place];
    if (next) onOpen(next.itemId, place);
    else onClose();
  }, [openId, at, lastAt, items, onOpen, onClose]);
  const item = at >= 0 ? items[at]! : null;
  const card = useMorphCard();
  const content = useRef<HTMLDivElement>(null);
  /* focus left with the card that held it, and keys pressed outside the
     dialog never reach its ← and →; a card that takes focus itself has done
     so already, its effects running before this one */
  useEffect(() => {
    const el = content.current;
    if (el && !el.contains(document.activeElement)) el.focus({ preventScroll: true });
  }, [item?.itemId]);
  const go = (index: number) => {
    const next = items[index];
    if (next) card.morph(() => onOpen(next.itemId, index));
  };
  const done = () => (at + 1 < items.length ? go(at + 1) : onClose());
  return (
    <Dialog open={openId !== null} onOpenChange={(open) => !open && onClose()}>
      {item && (
        <DialogContent
          ref={content}
          showCloseButton={false}
          className="grid-cols-[minmax(0,1fr)] grid-rows-[auto_minmax(0,1fr)_auto] gap-2 overflow-visible bg-transparent p-0 shadow-none ring-0 sm:max-w-xl dark:ring-0"
          /* the list stays readable behind, with the open row marked */
          overlayProps={{
            className: "bg-black/25 supports-backdrop-filter:backdrop-blur-[1px]",
          }}
          data-testid="mailbox-dialog"
          onKeyDown={(e) => {
            if (e.defaultPrevented || typing(e.target) || ownsArrows(e.target)) return;
            if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
              e.preventDefault();
              go(at + (e.key === "ArrowRight" ? 1 : -1));
            }
          }}
        >
          <div className="flex items-center justify-end gap-0.5 text-xs text-muted-foreground tabular-nums">
            {toMailbox && (
              <a
                href={href({ name: "mailbox" })}
                onClick={onClose}
                className="mr-auto inline-flex items-center gap-1 rounded-sm px-1 underline-offset-2 outline-none hover:text-foreground hover:underline focus-visible:underline"
              >
                Mailbox <ArrowUpRight className="size-3" />
              </a>
            )}
            {items.length > 1 && (
              <>
                <Button
                  variant="quiet"
                  size="icon-xs"
                  aria-label="Previous item"
                  disabled={at === 0}
                  onClick={() => go(at - 1)}
                >
                  <ChevronLeft />
                </Button>
                {at + 1} of {items.length}
                <Button
                  variant="quiet"
                  size="icon-xs"
                  aria-label="Next item"
                  disabled={at === items.length - 1}
                  onClick={() => go(at + 1)}
                >
                  <ChevronRight />
                </Button>
              </>
            )}
            <Button
              variant="quiet"
              size="icon-sm"
              aria-label="Close mailbox"
              onClick={onClose}
            >
              <X />
            </Button>
          </div>
          <MorphProvider value={card}>
            <Opened
              key={item.itemId}
              item={item}
              now={now}
              behind={Math.min(2, items.length - at - 1)}
              onDone={done}
            />
          </MorphProvider>
        </DialogContent>
      )}
    </Dialog>
  );
}

const ownsArrows = (target: EventTarget) =>
  target instanceof Element &&
  Boolean(
    target.closest(
      '[role="menu"], [role="listbox"], [role="slider"], [role="tablist"]',
    ),
  );

/* an empty field has no caret to move, so ← and → still move through the
   items when a question's only field takes focus as it opens */
const typing = (target: EventTarget) => {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  const field = target.closest("input, textarea, select");
  if (field instanceof HTMLInputElement || field instanceof HTMLTextAreaElement)
    return field.value !== "";
  return field !== null;
};

/** the edges of the next items, behind the card and sized to it */
function Stack({ behind }: { behind: number }) {
  const { part } = useMorph();
  return (
    <>
      {behind >= 2 && (
        <span
          aria-hidden
          className="absolute inset-x-6 top-4 -bottom-4 -z-10 rounded-2xl bg-popover/50 ring-1 ring-foreground/5 [view-transition-class:morph-edge]"
          ref={part("edge-2")}
        />
      )}
      {behind >= 1 && (
        <span
          aria-hidden
          className="absolute inset-x-3 top-2 -bottom-2 -z-10 rounded-2xl bg-popover/80 ring-1 ring-foreground/10 [view-transition-class:morph-edge]"
          ref={part("edge-1")}
        />
      )}
    </>
  );
}

function Opened({
  item: m,
  now,
  behind,
  onDone,
}: {
  item: MailboxItemView;
  now: number;
  behind: number;
  onDone: () => void;
}) {
  const { part } = useMorph();
  const question = parseQuestion(m);
  const { pending, confirmed, inFlight } = useAnswerDelivery(m);
  const answered = pending || confirmed;
  /* an answer already sent from here when the item opened shows where its
     delivery stands rather than asking again */
  const [answeredAtOpen] = useState(answered);
  const [confirmedAtOpen] = useState(confirmed);
  const moved = useRef(false);
  /* an answer moves on once its delivery is confirmed, from the question or
     from Resume; one whose delivery is uncertain stays, with Check and Resume */
  useEffect(() => {
    if (!question || !confirmed || confirmedAtOpen || moved.current) return;
    moved.current = true;
    onDone();
  }, [question, confirmed, confirmedAtOpen, onDone]);
  const asking =
    question !== null && !answeredAtOpen && (!answered || inFlight || confirmed);
  useEffect(() => mailboxSeen.open(m), [m]);
  return (
    <>
      {/* a question's prompt is its heading; the dialog is named by the item */}
      {asking && <DialogTitle className="sr-only">{m.title}</DialogTitle>}
      <div className="relative min-h-0 min-w-0">
        <Stack behind={behind} />
        <div
          ref={part("card")}
          className="h-full min-h-0 [view-transition-class:morph-card]"
        >
          {asking ? (
            <QuestionPanel
              items={[m]}
              className="h-full max-h-full"
              where={<AskedWhere item={m} />}
              aside={
                <span className="shrink-0 whitespace-nowrap">
                  <When item={m} now={now} />
                </span>
              }
              kind={<KindLabel item={m} />}
              icon={
                <NodeAgentStack
                  nodeDid={m.nodeDid}
                  agentId={m.targetAgentId}
                  size="sm"
                />
              }
            />
          ) : (
            <Details item={m} now={now} />
          )}
        </div>
      </div>
      <div className={cn(behind > 0 && "pt-3")}>
        <Footer item={m} question={question} onDone={onDone} />
      </div>
    </>
  );
}

/** " · in <session> · 4m ago", after the agent's name in a question's header */
function AskedWhere({ item: m }: { item: MailboxItemView }) {
  const session = useFleet((state) => listedSession(state, m.nodeDid, m.sessionId));
  return (
    <>
      {m.sessionId && (
        <>
          {" · in "}
          <a
            href={href({ name: "session", sessionId: m.sessionId })}
            className="text-foreground underline-offset-2 hover:underline"
          >
            {session?.title?.trim() || "a session"}
          </a>
        </>
      )}
    </>
  );
}

function Details({ item: m, now }: { item: MailboxItemView; now: number }) {
  const filer = useFleet((state) => nodeOf(state, m.nodeDid));
  const session = useFleet((state) => listedSession(state, m.nodeDid, m.sessionId));
  const { openMailboxItem } = useApp().actions;
  const question = parseQuestion(m);
  const body = [m.summary, m.payload && !question ? payloadMarkdown(m.payload) : null]
    .filter((part): part is string => Boolean(part?.trim()))
    .join("\n\n");
  return (
    <section
      className="flex h-full min-h-0 flex-col gap-2 rounded-2xl bg-popover px-4 pt-3 pb-4 text-sm text-popover-foreground shadow-xl ring-1 ring-foreground/5 dark:ring-foreground/10 [&>*]:min-w-0"
      onKeyDown={(e) => {
        if (e.key !== "Enter" || !openable(m)) return;
        if ((e.target as Element).closest(CONTROLS)) return;
        e.preventDefault();
        void openItem(m, openMailboxItem);
      }}
    >
      {/* the question panel's header, line for line: what and when, then who
          and where */}
      <div
        className="grid gap-2 text-xs text-muted-foreground"
        data-testid="mailbox-item-meta"
      >
        <div className="flex items-center gap-2">
          <span className="min-w-0 flex-1">
            <KindLabel item={m} />
          </span>
          <span className="whitespace-nowrap">
            <When item={m} now={now} />
          </span>
        </div>
        <div className="flex min-w-0 items-center gap-2">
          <NodeAgentStack nodeDid={m.nodeDid} agentId={m.targetAgentId} size="sm" />
          <span className="min-w-0 truncate">
            {agentName(m.targetAgentId, filer)}
            {m.sessionId && (
              <>
                {" · in "}
                <a
                  href={href({ name: "session", sessionId: m.sessionId })}
                  className="text-foreground underline-offset-2 hover:underline"
                >
                  {session?.title?.trim() || "a session"}
                </a>
              </>
            )}
          </span>
        </div>
      </div>
      <div>
        <DialogTitle className="mt-1 font-heading text-base font-normal text-pretty wrap-break-word text-foreground">
          {m.title}
        </DialogTitle>
      </div>
      {body && (
        <div
          data-testid="mailbox-item-body"
          className="prose-app min-h-0 min-w-0 overflow-y-auto"
        >
          <Markdown breaks>{body}</Markdown>
        </div>
      )}
      {question && <ChatDeliveryFeedback mailboxItem={m} />}
      {m.action === "write_document" && m.expectedCollection && (
        <p className="text-xs text-muted-foreground">
          Answer with a{" "}
          <span className="font-mono text-foreground">{m.expectedCollection}</span>{" "}
          document
        </p>
      )}
    </section>
  );
}

/** Dismiss, where the item allows it, and its way onward. */
function Footer({
  item: m,
  question,
  onDone,
}: {
  item: MailboxItemView;
  question: ReturnType<typeof parseQuestion>;
  onDone: () => void;
}) {
  const { dismissMailboxItem } = useApp().actions;
  const dismissible = isDismissible(m);
  return (
    <div className="flex items-center justify-end gap-2">
      {dismissible && (
        <Button
          size="sm"
          variant="raised"
          onClick={() =>
            dismissMailboxItem(m.itemId).then(onDone, (e: unknown) =>
              toastFailure("dismiss the item", e),
            )
          }
        >
          <X className="size-3" /> Dismiss
        </Button>
      )}
      {/* a question is answered in its panel; the way onward is its session */}
      {question
        ? m.sessionId && (
            <Button
              size="sm"
              variant="raised"
              nativeButton={false}
              render={<a href={href({ name: "session", sessionId: m.sessionId })} />}
            >
              Open session
            </Button>
          )
        : openable(m) && <Primary item={m} />}
    </div>
  );
}

export const openable = (m: MailboxItemView) =>
  Boolean(m.sessionId) || m.action !== "ack";

const openLabel = (m: MailboxItemView) =>
  m.action === "ack" ? "Open" : m.sessionId ? "Reply" : "Reply in new session";

type Extra = { key: string; label: string; run: () => void };

function extrasOf(m: MailboxItemView): Extra[] {
  const out: Extra[] = [];
  if (m.action !== "ack" && m.sessionId) {
    const sessionId = m.sessionId;
    out.push({
      key: "open-session",
      label: "Open session without replying",
      run: () => navigate({ name: "session", sessionId }),
    });
  }
  if (m.action === "write_document")
    out.push({
      key: "copy-key",
      label: "Copy item key",
      run: () => void navigator.clipboard?.writeText(m.itemKey),
    });
  if (m.payload) {
    const payload = m.payload;
    out.push({
      key: "copy-payload",
      label: "Copy payload",
      run: () => void navigator.clipboard?.writeText(pretty(payload)),
    });
  }
  return out;
}

export async function openItem(
  m: MailboxItemView,
  openMailboxItem: ShellActions["openMailboxItem"],
) {
  if (m.action === "ack") {
    if (m.sessionId) navigate({ name: "session", sessionId: m.sessionId });
    return;
  }
  try {
    const item = await openMailboxItem(m.itemId);
    if (!item) return;
    navigate(
      item.sessionId
        ? { name: "session", sessionId: item.sessionId }
        : { name: "session", sessionId: null },
    );
  } catch {}
}

export const KEY =
  "ml-0.5 h-4 min-w-4 rounded-md border-transparent! bg-current/12! text-[10px] opacity-100!";

const CONTROLS =
  'button, a, input, textarea, pre, [role="checkbox"], [role="button"], [role="link"]';

export function Primary({ item }: { item: MailboxItemView }) {
  const { openMailboxItem } = useApp().actions;
  const extras = extrasOf(item);
  return (
    <ButtonGroup>
      <Button
        size="sm"
        variant="outline"
        data-testid="mailbox-selection-open"
        onClick={() => void openItem(item, openMailboxItem)}
      >
        {openLabel(item)} <Kbd className={KEY}>↵</Kbd>
      </Button>
      {extras.length > 0 && (
        <DropdownMenu>
          <DropdownMenuTrigger
            render={
              <Button
                size="icon-sm"
                variant="outline"
                className="h-7 w-7"
                aria-label="More actions"
                data-testid="mailbox-selection-more"
              />
            }
          >
            <ChevronDown className="size-3.5" />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-60">
            {extras.map((x) => (
              <DropdownMenuItem key={x.key} onClick={x.run}>
                {x.label}
              </DropdownMenuItem>
            ))}
          </DropdownMenuContent>
        </DropdownMenu>
      )}
    </ButtonGroup>
  );
}

function Due({
  due,
  soon,
  overdue = false,
}: {
  due: string | null;
  soon: boolean;
  overdue?: boolean;
}) {
  if (!due) return null;
  return (
    <span
      className={cn(
        "inline-flex shrink-0 items-center gap-1 leading-5 tabular-nums",
        overdue ? "text-destructive" : soon ? "text-yellow" : "text-muted-foreground",
      )}
      title={overdue ? "Past its deadline" : `Expires in ${due}`}
    >
      <Clock className="size-3" />
      {due}
    </span>
  );
}

const pretty = (value: string) => {
  try {
    return JSON.stringify(JSON.parse(value), null, 2);
  } catch {
    return value;
  }
};

const payloadMarkdown = (value: string) => {
  let json: string;
  try {
    const parsed: unknown = JSON.parse(value);
    if (typeof parsed === "string") return parsed;
    json = JSON.stringify(parsed, null, 2);
  } catch {
    return value;
  }
  let fence = "```";
  while (json.includes(fence)) fence += "`";
  return `${fence}json\n${json}\n${fence}`;
};
