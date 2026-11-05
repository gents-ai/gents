import { useStore } from "zustand";
import { createStore } from "zustand/vanilla";

/** An install the person is asked to review before anything is written. */
export type PackReviewRequest = {
  /** what to install: a registry name (with `@version` from a link) or a
      `.pack` file's path */
  package: string;
  /** `invite`: a pack an invite brings, at the version the inviter runs */
  source: "registry" | "file" | "link" | "invite";
  /** the digest a link names; a pack that differs is not offered */
  digest?: string;
};

/** One review's identity: the same pack from the same place, pinned alike. */
export const reviewKey = (request: PackReviewRequest) =>
  JSON.stringify([request.source, request.package, request.digest ?? null]);

/* The reviews a pack's page previews from a file or a link, by the short key
   its route carries, so a file's path never goes in a route. */
const previewed = createStore<{ requests: Record<string, PackReviewRequest> }>(() => ({
  requests: {},
}));
let nextKey = 1;

/** The key a page previewing `request` is routed by; the same request
    keeps its key. */
export function packPreviewKey(request: PackReviewRequest): string {
  const { requests } = previewed.getState();
  const known = Object.entries(requests).find(
    ([, r]) => reviewKey(r) === reviewKey(request),
  );
  if (known) return known[0];
  const key = String(nextKey++);
  previewed.setState({ requests: { ...requests, [key]: request } });
  return key;
}

export const usePackPreviewRequest = (key: string | undefined) =>
  useStore(previewed, (s) => (key ? (s.requests[key] ?? null) : null));

const LINK_PACK = /^[a-z][a-z0-9_]*$/;
const LINK_VERSION = /^[A-Za-z0-9.+-]+$/;
const LINK_DIGEST = /^sha256:[0-9a-f]{64}$/;

export const isPackName = (part: string) => LINK_PACK.test(part);
export const isPackVersion = (version: string) =>
  LINK_VERSION.test(version) && version !== "." && version !== "..";
export const isPackDigest = (digest: string) => LINK_DIGEST.test(digest);

/** The link that opens `pack` on another machine and asks to install exactly
    this version: its digest pins the archive. */
export function packLink(
  pack: string,
  version?: string | null,
  digest?: string | null,
) {
  const query = new URLSearchParams();
  if (version) query.set("version", version);
  if (digest) query.set("digest", digest);
  const params = query.toString();
  return `gents://pack/${pack}${params ? `?${params}` : ""}`;
}
