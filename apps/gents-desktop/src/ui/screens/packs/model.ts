import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";
import { createStore } from "zustand/vanilla";
import type { InstalledPack, PackInfo } from "@source-inc/gents-desktop-client";
import type { UxPluginRecord } from "@/contrib/plugins-store";
import type { Contribution } from "@/contrib/types";
import type { NodeView } from "../../../hooks/fleetStore";
import { errorText } from "../../../lib/errorText";
import type { RegistryPage } from "../agent/useRegistrySearch";
import {
  kindOf,
  PACK_KINDS,
  packContents,
  SETTING_KINDS,
  type PackItem,
} from "../agent/packContents";

type PackApi = {
  listInstalledPacks: () => Promise<{ packs: InstalledPack[] }>;
  packInfo: (pack: string) => Promise<PackInfo>;
};

/* What the desktop's own node has installed, read once and shared by the
   page and search. A failed read is not an empty one. */
const installed = createStore<{
  packs: InstalledPack[] | null;
  unread: string | null;
  /* the registry's word on each pack, read once a pack is shown */
  info: Record<string, PackInfo | "unread">;
}>(() => ({ packs: null, unread: null, info: {} }));

export async function readInstalledPacks(api: PackApi) {
  try {
    const report = await api.listInstalledPacks();
    installed.setState({ packs: report.packs, unread: null });
  } catch (error) {
    installed.setState({ packs: [], unread: errorText(error) });
  }
}

export const useInstalledPacks = () => useStore(installed);

/* the registry account this device is signed in as: undefined until read,
   null when signed out or unreadable */
const account = createStore<{ username: string | null | undefined }>(() => ({
  username: undefined,
}));

export async function readRegistryAccount(api: {
  readPackAccount: () => Promise<{ account: { username: string } }>;
}) {
  if (account.getState().username !== undefined) return;
  try {
    account.setState({ username: (await api.readPackAccount()).account.username });
  } catch {
    account.setState({ username: null });
  }
}

export const useRegistryAccount = () => useStore(account, (s) => s.username);

/** Who published a pack, as the registry says: you, someone else by name,
    or unknown when the registry has no word on it. */
export function publisherOf(
  info: PackInfo | "unread" | undefined,
  me: string | null | undefined,
): { name: string; yours: boolean } | null {
  if (!info || info === "unread") return null;
  const owner = text(info.owner);
  if (!owner) return null;
  return { name: owner, yours: Boolean(me) && owner === me };
}

type PacksView = "installed" | "registry";

/* Which list the Packs page shows, what it searches, the registry's page it
   last showed and where each list was scrolled to, kept while the app runs
   so a pack opened and closed returns to the same place. */
const listView = createStore<{
  view: PacksView;
  query: string;
  /** the publisher (namespace) the registry list is narrowed to */
  author: string | null;
  registry: RegistryPage | null;
  scroll: Record<PacksView, number>;
}>(() => ({
  view: "installed",
  query: "",
  author: null,
  registry: null,
  scroll: { installed: 0, registry: 0 },
}));

/* what the page draws from; the registry's page and the scroll are read
   when the list mounts, so a scroll doesn't draw the page again */
export const usePacksListView = () =>
  useStore(
    listView,
    useShallow(({ view, query, author }) => ({ view, query, author })),
  );
export const packsListView = () => listView.getState();
export const rememberPacksView = (view: PacksView) => listView.setState({ view });
export const rememberPacksQuery = (query: string) => listView.setState({ query });
export const rememberPacksAuthor = (author: string | null) =>
  listView.setState({ author });
export const keepRegistryPage = (registry: RegistryPage) =>
  listView.setState({ registry });
export const rememberPacksScroll = (view: PacksView, top: number) =>
  listView.setState((s) => ({ scroll: { ...s.scroll, [view]: top } }));
/** the registry's packs from one publisher, on the Packs page */
export const showPublisher = (author: string) =>
  listView.setState({ view: "registry", query: "", author });

