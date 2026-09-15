import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";

const APP_CSS = join(__dirname, "..", "src", "App.css");
const PACKAGES_ROOT = join(__dirname, "..", "..", "..", "packages");

function cssFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((entry) => {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) return cssFiles(full);
    return entry.endsWith(".css") ? [full] : [];
  });
}

const packageStyleFiles = readdirSync(PACKAGES_ROOT)
  .filter((entry) => entry.startsWith("gents-desktop-"))
  .flatMap((entry) => cssFiles(join(PACKAGES_ROOT, entry)));

function stripComments(css: string): string {
  return css.replace(/\/\*[\s\S]*?\*\//g, "");
}

describe("kit entry", () => {
  it("App.css is the kit stylesheet, not a cascade-layer host", () => {
    const appCss = readFileSync(APP_CSS, "utf8");
    expect(appCss).toContain('@import "tailwindcss"');
    expect(appCss).toContain('@import "@gents/ui/styles.css"');
    expect(appCss).not.toMatch(/@layer\s+[\w\s,-]+;/);
  });
});

describe("package CSS", () => {
  it("never reaches into host-private brand tokens", () => {
    const violations = packageStyleFiles
      .filter((file) => stripComments(readFileSync(file, "utf8")).includes("--source-"))
      .map((file) => relative(PACKAGES_ROOT, file));
    expect(violations).toEqual([]);
  });
});
