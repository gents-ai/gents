import { isValidUxPluginId } from "@/sandbox/contract";
import type { Route } from "./router";
import { isPackDigest, isPackName, isPackVersion, packPreviewKey } from "./packReview";

/** A place a `gents:` link names, from outside the app (`gents://session/<id>`)
    or inside a message (`gents:session/<id>`), optionally with the node that
    holds it (`?node=<did>`); a UX plugin's page (`gents://plugin/<id>`),
    optionally at a place within it (`gents://plugin/<id>/<path>`); or a pack
    to review installing
    (`gents://pack/<namespace>/<name>?version=<v>&digest=<sha256:…>`), whose
    digest pins the archive. A link to a message waits until a message's
    sequence is known to be the same on every replica of its session, since
    a link outlives the copy it was made on. */
export type AppLink =
  | { kind: "session"; sessionId: string; nodeDid?: string }
  | { kind: "plugin"; pluginId: string; path?: string }
  | { kind: "pack"; pack: string; version?: string; digest?: string };

const LINK = /^gents:(?:\/\/)?([a-z][a-z0-9-]*)\/([^?#]*)(?:\?([^#]*))?/i;

/* a place within a plugin's page: what the page reads off its route, so
   plain path segments only, never a scheme, a query or a way up */
const SUBPATH_SEGMENT = /^[A-Za-z0-9_~-][A-Za-z0-9._~-]{0,63}$/;
export const PLUGIN_SUBPATH_MAX = 200;
const PLUGIN_SUBPATH_SEGMENTS = 8;

/** Whether `path` names a place within a plugin's page: up to eight plain
    segments joined by `/`, none of them `.` or `..`. */
export function isPluginSubpath(path: string): boolean {
  if (path.length === 0 || path.length > PLUGIN_SUBPATH_MAX) return false;
  const segments = path.split("/");
  return (
    segments.length <= PLUGIN_SUBPATH_SEGMENTS &&
    segments.every((s) => SUBPATH_SEGMENT.test(s) && s !== "." && s !== "..")
  );
}

function decoded(part: string): string | null {
  try {
    return decodeURIComponent(part).trim();
  } catch {
    return null;
  }
}

function sessionLink(parts: string[], query: string | undefined): AppLink | null {
  if (parts.length !== 1) return null;
  const sessionId = decoded(parts[0]!);
  if (!sessionId) return null;
  const nodeDid = new URLSearchParams(query ?? "").get("node")?.trim();
  return { kind: "session", sessionId, ...(nodeDid ? { nodeDid } : {}) };
}

function pluginLink(parts: string[]): AppLink | null {
  const [id, ...rest] = parts;
  const pluginId = id === undefined ? null : decoded(id);
  if (!pluginId || !isValidUxPluginId(pluginId)) return null;
  if (rest.length === 0) return { kind: "plugin", pluginId };
  const path = rest.map(decoded);
  if (path.some((s) => s === null || s.includes("/"))) return null;
  const joined = path.join("/");
  return isPluginSubpath(joined) ? { kind: "plugin", pluginId, path: joined } : null;
}

function packLink(parts: string[], query: string | undefined): AppLink | null {
  if (parts.length !== 2 || !parts.every(isPackName)) return null;
  const params = new URLSearchParams(query ?? "");
  const version = params.get("version")?.trim();
  const digest = params.get("digest")?.trim();
  if (version !== undefined && !isPackVersion(version)) return null;
  if (digest !== undefined && !isPackDigest(digest)) return null;
  return {
    kind: "pack",
    pack: parts.join("/"),
    ...(version ? { version } : {}),
    ...(digest ? { digest } : {}),
  };
}

export function parseLink(href: string): AppLink | null {
  const match = LINK.exec(href.trim());
  if (!match) return null;
  const [, kind, path, query] = match;
  const parts = path!.split("/").filter(Boolean);
  switch (kind!.toLowerCase()) {
    case "session":
      return sessionLink(parts, query);
    case "plugin":
      return pluginLink(parts);
    case "pack":
      return packLink(parts, query);
    default:
      return null;
  }
}

export const isAppLink = (href: string) => /^gents:/i.test(href.trim());

/** Where a link goes. Without a node, a session's owner is found among the
    nodes this app knows, as for any session route. */
export function linkRoute(link: AppLink): Route {
  if (link.kind === "plugin")
    return {
      name: "plugin",
      pluginId: link.pluginId,
      ...(link.path ? { path: link.path } : {}),
    };
  if (link.kind === "pack")
    return {
      name: "packs",
      review: packPreviewKey({
        package: link.version ? `${link.pack}@${link.version}` : link.pack,
        source: "link",
        ...(link.digest ? { digest: link.digest } : {}),
      }),
    };
  return {
    name: "session",
    sessionId: link.sessionId,
    ...(link.nodeDid ? { nodeDid: link.nodeDid } : {}),
  };
}
