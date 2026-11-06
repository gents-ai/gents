import { useStore } from "zustand";
import { createStore } from "zustand/vanilla";
import type {
  PackEditedChoice,
  PackInstallReport,
  PackReviewDocument,
} from "@source-inc/gents-desktop-client";
import type { Route } from "@/lib/router";
import { packPreviewKey, reviewKey, type PackReviewRequest } from "@/lib/packReview";
import { errorText } from "../../../lib/errorText";
import { kindOf } from "../agent/packContents";
import {
  otherLabel,
  partLabel,
  type Resource,
  type ResourceTree,
  resourceTree,
  shortName,
} from "./model";
import { fileName, previewParts, previewPlugins, previewSettings } from "./review";

export type PreviewRead =
  | { state: "reading" }
  | { state: "read"; report: PackInstallReport }
  | { state: "unread"; error: string };

type PreviewApi = {
  installPack: (request: {
    package: string;
    preview: boolean;
    expectDigest: string | null;
    inferenceSlots: string[];
  }) => Promise<PackInstallReport>;
};

/* What each pack's page and its documents' pages show of a pack before it's
   installed: the install's own preview, read once per review and kept while
   the app runs. A failed read is read again when asked again. */
const previews = createStore<Record<string, PreviewRead>>(() => ({}));

export async function readPackPreview(api: PreviewApi, request: PackReviewRequest) {
  const key = reviewKey(request);
  const known = previews.getState()[key];
  if (known && known.state !== "unread") return;
  previews.setState({ [key]: { state: "reading" } });
  try {
    const report = await api.installPack({
      package: request.package,
      preview: true,
      expectDigest: request.digest ?? null,
      inferenceSlots: [],
    });
    previews.setState({ [key]: { state: "read", report } });
  } catch (error) {
    previews.setState({ [key]: { state: "unread", error: errorText(error) } });
  }
}

/** keeps what a review read, so the pages it links to show it at once */
export const keepPackPreview = (
  request: PackReviewRequest,
  report: PackInstallReport,
) => previews.setState({ [reviewKey(request)]: { state: "read", report } });

export const usePackPreview = (request: PackReviewRequest | null) =>
  useStore(previews, (s) => (request ? (s[reviewKey(request)] ?? null) : null));

export const resetPackPreviews = () => previews.setState({}, true);

/** A review the person stepped out of to look at what it adds, with what it
    showed, everything they chose in it and the page it was open on, so it
    opens again as they left it. */
export type SetAsideReview = {
  request: PackReviewRequest;
  /** the report the review showed, and the models it was read with; null
      while the models chosen were still being read */
  report: PackInstallReport;
  bindings: string | null;
  chosen: Record<string, string>;
  allow: boolean;
  replace: boolean;
  edited: PackEditedChoice;
  /** the page it was open on, where going back opens it again */
  from: Route;
};

const aside = createStore<{ review: SetAsideReview | null }>(() => ({ review: null }));

export const setReviewAside = (review: SetAsideReview) => aside.setState({ review });
export const useSetAsideReview = () => useStore(aside, (s) => s.review);
/** forgets the review set aside, if it's this one */
export function forgetSetAsideReview(request: PackReviewRequest) {
  const review = aside.getState().review;
  if (review && reviewKey(review.request) === reviewKey(request))
    aside.setState({ review: null });
}
export const resetSetAsideReview = () => aside.setState({ review: null });

/** The review set aside for `request`, when there is one. */
export const asideFor = (
  review: SetAsideReview | null,
  request: PackReviewRequest | null,
) =>
  review && request && reviewKey(review.request) === reviewKey(request) ? review : null;

/** One document a preview reports, named in a route as `<collection>:<id>`. */
export const docRef = (collection: string, id: string) => `${collection}:${id}`;

export function parseDocRef(ref: string): { collection: string; id: string } | null {
  const at = ref.indexOf(":");
  return at > 0 ? { collection: ref.slice(0, at), id: ref.slice(at + 1) } : null;
}

/** The page of the pack a review previews: the registry's pack by its
    name, a file or a link by its preview key. */
export function previewPage(request: PackReviewRequest, doc?: string): Route {
  const page: Route =
    request.source === "registry"
      ? { name: "packs", item: request.package }
      : { name: "packs", review: packPreviewKey(request) };
  return doc ? { ...page, doc } : page;
}