/** The registry's page for a pack: its description and versions. Read once
    per pack; a pack the registry can't answer for is shown without them. */
export async function readPackInfo(api: PackApi, pack: string) {
  if (pack in installed.getState().info) return;
  installed.setState((s) => ({ info: { ...s.info, [pack]: "unread" } }));
  try {
    const info = await api.packInfo(pack);
    installed.setState((s) => ({ info: { ...s.info, [pack]: info } }));
  } catch {
    /* stays unread: the pack shows without the registry's words */
  }
}

/** forgets what was read, and the registry page and places the lists kept */
export function resetInstalledPacks() {
  installed.setState({ packs: null, unread: null, info: {} }, true);
  listView.setState({ registry: null, scroll: { installed: 0, registry: 0 } });
}

export const shortName = (pack: string) => pack.slice(pack.lastIndexOf("/") + 1);
export const namespaceOf = (pack: string) =>
  pack.includes("/") ? pack.slice(0, pack.indexOf("/")) : "";

export const text = (value: unknown): string | null =>
  typeof value === "string" && value.trim() ? value : null;

/** a version's size, as the registry lists it */
export function sizeOf(
  info: { versions?: unknown } | undefined | null,
  version: string | null,
) {
  if (!info || !Array.isArray(info.versions)) return null;
  const v = info.versions.find(
    (x) =>
      typeof x === "object" &&
      x !== null &&
      (x as { version?: unknown }).version === version,
  ) as { size_bytes?: unknown } | undefined;
  return typeof v?.size_bytes === "number" ? v.size_bytes : null;
}

export function bytes(n: number) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

export type PackVersion = { version: string; publishedAt: string | null };

/** the versions the registry offers, newest first, leaving out yanked ones */
export function versionsOf(info: PackInfo | undefined): PackVersion[] {
  if (!info || !Array.isArray(info.versions)) return [];
  return info.versions
    .filter(
      (v): v is { version: string; published_at?: string | null; yanked?: boolean } =>
        typeof v === "object" &&
        v !== null &&
        typeof (v as { version?: unknown }).version === "string" &&
        !(v as { yanked?: boolean }).yanked,
    )
    .map((v) => ({ version: v.version, publishedAt: v.published_at ?? null }))
    .reverse();
}

/** A pack's UI plugins, and the pages they draw. */
export function pluginsOf(
  records: Readonly<Record<string, UxPluginRecord>>,
  pack: string,
): UxPluginRecord[] {
  return Object.values(records)
    .filter((r) => r.door === "pack" && r.pack === pack)
    .sort((a, b) => a.name.localeCompare(b.name));
}

/** plugins loaded from the home folder rather than from a pack */
export function localPlugins(
  records: Readonly<Record<string, UxPluginRecord>>,
): UxPluginRecord[] {
  return Object.values(records)
    .filter((r) => r.door === "dev")
    .sort((a, b) => a.name.localeCompare(b.name));
}

export const pageOf = (pages: readonly Contribution[], pluginId: string) =>
  pages.find((c) => c.source === `plugin:${pluginId}` && c.render);

export type PackPart = {
  one: string;
  many: string;
  count: number;
  /** the configuration section that lists them; pages have none */
  section: string | null;
  /** each one, by its id in that section and its name */
  items: PackItem[];
};

/** What a pack added to the node, by kind, in the words people use: its
    documents (each kind listed in the node's configuration), then the pages
    its plugins draw. */
export function partsOf(
  node: NodeView,
  pack: string,
  records: Readonly<Record<string, UxPluginRecord>>,
  pages: readonly Contribution[],
): PackPart[] {
  const shown = pluginsOf(records, pack)
    .filter((r) => pageOf(pages, r.id))
    .map((r) => ({ id: r.id, name: pageOf(pages, r.id)?.title ?? r.name }));
  return [
    ...packContents(node, pack).map(({ one, many, count, section, items }) => ({
      one,
      many,
      count,
      section,
      items,
    })),
    ...(shown.length
      ? [
          {
            one: "page",
            many: "pages",
            count: shown.length,
            section: null,
            items: shown,
          },
        ]
      : []),
  ];
}

