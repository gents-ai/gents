import { defineConfig, devices } from "@playwright/test";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";

const viteBin = join(
  dirname(createRequire(import.meta.url).resolve("vite/package.json")),
  "bin/vite.js",
);

const port = Number(process.env.DESKTOP_UI_SCREENSHOT_PORT ?? 1425);
const baseURL =
  process.env.DESKTOP_UI_SCREENSHOT_BASE_URL ?? `http://127.0.0.1:${port}`;

export default defineConfig({
  testDir: "./tests/playwright-screenshots",
  outputDir: "./test-results/playwright-screenshots",
  fullyParallel: true,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 1 : 0,
  workers: process.env.CI ? 1 : undefined,
  reporter: process.env.CI
    ? [
        ["list"],
        ["html", { outputFolder: "playwright-screenshots-report", open: "never" }],
      ]
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
    command: `node ${viteBin} --host 127.0.0.1 --port ${port} --strictPort --clearScreen false`,
    url: `${baseURL}/tests/ui-harness/harness.html`,
    reuseExistingServer: !process.env.CI,
    timeout: 30_000,
  },
  projects: [
    {
      name: "chromium-screenshots-desktop",
      use: {
        ...devices["Desktop Chrome"],
        viewport: { width: 1440, height: 900 },
      },
    },
    {
      name: "chromium-screenshots-laptop",
      use: {
        ...devices["Desktop Chrome"],
        viewport: { width: 1280, height: 800 },
      },
    },
    {
      name: "chromium-screenshots-narrow",
      use: {
        ...devices["Desktop Chrome"],
        viewport: { width: 390, height: 844 },
      },
    },
  ],
});
