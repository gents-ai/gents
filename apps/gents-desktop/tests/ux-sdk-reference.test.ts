/* The `@gents/ux-sdk` reference and the lint's token list are generated into
   crates/gents/src/plugin/ux so the Rust side can carry them. This compares
   both with what they describe: the frame's export namespace, and the custom
   properties its built stylesheet declares at document scope.
   UPDATE_UX_SDK_REFERENCE=1 npx vitest run tests/ux-sdk-reference.test.ts
   writes them instead. */
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { beforeAll, describe, expect, it } from "vitest";

import * as sdk from "@/sandbox/sdk-exports";
import { renderSdkReference, SDK_ENTRIES, TOKEN_ROLES } from "@/sandbox/sdk-reference";

const UX_DIR = join(__dirname, "../../../crates/gents/src/plugin/ux");
const REFERENCE = join(UX_DIR, "sdk_reference.md");
const TOKENS = join(UX_DIR, "frame_tokens.txt");
const REGENERATE =
  "UPDATE_UX_SDK_REFERENCE=1 npx vitest run tests/ux-sdk-reference.test.ts";
const UPDATE = Boolean(process.env.UPDATE_UX_SDK_REFERENCE);

/* the frame in a subprocess: a vite build does not run under jsdom */
function builtFrameCss(): string {
  const out = mkdtempSync(join(tmpdir(), "ux-frame-css-"));
  try {
    const vite = join(
      dirname(createRequire(import.meta.url).resolve("vite/package.json")),
      "bin/vite.js",
    );
    execFileSync(
      process.execPath,
      [
        vite,
        "build",
        "-c",
        "vite.sandbox.config.ts",
        "--logLevel",
        "error",
        "--outDir",
        out,
      ],
      { cwd: join(__dirname, ".."), stdio: "pipe" },
    );
    return readFileSync(join(out, "runtime.css"), "utf8");
  } finally {
    rmSync(out, { recursive: true, force: true });
  }
}

const DOCUMENT_SELECTOR =
  /^(?::root|:host|\[data-(?:theme|context)=[^\]]+\]|\s|:not\(\[data-theme=[^\]]+\]\))+$/;

/* Not tokens: Tailwind's own variables and color aliases, which it keeps or
   drops by what the app happens to use, and the fixed brand palette, which
   does not follow the theme ("nothing in here is consumed directly by UI
   code", brand.css). */
const NOT_A_TOKEN =
  /^--(?:tw-|color-|green|gray|neutral-|white$|purple$|yellow$|lime$)/;

function documentTokens(css: string): string[] {
  const tokens = new Set<string>();
  for (const rule of css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    const selectors = rule[1]!.split(/,(?![^(]*\))/);
    if (!selectors.every((selector) => DOCUMENT_SELECTOR.test(selector.trim())))
      continue;
    for (const declaration of rule[2]!.matchAll(/(?:^|;)\s*(--[\w-]+)\s*:/g)) {
      if (!NOT_A_TOKEN.test(declaration[1]!)) tokens.add(declaration[1]!);
    }
  }
  return [...tokens].sort();
}

describe("the @gents/ux-sdk reference", () => {
  let tokens: string[];

  beforeAll(() => {
    tokens = documentTokens(builtFrameCss());
    if (UPDATE) {
      writeFileSync(TOKENS, `${tokens.join("\n")}\n`);
      writeFileSync(REFERENCE, renderSdkReference(tokens));
    }
  }, 120_000);

  it("documents exactly what the frame exports", () => {
    expect(Object.keys(SDK_ENTRIES).sort()).toEqual(Object.keys(sdk).sort());
  });

  it("gives every export its props or its call shape, and an example", () => {
    for (const [name, entry] of Object.entries(SDK_ENTRIES)) {
      expect(Boolean(entry.props ?? entry.signature), name).toBe(true);
      expect(entry.example, name).not.toBe("");
    }
  });

  it("names only tokens the frame defines", () => {
    expect(tokens).toContain("--bg");
    expect(tokens).toContain("--chart-8");
    expect(tokens).not.toContain("--background");
    expect(tokens).not.toContain("--foreground");
    for (const token of Object.keys(TOKEN_ROLES)) expect(tokens).toContain(token);
  });

  it("is what is checked in", () => {
    expect(readFileSync(TOKENS, "utf8"), `run ${REGENERATE}`).toBe(
      `${tokens.join("\n")}\n`,
    );
    expect(readFileSync(REFERENCE, "utf8"), `run ${REGENERATE}`).toBe(
      renderSdkReference(tokens),
    );
  });
});
