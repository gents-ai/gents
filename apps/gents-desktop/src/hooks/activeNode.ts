/* the node last made active on this device, so a reload opens on it; a
   per-device convenience, so storage that refuses only loses the choice */
const KEY = "gents.activeNode";

export function rememberedNode(): string | null {
  try {
    return localStorage.getItem(KEY);
  } catch {
    return null;
  }
}

export function rememberNode(nodeDid: string) {
  try {
    localStorage.setItem(KEY, nodeDid);
  } catch {
    /* nothing to keep it in */
  }
}

/* the node active before the current one, in this window, for the key that
   goes back to it; it isn't kept across a reload */
let current: string | null = null;
let previous: string | null = null;

/** Notes a node made active, so the one before it can be gone back to. */
export function noteActiveNode(nodeDid: string) {
  if (nodeDid === current) return;
  if (current) previous = current;
  current = nodeDid;
}

/** The node active before the current one, if there was one. */
export const previousNode = () => previous;
