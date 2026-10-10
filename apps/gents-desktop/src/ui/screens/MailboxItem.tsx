/* One mailbox item: its kind and status, what it asks, the action it
   wants and the controls that take it, its deadline and its payload. */
import {
  ChevronDown,
  CircleCheck,
  Clock,
  CircleHelp,
  CircleX,
  CornerDownLeft,
  Flag,
  OctagonPause,
  ScrollText,
  X,
} from "lucide-react";
import type { MailboxItemView } from "@source-inc/gents-desktop-client";
import { Badge } from "@gents/ui/components/badge";
import { Button } from "@gents/ui/components/button";
import { ButtonGroup } from "@gents/ui/components/button-group";
import { Checkbox } from "@gents/ui/components/checkbox";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import { Kbd } from "@gents/ui/components/kbd";
import { cn } from "@gents/ui/lib/utils";
import { href, navigate } from "@/lib/router";
import { useState, type ComponentProps } from "react";
import { agentName } from "./behavior";
import type { ShellActions } from "@/../hooks/shellActions";
import { NodeAgentStack } from "./NodeBehaviorStack";
import { deadlineOf, dueSoon } from "./mailbox-triage";
import { parseQuestion, QuestionAnswer } from "./MailboxQuestion";
import { Markdown } from "./Markdown";
import { span, when } from "./time";
import { useApp } from "@/app/AppContext";
import { toastFailure } from "@/lib/failure";
import { nodeOf } from "../../hooks/fleetStore";
import { useFleet } from "@/hooks/useFleet";

type BadgeVariant = ComponentProps<typeof Badge>["variant"];

/* the kind glyph on the rail, its word, and its badge */
export const KIND: Record<
  string,
  { icon: typeof CircleHelp; label: string; tone?: string; badge: BadgeVariant }
> = {
  ask: { icon: CircleHelp, label: "Question", badge: "purple" },
  gate: { icon: OctagonPause, label: "Gate", badge: "yellow" },
  finished: { icon: CircleCheck, label: "Finished", badge: "success" },
  failed: {
    icon: CircleX,
    label: "Failed",
    tone: "text-destructive",
    badge: "destructive",
  },
  flag: { icon: Flag, label: "Flag", badge: "secondary" },
};

const STATUS: Record<string, { label: string; badge: BadgeVariant }> = {
  open: { label: "Open", badge: "outline" },
  acted: { label: "Acted on", badge: "secondary" },
  dismissed: { label: "Dismissed", badge: "secondary" },
  expired: { label: "Expired", badge: "destructive" },
};

/* a body longer than this folds behind "Show more"; counted rather than
   measured so the fold does not depend on layout having happened */
const FOLD_CHARS = 600;

const FOLD_LINES = 10;

