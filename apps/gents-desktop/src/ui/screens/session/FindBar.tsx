import { useEffect, useRef, useState } from "react";
import { ChevronDown, ChevronUp, X } from "lucide-react";
import { useStore } from "zustand";
import { Button } from "@gents/ui/components/button";
import { cn } from "@gents/ui/lib/utils";
import { holdRow } from "@/lib/scroll";
import type { ReaderPlace } from "@/lib/transcriptList";
import { SEARCH_FIELD } from "@/lib/textAssist";
import { commandKey } from "../shortcutKeys";
import type { TranscriptEntry } from "../transcript-groups";
import {
  findPart,
  closeFind,
  findBar,
  openFind,
  startFind,
  type FindMatch,
  type Found,
} from "./find";

type Reasoning = { open: boolean; all: boolean };

/** the transcript's row state: what the find bar opens a match's group,
    step or reasoning with, and how each was before */
type Opener = {
  folded: Readonly<Record<string, boolean>>;
  openStep: Readonly<Record<string, string | null>>;
  reasoning: Readonly<Record<string, Reasoning>>;
  setFolded: (groupKey: string, folded: boolean) => void;
  setOpenStep: (groupKey: string, stepKey: string | null) => void;
  setReasoning: (key: string, state: Reasoning) => void;
};

/* something find opened to show a match: its row, how to put it back, and
   whether the person has since taken it over */
type Opened = { rowKey: string; undo: () => void; taken: boolean };

/** What find opens to show its matches, put back as it moves on: a group,
    step or reasoning it opened folds again once the current match leaves
    its row, and on close all but the last match's, unless the person took
    it over by clicking, typing or selecting in its row. Folding holds the
    current match's row in place. */
function useRefold(scroller: HTMLElement | null, opener: Opener, open: boolean) {
  const opened = useRef(new Map<string, Opened>());
  const state = useRef(opener);
  state.current = opener;

  /* the person's hand in a row: what find opened there stays */
  useEffect(() => {
    if (!open || !scroller) return;
    const take = (node: Node | null) => {
      const row = (
        node instanceof Element ? node : node?.parentElement
      )?.closest<HTMLElement>("[data-row-key]");
      if (!row) return;
      for (const entry of opened.current.values())
        if (entry.rowKey === row.dataset.rowKey) entry.taken = true;
    };
    const onEvent = (e: Event) => take(e.target as Node);
    const onSelect = () => {
      const selection = document.getSelection();
      if (
        selection &&
        !selection.isCollapsed &&
        scroller.contains(selection.anchorNode)
      )
        take(selection.anchorNode);
    };
    scroller.addEventListener("pointerdown", onEvent, true);
    scroller.addEventListener("keydown", onEvent, true);
    document.addEventListener("selectionchange", onSelect);
    return () => {
      scroller.removeEventListener("pointerdown", onEvent, true);
      scroller.removeEventListener("keydown", onEvent, true);
      document.removeEventListener("selectionchange", onSelect);
    };
  }, [open, scroller]);

  return {
    /** opens what holds `match`, keeping how each part was */
    reveal(match: FindMatch) {
      const s = state.current;
      const keep = (id: string, undo: () => void) => {
        if (!opened.current.has(id))
          opened.current.set(id, { rowKey: match.rowKey, undo, taken: false });
      };
      if (match.group) {
        const { key, step } = match.group;
        if (s.folded[key] !== false) {
          const was = s.folded[key];
          keep(`group:${key}`, () => state.current.setFolded(key, was ?? true));
        }
        s.setFolded(key, false);
        if (step && s.openStep[key] !== step) {
          const was = s.openStep[key] ?? null;
          keep(`step:${key}`, () => state.current.setOpenStep(key, was));
          s.setOpenStep(key, step);
        }
      }
      if (match.reasoning) {
        const was = s.reasoning[match.reasoning];
        if (!was?.open || !was.all)
          keep(`reasoning:${match.reasoning}`, () =>
            state.current.setReasoning(
              match.reasoning!,
              was ?? { open: false, all: false },
            ),
          );
        s.setReasoning(match.reasoning, { open: true, all: true });
      }
    },
    /** folds what find opened outside `keep`'s row, holding that row */
    foldBack(keep: string | null) {
      const back = [...opened.current].filter(([, e]) => e.rowKey !== keep);
      if (!back.length) return;
      const list = scroller?.querySelector("[data-transcript-list]");
      if (keep && list) holdRow(rowOf(list, keep));
      for (const [id, entry] of back) {
        opened.current.delete(id);
        if (!entry.taken) entry.undo();
      }
    },
    /** forgets what find opened, folding nothing: another session's rows */
    reset() {
      opened.current.clear();
    },
  };
}

/* where a match's line is held in the view, from its top */
const MATCH_LINE = 96;
const TYPING_MS = 120;
/* how long a match whose row is still opening is kept in view */
const PLACING_MS = 800;
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

/* the ranges of a row's part (see findPart): one step's output or one
   reasoning, or for "" the row's text outside every part */
function rangesInPart(row: Element, part: string, needle: string): Range[] {
  if (part) {
    const el = [...row.querySelectorAll("[data-find-part]")].find(
      (e) => e.getAttribute("data-find-part") === part,
    );
    return el ? rangesUnder(el, needle) : [];
  }
  return rangesUnder(row, needle).filter(
    (range) => !range.startContainer.parentElement?.closest("[data-find-part]"),
  );
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
  /* until when the current match's line is still being placed */
  const placing = useRef(0);

  useEffect(() => {
    /* one character matches nearly everything, so typing finds from two;
       Enter still finds one */
    const typed = text.trim().length === 1 ? "" : text;
    const timer = window.setTimeout(() => setQuery(typed), TYPING_MS);
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
  const refold = useRefold(scroller, opener, open);

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
    refold.reset();
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
    refold.foldBack(current?.rowKey ?? null);
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

  /* the current match: what find opened for the one before folds again,
     what holds this one is opened, and its row brought into view; once
     drawn, its line is held where the reader looks */
  useEffect(() => {
    if (!current) return;
    refold.foldBack(current.rowKey);
    refold.reveal(current);
    shown.current = current;
    placing.current = performance.now() + PLACING_MS;
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
      const mine = row && target ? rangesInPart(row, findPart(target), needle) : [];
      const range = mine[Math.min(target?.nth ?? 0, mine.length - 1)];
      if (!range) return CSS.highlights.delete("find-current");
      CSS.highlights.set("find-current", new Highlight(range));
      /* a match far down a tall row, or in a step still opening, is
         brought up to the reader's line while it settles; one in a box
         that scrolls on its own, as a group's steps do, first within it */
      const inner = range.startContainer.parentElement?.closest<HTMLElement>(
        "[data-slot=scroll-area-viewport]",
      );
      if (
        target &&
        performance.now() < placing.current &&
        inner &&
        row!.contains(inner)
      ) {
        const box = inner.getBoundingClientRect();
        const at = range.getBoundingClientRect();
        if (at.top < box.top || at.bottom > box.bottom)
          inner.scrollTop += at.top - box.top - box.height / 3;
      }
      const view = scroller!.getBoundingClientRect();
      const line = range.getBoundingClientRect().top;
      if (
        target &&
        performance.now() < placing.current &&
        (line < view.top || line > view.top + scroller!.clientHeight - 48)
      )
        goTo({
          key: target.rowKey,
          offset: MATCH_LINE - (line - row!.getBoundingClientRect().top),
        });
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
    ? text.trim()
      ? "Enter to find"
      : null
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
