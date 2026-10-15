import { expect, test } from "./desktopTest";

test.describe("a large session index", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto(
      "/tests/ui-harness/harness.html?scenario=mobile-performance&sessions=10000",
    );
    await expect(
      page.getByRole("list", { name: "Sessions", exact: true }),
    ).toHaveAttribute("data-session-count", "10000");
  });

  test("mounts a window and reaches the last session without mounting the fleet", async ({
    page,
  }) => {
    const list = page.getByRole("list", { name: "Sessions", exact: true });
    await expect
      .poll(() => list.locator("a[data-testid^='session-']").count())
      .toBeGreaterThan(0);
    expect(await list.locator("a[data-testid^='session-']").count()).toBeLessThan(80);
    await page
      .getByTestId("sessions-screen")
      .locator('[data-slot="scroll-area-viewport"]')
      .evaluate((viewport) => {
        viewport.scrollTop = viewport.scrollHeight;
      });
    const last = page.getByTestId("session-session-index-9999");
    await expect(last).toBeInViewport();
    expect(await list.locator("a[data-testid^='session-']").count()).toBeLessThan(80);
    await last.click();
    await expect(page.getByTestId("session-screen")).toBeVisible();
  });

  test("keyboard navigation reaches both ends and crosses a window boundary", async ({
    page,
  }) => {
    const list = page.getByRole("list", { name: "Sessions", exact: true });
    const first = list.locator("a[data-testid^='session-']").first();
    await first.focus();
    await first.press("End");
    const last = page.getByTestId("session-session-index-9999");
    await expect(last).toBeFocused();
    await last.press("Home");
    await expect(page.getByTestId("session-session-intro")).toBeFocused();
    for (let i = 0; i < 45; i += 1) await page.keyboard.press("Tab");
    const index = await page.evaluate(() =>
      Number(
        document.activeElement?.closest("[data-index]")?.getAttribute("data-index"),
      ),
    );
    expect(index).toBe(45);
    expect(await list.locator("a[data-testid^='session-']").count()).toBeLessThan(80);
  });

  test("search finds an unmounted session after scrolling to the end", async ({
    page,
  }) => {
    await page
      .getByTestId("sessions-screen")
      .locator('[data-slot="scroll-area-viewport"]')
      .evaluate((viewport) => {
        viewport.scrollTop = viewport.scrollHeight;
      });
    await page.getByRole("button", { name: "Search sessions", exact: true }).click();
    await page
      .getByRole("textbox", { name: "Search sessions", exact: true })
      .fill("Comparable session 5000");
    await expect(page.getByTestId("session-session-index-5000")).toBeVisible();
    await expect(
      page.getByRole("list", { name: "Sessions", exact: true }),
    ).toHaveAttribute("data-session-count", "1");
  });
});
