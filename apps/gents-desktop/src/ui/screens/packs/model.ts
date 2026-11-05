import { useStore } from "zustand";
import { createStore } from "zustand/vanilla";
import type { InstalledPack, PackInfo } from "@source-inc/gents-desktop-client";
import type { UxPluginRecord } from "@/contrib/plugins-store";
import type { Contribution } from "@/contrib/types";
import type { NodeView } from "../../../hooks/fleetStore";
import { errorText } from "../../../lib/errorText";
import { packContents, type PackItem } from "../agent/packContents";

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

/* Which list the Packs page shows and what it searches, kept while the app
   runs so a pack opened and closed returns to the same place. */
const listView = createStore<{
  view: "installed" | "registry";
  query: string;
  /** the publisher (namespace) the registry list is narrowed to */
  author: string | null;
}>(() => ({ view: "installed", query: "", author: null }));

export const usePacksListView = () => useStore(listView);
export const rememberPacksView = (view: "installed" | "registry") =>
  listView.setState({ view });
export const rememberPacksQuery = (query: string) => listView.setState({ query });
export const rememberPacksAuthor = (author: string | null) =>
  listView.setState({ author });
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

export function resetInstalledPacks() {
  installed.setState({ packs: null, unread: null, info: {} }, true);
}

export const shortName = (pack: string) => pack.slice(pack.lastIndexOf("/") + 1);
export const namespaceOf = (pack: string) =>
  pack.includes("/") ? pack.slice(0, pack.indexOf("/")) : "";

/** Two letters standing for a pack where it has no picture. */
export function initials(pack: string) {
  const words = shortName(pack)
    .split(/[-_\s.]+/)
    .filter(Boolean);
  const letters =
    words.length > 1 ? words[0]![0]! + words[1]![0]! : (words[0] ?? "?").slice(0, 2);
  return letters.toUpperCase();
}

export const text = (value: unknown): string | null =>
  typeof value === "string" && value.trim() ? value : null;

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
  /** its kind, as a word */
  kind: string;
  id: string;
  name: string;
  /** how it relates to the agent it's nested under */
  relation: string | null;
  children: Resource[];
};

/** What a pack added, the way it fits together: each agent with the tasks
    that run it and the tool sets and skills its context gives it, then
    whatever isn't tied to one of its agents. */
export function resourceTree(node: NodeView, parts: readonly PackPart[]): Resource[] {
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
      kind: one.charAt(0).toUpperCase() + one.slice(1),
      id: item.id,
      name: item.name,
      relation,
      children: [],
    };
  };
  const tied = new Set<string>();
  const agents = of("agents").map((a) => {
    const agent = node.agents.find((x) => x.agentId === a.id);
    const context = agent?.contextId
      ? node.contexts.find((c) => c.context_id === agent.contextId)
      : undefined;
    const children = [
      ...of("tasks")
        .filter((t) => node.tasks.find((x) => x.taskId === t.id)?.agentId === a.id)
        .map((t) => resource("tasks", t, "runs it")),
      ...of("tools")
        .filter((t) => context?.tools_id === t.id)
        .map((t) => resource("tools", t, "it uses")),
      ...of("skills")
        .filter((k) => context?.skill_ids?.includes(k.id))
        .map((k) => resource("skills", k, "it uses")),
    ];
    for (const c of children) tied.add(`${c.section}:${c.id}`);
    return { ...resource("agents", a), children };
  });
  const rest = listed
    .filter((p) => p.section !== "agents")
    .flatMap((p) =>
      p.items
        .filter((i) => !tied.has(`${p.section}:${i.id}`))
        .map((i) => resource(p.section, i)),
    );
  return [...agents, ...rest];
}

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
