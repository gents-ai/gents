import { writeFile } from "node:fs/promises";

import type { TestInfo } from "@playwright/test";

import {
  expect,
  gotoHarness,
  openChat,
  openConfig,
  test,
} from "../playwright/desktopTest";

type VisualReviewEntry = {
  state: string;
  scenario: string;
  snapshotName: string;
};

test.describe("desktop visual baselines", () => {
  test("matches stable shell states", async ({ page }, testInfo) => {
    const snapshots: VisualReviewEntry[] = [];

    await gotoHarness(page);
    await expect(page.getByTestId("fleet-dashboard")).toBeVisible();
    await expect(page).toHaveScreenshot("fleet-dashboard.png", {
      animations: "disabled",
      fullPage: true,
    });
    snapshots.push({
      state: "fleet dashboard",
      scenario: "default",
      snapshotName: "fleet-dashboard.png",
    });

    await openChat(page);
    await expect(page.getByTestId("transcript-panel")).toBeVisible();
    await expect(page).toHaveScreenshot("chat-transcript.png", {
      animations: "disabled",
      fullPage: true,
    });
    snapshots.push({
      state: "chat transcript",
      scenario: "default",
      snapshotName: "chat-transcript.png",
    });

    await page.getByRole("button", { name: /open operations drawer/i }).click();
    await expect(page.getByRole("complementary", { name: "Operations" })).toBeVisible();
    await expect(page).toHaveScreenshot("operations-drawer.png", {
      animations: "disabled",
      fullPage: true,
    });
    snapshots.push({
      state: "operations drawer",
      scenario: "default",
      snapshotName: "operations-drawer.png",
    });

    await gotoHarness(page);
    await openConfig(page);
    await expect(page.locator(".config-workspace")).toBeVisible();
    await expect(page).toHaveScreenshot("config-workspace.png", {
      animations: "disabled",
      fullPage: true,
    });
    snapshots.push({
      state: "config workspace",
      scenario: "default",
      snapshotName: "config-workspace.png",
    });

    await gotoHarness(page, "empty-fleet");
    await expect(page.getByTestId("fleet-empty")).toBeVisible();
    await expect(page).toHaveScreenshot("empty-fleet.png", {
      animations: "disabled",
      fullPage: true,
    });
    snapshots.push({
      state: "empty fleet",
      scenario: "empty-fleet",
      snapshotName: "empty-fleet.png",
    });

    await gotoHarness(page, "bridge-unavailable");
    await expect(page.getByTestId("error-banner")).toBeVisible();
    await expect(page).toHaveScreenshot("bridge-error.png", {
      animations: "disabled",
      fullPage: true,
    });
    snapshots.push({
      state: "bridge error",
      scenario: "bridge-unavailable",
      snapshotName: "bridge-error.png",
    });

    await attachVisualReviewManifest(testInfo, snapshots);
  });
});

async function attachVisualReviewManifest(
  testInfo: TestInfo,
  snapshots: VisualReviewEntry[],
) {
  const rows = snapshots
    .map(
      (snapshot) =>
        `| ${snapshot.state} | \`${snapshot.scenario}\` | \`${snapshot.snapshotName}\` |`,
    )
    .join("\n");
  const body = [
    "# Desktop Visual Baseline Review",
    "",
    `Project: \`${testInfo.project.name}\``,
    "Command: `npm run test:ui:visual`",
    "",
    "These are golden snapshot assertions for stable desktop shell states.",
    "",
    "| State | Harness scenario | Snapshot |",
    "| --- | --- | --- |",
    rows,
    "",
    "When a visual diff fails, inspect the Playwright visual report and decide",
    "whether the changed pixels are an intended UI update or a confirmed defect.",
    "File confirmed defects with labels `bug` and `ui`.",
    "",
  ].join("\n");
  const path = testInfo.outputPath("desktop-visual-review.md");
  await writeFile(path, body);

  await testInfo.attach("desktop-visual-review.md", {
    path,
    contentType: "text/markdown",
  });
}
