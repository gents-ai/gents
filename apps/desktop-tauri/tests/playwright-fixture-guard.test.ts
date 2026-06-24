import { readdir, readFile } from "node:fs/promises";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

const PLAYWRIGHT_DIRS = [
  join(process.cwd(), "tests", "playwright"),
  join(process.cwd(), "tests", "playwright-live"),
  join(process.cwd(), "tests", "playwright-screenshots"),
  join(process.cwd(), "tests", "playwright-visual"),
];

describe("Playwright fixture guard", () => {
  it("routes browser specs through the shared desktop fixture", async () => {
    const files = (
      await Promise.all(
        PLAYWRIGHT_DIRS.map(async (directory) =>
          (await readdir(directory))
            .filter((file) => file.endsWith(".spec.ts"))
            .map((file) => ({ directory, file })),
        ),
      )
    )
      .flat()
      .sort((left, right) => left.file.localeCompare(right.file));
    const violations: string[] = [];

    for (const { directory, file } of files) {
      const source = await readFile(join(directory, file), "utf8");
      if (!importsDesktopFixture(source)) {
        violations.push(`${file}: missing import from shared desktopTest fixture`);
      }
      if (importsPlaywrightValues(source)) {
        violations.push(`${file}: import Playwright values from "./desktopTest"`);
      }
    }

    expect(violations).toEqual([]);
  });
});

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
