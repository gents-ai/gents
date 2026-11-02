/* The side nav's top items, the app's own and the primary rows plugins
   contribute, and the panel's sections, arranged as this device chose. The
   rail and the panel both draw the items, so their order cannot drift. With
   customizing off the nav is the app's default: its own items, then the
   contributed ones, then Pinned and Recent; at its foot the sync status and
   the contributed footer rows, then Nodes. */
import { useMemo, type ComponentType, type ReactNode } from "react";
import {
  CircleCheck,
  CircleHelp,
  CircleX,
  Flag,
  History,
  Inbox,
  Pin,
  Plus,
  RefreshCw,
  ScrollText,
  SlidersHorizontal,
  Waypoints,
} from "lucide-react";
import type { Route } from "@/lib/router";
import {
  useFlag,
  useNavLayout,
  type ListedCount,
  type NavLayout,
  type RecentCount,
} from "@/preferences";
import { TRIAGE, type Triage } from "@/screens/mailbox-triage";
import {
  isListed,
  SECTIONS,
  useAgentSections,
  type AgentSection,
} from "@/screens/agent/sections";
import { useNavItems, type NavContext, type NavItem } from "./navRegistry";

export type NavRow = NavItem & {
  /** the command whose key its tooltip names */
  command?: string;
  /** drawn only once chosen on this device */
  optional?: boolean;
  /** the rail has no list of sessions, so it also counts an open session
      as this item's place */
  railActive?: (route: Route) => boolean;
  /** one of the app's own, drawn without a plugin's boundary */
  own?: boolean;
  /** the panel names the number of nodes on it, quietly, once there is
      more than one; the rail has no room */
  countsNodes?: boolean;
  /** what it belongs to, offered with its family and named where listed */
  group?: EntryGroup;
};

const sessionLists = (route: Route) =>
  route.name === "sessions" ||
  route.name === "sessionsUnder" ||
  route.name === "sessionsArchived";

const CONFIG_GROUP: EntryGroup = {
  id: "config",
  label: "Configuration",
  icon: <SlidersHorizontal className="size-4" />,
};

/** a page of the selected node's configuration, from the same list the
    configuration page draws, so a page added there, or by a plugin, can be
    put in the nav; nowhere while no node is selected */
export const configRow = (
  section: AgentSection & {
    label: string;
    icon: ComponentType<{ className?: string }>;
  },
  own: boolean,
): NavRow => {
  const Icon = section.icon;
  return {
    id: `config:${section.id}`,
    label: section.label,
    icon: <Icon className="size-4" />,
    to: (ctx: NavContext) =>
      ctx.selectedNodeDid
        ? ({
            name: "agent",
            nodeDid: ctx.selectedNodeDid,
            section: section.id,
          } as Route)
        : null,
    active: (route) => route.name === "agent" && route.section === section.id,
    placement: "primary",
    optional: true,
    own,
    group: CONFIG_GROUP,
  };
};

export const APP_NAV_ROWS: readonly NavRow[] = [
  {
    id: "new-session",
    label: "New session",
    command: "page.new-session",
    icon: <Plus className="size-4" />,
    to: { name: "session", sessionId: null },
    active: (route) => route.name === "session" && route.sessionId === null,
    placement: "primary",
    own: true,
  },
  {
    id: "mailbox",
    label: "Mailbox",
    command: "mailbox.stack",
    icon: <Inbox className="size-4" />,
    to: { name: "mailbox" },
    active: (route) => route.name === "mailbox",
    count: (ctx) => ctx.mailboxCount,
    placement: "primary",
    own: true,
  },
  {
    id: "sessions",
    label: "Sessions",
    command: "page.sessions",
    icon: <ScrollText className="size-4" />,
    to: { name: "sessions" },
    active: sessionLists,
    railActive: (route) =>
      sessionLists(route) || (route.name === "session" && route.sessionId !== null),
    placement: "primary",
    own: true,
  },
];

/** an item or section the layout places */
export type Arrangeable = { id: string; label: string; optional?: boolean };

/** the panel's lists of sessions; the rail has no room for them */
export type NavSection = Arrangeable & {
  id: "pinned" | "recent" | "node-list" | MailboxSection;
  icon: ReactNode;
  /** what the section belongs to, for sections that only make sense as
      part of something larger */
  group?: EntryGroup;
};

