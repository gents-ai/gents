import { writeFile } from "node:fs/promises";

import {
  expect,
  gotoHarness,
  openChat,
  openConfig,
  test,
  type TestInfo,
} from "../playwright/desktopTest";

type VisualReviewEntry = {
  state: string;
  scenario: string;
  snapshotName: string;
};

test.describe("desktop visual baselines", () => {
  test("matches stable kit shell states", async ({ page }, testInfo) => {
    const snapshots: VisualReviewEntry[] = [];

    await gotoHarness(page);
    await expect(page.getByTestId("sessions-screen")).toBeVisible();
    await expect(page).toHaveScreenshot("sessions.png", {
      animations: "disabled",
      fullPage: true,
    });
    snapshots.push({
      state: "sessions",
      scenario: "default",
      snapshotName: "sessions.png",
    });

    await openChat(page);
    await expect(page.getByTestId("session-screen")).toBeVisible();
    await expect(page).toHaveScreenshot("new-session.png", {
      animations: "disabled",
      fullPage: true,
    });
    snapshots.push({
      state: "new session",
      scenario: "default",
      snapshotName: "new-session.png",
    });

    await gotoHarness(page);
    await openConfig(page);
    await expect(page.getByTestId("agent-screen")).toBeVisible();
    await expect(page).toHaveScreenshot("agent-config.png", {
      animations: "disabled",
      fullPage: true,
    });
    snapshots.push({
      state: "agent configuration",
      scenario: "default",
      snapshotName: "agent-config.png",
    });

    await gotoHarness(page, "empty-fleet");
    await expect(page.getByTestId("setup-screen")).toBeVisible();
    await expect(page).toHaveScreenshot("setup.png", {
      animations: "disabled",
      fullPage: true,
    });
    snapshots.push({
      state: "first-run setup",
      scenario: "empty-fleet",
      snapshotName: "setup.png",
    });

    await gotoHarness(page, "bridge-unavailable");
    await expect(page.getByTestId("startup-screen")).toBeVisible();
    await expect(page).toHaveScreenshot("bridge-error.png", {
      animations: "disabled",
      fullPage: true,
    });
    snapshots.push({
      state: "startup error",
      scenario: "bridge-unavailable",
      snapshotName: "bridge-error.png",
    });

    await attachReview(testInfo, snapshots);
  });
});

async function attachReview(testInfo: TestInfo, snapshots: VisualReviewEntry[]) {
  const body = snapshots
    .map((entry) => `- ${entry.state} (${entry.scenario}): ${entry.snapshotName}`)
    .join("\n");
  const path = testInfo.outputPath("desktop-visual-review.md");
  await writeFile(path, `# Kit visual baselines\n\n${body}\n`);
  await testInfo.attach("desktop-visual-review.md", {
    path,
    contentType: "text/markdown",
  });
}
