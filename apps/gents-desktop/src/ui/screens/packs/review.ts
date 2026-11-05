import type {
  PackInstallReport,
  PackReviewDocument,
} from "@source-inc/gents-desktop-client";
import type { NodeView } from "../../../hooks/fleetStore";
import { packOrigin } from "../agent/ListDetail";
import { nodeDocuments, PACK_KINDS, type PackItem } from "../agent/packContents";
import type { PackPart } from "./model";

/** The file a path names, for the review's title. */
export const fileName = (path: string) => path.slice(path.search(/[^/\\]*$/));

export const isPackFile = (path: string) => /\.pack$/i.test(path);

/** Kinds a review names that a pack's page doesn't list. */
const REVIEW_KINDS = [
  { collection: "DatastoreToolSurface", one: "data tool set", many: "data tool sets" },
] as const;

const toolsOf = (tools: readonly string[]) =>
  `${tools.length} ${tools.length === 1 ? "tool" : "tools"}: ${tools.join(", ")}`;

const itemOf = (d: PackReviewDocument): PackItem => ({
  collection: d.collection,
  id: d.id ?? "",
  name: d.name ?? d.id ?? d.collection,
  ...(d.tools?.length ? { detail: toolsOf(d.tools) } : {}),
});

const kinds = new Set<string>(
  [...PACK_KINDS, ...REVIEW_KINDS].map((k) => k.collection),
);

/** What a pack would add, by the kinds people look for, from the documents
    its preview reports: the kinds its page shows, then the kinds only a
    review names. */
export function previewParts(documents: readonly PackReviewDocument[]): PackPart[] {
  const part = (one: string, many: string, section: string | null, items: PackItem[]) =>
    ({ one, many, section, count: items.length, items }) as PackPart;
  const of = (collection: string) =>
    documents.filter((d) => d.collection === collection).map(itemOf);
  return [
    ...PACK_KINDS.map((k) => part(k.one, k.many, k.section, of(k.collection))),
    ...REVIEW_KINDS.map((k) => part(k.one, k.many, null, of(k.collection))),
  ].filter((p) => p.count > 0);
}

/** The rest of what a pack would add: settings that belong to something
    else it adds, or to nothing. */
export const previewSettings = (documents: readonly PackReviewDocument[]) =>
  documents.filter((d) => !kinds.has(d.collection)).map(itemOf);

/** A document already on the node that an install would write over. */
export type PackOverlap = {
  key: string;
  kind: string;
  name: string;
  /** the pack that installed it, or null when someone made it */
  owner: string | null;
};

/** The documents a preview would write that the node already has under the
    same id, other than ones this pack installed: one someone made is taken
    over and replaced, and one another pack installed refuses the install.
    A document carries its pack's name alone; `installed` names it in full
    where it can. */
export function overlapsOf(
  documents: readonly PackReviewDocument[],
  node: NodeView,
  pack: string,
  installed: readonly { pack: string }[],
): PackOverlap[] {
  const existing = nodeDocuments(node);
  return documents.flatMap((d) => {
    const kind = existing.find((k) => k.collection === d.collection);
    const doc = kind?.items.find((item) => item.id === d.id);
    if (!kind || !doc) return [];
    const origin = packOrigin(doc.tags);
    if (origin === pack) return [];
    const owner = origin
      ? (installed.find((p) => p.pack.slice(p.pack.lastIndexOf("/") + 1) === origin)
          ?.pack ?? origin)
      : null;
    return [
      { key: `${d.collection}:${doc.id}`, kind: kind.one, name: doc.name, owner },
    ];
  });
}

/** The plugins and pages a plugins pack would install, from its preview. */
export function previewPlugins(report: PackInstallReport): {
  plugins: string[];
  pages: string[];
} {
  const names = (value: unknown) =>
    Array.isArray(value)
      ? value
          .map((v) =>
            typeof v === "object" && v !== null ? (v as { name?: unknown }).name : null,
          )
          .filter((n): n is string => typeof n === "string")
      : [];
  return { plugins: names(report.installed_plugins), pages: names(report.ux_plugins) };
}

/** Why an install stopped, as the review asks about it: the pack needs
    authority the person hasn't allowed, documents it would replace were
    edited, or the pack is not the one reviewed. Read from the error's words,
    since the install reports no typed refusal: replace with typed errors
    from core. */
export function refusalOf(text: string): "authority" | "edited" | "changed" | null {
  if (text.includes("--grant-authority")) return "authority";
  if (text.includes("--overwrite")) return "edited";
  if (text.includes("not the expected")) return "changed";
  return null;
}
