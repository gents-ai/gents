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
async function captureScene(clicked, filename) {
  const page = await browser.newPage({
    viewport: { width: 1280, height: 900 },
    offline: true,
  });
  await page.clock.install({ time: new Date("2026-01-01T00:00:00Z") });
  await page.clock.pauseAt(new Date("2026-01-01T00:00:01Z"));
  await page.addInitScript(() => {
    let seed = 123456789;
    Math.random = () => {
      seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
      return seed / 4294967296;
    };
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
  await page.clock.runFor(500);
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
  if (clicked) await toggle.click({ timeout: 5000 });
  await page.mouse.move(0, 0);
  await page.evaluate(() => document.activeElement?.blur());
  await page.clock.runFor(500);
  const screenshot = await page.screenshot({
    ...capture,
    path: path.join(evidence, filename),
  });
  await page.close();
  return screenshot;
}

try {
  // Independent controls must match before a clicked/control difference counts.
  const baseline = await captureScene(false, "control.png");
  const day = await captureScene(false, "day.png");
  const night = await captureScene(true, "night.png");
  assert.deepEqual(errors, [], "page must run without JavaScript errors");
  assert.deepEqual(network, [], "page must not request network dependencies");
  inconclusive = !baseline.equals(day);
  assert(
    !inconclusive,
    "visible-change inconclusive: matched-time control renders differ; inspect retained screenshots",
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
        grader: "matched-clock-v2",
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
        grader: "matched-clock-v2",
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
