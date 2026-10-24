import { fileURLToPath } from "node:url";
import { expect, gotoHarness, openChat, test } from "../playwright/desktopTest";

test("attachment tray follows the composer in light and dark themes", async ({
  page,
}) => {
  await gotoHarness(page);
  await openChat(page);
  await page
    .getByLabel("Choose attachments")
    .setInputFiles(
      fileURLToPath(new URL("../../public/avatars/1.png", import.meta.url)),
    );
  const tray = page.getByTestId("attachment-tray");
  await expect(tray).toBeVisible();
  await expect(page.getByRole("button", { name: "Send", exact: true })).toBeEnabled();
  await expect(page.getByTestId("composer")).toHaveScreenshot("attachments-light.png", {
    animations: "disabled",
    maxDiffPixels: 0,
  });
  await page.evaluate(() => {
    document.documentElement.dataset.theme = "dark";
  });
  await expect(page.getByTestId("composer")).toHaveScreenshot("attachments-dark.png", {
    animations: "disabled",
    maxDiffPixels: 0,
  });
});
