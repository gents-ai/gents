import { expect, gotoHarness, PEER_ID, test } from "./desktopTest";

test.describe("fleet deployment navigation", () => {
  test("server status authors a pending authenticated enrollment", async ({ page }) => {
    await gotoHarness(page, "default");
    await page.getByRole("button", { name: "Add Agent", exact: true }).click();

    await page.getByTestId("fleet-add-server-address").fill("http://amy:9191");
    await page.getByTestId("fleet-fetch-status").click();

    await expect(page.getByTestId("fleet-enrollment-pending")).toContainText(
      "Waiting for Bombadil UI Agent approval",
    );
    await expect(page.getByTestId("fleet-enrollment-pending")).toContainText(
      "enrollment-request-harness",
    );
    await expect(page.getByTestId("fleet-add-server-address")).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: "Enrollment requested" }),
    ).toBeDisabled();
    await expect(page.getByTestId(`fleet-row-${PEER_ID}`)).toHaveCount(1);
  });

  test("phone fleet cards expose saved-label rename", async ({ page }) => {
    test.skip(
      (page.viewportSize()?.width ?? Number.POSITIVE_INFINITY) > 760,
      "mobile rename affordance",
    );
    await gotoHarness(page, "default");

    const rename = page.getByTestId(`fleet-rename-${PEER_ID}`);
    await expect(rename).toBeVisible();
    await expect(rename).toHaveAccessibleName("Rename Bombadil UI Agent");
    const rowBounds = await page.getByTestId(`fleet-row-${PEER_ID}`).boundingBox();
    const renameBounds = await rename.boundingBox();
    expect(rowBounds).not.toBeNull();
    expect(renameBounds).not.toBeNull();
    expect(renameBounds!.x).toBeGreaterThan(rowBounds!.x + rowBounds!.width / 2);
    await rename.click();
    const input = page.getByTestId(`fleet-rename-input-${PEER_ID}`);
    await input.fill("Amy");
    await input.press("Enter");

    await expect(page.getByTestId(`fleet-detail-name-${PEER_ID}`)).toHaveText("Amy");
  });

  test("deployment row opens the chat workspace", async ({ page }) => {
    await gotoHarness(page, "default");
    await expect(page.getByTestId("fleet-dashboard")).toBeVisible();

    await page.getByTestId(`fleet-row-${PEER_ID}`).click();
    if ((page.viewportSize()?.width ?? Number.POSITIVE_INFINITY) <= 760) {
      await page.getByTestId("conversation-session-intro").click();
    }

    await expect(page.getByTestId("composer-input")).toBeVisible();
  });

  test("deployment workspace opens config", async ({ page }) => {
    await gotoHarness(page, "default");
    await expect(page.getByTestId("fleet-dashboard")).toBeVisible();

    await page.getByTestId(`fleet-row-${PEER_ID}`).click();
    await page.getByTestId("agent-actions").click();
    await page.getByRole("button", { name: "Configure" }).click();

    await expect(page.locator(".config-workspace")).toBeVisible();
  });

  test("P2P repair is fleet-level and hidden while healthy, never a row action", async ({
    page,
  }) => {
    await gotoHarness(page, "default");
    await expect(page.getByTestId("fleet-dashboard")).toBeVisible();

    await expect(page.getByTestId(`fleet-repair-${PEER_ID}`)).toHaveCount(0);
    await expect(page.getByTestId("fleet-repair-p2p")).toHaveCount(0);
  });
});
