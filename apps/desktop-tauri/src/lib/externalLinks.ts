/// External-link handling for the desktop webview.
///
/// In Tauri 2's WKWebView a plain `<a href="https://...">` click performs an
/// in-place navigation that replaces the entire app UI with no way back.
/// A document-level guard intercepts anchor clicks and routes external URLs
/// to the OS default browser (opener plugin under Tauri, window.open in
/// plain browsers like the Playwright harness).

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

export async function openExternalUrl(url: string): Promise<void> {
  if (typeof window !== "undefined" && "__TAURI_INTERNALS__" in window) {
    const { openUrl } = await import("@tauri-apps/plugin-opener");
    await openUrl(url);
    return;
  }
  window.open(url, "_blank", "noopener,noreferrer");
}

/// Document-level click guard. Anchors pointing at external URLs open in the
/// OS browser; any other anchor navigation (relative hrefs, unknown schemes)
/// is suppressed — in-place navigation replaces the entire app UI. Pure hash
/// anchors and already-handled clicks are left alone.
export function handleExternalLinkClick(event: MouseEvent): void {
  if (event.defaultPrevented) return;
  const target = event.target as Element | null;
  const anchor = target?.closest?.("a[href]");
  if (!anchor) return;
  const href = anchor.getAttribute("href");
  if (!href || href.startsWith("#")) return;
  event.preventDefault();
  if (isExternalUrl(href)) {
    void openExternalUrl(href);
  }
}

export function installExternalLinkGuard(doc: Document): () => void {
  const listener = (event: MouseEvent) => handleExternalLinkClick(event);
  doc.addEventListener("click", listener);
  return () => doc.removeEventListener("click", listener);
}