export function Item({
  item: m,
  now,
  selected,
  onSelect,
  last,
}: {
  item: MailboxItemView;
  now: number;
  selected: boolean;
  onSelect: (next: boolean) => void;
  last: boolean;
}) {
  /* the node that filed the item names its agent; its session may be listed by any node */
  const filer = useFleet((state) => nodeOf(state, m.nodeDid));
  const session = useFleet((state) =>
    m.sessionId ? state.bySessionId[m.sessionId] : undefined,
  );
  const { answerMailboxQuestion, dismissMailboxItem, openMailboxItem } =
    useApp().actions;
  const kind = KIND[m.kind] ?? {
    icon: CircleHelp,
    label: m.kind,
    badge: "secondary" as BadgeVariant,
  };
  const status = STATUS[m.status] ?? {
    label: m.status,
    badge: "outline" as BadgeVariant,
  };
  /* a kind with its own answer surface renders it; any other item keeps
     the generic reading view */
  const question = parseQuestion(m);
  const body = [m.summary, m.payload && !question ? payloadMarkdown(m.payload) : null]
    .filter((part): part is string => Boolean(part?.trim()))
    .join("\n\n");
  const foldable = body.length > FOLD_CHARS || body.split("\n").length > FOLD_LINES;
  const [expanded, setExpanded] = useState(false);
  const Icon = kind.icon;
  const open = () => openItem(m, openMailboxItem);
  const deadline = deadlineOf(m);
  /* a countdown never reads below zero; a passed deadline shows as overdue
     until the runtime expires the item */
  const due = deadline === null ? null : span(Math.max(0, deadline - now));
  const soon = dueSoon(deadline, now);
  const age = when(m.createdAt, now);
  const created = Number.isNaN(Date.parse(m.createdAt))
    ? undefined
    : new Date(m.createdAt).toLocaleString();
  const time = (
    <time dateTime={m.createdAt} title={created}>
      {age}
    </time>
  );
  const overdue = deadline !== null && deadline < now;
  const agent = agentName(m.targetAgentId, filer);
  return (
    <li
      className="group/item relative"
      data-testid="mailbox-item"
      data-selected={selected || undefined}
    >
      {/* the rail is one line for the whole list; the last card covers
          it below its own glyph so the line ends there */}
      {last && (
        <span
          aria-hidden="true"
          className="absolute top-6 bottom-0 -left-10 w-6 bg-background"
        />
      )}
      {/* the rail glyph is the card's kind until the pointer is over the
          card, when it becomes the checkbox; it stays the checkbox while
          the card is selected or the box has focus, so a keyboard reaches
          it and a selection never hides itself */}
      <span
        className={cn(
          "absolute top-3 -left-10 grid size-6 place-items-center rounded-full bg-background text-muted-foreground",
          kind.tone,
        )}
        title={kind.label}
      >
        <Icon
          className={cn(
            "col-start-1 row-start-1 size-4 transition-opacity duration-100 group-hover/item:opacity-0 group-focus-within/item:opacity-0 motion-reduce:transition-none",
            selected && "opacity-0",
          )}
        />
        <Checkbox
          checked={selected}
          onCheckedChange={(checked) => onSelect(Boolean(checked))}
          aria-label={`Select: ${m.title}`}
          className={cn(
            "col-start-1 row-start-1 transition-opacity duration-100 group-hover/item:opacity-100 group-focus-within/item:opacity-100 motion-reduce:transition-none",
            selected ? "opacity-100" : "opacity-0",
          )}
        />
      </span>
      {/* the card is its own checkbox: a click on its surface toggles the
          selection, except on a control inside it or when the click ended
          a text selection */}
      <article
        className={cn(
          "cursor-default rounded-2xl border bg-raised px-4 pt-3 pb-5 transition-[border-color,box-shadow] duration-150 motion-reduce:transition-none",
          selected
            ? "border-brand/60 ring-1 ring-brand/30"
            : "border-border/60 hover:border-border",
        )}
        onClick={(e) => {
          if ((e.target as Element).closest(CONTROLS)) return;
          if (!window.getSelection()?.isCollapsed) return;
          onSelect(!selected);
        }}
      >
        <div className="grid grid-cols-[auto_minmax(0,1fr)_auto] items-start gap-x-3 max-md:gap-y-2">
          {/* room for a node behind the agent on every row, so titles
              line up whether or not this one is remote; the stack keeps
              to the title's side, the node reaching left */}
          <div className="col-start-1 row-start-1 flex justify-end max-md:self-center md:mt-0.5 md:w-12">
            <NodeAgentStack nodeDid={m.nodeDid} agentId={m.targetAgentId} />
          </div>
          <span className="col-start-2 row-start-1 flex items-center gap-3 self-center justify-self-end text-xs leading-5 text-muted-foreground md:hidden">
            <Due due={due} soon={soon} />
            {time}
          </span>
          <div className="min-w-0 max-md:col-span-full max-md:row-start-2 md:col-start-2 md:row-start-1">
            <div className="flex items-baseline gap-3">
              <h2 className="min-w-0 flex-1 font-heading text-sm font-medium text-pretty wrap-break-word text-heading">
                {m.title}
              </h2>
              {/* one centred group, so the chip's icon cannot set the age
                  off its own baseline */}
              <span className="flex shrink-0 items-center gap-3 text-xs leading-5 text-muted-foreground max-md:hidden">
                <Due due={due} soon={soon} />
                {time}
              </span>
            </div>
            <div
              className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground"
              data-testid="mailbox-item-meta"
            >
              <Badge variant={kind.badge}>{kind.label}</Badge>
              <Badge variant={status.badge}>{status.label}</Badge>
              <span>
                from <span className="text-foreground">{agent}</span>
              </span>
              {m.sessionId && (
                <a
                  href={href({ name: "session", sessionId: m.sessionId })}
                  className="max-w-64 truncate underline-offset-2 hover:text-foreground hover:underline"
                >
                  in {session?.title?.trim() || "session"}
                </a>
              )}
              {due && (
                <span
                  className={cn("whitespace-nowrap", overdue && "text-destructive")}
                >
                  {due}
                </span>
              )}
            </div>
            {body && (
              <div className="mt-2 max-w-prose">
                <div
                  data-testid="mailbox-item-body"
                  className={cn(
                    "prose-app relative",
                    foldable && !expanded && "max-h-48 overflow-hidden",
                  )}
                >
                  <Markdown breaks>{body}</Markdown>
                  {foldable && !expanded && (
                    <div className="pointer-events-none absolute inset-x-0 bottom-0 h-12 bg-gradient-to-t from-raised to-transparent" />
                  )}
                </div>
                {foldable && (
                  <Button
                    variant="quiet"
                    size="xs"
                    className="mt-1 -ml-2"
                    aria-expanded={expanded}
                    onClick={() => setExpanded((v) => !v)}
                  >
                    {expanded ? "Show less" : "Show more"}
                  </Button>
                )}
              </div>
            )}
            {question && (
              <QuestionAnswer
                question={question}
                onAnswer={(answer) => answerMailboxQuestion(m, answer)}
              />
            )}
            {m.action === "write_document" && m.expectedCollection && (
              <p className="mt-2 text-xs text-muted-foreground">
                Answer with a{" "}
                <span className="font-mono text-foreground">
                  {m.expectedCollection}
                </span>{" "}
                document
              </p>
            )}
          </div>
          {/* open it, where opening means anything */}
          <div className="col-start-3 row-start-1 -mr-1 flex items-center gap-1 max-md:self-center md:-mt-1 md:self-start">
            {openable(m) && (
              <Button
                size="icon-sm"
                variant="ghost"
                aria-label={openLabel(m)}
                title={openLabel(m)}
                onClick={() => void open()}
              >
                {/* the glyph says which: the reply arrow the composer's
                    chip carries, or the sessions scroll for an open */}
                {m.action === "ack" ? <ScrollText /> : <CornerDownLeft />}
              </Button>
            )}
          </div>
        </div>
      </article>
      <Button
        size="sm"
        variant="raised"
        className="-mt-3.5 mr-4 ml-auto flex h-7 w-fit rounded-full px-3 text-xs"
        onClick={() =>
          dismissMailboxItem(m.itemId).then(
            () => onSelect(false),
            (e: unknown) => toastFailure("dismiss the item", e),
          )
        }
      >
        <X className="size-3" /> Dismiss
      </Button>
    </li>
  );
}

