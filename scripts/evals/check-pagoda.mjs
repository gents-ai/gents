import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { chromium } from "@playwright/test";

// Evaluator-owned checks. Never load tests or executable code from the model's
// workspace into Node; the submitted page runs only in the browser context.
const [project, evidence] = process.argv.slice(2);
assert(project && evidence, "usage: check-pagoda.mjs PROJECT EVIDENCE");
await mkdir(evidence, { recursive: true });
const browser = await chromium.launch({ channel: "chrome", headless: true });
const errors = [];
const network = [];
let inconclusive = false;
try {
  const page = await browser.newPage({
    viewport: { width: 1280, height: 900 },
    offline: true,
  });
  page.on("pageerror", (error) => errors.push(error.message));
  await page.route("**/*", (route) => {
    const url = route.request().url();
    if (/^(data|blob):/.test(url)) return route.continue();
    if (url.startsWith("file:")) {
      const relative = path.relative(path.resolve(project), fileURLToPath(url));
      if (!relative.startsWith("..") && !path.isAbsolute(relative))
        return route.continue();
    }
    network.push(url);
    return route.abort();
  });
  await page.goto(pathToFileURL(path.join(project, "index.html")).href);
  await page.waitForTimeout(500);
  assert((await page.title()).trim(), "page must have a document title");
  const exactToggle = page.getByRole("button", {
    name: "Toggle night",
    exact: true,
  });
  // A descriptive accessible name may extend the exact visible label.
  // Do not admit differently labelled controls or lose the uniqueness check.
  const labelledToggle = page
    .getByRole("button", { name: /^Toggle night\b/ })
    .filter({ hasText: /^\s*Toggle\s+night\s*$/ });
  const toggle = exactToggle.or(labelledToggle);
  assert.equal(
    await toggle.count(),
    1,
    "one accessible Toggle night button is required",
  );
  assert(await toggle.isVisible(), "toggle must be visible");
  await page.mouse.move(0, 0);
  await page.evaluate(() => document.activeElement?.blur());
  const capture = { mask: [page.getByRole("button")], animations: "disabled" };
  const baseline = await page.screenshot(capture);
  await page.waitForTimeout(500);
  const day = await page.screenshot({
    ...capture,
    path: path.join(evidence, "day.png"),
  });
  await toggle.click();
  await page.mouse.move(0, 0);
  await page.evaluate(() => document.activeElement?.blur());
  await page.waitForTimeout(500);
  const night = await page.screenshot({
    ...capture,
    path: path.join(evidence, "night.png"),
  });
  assert.deepEqual(errors, [], "page must run without JavaScript errors");
  assert.deepEqual(network, [], "page must not request network dependencies");
  inconclusive = !baseline.equals(day);
  assert(
    !inconclusive,
    "visible-change inconclusive: scene animates before clicking; inspect retained screenshots",
  );
  assert(
    !day.equals(night),
    "toggle must produce a visible change outside the button",
  );
  await writeFile(
    path.join(evidence, "browser.json"),
    JSON.stringify(
      {
        passed: true,
        errors,
        network,
        checks: [
          "local-load",
          "document-title",
          "accessible-toggle",
          "visible-change",
          "no-js-errors",
          "no-network",
        ],
        limitation:
          "Screenshots are evidence, not an automated judgment of pagoda/garden aesthetics.",
      },
      null,
      2,
    ),
  );
} catch (error) {
  await writeFile(
    path.join(evidence, "browser.json"),
    JSON.stringify(
      {
        passed: false,
        inconclusive,
        error: String(error),
        errors,
        network,
      },
      null,
      2,
    ),
  );
  throw error;
} finally {
  await browser.close();
}
