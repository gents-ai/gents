/* Mailbox: what the agent filed for a person, after the Figma "Mailbox"
   section and the desktop's vocabulary. An item is a stamped envelope
   the agent wrote with file_mailbox_item: a kind (ask, gate, finished,
   failed, flag), an action it wants (ack: just read it; start_request:
   open a conversation on it; write_document: a document is expected),
   its source, and a summary. A rail down the left carries one glyph per
   kind; each card offers the action the desktop offers for it (Open
   source for ack, Open compose otherwise) and Dismiss. Held tool calls
   are not mailbox items; they live in the session.

   The list is triaged rather than chronological (mailbox-triage.ts): what
   needs a decision first, the soonest deadline first within a group. The heading row
   carries the same quiet controls the sessions list has — a kind axis
   and a search — so the two screens narrow the same way. Cards can be
   selected for one dismissal of many. All of it is over the open items
   the snapshot already carries — nothing here asks the runtime for more. */
import {
  ChevronDown,
  CircleCheck,
  Clock,
  CircleHelp,
  CircleX,
  CornerDownLeft,
  Flag,
  Inbox,
  ListFilter,
  OctagonPause,
  ScrollText,
  Search,
  Plus,
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
import { Input } from "@gents/ui/components/input";
import { Kbd } from "@gents/ui/components/kbd";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { cn } from "@gents/ui/lib/utils";
import type { Shell } from "@/hooks/useShell";
import { href, navigate } from "@/lib/router";
import { useEffect, useState, type ComponentProps } from "react";
import { behaviorName } from "./behavior";
import {
  defaultScope,
  knownNodeIds,
  mailboxInScope,
  scopeContextOf,
  type Scope,
} from "@/lib/scope";
import { useStoredStrings } from "@/lib/stored";
import { nodeDidOf } from "@/lib/nodes";
import { NodeAxis } from "./NodeAxis";
import { NodeBehaviorStack } from "./NodeBehaviorStack";
import { groupItems, matches } from "./mailbox-triage";
import { parseQuestion, QuestionAnswer } from "./MailboxQuestion";
import { Markdown } from "./Markdown";
import { Axis, type Option } from "./SessionFilters";
import { span, when } from "./time";
import { useFleet } from "../hooks/useFleet";

type BadgeVariant = ComponentProps<typeof Badge>["variant"];

/* the kind glyph on the rail, its word, and its badge */
const KIND: Record<
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

/* the kind axis, in the order a person works through them */
const KINDS: Option<string>[] = ["ask", "gate", "failed", "flag", "finished"].map(
  (value) => ({
    value,
    label: KIND[value]!.label,
    icon: KIND[value]!.icon,
    tint: KIND[value]!.tone,
  }),
);

export function MailboxScreen({
  shell,
  nodeDid,
}: {
  shell: Shell;
  /** a node named on the route: the list opens narrowed to it */
  nodeDid?: string;
}) {
  /* the mailbox is what waits on the person wherever it came from: every
     node to start, then whatever the chips choose */
  const ctx = scopeContextOf(
    shell,
    useFleet((s) => s),
  );
  const [storedNodeIds, setNodeIds] = useStoredStrings("gents-prototype-mailbox-nodes");
  useEffect(() => {
    if (nodeDid) setNodeIds([nodeDid]);
  }, [nodeDid, setNodeIds]);
  const nodeIds = knownNodeIds(storedNodeIds, ctx);
  const scope: Scope = {
    nodes: nodeIds.length ? nodeIds : defaultScope("mailbox").nodes,
    agents: [],
  };
  const items = mailboxInScope(scope, ctx);
  const nodeCounts = Object.fromEntries(
    shell.deployments.map((n) => [
      nodeDidOf(n),
      n.mailboxItems.filter((m) => m.status === "open").length,
    ]),
  );
  /* "now" is fixed when the screen opens: a countdown does not tick over
     under the reader's eye and reorder the list while they read it */
  const [now] = useState(() => Date.now());
  /* null: the search is closed, not merely empty — the same two states
     the sessions list keeps */
  const [query, setQuery] = useState<string | null>(null);
  const [kinds, setKinds] = useStoredStrings("gents-prototype-mailbox-kinds");
  const [selected, setSelected] = useState<ReadonlySet<string>>(() => new Set());
  const [dismissing, setDismissing] = useState(false);

  const searched = items.filter((m) => matches(m, query ?? ""));
  /* a kind's count answers "and how many of those" against the search,
     its own axis dropped, the way the sessions filters count */
  const kindCounts = Object.fromEntries(
    KINDS.map((k) => [k.value, searched.filter((m) => m.kind === k.value).length]),
  );
  const shown = searched.filter((m) => kinds.length === 0 || kinds.includes(m.kind));
  const groups = groupItems(shown);
  /* a selection outlives a filter change; only what is still shown counts */
  const chosen = [...selected].filter((id) => shown.some((m) => m.itemId === id));
  const narrowed = kinds.length > 0 || Boolean(query?.trim());
  const one =
    chosen.length === 1
      ? (shown.find((m) => m.itemId === chosen[0] && openable(m)) ?? null)
      : null;

  const select = (ids: string[], next: boolean) =>
    setSelected((current) => {
      const out = new Set(current);
      for (const id of ids)
        if (next) out.add(id);
        else out.delete(id);
      return out;
    });
  const dismissChosen = async () => {
    setDismissing(true);
    try {
      /* one at a time: the contract dismisses one item per call, and a
         failure part-way leaves the rest selected rather than lost */
      for (const id of chosen) {
        await shell.dismissMailboxItem(id);
        setSelected((current) => {
          const out = new Set(current);
          out.delete(id);
          return out;
        });
      }
    } catch {
      /* reported by the shell's action error */
    } finally {
      setDismissing(false);
    }
  };
  /* Enter takes the one selected item's action and Backspace (or Delete)
     dismisses the selection, whatever its size, so the dismiss key never
     moves. Neither fires while the person is typing, or on a button or
     link, which Enter would also press; a checkbox is toggled by Space,
     so after selecting by checkbox the keys still work */
  useEffect(() => {
    if (chosen.length === 0) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || e.shiftKey) return;
      const el = document.activeElement;
      if (
        el instanceof HTMLElement &&
        (el.isContentEditable ||
          el.closest('input, textarea, button, a, [role="button"], [role="menuitem"]'))
      )
        return;
      if (e.key === "Enter" && one) {
        e.preventDefault();
        void openItem(one, shell);
      } else if ((e.key === "Backspace" || e.key === "Delete") && !dismissing) {
        e.preventDefault();
        void dismissChosen();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  return (
    <ScrollArea className="h-full" data-testid="mailbox-screen">
      <div className="mx-auto max-w-page px-6 py-6">
        {items.length > 0 && (
          <>
            <div className="flex h-10 items-center gap-2">
              <h1 className="font-heading text-lg font-medium text-heading">
                Needs your attention
              </h1>
              <div
                aria-label="Mailbox filters"
                className="ml-auto flex min-w-0 items-center gap-0.5 text-muted-foreground"
              >
                <NodeAxis
                  nodes={shell.deployments}
                  homeDid={shell.snapshot?.bootstrap.initAgentDid}
                  counts={nodeCounts}
                  value={nodeIds}
                  onChange={(next) => {
                    setNodeIds(next);
                    if (nodeDid) navigate({ name: "mailbox" });
                  }}
                />
                <Axis
                  label="Kind"
                  icon={ListFilter}
                  options={KINDS}
                  counts={kindCounts}
                  value={kinds}
                  onChange={setKinds}
                />
                {(kinds.length > 0 || nodeIds.length > 0) && (
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label="Clear filters"
                    onClick={() => {
                      setKinds([]);
                      setNodeIds([]);
                    }}
                  >
                    <X />
                  </Button>
                )}
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label="Search the mailbox"
                  aria-expanded={query !== null}
                  className={cn("shrink-0", query !== null && "text-foreground")}
                  onClick={() => setQuery(query === null ? "" : null)}
                >
                  <Search />
                </Button>
              </div>
            </div>
            <p className="text-xs text-muted-foreground" data-testid="mailbox-count">
              {items.length} {items.length === 1 ? "item" : "items"}
            </p>
            {query !== null && (
              <div className="relative mt-3">
                <Input
                  autoFocus
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  onKeyDown={(e) => e.key === "Escape" && setQuery(null)}
                  placeholder="Search titles, summaries, payloads"
                  className="h-9 w-full pr-9"
                  aria-label="Search the mailbox"
                />
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label="Clear search"
                  className="absolute top-1/2 right-1 -translate-y-1/2 text-muted-foreground"
                  onClick={() => setQuery(null)}
                >
                  <X />
                </Button>
              </div>
            )}
          </>
        )}
        {groups.length > 0 && (
          /* one rail for the whole list: the line runs from the first
             group's glyph to the last, and each label sits in line with its
             rows rather than over the rail */
          <div className="relative mt-7">
            <span
              aria-hidden="true"
              className="absolute top-6 bottom-3 left-[11px] w-px bg-border"
            />
            {groups.map((g, i) => (
              <section
                key={g.key}
                className={cn("group/section", i > 0 && "mt-6")}
                data-testid={`mailbox-group-${g.key}`}
              >
                {/* the label is the group's checkbox as the card is its
                    own: a click on it selects the whole group, or clears it
                    when the whole group is selected */}
                <h2 className="relative flex items-baseline gap-2 pl-10 font-mono text-[11px] tracking-wide text-muted-foreground uppercase">
                  <GroupSelect
                    label={g.label}
                    chosen={g.items.filter((m) => selected.has(m.itemId)).length}
                    total={g.items.length}
                    onChange={(next) =>
                      select(
                        g.items.map((m) => m.itemId),
                        next,
                      )
                    }
                  />
                  <button
                    type="button"
                    className="flex cursor-default items-baseline gap-2 uppercase hover:text-foreground"
                    data-testid="mailbox-group-label"
                    onClick={(e) => {
                      select(
                        g.items.map((m) => m.itemId),
                        !g.items.every((m) => selected.has(m.itemId)),
                      );
                      /* a mouse click leaves focus on the label, where Enter
                         would press it again; give the keys back to the page */
                      if (e.detail > 0) e.currentTarget.blur();
                    }}
                  >
                    {g.label}
                    <span className="text-muted-foreground/70">{g.items.length}</span>
                  </button>
                </h2>
                <ol className="mt-3 grid grid-cols-[minmax(0,1fr)] gap-4 pl-10">
                  {g.items.map((m, j) => (
                    <Item
                      key={m.itemId}
                      item={m}
                      last={i === groups.length - 1 && j === g.items.length - 1}
                      shell={shell}
                      now={now}
                      selected={selected.has(m.itemId)}
                      onSelect={(next) => select([m.itemId], next)}
                    />
                  ))}
                </ol>
              </section>
            ))}
          </div>
        )}
        {items.length > 0 && groups.length === 0 && (
          <p
            className="mt-8 text-sm text-muted-foreground"
            data-testid="mailbox-no-match"
          >
            Nothing matches.{" "}
            <button
              type="button"
              className="underline decoration-border underline-offset-4 hover:text-foreground"
              onClick={() => {
                setQuery(null);
                setKinds([]);
              }}
            >
              Clear the search and filter
            </button>
          </p>
        )}
        {items.length === 0 && (
          <div className="grid min-h-[60vh] place-items-center animate-in fade-in-0 slide-in-from-bottom-2 duration-300 ease-out fill-mode-both motion-reduce:animate-none">
            <div className="text-center">
              <Inbox className="mx-auto size-6 text-muted-foreground" />
              <p className="mt-3 font-heading text-lg font-medium text-heading">
                Nothing needs your attention
              </p>
              <p className="mx-auto mt-2 max-w-sm text-sm text-muted-foreground">
                When an agent has a question, needs your approval, finishes or fails
                work, or flags something for you, it shows up here. To give an agent new
                work, start a session.
              </p>
              <Button
                variant="brand"
                className="mt-5"
                nativeButton={false}
                render={<a href={href({ name: "session", sessionId: null })} />}
              >
                <Plus /> Start a session
              </Button>
            </div>
          </div>
        )}
        {/* a selection's one action, kept in reach at the foot of the
            scroller rather than at the top of a long list */}
        {chosen.length > 0 && (
          /* centred on the cards, not the rail column beside them */
          <div className="sticky bottom-4 mt-8 pl-10">
            <div
              className="mx-auto flex w-full max-w-xl items-center justify-between gap-3 rounded-full border border-border bg-raised/95 py-2.5 pr-2.5 pl-5 shadow-2xl shadow-black/60 backdrop-blur animate-in fade-in-0 slide-in-from-bottom-2 duration-200 motion-reduce:animate-none"
              data-testid="mailbox-selection"
            >
              <span className="flex items-center gap-2 text-sm">
                {chosen.length} selected
                {narrowed && chosen.length < selected.size && (
                  <span className="text-muted-foreground">of {selected.size}</span>
                )}
                <Button
                  size="sm"
                  variant="quiet"
                  onClick={() => setSelected(new Set())}
                >
                  <X className="size-3" /> Clear
                </Button>
              </span>
              <div className="flex items-center gap-1">
                <Button
                  size="sm"
                  variant={one ? "raised" : "outline"}
                  className="rounded-full"
                  disabled={dismissing}
                  onClick={() => void dismissChosen()}
                >
                  Dismiss {chosen.length} <Kbd className={KEY}>⌫</Kbd>
                </Button>
                {/* one item chosen: its own action leads, since that is what
                  a person picked it out to do */}
                {one && <Primary item={one} shell={shell} />}
              </div>
            </div>
          </div>
        )}
      </div>
    </ScrollArea>
  );
}

function Item({
  item: m,
  shell,
  now,
  selected,
  onSelect,
  last,
}: {
  item: MailboxItemView;
  shell: Shell;
  now: number;
  selected: boolean;
  onSelect: (next: boolean) => void;
  last: boolean;
}) {
  const kind = KIND[m.kind] ?? {
    icon: CircleHelp,
    label: m.kind,
    badge: "secondary" as BadgeVariant,
  };
  const status = STATUS[m.status] ?? {
    label: m.status,
    badge: "outline" as BadgeVariant,
  };
  const session = m.sessionId
    ? shell.selectedDeployment?.sessions.find((s) => s.sessionId === m.sessionId)
    : undefined;
  /* a kind with its own answer surface renders it; any other item keeps
     the generic reading view */
  const question = parseQuestion(m);
  const body = [m.summary, m.payload && !question ? payloadMarkdown(m.payload) : null]
    .filter((part): part is string => Boolean(part?.trim()))
    .join("\n\n");
  const foldable = body.length > FOLD_CHARS || body.split("\n").length > FOLD_LINES;
  const [expanded, setExpanded] = useState(false);
  const Icon = kind.icon;
  const open = () => openItem(m, shell);
  const deadline = m.deadlineAt ? Date.parse(m.deadlineAt) : null;
  /* a deadline that has passed is the runtime's to expire, not a state
     to show; a countdown never reads below zero */
  const due = deadline === null ? null : span(Math.max(0, deadline - now));
  const soon = deadline !== null && deadline - now < SOON_MS;
  const age = when(m.createdAt);
  const created = Number.isNaN(Date.parse(m.createdAt))
    ? undefined
    : new Date(m.createdAt).toLocaleString();
  const time = (
    <time dateTime={m.createdAt} title={created}>
      {age}
    </time>
  );
  /* a deadline that has passed is still shown as such until the runtime
     expires the item */
  const overdue = deadline !== null && deadline < now;
  /* the sender's behavior, named on the node that filed the item */
  const behavior = behaviorName(
    m.targetBehaviorId,
    shell.deployments.find((n) => nodeDidOf(n) === m.agentDid) ??
      shell.selectedDeployment,
  );
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
          {/* room for a node behind the behavior on every row, so titles
              line up whether or not this one is remote; the stack keeps
              to the title's side, the node reaching left */}
          <div className="col-start-1 row-start-1 flex justify-end max-md:self-center md:mt-0.5 md:w-12">
            <NodeBehaviorStack
              nodes={shell.deployments}
              homeDid={shell.snapshot?.bootstrap.initAgentDid}
              nodeDid={m.agentDid}
              behaviorId={m.targetBehaviorId}
              deployment={shell.selectedDeployment}
            />
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
                from <span className="text-foreground">{behavior}</span>
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
                onAnswer={(answer) => shell.answerMailboxQuestion(m, answer)}
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
        onClick={() => shell.dismissMailboxItem(m.itemId)}
      >
        <X className="size-3" /> Dismiss
      </Button>
    </li>
  );
}

/* ack: there is nothing to do but read it, so opening goes to its source;
   anything else opens a conversation on it (the desktop's "Open compose") */
const openable = (m: MailboxItemView) => Boolean(m.sessionId) || m.action !== "ack";

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

async function openItem(m: MailboxItemView, shell: Shell) {
  if (m.action === "ack") {
    if (m.sessionId) navigate({ name: "session", sessionId: m.sessionId });
    return;
  }
  try {
    const item = await shell.openMailboxItem(m.itemId);
    if (!item) return;
    navigate(
      item.sessionId
        ? { name: "session", sessionId: item.sessionId }
        : { name: "session", sessionId: null },
    );
  } catch {
    /* reported by the shell's action error */
  }
}

/* the key hint inside a pill: a box inside a box read as two outlines,
   so the hint is a tint of the button's own colour with no edge */
const KEY =
  "ml-0.5 h-4 min-w-4 rounded-md border-transparent! bg-current/12! text-[10px] opacity-100!";

/* what a click inside a card reaches for itself, rather than the card */
const CONTROLS =
  'button, a, input, textarea, pre, [role="checkbox"], [role="button"], [role="link"]';

/* the one item's action as a split button: the action itself, and a
   caret for whatever else the item allows, present only when there is
   something to put in it */
function Primary({ item, shell }: { item: MailboxItemView; shell: Shell }) {
  const extras = extrasOf(item);
  return (
    <ButtonGroup>
      <Button
        size="sm"
        variant="outline"
        data-testid="mailbox-selection-open"
        onClick={() => void openItem(item, shell)}
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

/* the group's checkbox sits on the rail beside its label, shown while the
   pointer is over the group, while it has focus, and while any of the
   group is selected; some of the group selected shows as indeterminate */
function GroupSelect({
  label,
  chosen,
  total,
  onChange,
}: {
  label: string;
  chosen: number;
  total: number;
  onChange: (next: boolean) => void;
}) {
  const all = chosen === total;
  const some = chosen > 0 && !all;
  return (
    <span className="absolute top-1/2 left-0 grid size-6 -translate-y-1/2 place-items-center rounded-full bg-background">
      <Checkbox
        checked={all}
        indeterminate={some}
        onCheckedChange={() => onChange(!all)}
        aria-label={all ? `Deselect all: ${label}` : `Select all: ${label}`}
        data-testid="mailbox-group-select"
        className={cn(
          "transition-opacity duration-100 group-hover/section:opacity-100 group-focus-within/section:opacity-100 data-indeterminate:border-brand data-indeterminate:bg-brand/40 motion-reduce:transition-none",
          chosen > 0 ? "opacity-100" : "opacity-0",
        )}
      />
    </span>
  );
}

/* when it expires: a clock and a span, the one fact of the old detail strip
   a person acted on; within the hour it turns the warm colour */
const SOON_MS = 60 * 60_000;

function Due({
  due,
  soon,
  className,
}: {
  due: string | null;
  soon: boolean;
  className?: string;
}) {
  if (!due) return null;
  return (
    <span
      className={cn(
        "inline-flex shrink-0 items-center gap-1 leading-5 tabular-nums",
        soon ? "text-yellow" : "text-muted-foreground",
        className,
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