/* ack: there is nothing to do but read it, so opening goes to its source;
   anything else opens a conversation on it (the desktop's "Open compose") */
export const openable = (m: MailboxItemView) =>
  Boolean(m.sessionId) || m.action !== "ack";

/* the label says what happens next, not which part of the app opens */
const openLabel = (m: MailboxItemView) =>
  m.action === "ack" ? "Open" : m.sessionId ? "Reply" : "Reply in new session";

type Extra = { key: string; label: string; run: () => void };

/* what else a person can do with an item, beyond its own action and
   dismissing it: only actions the contract gives us today */
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
  } catch {
    /* reported by the action */
  }
}

/* the key hint inside a pill: a box inside a box read as two outlines,
   so the hint is a tint of the button's own colour with no edge */
export const KEY =
  "ml-0.5 h-4 min-w-4 rounded-md border-transparent! bg-current/12! text-[10px] opacity-100!";

/* what a click inside a card reaches for itself, rather than the card */
const CONTROLS =
  'button, a, input, textarea, pre, [role="checkbox"], [role="button"], [role="link"]';

/* the one item's action as a split button: the action itself, and a
   caret for whatever else the item allows, present only when there is
   something to put in it */
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

function Due({ due, soon }: { due: string | null; soon: boolean }) {
  if (!due) return null;
  return (
    <span
      className={cn(
        "inline-flex shrink-0 items-center gap-1 leading-5 tabular-nums",
        soon ? "text-yellow" : "text-muted-foreground",
      )}
      title={`Expires in ${due}`}
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

/* a payload that is JSON reads as a fenced block; anything else is the
   agent's own markdown */
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
