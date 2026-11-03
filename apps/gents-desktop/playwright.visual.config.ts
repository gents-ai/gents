import { defineConfig, devices } from "@playwright/test";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";

const viteBin = join(
  dirname(createRequire(import.meta.url).resolve("vite/package.json")),
  "bin/vite.js",
);

/* specs whose point is a 2× display; the 1× projects skip them */
const RETINA_ONLY = /(?:^|[\\/])avatar-zoom-visual\.spec\.ts$/;

const port = Number(process.env.DESKTOP_UI_VISUAL_PORT ?? 1423);
const baseURL = process.env.DESKTOP_UI_VISUAL_BASE_URL ?? `http://127.0.0.1:${port}`;

export default defineConfig({
  testDir: "./tests/playwright-visual",
  outputDir: "./test-results/playwright-visual",
  fullyParallel: false,
  forbidOnly: Boolean(process.env.CI),
  retries: 0,
  workers: 1,
  reporter: process.env.CI
    ? [["list"], ["html", { outputFolder: "playwright-visual-report", open: "never" }]]
    : "list",
  expect: {
    timeout: 10_000,
    toHaveScreenshot: {
      maxDiffPixels: 64,
    },
  },
  use: {
    baseURL,
    actionTimeout: 10_000,
    navigationTimeout: 30_000,
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
  },
  webServer: {
    command: `node ${viteBin} --host 127.0.0.1 --port ${port} --strictPort --clearScreen false`,
    url: `${baseURL}/tests/ui-harness/harness.html`,
    reuseExistingServer: !process.env.CI,
    timeout: 30_000,
  },
  projects: [
    {
      name: "webkit-visual-retina",
      testMatch: RETINA_ONLY,
      use: {
        ...devices["Desktop Safari"],
        viewport: { width: 1280, height: 900 },
        deviceScaleFactor: 2,
      },
    },
    {
      name: "chromium-visual-desktop",
      testIgnore: RETINA_ONLY,
      use: {
        ...devices["Desktop Chrome"],
        viewport: { width: 1440, height: 900 },
      },
    },
    {
      name: "chromium-visual-laptop",
      testIgnore: RETINA_ONLY,
      use: {
        ...devices["Desktop Chrome"],
        viewport: { width: 1280, height: 800 },
      },
    },
    {
      name: "chromium-visual-narrow",
      testIgnore: RETINA_ONLY,
      use: {
        ...devices["Desktop Chrome"],
        viewport: { width: 390, height: 844 },
      },
    },
  ],
});
