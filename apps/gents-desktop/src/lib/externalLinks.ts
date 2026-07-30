
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

export function handleExternalLinkClick(event: MouseEvent): void {
  if (event.defaultPrevented) return;
  const target = event.target as Element | null;
  const anchor = target?.closest?.("a[href]");
  if (!anchor) return;
  const href = anchor.getAttribute("href");
  if (href === null || href.startsWith("#")) return;
  event.preventDefault();
  if (isExternalUrl(href)) {
    void openExternalUrl(href);
  }
}

export function installExternalLinkGuard(doc: Document): () => void {
  const listener = (event: MouseEvent) => handleExternalLinkClick(event);
  doc.addEventListener("click", listener, { capture: true });
  return () => doc.removeEventListener("click", listener, { capture: true });
}
