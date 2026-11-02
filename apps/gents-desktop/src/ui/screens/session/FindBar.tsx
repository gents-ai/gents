import { useEffect, useRef, useState } from "react";
import { ChevronDown, ChevronUp, X } from "lucide-react";
import { useStore } from "zustand";
import { Button } from "@gents/ui/components/button";
import { cn } from "@gents/ui/lib/utils";
import type { ReaderPlace } from "@/lib/transcriptList";
import { SEARCH_FIELD } from "@/lib/textAssist";
import { commandKey } from "../shortcutKeys";
import type { TranscriptEntry } from "../transcript-groups";
import {
  closeFind,
  findBar,
  openFind,
  startFind,
  type FindMatch,
  type Found,
} from "./find";

/** what the find bar opens a match's step, group or reasoning with */
type Opener = {
  setFolded: (groupKey: string, folded: boolean) => void;
  setOpenStep: (groupKey: string, stepKey: string | null) => void;
  setReasoning: (key: string, state: { open: boolean; all: boolean }) => void;
};

/* where a match's line is held in the view, from its top */
const MATCH_LINE = 96;
const TYPING_MS = 120;
const NOTHING: Found = { matches: [], more: false, done: true };

const supported = () => typeof CSS !== "undefined" && "highlights" in CSS;

/** Every drawn occurrence of `needle` under `root`, in drawing order. */
function rangesUnder(root: Element, needle: string): Range[] {
  const out: Range[] = [];
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    const hay = (node.nodeValue ?? "").toLowerCase();
    for (
      let at = hay.indexOf(needle);
      at !== -1;
      at = hay.indexOf(needle, at + needle.length)
    ) {
      const range = document.createRange();
      range.setStart(node, at);
      range.setEnd(node, at + needle.length);
      out.push(range);
    }
  }
  return out;
}

const rowOf = (list: Element, key: string) =>
  [...list.querySelectorAll<HTMLElement>("[data-row-key]")].find(
    (row) => row.dataset.rowKey === key,
  ) ?? null;

/** The open session's find: ⌘F opens a bar over the transcript that finds
    in the rows read so far, steps between matches and marks them in the
    rows drawn. Rows are windowed, so matches come from the rows' data and
    the marks from whichever of them are drawn. */
