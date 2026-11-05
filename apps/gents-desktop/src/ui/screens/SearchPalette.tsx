import {
  useDeferredValue,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type RefObject,
} from "react";
import { createStore } from "zustand/vanilla";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";
import {
  Bell,
  Inbox,
  LayoutGrid,
  ListTree,
  Plus,
  Search,
  Settings,
  Workflow,
} from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { Dialog, DialogContent, DialogTitle } from "@gents/ui/components/dialog";
import { Kbd } from "@gents/ui/components/kbd";
import { cn } from "@gents/ui/lib/utils";
import { currentRoute, navigate, reviewNavigation, type Route } from "@/lib/router";
import { isLocalNode } from "@/lib/firstRun";
import { shownNodes, useHidden } from "@/lib/hidden";
import { switchedRoute } from "@/app/NodeSwitcher";
import { NodeAvatar } from "./AgentAvatar";
import { NodeActivity } from "@/app/NodeActivity";
import { packOrigin } from "./agent/ListDetail";
import {
  searchEntries,
  searchGroups,
  type SearchEntry,
  type SearchFilter,
} from "@/lib/search";
import { useApp } from "@/app/AppContext";
import { useHomeDid, useSelectedNodeDid } from "@/hooks/useClient";
import { useExclusivePopover } from "@/hooks/useExclusivePopover";
import { useFleet } from "../hooks/useFleet";
import { isListed, useAgentSections } from "./agent/sections";
import { SETTINGS_SECTIONS } from "./settings/sections";
import { KIND, openItem } from "./MailboxItem";
import { openInStack } from "./MailboxStack";
import { NodeAgentStack } from "./NodeBehaviorStack";
import { SessionStatus } from "./SessionStatus";
import { agentName } from "./behavior";
import { Hint } from "./Hint";
import { SEARCH_FIELD } from "@/lib/textAssist";

const IS_MAC = navigator.platform.toUpperCase().includes("MAC");
/** The palette's shortcut, as shown and as `aria-keyshortcuts` names it. */
export const SEARCH_KEYS = IS_MAC ? "⌘K" : "Ctrl+K";
export const SEARCH_ARIA_KEYS = IS_MAC ? "Meta+K" : "Control+K";

const store = createStore<{ open: boolean }>(() => ({ open: false }));
export const openSearch = () => store.setState({ open: true });
const closeSearch = () => store.setState({ open: false });

const FILTERS: { id: SearchFilter; label: string }[] = [
  { id: "all", label: "All" },
  { id: "session", label: "Sessions" },
  { id: "agent", label: "Agents" },
  { id: "setting", label: "Settings" },
  { id: "action", label: "Actions" },
];

const ACTIONS: { key: string; title: string; route: Route; icon: typeof Plus }[] = [
  {
    key: "action:new",
    title: "New session",
    route: { name: "session", sessionId: null },
    icon: Plus,
  },
  {
    key: "action:sessions",
    title: "Go to sessions",
    route: { name: "sessions" },
    icon: ListTree,
  },
  {
    key: "action:mailbox",
    title: "Go to mailbox",
    route: { name: "mailbox" },
    icon: Inbox,
  },
  {
    key: "action:nodes",
    title: "Go to nodes",
    route: { name: "agents" },
    icon: LayoutGrid,
  },
  {
    key: "action:settings",
    title: "Open settings",
    route: { name: "settings" },
    icon: Settings,
  },
];

/** The title bar's way in, beside the history it shares a scope with: search
    covers every node the app holds, not the one selected in the rail. */
export function SearchButton({ className }: { className?: string }) {
  return (
    <Hint label="Search" keys={SEARCH_KEYS}>
      <Button
        variant="ghost"
        size="icon-sm"
        aria-label="Search"
        aria-keyshortcuts={SEARCH_ARIA_KEYS}
        className={className}
        onClick={openSearch}
      >
        <Search />
      </Button>
    </Hint>
  );
}

