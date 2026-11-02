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
