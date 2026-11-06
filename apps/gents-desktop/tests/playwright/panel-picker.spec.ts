import { expect, gotoHarness, test } from "./desktopTest";

test("the panel picker finds a panel by name or number, and closes with Esc", async ({
  page,
}, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "one layout is enough");
  await page.setViewportSize({ width: 1500, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  await page.getByTestId("transcript-panel").getByText("stream-start").last().waitFor();

  /* its own key is the desktop app's; search reaches it in a browser */
  const openPicker = async () => {
    await page.keyboard.press("ControlOrMeta+k");
    await page.getByRole("combobox", { name: "Search" }).fill("choose a panel");
    await page.getByRole("option", { name: /Choose a panel/ }).click();
    await expect(page.getByTestId("panel-picker")).toBeVisible();
  };

  await openPicker();
  const find = page.getByRole("combobox", { name: "Find a panel" });
  await expect(find).toBeFocused();
  await find.fill("back");
  await expect(page.getByTestId("panel-picker").getByRole("option")).toHaveCount(1);
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("panel-picker")).toBeHidden();
  await expect(page.getByRole("tab", { name: /Background/ })).toHaveAttribute(
    "aria-selected",
    "true",
  );

  /* the open tab is first, so Changes, the last of three, is 3 */
  await openPicker();
  await expect(
    page.getByTestId("panel-picker").getByRole("option").first(),
  ).toContainText("Background");
  await page.keyboard.press("3");
  await expect(page.getByRole("tab", { name: /Changes/ })).toHaveAttribute(
    "aria-selected",
    "true",
  );

  await openPicker();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("panel-picker")).toBeHidden();
});