/** Search sessions, agents, settings and actions from anywhere. */
export function SearchPalette() {
  const wanted = useStore(store, (s) => s.open);
  const searchInput = useRef<HTMLInputElement | null>(null);
  const palette = useExclusivePopover(closeSearch);
  const { open, onOpenChange } = palette;
  useEffect(() => {
    if (wanted !== open) onOpenChange(wanted);
  }, [wanted, open, onOpenChange]);
  return (
    <Dialog
      open={open}
      onOpenChange={onOpenChange}
      onOpenChangeComplete={palette.onOpenChangeComplete}
    >
      <DialogContent
        ref={palette.popupRef}
        initialFocus={searchInput}
        showCloseButton={false}
        aria-modal="true"
        className="flex flex-col gap-0 overflow-hidden p-0 sm:max-w-xl"
        style={{
          top: "calc(var(--viewport-top) + var(--viewport-height) * 0.12)",
          /* a steady frame: results scroll inside it, so the field doesn't
             move as they change */
          height: "min(calc(var(--viewport-height) * 0.76), 34rem)",
          translate: "-50% 0",
        }}
        data-testid="search-palette"
      >
        <DialogTitle className="sr-only">Search</DialogTitle>
        {open && <PaletteBody searchInput={searchInput} />}
      </DialogContent>
    </Dialog>
  );
}

function useEntries(): SearchEntry[] {
  const homeDid = useHomeDid();
  const selected = useSelectedNodeDid();
  const sections = useAgentSections();
  const hidden = useHidden();
  const fleet = useFleet(
    useShallow((s) => ({
      nodes: s.nodes,
      sessions: s.sessions,
      sessionsOf: s.sessionsOf,
      mailboxOf: s.mailboxOf,
      nodeKeys: s.nodeKeys,
    })),
  );
  return useMemo(() => {
    const nodes = fleet.nodeKeys.flatMap((key) => fleet.nodes[key] ?? []);
    const nodeName = (nodeDid: string) => {
      const n = fleet.nodes[nodeDid];
      return n?.node.displayName ?? n?.label ?? "Node";
    };
    const nodeDid = selected ?? nodes[0]?.nodeDid ?? null;
    return searchEntries({
      sessions: Object.values(fleet.sessions),
      attention: nodes.flatMap((n) =>
        (fleet.mailboxOf[n.nodeDid] ?? []).filter(
          (m) => m.status === "open" && m.kind !== "finished",
        ),
      ),
      /* the hidden ones are left out, and so is a hidden node's agents */
      agents: nodes
        .filter((n) => !hidden.node(n.nodeDid))
        .flatMap((n) =>
          n.agents
            .filter((a) => !hidden.agent(n.nodeDid, a.agentDid))
            .map((a) => ({
              agentDid: a.agentDid,
              nodeDid: n.nodeDid,
              name: a.displayName ?? a.agentDid,
              node: nodeName(n.nodeDid),
              pack: packOrigin(a.tags),
            })),
        ),
      nodes: shownNodes(nodes, hidden, null).map((n) => ({
        nodeDid: n.nodeDid,
        name: nodeName(n.nodeDid),
        avatar: n.nodeConfig?.avatar ?? null,
        place: isLocalNode(n, homeDid) ? "Local" : "Remote",
        offline: !n.dialSucceeded,
      })),
      agentName: (s) => agentName(s.agentDid, fleet.nodes[s.nodeDid] ?? null),
      nodeName,
      places: [
        ...(nodeDid
          ? sections.filter(isListed).map((s) => ({
              key: `setting:node:${s.id}`,
              title: s.label,
              group: `${nodeName(nodeDid)} · ${s.group}`,
              route: { name: "agent", nodeDid, section: s.id } as Route,
              icon: s.icon,
            }))
          : []),
        ...SETTINGS_SECTIONS.map((s) => ({
          key: `setting:app:${s.id}`,
          title: s.label,
          group: "App settings",
          route: { name: "settings", section: s.id } as Route,
          icon: s.icon,
        })),
      ],
      actions: ACTIONS,
    });
  }, [fleet, selected, sections, homeDid, hidden]);
}

