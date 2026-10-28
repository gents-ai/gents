import { expect, gotoHarness, test } from "./desktopTest";

test.beforeEach(async ({ page }) => {
  await page.setViewportSize({ width: 320, height: 568 });
  await gotoHarness(page, "empty-fleet");
});

test("setup actions do not overlap the theme control", async ({ page }) => {
  const next = page.getByTestId("setup-next");
  await next.scrollIntoViewIfNeeded();
  const action = await next.boundingBox();
  const theme = await page.getByRole("button", { name: "Toggle theme" }).boundingBox();
  expect(action).not.toBeNull();
  expect(theme).not.toBeNull();
  expect(theme!.y).toBeGreaterThanOrEqual(action!.y + action!.height);
  await next.click();
  await expect(
    page.getByRole("heading", { name: "Choose an inference provider" }),
  ).toBeVisible();
});

test("remote connection fields fit a narrow viewport with the keyboard open", async ({
  page,
}) => {
  await page.getByRole("radio", { name: /Remote connect/ }).click();
  const address = page.getByRole("textbox", { name: "Server address" });
  await address.fill("https://gents.example.net:8787");
  await page.evaluate(() => {
    Object.defineProperties(window.visualViewport, {
      height: { configurable: true, value: 260 },
      offsetTop: { configurable: true, value: 0 },
    });
    window.visualViewport!.dispatchEvent(new Event("resize"));
  });
  await expect
    .poll(() => page.getByTestId("setup-screen").evaluate((el) => el.clientHeight))
    .toBe(260);
  await address.scrollIntoViewIfNeeded();
  const bounds = await address.boundingBox();
  expect(bounds).not.toBeNull();
  expect(bounds!.x).toBeGreaterThanOrEqual(0);
  expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(320);
  const next = page.getByTestId("setup-next");
  await next.scrollIntoViewIfNeeded();
  await expect(next).toBeInViewport();
  await expect(next).toBeEnabled();
  expect(
    await page
      .getByTestId("setup-screen")
      .locator("[data-slot=scroll-area-viewport]")
      .evaluate((el) => el.scrollWidth - el.clientWidth),
  ).toBeLessThanOrEqual(1);
});