/** The pack a review names, before its preview reports it. */
export const requestedName = (request: PackReviewRequest) =>
  request.source === "file"
    ? fileName(request.package)
    : shortName(request.package.replace(/@[^/]*$/, ""));

/** The pack's coordinate a review names, before its preview reports it; a
    file has none until it's read. */
export const requestedPack = (request: PackReviewRequest) =>
  request.source === "file" ? null : request.package.replace(/@[^/]*$/, "");

const content = (d: PackReviewDocument | undefined): Record<string, unknown> =>
  (d?.content as Record<string, unknown> | undefined) ?? {};

const str = (value: unknown) => (typeof value === "string" ? value : null);
const capital = (word: string) => word.charAt(0).toUpperCase() + word.slice(1);

const strs = (value: unknown) =>
  Array.isArray(value) ? value.filter((s): s is string => typeof s === "string") : [];

/* a JSON field may come as its value or as its text */
const object = (value: unknown): Record<string, unknown> => {
  if (typeof value === "string") {
    try {
      return object(JSON.parse(value));
    } catch {
      return {};
    }
  }
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
};

/* a tool set also reaches agents through its plugins' background jobs */
const jobTargets = (integrations: unknown) => {
  const plugins = object(integrations).plugins;
  return (Array.isArray(plugins) ? plugins : []).flatMap((plugin) =>
    strs(object(object(plugin).job).agent_targets),
  );
};

/** What a preview would add, laid out as a pack's page lays out what it
    added (see `resourceTree`), then the documents its page has no section
    for, then its plugins; the settings that belong to nothing else it adds
    are in `other`. Each document carries its collection, so it opens its
    own page. */
export function previewTree(report: PackInstallReport): ResourceTree {
  const documents = report.documents ?? [];
  const find = (collection: string, id: string | null | undefined) =>
    id ? documents.find((d) => d.collection === collection && d.id === id) : undefined;
  const parts = previewParts(documents);
  const { tree, other } = resourceTree(parts, previewSettings(documents), {
    agentOfTask: (taskId) => str(content(find("Task", taskId)).agent_id),
    contextOfAgent: (agentId) => {
      const context = find(
        "AgentContext",
        str(content(find("Agent", agentId)).context_id),
      );
      if (!context) return undefined;
      const fields = content(context);
      return {
        context_id: context.id,
        tools_id: str(fields.tools_id),
        compaction_id: str(fields.compaction_id),
        skill_ids: strs(fields.skill_ids),
      };
    },
    targetsOfTools: (toolsId) => {
      const tools = content(find("Tools", toolsId));
      return [
        ...strs(object(tools.agents).target_ids),
        ...jobTargets(tools.integrations),
      ];
    },
    agentOfTarget: (targetId) => str(content(find("AgentTarget", targetId)).agent_id),
  });
  const unsectioned = parts
    .filter((p) => p.section === null)
    .flatMap((p) =>
      p.items.map((item): Resource => ({
        section: "",
        collection: item.collection,
        kind: capital(item.collection ? kindOf(item.collection) : p.one),
        id: item.id,
        name: item.name,
        relation: null,
        children: [],
      })),
    );
  const { plugins, pages } = previewPlugins(report);
  const named = (kind: string) => (name: string) => ({
    section: "",
    kind,
    id: name,
    name,
    relation: null,
    children: [],
  });
  return {
    tree: [
      ...tree,
      ...unsectioned,
      ...plugins.map(named("Plugin")),
      ...pages.map(named("Interface plugin")),
    ],
    other,
  };
}

/** what a preview adds, counted by the kinds people look for, then the
    settings that belong to nothing else, for the section's heading */
export function previewCounts(report: PackInstallReport) {
  const { plugins, pages } = previewPlugins(report);
  const { other } = previewTree(report);
  const count = (n: number, one: string, many: string) =>
    n ? [`${n} ${n === 1 ? one : many}`] : [];
  return [
    ...previewParts(report.documents ?? []).map(partLabel),
    ...count(plugins.length, "plugin", "plugins"),
    ...count(pages.length, "interface plugin", "interface plugins"),
    ...(other.length ? [otherLabel(other.length)] : []),
  ];
}