function PaletteBody({
  searchInput,
}: {
  searchInput: RefObject<HTMLInputElement | null>;
}) {
  const { actions } = useApp();
  const selected = useSelectedNodeDid();
  const entries = useEntries();
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<SearchFilter>("all");
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set());
  const deferred = useDeferredValue(query);
  const groups = useMemo(
    () => searchGroups(entries, deferred, filter, expanded),
    [entries, deferred, filter, expanded],
  );
  /* every row the arrows reach: a group's entries, then its "more" */
  const flat = useMemo(
    () =>
      groups.flatMap((g): Item[] => [
        ...g.entries,
        ...(g.more > 0 ? [{ key: `more:${g.kind}`, more: g.more, group: g.kind }] : []),
      ]),
    [groups],
  );
  const [active, setActive] = useState(0);
  const listId = useId();
  const list = useRef<HTMLDivElement>(null);
  useEffect(() => setActive(0), [deferred, filter]);
  useEffect(() => setExpanded(new Set()), [deferred, filter]);
  const current = flat[Math.min(active, flat.length - 1)];
  useEffect(() => {
    if (!current) return;
    list.current
      ?.querySelector(`[data-key="${CSS.escape(current.key)}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }, [current]);

  const run = (e: Item) => {
    if ("more" in e) {
      setExpanded((was) => new Set([...was, e.group]));
      return;
    }
    closeSearch();
    /* an item on the active node opens in the mailbox's dialog over this
       page; one on another node is reached through its session, which
       carries the switch to that node */
    if (e.kind === "node") {
      /* as the nav's node switcher does, asked first since a draft stays
         with the node it was written for */
      const here = currentRoute();
      const next = here ? switchedRoute(here, e.nodeDid) : null;
      if (e.nodeDid !== selected)
        reviewNavigation({ name: "sessions", nodeDid: e.nodeDid }, () => {
          actions.selectNode(e.nodeDid);
          if (next) navigate(next);
        });
    } else if (e.kind === "attention") {
      if (e.item.nodeDid === selected) openInStack(e.item);
      else void openItem(e.item, actions.openMailboxItem);
    } else if (e.kind === "session")
      navigate({
        name: "session",
        sessionId: e.session.sessionId,
        nodeDid: e.session.nodeDid,
      });
    else navigate(e.route);
  };

  const onKeyDown = (ev: React.KeyboardEvent) => {
    if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
      ev.preventDefault();
      const step = ev.key === "ArrowDown" ? 1 : -1;
      setActive((i) => (flat.length ? (i + step + flat.length) % flat.length : 0));
    } else if (ev.key === "Enter" && current) {
      ev.preventDefault();
      run(current);
    } else if (ev.key === "Tab") {
      ev.preventDefault();
      const at = FILTERS.findIndex((f) => f.id === filter);
      const next = (at + (ev.shiftKey ? -1 : 1) + FILTERS.length) % FILTERS.length;
      setFilter(FILTERS[next]!.id);
    }
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col" onKeyDown={onKeyDown}>
      <div className="flex items-center gap-2 px-4 pt-4 pb-3">
        <Search className="size-4 shrink-0 text-muted-foreground" />
        <input
          ref={searchInput}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          {...SEARCH_FIELD}
          placeholder="Search sessions, agents and settings"
          role="combobox"
          aria-expanded
          aria-controls={listId}
          aria-activedescendant={current ? `${listId}-${current.key}` : undefined}
          aria-label="Search"
          className="min-w-0 flex-1 bg-transparent text-base outline-none placeholder:text-muted-foreground"
        />
      </div>
      <div role="radiogroup" aria-label="Kind" className="flex gap-1 px-3 pb-2">
        {FILTERS.map((f) => (
          <button
            key={f.id}
            type="button"
            role="radio"
            aria-checked={filter === f.id}
            tabIndex={-1}
            onClick={() => setFilter(f.id)}
            className={cn(
              "rounded-md px-2 py-1 text-xs text-muted-foreground hover:text-foreground",
              filter === f.id && "bg-accent text-foreground",
            )}
          >
            {f.label}
          </button>
        ))}
      </div>
      <div
        ref={list}
        id={listId}
        role="listbox"
        aria-label="Results"
        className="min-h-0 flex-1 overflow-y-auto border-t border-border px-2 py-2"
      >
        {groups.length === 0 && (
          <p className="px-3 py-8 text-center text-sm text-muted-foreground">
            {deferred.trim()
              ? `Nothing matches “${deferred.trim()}”`
              : "Nothing here yet"}
          </p>
        )}
        {groups.map((g) => (
          <div key={g.kind} role="group" aria-label={g.label} className="pb-1">
            <p className="px-3 pt-2 pb-1 text-xs text-muted-foreground">{g.label}</p>
            {g.entries.map((e) => (
              <Row
                key={e.key}
                id={`${listId}-${e.key}`}
                entry={e}
                active={e === current}
                onHover={() => setActive(flat.indexOf(e))}
                onRun={() => run(e)}
              />
            ))}
            {g.more > 0 && (
              <MoreRow
                id={`${listId}-more:${g.kind}`}
                itemKey={`more:${g.kind}`}
                more={g.more}
                active={current?.key === `more:${g.kind}`}
                onHover={() =>
                  setActive(flat.findIndex((i) => i.key === `more:${g.kind}`))
                }
                onRun={() => setExpanded((was) => new Set([...was, g.kind]))}
              />
            )}
          </div>
        ))}
      </div>
      <div className="flex items-center gap-4 border-t border-border px-4 py-2 text-xs text-muted-foreground">
        <span className="flex items-center gap-1.5">
          Open <Kbd>↵</Kbd>
        </span>
        <span className="flex items-center gap-1.5">
          Move <Kbd>↑</Kbd>
          <Kbd>↓</Kbd>
        </span>
        <span className="flex items-center gap-1.5">
          Kind <Kbd>⇥</Kbd>
        </span>
        <span className="ml-auto flex items-center gap-1.5">
          Close <Kbd>Esc</Kbd>
        </span>
      </div>
    </div>
  );
}

/** a row of the list: an entry, or a group's "Show N more" */
type Item = SearchEntry | { key: string; more: number; group: string };

function MoreRow({
  id,
  itemKey,
  more,
  active,
  onHover,
  onRun,
}: {
  id: string;
  itemKey: string;
  more: number;
  active: boolean;
  onHover: () => void;
  onRun: () => void;
}) {
  return (
    <div
      id={id}
      data-key={itemKey}
      role="option"
      aria-selected={active}
      onMouseMove={onHover}
      onClick={onRun}
      className={cn(
        "flex h-8 cursor-default items-center gap-3 rounded-lg px-3 text-xs text-muted-foreground",
        active && "bg-accent text-foreground",
      )}
    >
      <span className="size-4 shrink-0" aria-hidden="true" />
      Show {more} more
    </div>
  );
}

function Row({
  id,
  entry: e,
  active,
  onHover,
  onRun,
}: {
  id: string;
  entry: SearchEntry;
  active: boolean;
  onHover: () => void;
  onRun: () => void;
}) {
  return (
    <div
      id={id}
      data-key={e.key}
      role="option"
      aria-selected={active}
      onMouseMove={onHover}
      onClick={onRun}
      className={cn(
        "flex h-8 cursor-default items-center gap-3 rounded-lg px-3 text-sm",
        active && "bg-accent",
      )}
    >
      <EntryIcon entry={e} />
      <span className="min-w-0 flex-1 truncate">
        {e.title}
        {e.subtitle && <span className="ml-2 text-muted-foreground">{e.subtitle}</span>}
      </span>
      {e.kind === "node" && <NodeActivity nodeDid={e.nodeDid} />}
      {e.kind === "session" && (
        <NodeAgentStack
          nodeDid={e.session.nodeDid}
          agentDid={e.session.agentDid}
          size="sm"
        />
      )}
    </div>
  );
}

function EntryIcon({ entry: e }: { entry: SearchEntry }) {
  const box = "grid size-4 shrink-0 place-items-center text-muted-foreground";
  if (e.kind === "session") return <SessionStatus turnState={e.session.turnState} />;
  if (e.kind === "attention") {
    const k = KIND[e.item.kind];
    const Icon = k?.icon ?? Bell;
    return (
      <Icon className={cn("size-4 shrink-0", k?.tone ?? "text-muted-foreground")} />
    );
  }
  if (e.kind === "agent") return <Workflow className={box} />;
  if (e.kind === "node")
    return (
      <NodeAvatar
        name={e.name}
        avatar={e.avatar}
        rollId={e.nodeDid}
        className="size-4 shrink-0"
      />
    );
  const Icon = e.icon;
  return <Icon className={box} />;
}
