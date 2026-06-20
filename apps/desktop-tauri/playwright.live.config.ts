import { defineConfig, devices } from "@playwright/test";

const port = Number(process.env.DESKTOP_UI_LIVE_E2E_PORT ?? 1424);
const baseURL = process.env.DESKTOP_UI_LIVE_E2E_BASE_URL ?? `http://127.0.0.1:${port}`;

export default defineConfig({
  testDir: "./tests/playwright-live",
  outputDir: "./test-results/playwright-live",
  fullyParallel: false,
  forbidOnly: Boolean(process.env.CI),
  retries: 0,
  workers: 1,
  reporter: process.env.CI
    ? [["list"], ["html", { outputFolder: "playwright-live-report", open: "never" }]]
    : "list",
  expect: {
    timeout: 30_000,
  },
  use: {
    baseURL,
    actionTimeout: 30_000,
    navigationTimeout: 30_000,
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
    video: "retain-on-failure",
  },
  webServer: {
    command: `node ./node_modules/vite/bin/vite.js --host 127.0.0.1 --port ${port} --strictPort --clearScreen false`,
    url: `${baseURL}/tests/ui-harness/harness.html`,
    reuseExistingServer: !process.env.CI,
    timeout: 30_000,
  },
  projects: [
    {
      name: "chromium-live-desktop",
      use: {
        ...devices["Desktop Chrome"],
        viewport: { width: 1440, height: 900 },
      },
    },
  ],
});
