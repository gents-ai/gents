import { writeFile } from "node:fs/promises";

import {
  captureStableScreenshot,
  expect,
  gotoHarness,
  openChat,
  openConfig,
  test,
  type TestInfo,
} from "../playwright/desktopTest";

type ScreenshotReviewEntry = {
  state: string;
  scenario: string;
  attachmentName: string;
};

test.describe("desktop stable screenshot states", () => {
  test("captures core kit shell states", async ({ page }, testInfo) => {
    const screenshots: ScreenshotReviewEntry[] = [];
    const captureReviewScreenshot = async (
      state: string,
      scenario: string,
      name: string,
    ) => {
      const capture = await captureStableScreenshot(page, testInfo, name);
      screenshots.push({
        state,
        scenario,
        attachmentName: capture.attachmentName,
      });
    };

    await gotoHarness(page);
    await expect(page.getByTestId("sessions-screen")).toBeVisible();
    await captureReviewScreenshot("sessions", "default", "stable-sessions");

    await openChat(page);
    await expect(page.getByTestId("session-screen")).toBeVisible();
    await captureReviewScreenshot("new session", "default", "stable-new-session");

    await gotoHarness(page);
    await openConfig(page);
    await expect(page.getByTestId("agent-screen")).toBeVisible();
    await captureReviewScreenshot(
      "agent configuration",
      "default",
      "stable-agent-config",
    );

    await gotoHarness(page, "empty-fleet");
    await expect(page.getByTestId("setup-screen")).toBeVisible();
    await captureReviewScreenshot("first-run setup", "empty-fleet", "stable-setup");

    const path = testInfo.outputPath("desktop-screenshot-review.md");
    await writeFile(
      path,
      screenshots
        .map((entry) => `- ${entry.state} (${entry.scenario}): ${entry.attachmentName}`)
        .join("\n") + "\n",
    );
    await testInfo.attach("desktop-screenshot-review.md", {
      path,
      contentType: "text/markdown",
    });
  });
});