/** One thing a pack added, with what it's tied to nested under it. */
export type Resource = {
  section: string;
  /** the collection a review reports it in, where it came from a review */
  collection?: string;
  /** its kind, as a word */
  kind: string;
  id: string;
  name: string;
  /** how it relates to what it's nested under */
  relation: string | null;
  children: Resource[];
};

/** How a pack's documents are tied together: the agent a task runs, the
    context an agent runs with (its tool set, skills and compaction), the
    targets a tool set calls, and the agent a target reaches. */
export type ResourceLinks = {
  agentOfTask: (taskId: string) => string | null | undefined;
  contextOfAgent: (agentId: string) =>
    | {
        context_id?: string | null;
        tools_id?: string | null;
        skill_ids?: readonly string[] | null;
        compaction_id?: string | null;
      }
    | undefined;
  targetsOfTools: (toolsId: string) => readonly string[];
  agentOfTarget: (targetId: string) => string | null | undefined;
};

/** the ties among what a node has */
export function nodeLinks(node: NodeView): ResourceLinks {
  return {
    agentOfTask: (taskId) => node.tasks.find((x) => x.taskId === taskId)?.agentId,
    contextOfAgent: (agentId) => {
      const agent = node.agents.find((x) => x.agentId === agentId);
      return agent?.contextId
        ? node.contexts.find((c) => c.context_id === agent.contextId)
        : undefined;
    },
    targetsOfTools: (toolsId) =>
      node.tools.find((t) => t.tools_id === toolsId)?.agents?.target_ids ?? [],
    agentOfTarget: (targetId) =>
      node.agentTargets?.find((t) => t.target_id === targetId)?.agent_id,
  };
}

/** What a pack added, the way it fits together. */
export type ResourceTree = {
  /** each agent with the tasks that run it, the tool sets and skills its
      context gives it and the targets that reach it, then whatever else
      people look for, each tool set with the targets it calls */
  tree: Resource[];
  /** the settings that belong to nothing else it added */
  other: Resource[];
};

const capital = (word: string) => word.charAt(0).toUpperCase() + word.slice(1);

/** What a pack added, the way it fits together. An agent's context, and the
    compaction that context uses, are part of the agent: its page shows them.
    A target is listed under the tool set that calls it, or else under the
    agent it reaches. A setting none of that ties to anything is listed in
    `other`, never left out. */
