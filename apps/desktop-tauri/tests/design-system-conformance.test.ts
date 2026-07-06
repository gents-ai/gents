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

describe("motion and focus", () => {
  it("transition/animation durations use --motion-* tokens", () => {
    const raw: string[] = [];
    for (const [file, css] of sources) {
      if (file.endsWith("tokens.css")) continue;
      for (const match of css.matchAll(
        /(?:transition|animation)[^:;{}]*:\s*[^;{}]*?(\d+(?:\.\d+)?m?s)\b/g,
      )) {
        // 0.01ms is the reduced-motion kill value in base.css.
        if (match[1] === "0.01ms") continue;
        raw.push(`${relative(STYLES_ROOT, file)}: ${match[1]}`);
      }
    }
    expect(raw).toEqual([]);
  });

  it("focus outlines are never removed", () => {
    const removals: string[] = [];
    for (const [file, css] of sources) {
      // All removal spellings: longhands, 0px, !important, any casing.
      for (const match of css.matchAll(
        /outline(?:-style|-width)?\s*:\s*(?:none|0(?:px)?)\b(?:\s*!important)?\s*[;}]/gi,
      )) {
        removals.push(`${relative(STYLES_ROOT, file)}: ${match[0]}`);
      }
    }
    expect(removals).toEqual([]);
  });
});

describe("type scale", () => {
  it("every font-size declaration uses a --text-* token", () => {
    const raw: string[] = [];
    for (const [file, css] of sources) {
      // Case-insensitive; the final declaration of a block needs no ';'.
      for (const match of css.matchAll(/font-size:\s*([^;}]+)[;}]/gi)) {
        const value = match[1].trim();
        if (!/^var\(--text-[\w-]+\)$/.test(value) && value !== "inherit") {
          // tokens.css defines the scale itself in raw px.
          if (file.endsWith("tokens.css")) continue;
          raw.push(`${relative(STYLES_ROOT, file)}: font-size: ${value}`);
        }
      }
    }
    expect(raw).toEqual([]);
  });

  it("the font shorthand never smuggles a raw size past the fence", () => {
    const raw: string[] = [];
    for (const [file, css] of sources) {
      for (const match of css.matchAll(/(?<![\w-])font:\s*([^;}]+)[;}]/gi)) {
        const value = match[1].trim();
        // `font: inherit` is the only shorthand the fence allows; any other
        // value can carry an off-scale raw size (e.g. `font: 12px/1.4 …`).
        if (value !== "inherit") {
          raw.push(`${relative(STYLES_ROOT, file)}: font: ${value}`);
        }
      }
    }
    expect(raw).toEqual([]);
  });
});

describe("token ratchets", () => {
  // These counts may only go DOWN. They hold the line on raw values the
  // per-screen polish passes are still sweeping onto tokens — a new raw
  // literal fails here; when you tokenize some, lower the ceiling.
  function countMatches(pattern: RegExp): number {
    let count = 0;
    for (const css of sources.values()) {
      count += [...css.matchAll(pattern)].length;
    }
    return count;
  }

  it("raw px inside spacing declarations does not grow (ceiling 40)", () => {
    let count = 0;
    for (const css of sources.values()) {
      for (const match of css.matchAll(
        // (?<![\w-]) keeps scroll-padding-* out; -start/-end covers the
        // logical longhands so raw px can't smuggle through them.
        /(?<![\w-])(?:padding|margin|gap|row-gap|column-gap)(?:-(?:top|right|bottom|left|inline|block))?(?:-(?:start|end))?\s*:\s*([^;{}]+)/g,
      )) {
        count += (match[1].match(/\d+px\b/g) ?? []).length;
      }
    }
    expect(count).toBeLessThanOrEqual(40);
  });

  it("raw rgb() literals do not grow (ceiling 90)", () => {
    expect(countMatches(/rgb\(\d+ \d+ \d+/g)).toBeLessThanOrEqual(90);
  });

  it("bespoke box-shadows do not grow (ceiling 38)", () => {
    // 'none' and pure --shadow-* token values are conformant, not bespoke:
    // tokenizing shadows must lower this pressure, not preserve it.
    expect(
      countMatches(/box-shadow:\s*(?!none[;}\s]|var\(--shadow-)[^;{}]+/gi),
    ).toBeLessThanOrEqual(38);
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
