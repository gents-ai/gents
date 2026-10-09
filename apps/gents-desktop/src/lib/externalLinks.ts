import { inNativeShell, openWithOpener } from "./nativeShell";

const EXTERNAL_PROTOCOLS = new Set(["http:", "https:", "mailto:"]);
const ABSOLUTE_URL = /^[a-z][a-z0-9+.-]*:/i;

export function isExternalUrl(href: string): boolean {
  if (!ABSOLUTE_URL.test(href)) return false;
  try {
    return EXTERNAL_PROTOCOLS.has(new URL(href).protocol);
  } catch {
    return false;
  }
}

/** Opens a URL outside the app without the bridge: the OS's opener in the
    shell, a new tab in a plain browser. */
export async function openInBrowser(url: string): Promise<void> {
  if (inNativeShell()) return openWithOpener(url);
  window.open(url, "_blank", "noopener,noreferrer");
}

export function handleExternalLinkClick(
  event: MouseEvent,
  open: (url: string) => Promise<void> = openInBrowser,
): void {
  if (event.defaultPrevented) return;
  const target = event.target as Element | null;
  const anchor = target?.closest?.("a[href]");
  if (!anchor) return;
  const href = anchor.getAttribute("href");
  if (href === null || href.startsWith("#")) return;
  event.preventDefault();
  if (isExternalUrl(href)) {
    void open(href);
  }
}

/** Keeps every link click inside the webview: an external one opens with
    `open`, anything else goes nowhere. */
export function installExternalLinkGuard(
  doc: Document,
  open: (url: string) => Promise<void> = openInBrowser,
): () => void {
  const listener = (event: MouseEvent) => handleExternalLinkClick(event, open);
  doc.addEventListener("click", listener, { capture: true });
  return () => doc.removeEventListener("click", listener, { capture: true });
}
