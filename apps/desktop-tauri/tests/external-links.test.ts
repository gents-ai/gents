import { afterEach, describe, expect, it, vi } from "vitest";

import {
  handleExternalLinkClick,
  installExternalLinkGuard,
  isExternalUrl,
} from "../src/lib/externalLinks";

function clickAnchor(href: string): MouseEvent {
  const anchor = document.createElement("a");
  anchor.setAttribute("href", href);
  anchor.textContent = "link";
  document.body.appendChild(anchor);
  const event = new MouseEvent("click", { bubbles: true, cancelable: true });
  anchor.dispatchEvent(event);
  return event;
}

describe("external link guard", () => {
  afterEach(() => {
    document.body.innerHTML = "";
    vi.restoreAllMocks();
  });

  it("classifies URLs", () => {
    expect(isExternalUrl("https://example.com")).toBe(true);
    expect(isExternalUrl("http://example.com")).toBe(true);
    expect(isExternalUrl("mailto:a@b.c")).toBe(true);
    expect(isExternalUrl("javascript:alert(1)")).toBe(false);
    expect(isExternalUrl("docs/relative")).toBe(false);
  });

  it("suppresses navigation for relative and unknown-scheme anchors", () => {
    const openSpy = vi.spyOn(window, "open").mockReturnValue(null);
    const uninstall = installExternalLinkGuard(document);

    const event = clickAnchor("docs/relative");

    expect(event.defaultPrevented).toBe(true);
    expect(openSpy).not.toHaveBeenCalled();
    uninstall();
  });

  it("prevents webview navigation and opens external links in the browser", () => {
    const openSpy = vi.spyOn(window, "open").mockReturnValue(null);
    const uninstall = installExternalLinkGuard(document);

    const event = clickAnchor("https://example.com/docs");

    expect(event.defaultPrevented).toBe(true);
    expect(openSpy).toHaveBeenCalledWith(
      "https://example.com/docs",
      "_blank",
      "noopener,noreferrer",
    );
    uninstall();
  });

  it("leaves hash anchors and already-handled clicks alone", () => {
    const openSpy = vi.spyOn(window, "open").mockReturnValue(null);

    const hashEvent = clickAnchor("#section");
    handleExternalLinkClick(hashEvent as unknown as MouseEvent);
    expect(hashEvent.defaultPrevented).toBe(false);

    const handled = clickAnchor("https://example.com");
    handled.preventDefault();
    handleExternalLinkClick(handled as unknown as MouseEvent);
    expect(openSpy).not.toHaveBeenCalled();
  });

  it("suppresses hostile schemes and scheme-relative URLs without opening them", () => {
    const openSpy = vi.spyOn(window, "open").mockReturnValue(null);
    const uninstall = installExternalLinkGuard(document);

    for (const href of [
      "javascript:alert(1)",
      "data:text/html,<script>1</script>",
      "file:///etc/passwd",
      "//evil.example.com/x",
    ]) {
      const event = clickAnchor(href);
      expect(event.defaultPrevented, href).toBe(true);
    }
    expect(openSpy).not.toHaveBeenCalled();
    uninstall();
  });

  it('suppresses empty hrefs (markdown sanitizers rewrite hostile links to href="")', () => {
    const openSpy = vi.spyOn(window, "open").mockReturnValue(null);
    const uninstall = installExternalLinkGuard(document);

    const event = clickAnchor("");

    // Default action of <a href=""> is a full document reload — must not run.
    expect(event.defaultPrevented).toBe(true);
    expect(openSpy).not.toHaveBeenCalled();
    uninstall();
  });

  it("is not bypassed by a component-level stopPropagation", () => {
    const openSpy = vi.spyOn(window, "open").mockReturnValue(null);
    const uninstall = installExternalLinkGuard(document);

    const wrapper = document.createElement("div");
    // Bubble-phase swallow, as React components do (e.g. dialog click guards).
    wrapper.addEventListener("click", (event) => event.stopPropagation());
    const anchor = document.createElement("a");
    anchor.setAttribute("href", "https://example.com/docs");
    wrapper.appendChild(anchor);
    document.body.appendChild(wrapper);

    const event = new MouseEvent("click", { bubbles: true, cancelable: true });
    anchor.dispatchEvent(event);

    expect(event.defaultPrevented).toBe(true);
    expect(openSpy).toHaveBeenCalledWith(
      "https://example.com/docs",
      "_blank",
      "noopener,noreferrer",
    );
    uninstall();
  });
});
