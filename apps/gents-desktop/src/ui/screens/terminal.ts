/* A read-only terminal for showing a command's output. xterm is loaded only
   when the first one opens. */
import type { ITheme, Terminal } from "@xterm/xterm";
import { isExternalUrl } from "../../lib/externalLinks";

export type LogTerminal = {
  /* writes text already in xterm's line endings; reset clears first */
  write: (data: string, reset: boolean) => void;
  /* lines the reader is above the newest, to give back on reopening */
  linesAbove: () => number;
  find: (query: string, backwards?: boolean) => boolean;
  clearFind: () => void;
  dispose: () => void;
};

const token = (style: CSSStyleDeclaration, name: string) =>
  style.getPropertyValue(name).trim() || undefined;

/* the app's colors, read from the document root as they stand now */
export function terminalTheme(): ITheme {
  const style = getComputedStyle(document.documentElement);
  const background = token(style, "--surface");
  return {
    background,
    foreground: token(style, "--text"),
    cursor: background,
    cursorAccent: background,
    selectionBackground: token(style, "--accent"),
    selectionForeground: token(style, "--accent-foreground"),
    red: token(style, "--destructive"),
    brightRed: token(style, "--destructive"),
    brightBlack: token(style, "--text-muted"),
  };
}

const fontFamily = () =>
  token(getComputedStyle(document.documentElement), "--face-mono") ?? "monospace";

/* lines the reader has scrolled up from the newest; 0 while following */
const aboveFoot = (term: Terminal) =>
  Math.max(0, term.buffer.active.baseY - term.buffer.active.viewportY);

export async function openTerminal(
  host: HTMLElement,
  { onLink, above = 0 }: { onLink: (url: string) => void; above?: number },
): Promise<LogTerminal> {
  const [{ Terminal }, { FitAddon }, { WebLinksAddon }, { SearchAddon }] =
    await Promise.all([
      import("@xterm/xterm"),
      import("@xterm/addon-fit"),
      import("@xterm/addon-web-links"),
      import("@xterm/addon-search"),
      import("@xterm/xterm/css/xterm.css"),
    ]);
  const term = new Terminal({
    linkHandler: {
      activate: (_event, uri) => {
        if (isExternalUrl(uri)) onLink(uri);
      },
    },
    disableStdin: true,
    screenReaderMode: true,
    cursorBlink: false,
    cursorInactiveStyle: "none",
    scrollback: 10_000,
    fontFamily: fontFamily(),
    fontSize: 11,
    lineHeight: 1.4,
    theme: terminalTheme(),
  });
  const fit = new FitAddon();
  const search = new SearchAddon();
  term.loadAddon(fit);
  term.loadAddon(search);
  term.loadAddon(
    new WebLinksAddon((_event, uri) => {
      if (isExternalUrl(uri)) onLink(uri);
    }),
  );
  term.open(host);
  fit.fit();
  const sizes = new ResizeObserver(() => fit.fit());
  sizes.observe(host);
  const themes = new MutationObserver(() => {
    term.options.theme = terminalTheme();
  });
  themes.observe(document.documentElement, {
    attributes: true,
    attributeFilter: ["data-theme"],
  });
  let disposed = false;
  /* the place a reopened log was left at, given back after its first write */
  let returning = above;
  return {
    /* a reader following stays at the newest line; one scrolled up keeps
       their distance from it, which a reset would otherwise lose */
    write(data, reset) {
      const above = aboveFoot(term);
      if (reset) term.reset();
      term.write(data, () => {
        if (disposed) return;
        if (returning > 0) {
          term.scrollToLine(Math.max(0, term.buffer.active.baseY - returning));
          returning = 0;
          return;
        }
        if (above === 0) term.scrollToBottom();
        else if (reset)
          term.scrollToLine(Math.max(0, term.buffer.active.baseY - above));
      });
    },
    linesAbove: () => aboveFoot(term),
    find: (query, backwards) =>
      backwards ? search.findPrevious(query) : search.findNext(query),
    clearFind: () => search.clearDecorations(),
    dispose() {
      disposed = true;
      sizes.disconnect();
      themes.disconnect();
      term.dispose();
    },
  };
}
