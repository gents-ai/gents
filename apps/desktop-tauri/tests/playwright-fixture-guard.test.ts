import { readdir, readFile } from "node:fs/promises";
import { join, relative } from "node:path";

import { describe, expect, it } from "vitest";

const PLAYWRIGHT_DIRS = [
  join(process.cwd(), "tests", "playwright"),
  join(process.cwd(), "tests", "playwright-live"),
  join(process.cwd(), "tests", "playwright-screenshots"),
  join(process.cwd(), "tests", "playwright-visual"),
];

describe("Playwright fixture guard", () => {
  it("routes browser specs through the shared desktop fixture", async () => {
    const files = (await Promise.all(PLAYWRIGHT_DIRS.map(listSpecFiles)))
      .flat()
      .sort((left, right) => left.path.localeCompare(right.path));
    const violations: string[] = [];

    for (const file of files) {
      const source = await readFile(file.absolutePath, "utf8");
      if (!importsDesktopFixture(source)) {
        violations.push(`${file.path}: missing import from shared desktopTest fixture`);
      }
      if (importsPlaywrightValues(source)) {
        violations.push(`${file.path}: import Playwright values from "./desktopTest"`);
      }
    }

    expect(violations).toEqual([]);
  });
});

type SpecFile = {
  absolutePath: string;
  path: string;
};

async function listSpecFiles(directory: string): Promise<SpecFile[]> {
  const entries = await readdir(directory, { withFileTypes: true });
  const files: SpecFile[] = [];

  for (const entry of entries) {
    const absolutePath = join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await listSpecFiles(absolutePath)));
      continue;
    }
    if (entry.isFile() && entry.name.endsWith(".spec.ts")) {
      files.push({
        absolutePath,
        path: relative(process.cwd(), absolutePath),
      });
    }
  }

  return files;
}

function importsDesktopFixture(source: string) {
  return /from\s+["'](?:\.\/desktopTest|\.\.\/playwright\/desktopTest)["']/.test(
    source,
  );
}

function importsPlaywrightValues(source: string) {
  return /(?:^|\n)\s*import\s+(?!type\b)[\s\S]*?from\s+["']@playwright\/test["']/.test(
    source,
  );
}
