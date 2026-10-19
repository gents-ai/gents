import type { CapturedContext } from "./generated/CapturedContext.js";

export type { CapturedContext } from "./generated/CapturedContext.js";

export function capturedContextLabel(context: CapturedContext): string {
  let label =
    context.terminalName ||
    {
      selection: "Selection",
      file: "File",
      diagnostics: "Diagnostics",
      terminal: "Terminal",
    }[context.kind];
  if (context.uri) {
    try {
      const name = decodeURIComponent(
        new URL(context.uri).pathname.split("/").at(-1) || "",
      );
      if (name)
        label = context.kind === "diagnostics" ? `Diagnostics: ${name}` : name;
    } catch {}
  }
  if (context.range) {
    const { start, end } = context.range;
    label += `:${start.line + 1}${end.line > start.line ? `-${end.line + 1}` : ""}`;
  }
  return label.slice(0, 1024);
}
