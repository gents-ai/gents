import { composer, expect, gotoHarness, test } from "./desktopTest";

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

  test("keyboard navigation reaches both ends", async ({ page }) => {
    const list = page.getByRole("list", { name: "Sessions", exact: true });
    const first = list.locator("a[data-testid^='session-']").first();
    await first.focus();
    await first.press("End");
    const last = page.getByTestId("session-session-index-9999");
    await expect(last).toBeFocused();
    await last.press("Home");
    await expect(page.getByTestId("session-session-intro")).toBeFocused();
    expect(await list.locator("a[data-testid^='session-']").count()).toBeLessThan(80);
  });

  test("Tab crosses a window boundary and returns to the previous row", async ({
    page,
  }) => {
    const list = page.getByRole("list", { name: "Sessions", exact: true });
    const edge = list.locator("li[data-index]").last();
    await expect(edge).toBeAttached();
    const index = Number(await edge.getAttribute("data-index"));
    /* a row's controls: its link, then its actions; Tab leaves a row from the
       last of them and enters the next at the first */
    const controls = "a[href], button:not([disabled])";
    const previous = list.locator(`li[data-index="${index}"]`).locator(controls).last();
    const next = list
      .locator(`li[data-index="${index + 1}"]`)
      .locator(controls)
      .first();
    await expect(list.locator(`li[data-index="${index + 1}"]`)).toHaveCount(0);
    await previous.evaluate((control: HTMLElement) => {
      control.focus({ preventScroll: true });
    });
    await expect(previous).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(next).toBeFocused();
    await expect(next).toBeInViewport();
    await page.keyboard.press("Shift+Tab");
    await expect(previous).toBeFocused();
    await expect(previous).toBeInViewport();
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

test("configuration returns to a responsive new session", async ({ page }) => {
  test.skip(test.info().project.name === "chromium-narrow", "Desktop rail navigation");
  await gotoHarness(page);
  for (let pass = 0; pass < 5; pass++) {
    await page.getByRole("link", { name: "Active node", exact: true }).click();
    await expect(page.getByTestId("agent-screen")).toBeVisible();
    await page.getByRole("link", { name: "New session", exact: true }).first().click();
    await expect(page.getByTestId("session-screen")).toBeVisible();
    await composer(page).click();
    await composer(page).fill(`Navigation ${pass}`);
    await expect(composer(page)).toHaveText(`Navigation ${pass}`);
  }
});
