import { expect, gotoHarness, test } from "./desktopTest";

test.describe("moving between turns", () => {
  test.beforeEach(async ({ page }, testInfo) => {
    test.skip(
      !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
      "one layout per engine",
    );
    await page.setViewportSize({ width: 1280, height: 900 });
    await gotoHarness(page, "mobile-performance");
    await page.locator('[data-testid="session-session-large"]').click();
    await page
      .getByTestId("transcript-panel")
      .getByText("stream-start", { exact: false })
      .last()
      .waitFor();
  });

  const current = (page: import("@playwright/test").Page) =>
    page.evaluate(() => {
      const scroller = document.querySelector<HTMLElement>(
        '[data-slot="scroll-area-viewport"][data-following]',
      )!;
      const top = scroller.getBoundingClientRect().top;
      const turns = Array.from(scroller.querySelectorAll<HTMLElement>("[data-turn]"));
      const at = turns
        .filter((row) => row.getBoundingClientRect().top <= top + 72.5)
        .at(-1);
      return at
        ? {
            key: at.dataset.timelineKey!,
            offset: Math.round(at.getBoundingClientRect().top - top),
            following: scroller.dataset.following,
          }
        : null;
    });

  test("brings the previous turn to the reading line, then the next", async ({
    page,
  }) => {
    const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
    await expect(viewport).toHaveAttribute("data-following", "true");
    const start = await current(page);
    await page.getByTestId("transcript-panel").hover();
    await page.keyboard.press("Alt+Meta+ArrowUp");
    await expect.poll(() => current(page).then((c) => c?.following)).toBe("false");
    const up = (await current(page))!;
    expect(up.key).not.toBe(start?.key);

    expect(Math.abs(up.offset - 56)).toBeLessThanOrEqual(1);

    await page.keyboard.press("Alt+Meta+ArrowUp");
    await expect.poll(() => current(page).then((c) => c?.key)).not.toBe(up.key);
    const upAgain = (await current(page))!;
    expect(Math.abs(upAgain.offset - 56)).toBeLessThanOrEqual(1);

    await page.keyboard.press("Alt+Meta+ArrowDown");
    await expect.poll(() => current(page).then((c) => c?.key)).toBe(up.key);
    expect(Math.abs((await current(page))!.offset - 56)).toBeLessThanOrEqual(1);
  });
});