export function resourceTree(
  parts: readonly PackPart[],
  settings: readonly PackItem[],
  links: ResourceLinks,
): ResourceTree {
  const listed = parts.filter(
    (p): p is PackPart & { section: string } => p.section !== null,
  );
  const of = (section: string) =>
    listed.find((p) => p.section === section)?.items ?? [];
  const resource = (
    section: string,
    item: PackItem,
    relation: string | null = null,
  ): Resource => {
    const one = listed.find((p) => p.section === section)?.one ?? section;
    return {
      section,
      ...(item.collection ? { collection: item.collection } : {}),
      kind: capital(one),
      id: item.id,
      name: item.name,
      relation,
      children: [],
    };
  };
  const folded = new Set<PackItem>();
  const setting = (collection: string, id: string | null | undefined) => {
    const found = id
      ? settings.find((s) => s.collection === collection && s.id === id)
      : undefined;
    if (found) folded.add(found);
    return found;
  };
  const asSetting = (item: PackItem, relation: string | null = null): Resource => {
    const known = SETTING_KINDS.find((k) => k.collection === item.collection);
    return {
      section: known?.section ?? "",
      ...(item.collection ? { collection: item.collection } : {}),
      kind: capital(kindOf(item.collection ?? "")),
      id: item.id,
      name: item.name,
      relation,
      children: [],
    };
  };
  const agentName = (id: string | null | undefined) =>
    of("agents").find((a) => a.id === id)?.name;
  const called = new Set(of("tools").flatMap((t) => links.targetsOfTools(t.id)));
  const toolSet = (item: PackItem, relation: string | null = null): Resource => ({
    ...resource("tools", item, relation),
    children: links.targetsOfTools(item.id).flatMap((id) => {
      const target = setting("AgentTarget", id);
      if (!target) return [];
      const agent = agentName(links.agentOfTarget(id));
      return [asSetting(target, agent ? `calls ${agent}` : "it calls")];
    }),
  });
  const tied = new Set<string>();
  const agents = of("agents").map((a) => {
    const context = links.contextOfAgent(a.id);
    if (setting("AgentContext", context?.context_id)) {
      setting("CompactionConfig", context?.compaction_id);
    }
    const tools = of("tools").filter((t) => context?.tools_id === t.id);
    const children = [
      ...of("tasks")
        .filter((t) => links.agentOfTask(t.id) === a.id)
        .map((t) => resource("tasks", t, "runs it")),
      ...tools.map((t) => toolSet(t, "it uses")),
      ...of("skills")
        .filter((k) => context?.skill_ids?.includes(k.id))
        .map((k) => resource("skills", k, "it uses")),
    ];
    for (const c of children) tied.add(`${c.section}:${c.id}`);
    const reaching = settings
      .filter(
        (s) =>
          s.collection === "AgentTarget" &&
          !called.has(s.id) &&
          links.agentOfTarget(s.id) === a.id,
      )
      .map((s) => {
        folded.add(s);
        return asSetting(s, "reaches it");
      });
    return { ...resource("agents", a), children: [...children, ...reaching] };
  });
  const rest = listed
    .filter((p) => p.section !== "agents")
    .flatMap((p) =>
      p.items
        .filter((i) => !tied.has(`${p.section}:${i.id}`))
        .map((i) => (p.section === "tools" ? toolSet(i) : resource(p.section, i))),
    );
  return {
    tree: [...agents, ...rest],
    other: settings.filter((s) => !folded.has(s)).map((s) => asSetting(s)),
  };
}

/** what's nested under one thing a pack added, counted by kind */
export function summaryOf(children: readonly Resource[]): string[] {
  const counts = new Map<string, number>();
  for (const c of children) counts.set(c.section, (counts.get(c.section) ?? 0) + 1);
  return [...counts].map(([section, n]) => {
    const known = [...PACK_KINDS, ...SETTING_KINDS].find((k) => k.section === section);
    const one = known?.one ?? children.find((c) => c.section === section)!.kind;
    return `${n} ${n === 1 ? one.toLowerCase() : (known?.many ?? `${one.toLowerCase()}s`)}`;
  });
}

/** the settings that belong to nothing else, counted */
export const otherLabel = (n: number) =>
  `${n} other ${n === 1 ? "setting" : "settings"}`;

export const partLabel = (p: PackPart) =>
  `${p.count} ${p.count === 1 ? p.one : p.many}`;

/** what an installed pack's card says under its name */
export function statusOf(pack: InstalledPack): { text: string; warn: boolean } {
  if (pack.outdated && pack.latest) {
    return { text: `${pack.installed} · ${pack.latest} available`, warn: true };
  }
  /* a failed check says so on the pack's own page, not in every row */
  return { text: pack.installed, warn: false };
}

/** installed packs in a steady order: ones with an update first, then by
    the name the row shows */
export function byAttention(a: InstalledPack, b: InstalledPack) {
  const due = (p: InstalledPack) => (p.outdated && p.latest ? 0 : 1);
  return (
    due(a) - due(b) ||
    shortName(a.pack).localeCompare(shortName(b.pack)) ||
    a.pack.localeCompare(b.pack)
  );
}

export const matches = (pack: string, query: string) =>
  query
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
    .every((word) => pack.toLowerCase().includes(word));

/** why a pack can't be changed from here, or null when it can: packs
    install on the desktop's own node, and another node's on that machine */
export const elsewhere = (local: boolean, name: string) =>
  local ? null : `Packs on ${name} are installed on that machine`;