/** a family of entries, offered together and named where each is listed */
export type EntryGroup = { id: string; label: string; icon: ReactNode };

const MAILBOX_GROUP: EntryGroup = {
  id: "mailbox",
  label: "Mailbox",
  icon: <Inbox className="size-4" />,
};

/** a mailbox category, listed under the Mailbox page's name for it, or
    every open item */
export type MailboxSection = `mailbox-${Triage | "all"}`;

const MAILBOX_ICONS: Record<Triage, ReactNode> = {
  decide: <CircleHelp className="size-4" />,
  wrong: <CircleX className="size-4" />,
  look: <Flag className="size-4" />,
  done: <CircleCheck className="size-4" />,
};

export const mailboxGroupOf = (id: string): Triage | "all" | null =>
  id === "mailbox-all"
    ? "all"
    : id.startsWith("mailbox-")
      ? (TRIAGE.find((t) => `mailbox-${t.key}` === id)?.key ?? null)
      : null;

export const NAV_SECTIONS: readonly NavSection[] = [
  { id: "pinned", label: "Pinned", icon: <Pin className="size-4" /> },
  { id: "recent", label: "Recent", icon: <History className="size-4" /> },
  /* "nodes" is the foot's Nodes row */
  {
    id: "node-list",
    label: "Nodes",
    icon: <Waypoints className="size-4" />,
    optional: true,
  },
  {
    id: "mailbox-all",
    label: "All",
    icon: <Inbox className="size-4" />,
    optional: true,
    group: MAILBOX_GROUP,
  },
  ...TRIAGE.map((t): NavSection => ({
    id: `mailbox-${t.key}`,
    label: t.label,
    icon: MAILBOX_ICONS[t.key],
    optional: true,
    group: MAILBOX_GROUP,
  })),
];

export const DEFAULT_RECENT: RecentCount = 8;
export const DEFAULT_LISTED: ListedCount = 5;

/** the sections the panel draws, in order, and how many recent sessions */
/** the panel's sections and the person's dividers between them, in
    default order */
export function useNavSectionCandidates(): (NavSection | NavDivider)[] {
  const layout = useNavLayout();
  return [...NAV_SECTIONS, ...dividersIn(layout.order, "sections")];
}

export function useNavSections(): {
  sections: (NavSection | NavDivider)[];
  recent: RecentCount;
  /** how many items a mailbox section lists */
  listed: (id: string) => ListedCount;
} {
  const customizable = useFlag("customize-nav");
  const layout = useNavLayout();
  const candidates = useNavSectionCandidates();
  if (!customizable)
    return {
      sections: NAV_SECTIONS.filter((s) => !s.optional),
      recent: DEFAULT_RECENT,
      listed: () => DEFAULT_LISTED,
    };
  return {
    sections: arrange(candidates, layout)
      .filter(({ shown }) => shown)
      .map(({ row }) => row),
    recent: layout.recent ?? DEFAULT_RECENT,
    listed: (id) => layout.listed?.[id] ?? DEFAULT_LISTED,
  };
}

export const APP_FOOTER_ROWS: readonly NavRow[] = [
  {
    id: "nodes",
    label: "Nodes",
    command: "page.nodes",
    icon: <Waypoints className="size-4" />,
    to: { name: "nodes" },
    active: (route) => route.name === "nodes" || route.name === "agents",
    placement: "footer",
    own: true,
    countsNodes: true,
  },
];

/** the sync status at the nav's foot; it can be hidden but keeps its place,
    since on hover the rail's dot becomes the panel's row in that place */
export const NAV_STATUS: Arrangeable & { icon: ReactNode } = {
  id: "status",
  label: "Sync status",
  icon: <RefreshCw className="size-4" />,
};

/** the plugins' footer rows and the app's own, in default order */
export function useNavFooterCandidates(): NavRow[] {
  const contributed = useNavItems("footer");
  return [...contributed, ...APP_FOOTER_ROWS];
}

/** the rows at the nav's foot, in order, and whether the sync status shows;
    Settings is drawn after them, always */
