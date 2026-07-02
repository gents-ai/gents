import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";

// Design-system conformance fence.
//
// 1. Token integrity: a `var(--x)` without a fallback silently invalidates
//    its whole declaration when --x is undefined (this shipped as collapsed
//    padding and un-monospaced IDs). Every fallback-less reference must
//    resolve to a definition somewhere in the stylesheet set.
// 2. Layer integrity: App.css declares the cascade-layer order. A sheet with
//    top-level rules outside @layer outranks every layer and breaks the
//    system; a sheet using an undeclared layer name lands in arbitrary order.

const STYLES_ROOT = join(__dirname, "..", "src", "styles");
const APP_CSS = join(__dirname, "..", "src", "App.css");

function cssFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((entry) => {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) return cssFiles(full);
    return entry.endsWith(".css") ? [full] : [];
  });
}

const files = [APP_CSS, ...cssFiles(STYLES_ROOT)];

function stripComments(css: string): string {
  return css.replace(/\/\*[\s\S]*?\*\//g, "");
}

const sources = new Map(
  files.map((file) => [file, stripComments(readFileSync(file, "utf8"))]),
);

describe("design tokens", () => {
  it("every fallback-less var() reference resolves to a defined token", () => {
    const defined = new Set<string>();
    for (const css of sources.values()) {
      for (const match of css.matchAll(/(?:^|[{;\s])(--[\w-]+)\s*:/g)) {
        defined.add(match[1]);
      }
    }

    const undefinedRefs: string[] = [];
    for (const [file, css] of sources) {
      for (const match of css.matchAll(/var\(\s*(--[\w-]+)\s*([,)])/g)) {
        const [, token, terminator] = match;
        const hasFallback = terminator === ",";
        if (!hasFallback && !defined.has(token)) {
          undefinedRefs.push(`${relative(STYLES_ROOT, file)}: ${token}`);
        }
      }
    }

    expect(undefinedRefs).toEqual([]);
  });
});

describe("cascade layers", () => {
  const appCss = sources.get(APP_CSS) ?? "";
  const orderMatch = appCss.match(/@layer\s+([\w\s,-]+);/);
  const declaredOrder = (orderMatch?.[1] ?? "")
    .split(",")
    .map((name) => name.trim())
    .filter(Boolean);

  it("App.css declares the layer order", () => {
    expect(declaredOrder.length).toBeGreaterThan(0);
  });

  it("every sheet keeps all rules inside @layer blocks", () => {
    const violations: string[] = [];
    for (const [file, css] of sources) {
      if (file === APP_CSS) continue;
      let depth = 0;
      let statement = "";
      for (const char of css) {
        if (char === "{") {
          if (depth === 0) {
            const head = statement.trim();
            if (!head.startsWith("@layer")) {
              violations.push(
                `${relative(STYLES_ROOT, file)}: top-level rule outside @layer: "${head.slice(0, 60)}"`,
              );
            }
            statement = "";
          }
          depth += 1;
        } else if (char === "}") {
          depth -= 1;
        } else if (depth === 0) {
          if (char === ";") {
            statement = "";
          } else {
            statement += char;
          }
        }
      }
    }
    expect(violations).toEqual([]);
  });

  it("every @layer name used is in App.css's declared order", () => {
    const unknown: string[] = [];
    for (const [file, css] of sources) {
      if (file === APP_CSS) continue;
      for (const match of css.matchAll(/@layer\s+([\w-]+)\s*\{/g)) {
        if (!declaredOrder.includes(match[1])) {
          unknown.push(`${relative(STYLES_ROOT, file)}: @layer ${match[1]}`);
        }
      }
    }
    expect(unknown).toEqual([]);
  });
});