export function FindBar({
  scroller,
  sessionKey,
  entries,
  keyOf,
  goTo,
  opener,
  older,
}: {
  scroller: HTMLElement | null;
  sessionKey: string | null;
  entries: readonly TranscriptEntry[];
  keyOf: (entry: TranscriptEntry) => string;
  goTo: (place: ReaderPlace) => void;
  opener: Opener;
  /** the session's older pages: whether any are unread, reading one, and
      how many messages are read of all, when that is known */
  older: {
    more: boolean;
    loading: boolean;
    load: () => void;
    messages: { loaded: number; total: number } | null;
  };
}) {
  const { open, focus } = useStore(findBar);
  const [text, setText] = useState("");
  const [query, setQuery] = useState("");
  /* the current match, by its row and place in it, so matches found later
     around it leave it where it is */
  const [pick, setPick] = useState<{
    query: string;
    rowKey: string;
    nth: number;
  } | null>(null);
  const input = useRef<HTMLInputElement>(null);
  const returnTo = useRef<HTMLElement | null>(null);
  const shown = useRef<FindMatch | null>(null);

  useEffect(() => {
    const timer = window.setTimeout(() => setQuery(text), TYPING_MS);
    return () => window.clearTimeout(timer);
  }, [text]);
  const [found, setFound] = useState<Found>(NOTHING);
  useEffect(
    () => (open ? startFind(entries, keyOf, query, setFound) : setFound(NOTHING)),
    [open, entries, keyOf, query],
  );
  const { matches, more, done } = found;
  const at =
    pick && pick.query === query
      ? matches.findIndex((m) => m.rowKey === pick.rowKey && m.nth === pick.nth)
      : -1;
  const current = at === -1 ? null : matches[at]!;

  /* Search all: the older pages, read one at a time through the same reads
     scrolling up makes, until the session's start or Stop; never begun by
     typing */
  const [all, setAll] = useState(false);
  const { more: moreOlder, loading: reading, load } = older;
  useEffect(() => {
    if (!all) return;
    if (!moreOlder) setAll(false);
    else if (!reading) load();
  }, [all, moreOlder, reading, load]);
  useEffect(() => {
    if (!open) setAll(false);
  }, [open]);

  /* another session closes the bar */
  useEffect(() => {
    setAll(false);
    closeFind();
    setText("");
    setQuery("");
  }, [sessionKey]);

  useEffect(() => {
    if (!open) return;
    if (!returnTo.current && document.activeElement instanceof HTMLElement)
      returnTo.current = document.activeElement;
    input.current?.focus();
    input.current?.select();
  }, [open, focus]);

  const close = () => {
    closeFind();
    const back = returnTo.current;
    returnTo.current = null;
    if (back?.isConnected) back.focus();
  };

  const choose = (match: FindMatch) =>
    setPick({ query, rowKey: match.rowKey, nth: match.nth });
  const step = (way: 1 | -1) => {
    if (!matches.length) return;
    const from = at === -1 ? (way === 1 ? -1 : 0) : at;
    choose(matches[(from + way + matches.length) % matches.length]!);
  };

  /* a new query starts from the first match at or below the top of the
     view; a current match that is gone starts it again once all is found */
  useEffect(() => {
    if (!matches.length || (pick?.query === query && (at !== -1 || !done))) return;
    const list = scroller?.querySelector("[data-transcript-list]");
    const top = scroller?.getBoundingClientRect().top ?? 0;
    const drawn = [...(list?.querySelectorAll<HTMLElement>("[data-row-key]") ?? [])]
      .filter((row) => row.getBoundingClientRect().bottom > top)
      .sort((a, b) => a.getBoundingClientRect().top - b.getBoundingClientRect().top)[0];
    const order = new Map(entries.map((entry, index) => [keyOf(entry), index]));
    const from = drawn ? (order.get(drawn.dataset.rowKey!) ?? 0) : 0;
    const first = matches.find((m) => (order.get(m.rowKey) ?? 0) >= from);
    choose(first ?? matches[matches.length - 1]!);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [query, matches, done]);

  /* the current match: what holds it is opened, and its row brought into
     view; once drawn, its line is held where the reader looks */
  useEffect(() => {
    if (!current) return;
    if (current.group) {
      opener.setFolded(current.group.key, false);
      if (current.group.step) opener.setOpenStep(current.group.key, current.group.step);
    }
    if (current.reasoning)
      opener.setReasoning(current.reasoning, { open: true, all: true });
    shown.current = current;
    goTo({ key: current.rowKey, offset: MATCH_LINE });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [current?.rowKey, current?.nth, current?.group?.step, current?.reasoning]);

  /* the marks: every match drawn, and the current one */
  useEffect(() => {
    const list = scroller?.querySelector("[data-transcript-list]");
    const needle = query.trim().toLowerCase();
    if (!open || !list || !needle || !supported()) {
      if (supported()) {
        CSS.highlights.delete("find");
        CSS.highlights.delete("find-current");
      }
      return;
    }
    let frame = 0;
    const paint = () => {
      frame = 0;
      CSS.highlights.set("find", new Highlight(...rangesUnder(list, needle)));
      const target = shown.current;
      const row = target && rowOf(list, target.rowKey);
      const mine = row ? rangesUnder(row, needle) : [];
      const range = mine[Math.min(target?.nth ?? 0, mine.length - 1)];
      if (!range) return CSS.highlights.delete("find-current");
      CSS.highlights.set("find-current", new Highlight(range));
      /* a match far down a tall row is brought up to the reader's line */
      const into = range.getBoundingClientRect().top - row!.getBoundingClientRect().top;
      if (target && into > scroller!.clientHeight / 2) {
        shown.current = null;
        goTo({ key: target.rowKey, offset: MATCH_LINE - into });
      }
    };
    const soon = () => {
      if (!frame) frame = requestAnimationFrame(paint);
    };
    soon();
    const observer = new MutationObserver(soon);
    observer.observe(list, { childList: true, subtree: true, characterData: true });
    return () => {
      observer.disconnect();
      cancelAnimationFrame(frame);
      CSS.highlights.delete("find");
      CSS.highlights.delete("find-current");
    };
  }, [open, query, scroller, current, goTo]);

  /* ⌘F opens or selects; ⌘G and ⌘⇧G step while it is open. The webview's
     own find bar would take ⌘F on Windows, so the key is kept. */
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!commandKey(e) || e.altKey) return;
      const key = e.key.toLowerCase();
      if (key === "f" && !e.shiftKey) {
        e.preventDefault();
        openFind();
      } else if (key === "g" && findBar.getState().open) {
        e.preventDefault();
        step(e.shiftKey ? -1 : 1);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  if (!open) return null;
  const count = !query.trim()
    ? null
    : !done && !matches.length
      ? "Finding…"
      : matches.length
        ? `${at + 1} of ${matches.length}${more ? "+" : ""}`
        : "No matches in the loaded messages";
  return (
    <div className="sticky top-2 z-20 h-0">
      <div
        role="search"
        aria-label="Find in session"
        data-testid="find-bar"
        className="absolute top-0 right-0 grid w-full max-w-sm gap-1 rounded-xl border border-border bg-background p-1 text-foreground shadow-lg"
      >
        <div className="flex min-w-0 items-center gap-1">
          <input
            ref={input}
            {...SEARCH_FIELD}
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                /* typed faster than the find follows: find it first */
                if (text !== query) setQuery(text);
                else step(e.shiftKey ? -1 : 1);
              } else if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                close();
              }
            }}
            placeholder="Find in session"
            aria-label="Find in session"
            className="h-7 min-w-0 flex-1 bg-transparent px-2 text-sm outline-none placeholder:text-muted-foreground"
          />
          <span
            role="status"
            aria-live="polite"
            className={cn(
              "shrink-0 px-1 text-xs text-muted-foreground tabular-nums",
              !count && "sr-only",
            )}
          >
            {count}
          </span>
          <Button
            variant="ghost"
            size="icon-xs"
            aria-label="Previous match"
            disabled={matches.length === 0}
            onClick={() => step(-1)}
          >
            <ChevronUp />
          </Button>
          <Button
            variant="ghost"
            size="icon-xs"
            aria-label="Next match"
            disabled={matches.length === 0}
            onClick={() => step(1)}
          >
            <ChevronDown />
          </Button>
          <Button
            variant="ghost"
            size="icon-xs"
            aria-label="Close find"
            onClick={close}
          >
            <X />
          </Button>
        </div>
        {(older.more || all) && (
          <p className="flex min-w-0 items-center gap-1 px-2 pb-1 text-xs text-muted-foreground">
            <span className="min-w-0 truncate" aria-live="polite">
              {!all
                ? "Searching loaded messages"
                : older.messages
                  ? `Searched ${older.messages.loaded} of ${older.messages.total} messages`
                  : "Searching older messages…"}
            </span>
            ·
            <button
              type="button"
              className="shrink-0 underline underline-offset-2 hover:text-foreground"
              onClick={() => setAll(!all)}
            >
              {all ? "Stop" : "Search all"}
            </button>
          </p>
        )}
      </div>
    </div>
  );
}