export function useNavFooter(): { rows: NavRow[]; status: boolean } {
  const customizable = useFlag("customize-nav");
  const layout = useNavLayout();
  const candidates = useNavFooterCandidates();
  if (!customizable) return { rows: candidates, status: true };
  return {
    rows: arrange(candidates, layout)
      .filter(({ shown }) => shown)
      .map(({ row }) => row),
    status: layout.shown[NAV_STATUS.id] ?? true,
  };
}

/** every item that may sit at the nav's top, in the order chosen, each with
    whether it is shown; an item the layout doesn't name yet (a plugin's row
    added since) keeps its default place after the named ones */
export function arrange<T extends Arrangeable>(
  rows: readonly T[],
  layout: NavLayout,
): { row: T; shown: boolean }[] {
  const byId = new Map(rows.map((row) => [row.id, row]));
  const placed = new Set<string>();
  const ordered: T[] = [];
  for (const id of layout.order) {
    const row = byId.get(id);
    if (row && !placed.has(id)) {
      placed.add(id);
      ordered.push(row);
    }
  }
  for (const row of rows) if (!placed.has(row.id)) ordered.push(row);
  return ordered.map((row) => ({ row, shown: layout.shown[row.id] ?? !row.optional }));
}

/** a line between the nav's top items, placed by the person; any number
    may be added, each kept under its own id in the layout */
export type NavDivider = {
  id: string;
  label: "Divider";
  divider: true;
  optional?: false;
};

export const DIVIDER_PREFIX = "divider:";

export const isDivider = (entry: { id: string }): entry is NavDivider =>
  entry.id.startsWith(DIVIDER_PREFIX);

const divider = (id: string): NavDivider => ({ id, label: "Divider", divider: true });

/* the items' dividers are `divider:<n>`, the sections' `divider:sections:<n>` */
const dividerPrefix = (group: "items" | "sections") =>
  group === "items" ? DIVIDER_PREFIX : `${DIVIDER_PREFIX}sections:`;

const dividersIn = (order: readonly string[], group: "items" | "sections") =>
  order
    .filter(
      (id) =>
        id.startsWith(dividerPrefix(group)) &&
        /^\d+$/.test(id.slice(dividerPrefix(group).length)),
    )
    .map(divider);

/** a new divider's id for `group`, distinct from those in `order` */
export function newDividerId(
  order: readonly string[],
  group: "items" | "sections" = "items",
): string {
  const prefix = dividerPrefix(group);
  let n = 1;
  while (order.includes(`${prefix}${n}`)) n++;
  return `${prefix}${n}`;
}

/** the app's own items, the plugins' primary rows and the person's
    dividers, in default order */
export function useNavCandidates(): (NavRow | NavDivider)[] {
  const contributed = useNavItems("primary");
  const sections = useAgentSections();
  const config = useMemo(
    () =>
      sections
        .filter(isListed)
        .map((section) => configRow(section, SECTIONS.includes(section))),
    [sections],
  );
  const layout = useNavLayout();
  const dividers = dividersIn(layout.order, "items");
  return [...APP_NAV_ROWS, ...config, ...contributed, ...dividers];
}

/** the entries the nav's top draws, in order */
export function useNavRows(): (NavRow | NavDivider)[] {
  const customizable = useFlag("customize-nav");
  const layout = useNavLayout();
  const candidates = useNavCandidates();
  if (!customizable)
    return candidates.filter((row) => !isDivider(row) && !row.optional);
  return arrange(candidates, layout)
    .filter(({ shown }) => shown)
    .map(({ row }) => row);
}

/** draws the nav's top: a row may draw nothing (nowhere to go right now),
    and a divider is drawn only between two drawn rows, so no line sits at
    either end or beside another */
export function drawNavRows<T extends { id: string }>(
  entries: readonly (T | NavDivider)[],
  drawRow: (row: T) => ReactNode,
  drawDivider: (id: string) => ReactNode,
): ReactNode[] {
  const drawn: { line: boolean; node: ReactNode }[] = [];
  for (const entry of entries) {
    if (isDivider(entry)) {
      const last = drawn[drawn.length - 1];
      if (last && !last.line) drawn.push({ line: true, node: drawDivider(entry.id) });
      continue;
    }
    const node = drawRow(entry as T);
    if (node) drawn.push({ line: false, node });
  }
  while (drawn[drawn.length - 1]?.line) drawn.pop();
  return drawn.map(({ node }) => node);
}
