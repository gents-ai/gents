/// Clipboard writes for the desktop webview. navigator.clipboard is the
/// happy path; WKWebView permission quirks make the hidden-textarea
/// execCommand fallback worth keeping.
export async function copyText(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // fall through to the legacy path
  }
  const previouslyFocused = document.activeElement;
  let area: HTMLTextAreaElement | null = null;
  try {
    area = document.createElement("textarea");
    area.value = text;
    area.setAttribute("readonly", "");
    area.style.position = "fixed";
    area.style.left = "-9999px";
    document.body.appendChild(area);
    area.select();
    return document.execCommand("copy");
  } catch {
    return false;
  } finally {
    area?.remove();
    if (
      previouslyFocused instanceof HTMLElement &&
      previouslyFocused.isConnected
    ) {
      previouslyFocused.focus();
    }
  }
}
