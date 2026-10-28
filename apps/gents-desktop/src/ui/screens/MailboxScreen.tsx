import { Inbox, ListFilter, Search, Plus, X } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { Checkbox } from "@gents/ui/components/checkbox";
import { Input } from "@gents/ui/components/input";
import { Kbd } from "@gents/ui/components/kbd";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { cn } from "@gents/ui/lib/utils";
import { href } from "@/lib/router";
import { useEffect, useState } from "react";
import { defaultScope, mailboxInScope, type Scope } from "@/lib/scope";
import { useInScope, useSelectedNodeDid } from "@/hooks/useClient";
import { listViews, useListViews } from "@/app/listViews";
import { groupItems, KIND_ORDER, matches } from "./mailbox-triage";
import { Axis, type Option } from "./SessionFilters";
import { useApp } from "@/app/AppContext";
import { Item, KEY, KIND, openable, openItem, Primary } from "./MailboxItem";

const KINDS: Option<string>[] = KIND_ORDER.map((value) => ({
  value,
  label: KIND[value]!.label,
  icon: KIND[value]!.icon,
  tint: KIND[value]!.tone,
}));

export function MailboxScreen() {
  const selectedNodeDid = useSelectedNodeDid();
  const {
    stores,
    actions: {
      dismissMailboxItem,
      openMailboxItem,
      captureComposeIntent,
      acceptsComposeIntent,
    },
  } = useApp();
  const scope: Scope = defaultScope("mailbox");
  const items = useInScope((ctx) => mailboxInScope(scope, ctx));
  const [now] = useState(() => Date.now());
  const [query, setQuery] = useState<string | null>(null);
  const kinds = useListViews((v) => v.mailboxKinds);
  const [picked, setPicked] = useState<{
    nodeDid: string | null;
    ids: ReadonlySet<string>;
  }>(() => ({ nodeDid: selectedNodeDid, ids: new Set() }));
  const selected = picked.nodeDid === selectedNodeDid ? picked.ids : new Set<string>();
  const [dismissing, setDismissing] = useState(false);

  const searched = items.filter((m) => matches(m, query ?? ""));
  const kindCounts = Object.fromEntries(
    KINDS.map((k) => [k.value, searched.filter((m) => m.kind === k.value).length]),
  );
  const shown = searched.filter((m) => kinds.length === 0 || kinds.includes(m.kind));
  const groups = groupItems(shown);
  const chosen = [...selected].filter((id) => shown.some((m) => m.itemId === id));
  const narrowed = kinds.length > 0 || Boolean(query?.trim());
  const one =
    chosen.length === 1
      ? (shown.find((m) => m.itemId === chosen[0] && openable(m)) ?? null)
      : null;

  const select = (ids: string[], next: boolean) =>
    setPicked((current) => {
      const out = new Set(current.nodeDid === selectedNodeDid ? current.ids : []);
      for (const id of ids)
        if (next) out.add(id);
        else out.delete(id);
      return { nodeDid: selectedNodeDid, ids: out };
    });
  const dismissChosen = async () => {
    setDismissing(true);
    const intent = captureComposeIntent();
    try {
      /* The contract dismisses one item per call. If one fails, keep the rest
         selected rather than losing them. */
      for (const id of chosen) {
        if (
          !acceptsComposeIntent(intent) ||
          stores.selection.getState().nodeDid !== selectedNodeDid
        )
          break;
        await dismissMailboxItem(id);
        setPicked((current) => {
          if (current.nodeDid !== selectedNodeDid) return current;
          const out = new Set(current.ids);
          out.delete(id);
          return { ...current, ids: out };
        });
      }
    } catch {
    } finally {
      setDismissing(false);
    }
  };
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
        void openItem(one, openMailboxItem);
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
        {(items.length > 0 || selectedNodeDid !== null) && (
          <>
            <div className="flex h-10 items-center gap-2">
              <h1 className="font-heading text-lg font-medium text-heading">
                Needs your attention
              </h1>
              <div
                aria-label="Mailbox filters"
                className="ml-auto flex min-w-0 items-center gap-0.5 text-muted-foreground"
              >
                <Axis
                  label="Kind"
                  icon={ListFilter}
                  options={KINDS}
                  counts={kindCounts}
                  value={kinds}
                  onChange={listViews.setMailboxKinds}
                />
                {kinds.length > 0 && (
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label="Clear filters"
                    onClick={() => {
                      listViews.setMailboxKinds([]);
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
                listViews.setMailboxKinds([]);
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
                  onClick={() =>
                    setPicked({ nodeDid: selectedNodeDid, ids: new Set() })
                  }
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
                {one && <Primary item={one} />}
              </div>
            </div>
          </div>
        )}
      </div>
    </ScrollArea>
  );
}

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
