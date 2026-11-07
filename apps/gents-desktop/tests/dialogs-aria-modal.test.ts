import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";

/* The kit's dialog content doesn't mark itself modal, so each dialog says
   so; without aria-modal a screen reader leaves the dialog for the page
   behind it. */
const ROOT = join(__dirname, "..", "src");
const OPENERS = /<(DialogContent|AlertDialogContent)\b/g;

function sources(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return sources(path);
    return path.endsWith(".tsx") ? [path] : [];
  });
}

/* the opening tag's text, up to the `>` that closes it outside braces and
   strings, so an arrow function in a prop doesn't end it early */
function openingTag(source: string, from: number): string {
  let depth = 0;
  let quote: string | null = null;
  for (let i = from; i < source.length; i++) {
    const c = source[i]!;
    if (quote) {
      if (c === quote && source[i - 1] !== "\\") quote = null;
    } else if (c === '"' || c === "'" || c === "`") quote = c;
    else if (c === "{") depth++;
    else if (c === "}") depth--;
    else if (c === ">" && depth === 0) return source.slice(from, i + 1);
  }
  return source.slice(from);
}

describe("dialogs", () => {
  it("every dialog's content says it is modal", () => {
    const missing: string[] = [];
    for (const path of sources(ROOT)) {
      const source = readFileSync(path, "utf8");
      for (const match of source.matchAll(OPENERS)) {
        const tag = openingTag(source, match.index);
        if (!/\baria-modal=(?:"true"|\{true\})/.test(tag)) {
          const line = source.slice(0, match.index).split("\n").length;
          missing.push(`${relative(ROOT, path)}:${line}`);
        }
      }
    }
    expect(missing).toEqual([]);
  });
});
