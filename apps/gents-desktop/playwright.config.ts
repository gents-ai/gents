import { defineConfig, devices } from "@playwright/test";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";

import { OWN_VIEWPORT } from "./tests/playwright/ownViewport";

const viteBin = join(
  dirname(createRequire(import.meta.url).resolve("vite/package.json")),
  "bin/vite.js",
);

const port = Number(process.env.DESKTOP_UI_E2E_PORT ?? 1421);
/* CI splits the journeys across runners: "1/3" runs the first third */
const shard = /^(\d+)\/(\d+)$/.exec(process.env.DESKTOP_UI_E2E_SHARD ?? "");
const baseURL = process.env.DESKTOP_UI_E2E_BASE_URL ?? `http://127.0.0.1:${port}`;

export default defineConfig({
  testDir: "./tests/playwright",
  outputDir: "./test-results/playwright",
  fullyParallel: true,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 1 : 0,
  /* the journeys are CPU-bound: on a four-core hosted runner two workers
     took 24-26 min against 52 at one, and four workers were no faster */
  workers: process.env.CI ? 2 : undefined,
  shard: shard ? { current: Number(shard[1]), total: Number(shard[2]) } : null,
  reporter: process.env.CI
    ? [["list"], ["html", { outputFolder: "playwright-report", open: "never" }]]
    : "list",
  expect: {
    timeout: 10_000,
  },
  use: {
    baseURL,
    actionTimeout: 10_000,
    navigationTimeout: 30_000,
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
    video: "retain-on-failure",
  },
  webServer: {
    /* the plugin frames inline a prebuilt runtime; build it before serving */
    command: `node ${viteBin} build -c vite.sandbox.config.ts --logLevel warn && node ${viteBin} --host 127.0.0.1 --port ${port} --strictPort --clearScreen false --mode browser-test`,
    url: `${baseURL}/tests/ui-harness/harness.html`,
    reuseExistingServer: false,
    timeout: 30_000,
  },
  projects: [
    {
      name: "chromium-desktop",
      use: {
        ...devices["Desktop Chrome"],
        viewport: { width: 1440, height: 900 },
      },
    },
    {
      name: "chromium-laptop",
      /* a test that sets its own window size is the same test here as on
         chromium-desktop (tests/playwright/ownViewport.ts); pure form/logic
         journeys already run at both desktop and narrow widths */
      grepInvert: new RegExp(OWN_VIEWPORT),
      testIgnore:
        /(?:^|[\\/])(?:desktop-first-run|chat-delivery|goal-delivery|mailbox-delivery|active-node|remote-config-readonly)\.spec\.ts$/,
      use: {
        ...devices["Desktop Chrome"],
        viewport: { width: 1280, height: 800 },
      },
    },
    {
      /* the desktop app draws in WebKit, which has no scroll anchoring and
         handles wheels its own way */
      name: "webkit-desktop",
      /* matched against the whole path: anchored to the file name, so a
         checkout folder named transcript-… does not match every spec */
      testMatch:
        /(?:^|[\\/])(?:transcript-[^\\/]*|turn-changes|editor-sheet-wheel|sessions-window|ux-sandbox|attachments|composer-chips|composer-history|mobile-interactions|onboarding-layout|mailbox-stack)\.spec\.ts$/,
      /* one WebKit page at a time. Playwright's Linux WebKit (WPE) crashes in
         its compositor thread (a segfault in libWPEWebKit, the page closes)
         when two WebKit processes share four cores: turn-changes.spec.ts:180
         alone crashed in 11 of 150 runs that way and 0 of 90 with one. Its
         wheel- and frame-paced journeys also slow past their budget when a
         second WebKit draws beside them. A Chromium worker still runs
         alongside it */
      workers: 1,
      use: {
        ...devices["Desktop Safari"],
        viewport: { width: 1280, height: 900 },
      },
    },
    {
      name: "chromium-narrow",
      grepInvert: new RegExp(OWN_VIEWPORT),
      use: {
        ...devices["Desktop Chrome"],
        viewport: { width: 390, height: 844 },
      },
    },
  ],
});
