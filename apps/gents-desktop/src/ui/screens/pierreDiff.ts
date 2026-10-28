import type { CSSProperties } from "react";
import { canUseWASM, type HighlighterTypes } from "@pierre/diffs";
import type { FileDiffOptions } from "@pierre/diffs/react";
import type { useTheme } from "@/preferences";

/* the highlighted diffs' colours and type follow the app's own tokens */
export const TOKENS = {
  "--diffs-font-family": "var(--font-mono)",
  "--diffs-header-font-family": "var(--font-sans)",
  "--diffs-font-size": "12px",
  "--diffs-line-height": "18px",
  "--diffs-bg-addition-override": "var(--diff-added)",
  "--diffs-bg-deletion-override": "var(--diff-removed)",
  "--diffs-addition-color-override": "var(--diff-added-fg)",
  "--diffs-deletion-color-override": "var(--diff-removed-fg)",
} as CSSProperties;

/* The highlighter's theme paints its own background, and blends a changed
   line's colour into it, which washes it out. The view takes the app's
   background, or `--diff-view-bg` where it sits on another surface, and
   changed lines take its diff colours in full; this stylesheet layer comes
   after the theme's. Colours that arrive from a worker after the plain text
   fade in rather than snap. */
const LINE_TINTS = `
:host {
  background-color: var(--diff-view-bg, var(--color-background));
}
pre {
  --diffs-bg: var(--diff-view-bg, var(--color-background));
}
[data-line] span[style*="--diffs-token"] {
  animation: diffs-colour-in 150ms ease-out;
}
@keyframes diffs-colour-in {
  from {
    color: light-dark(var(--diffs-light), var(--diffs-dark));
  }
}
[data-line-type=change-addition]:is([data-line],[data-column-number],[data-no-newline],[data-gutter-buffer]) {
  background-color: var(--diff-added);
}
[data-line-type=change-deletion]:is([data-line],[data-column-number],[data-no-newline],[data-gutter-buffer]) {
  background-color: var(--diff-removed);
}`;

/* Highlights, the highlighter built for Diffs, is compiled to WebAssembly
   and colours a long file in milliseconds where the regular-expression
   engines take seconds; the webview's CSP admits WebAssembly for it. A
   language it does not know shows as plain text. Where WebAssembly can't
   compile, the regular-expression engine colours instead: a highlighter
   that can't load draws nothing at all, not even plain text. The workers
   take the same engine, since they run under the page's policy. */
export const HIGHLIGHTER: HighlighterTypes = canUseWASM() ? "highlights" : "shiki-js";

/* Colouring runs in workers, so a long file no longer holds the view; a
   file past this many lines, beyond what a 1 MB copy usually holds, still
   shows uncoloured rather than tie the workers up. */
const HIGHLIGHTED_LINES = 30000;

type Variant = "file" | "whole-file" | "excerpt" | "unnumbered";

/* One options object per theme and variant, so a render with nothing new
   keeps the highlighted view as it is. A whole file shows its unchanged
   lines too. An excerpt has no file around it, so its separators cannot
   expand into unchanged lines. */
const OPTIONS = new Map<string, FileDiffOptions<undefined, undefined>>();
export function optionsFor(theme: ReturnType<typeof useTheme>, variant: Variant) {
  const key = `${theme}\0${variant}`;
  let options = OPTIONS.get(key);
  if (!options)
    OPTIONS.set(
      key,
      (options = {
        diffStyle: "unified",
        overflow: "wrap",
        themeType: theme,
        preferredHighlighter: HIGHLIGHTER,
        disableFileHeader: true,
        diffIndicators: "classic",
        unsafeCSS: LINE_TINTS,
        tokenizeMaxLength: HIGHLIGHTED_LINES,
        ...(variant === "whole-file" && { expandUnchanged: true }),
        ...((variant === "excerpt" || variant === "unnumbered") && {
          hunkSeparators: "simple" as const,
        }),
        ...(variant === "unnumbered" && { disableLineNumbers: true }),
      }),
    );
  return options;
}
