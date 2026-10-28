import { expect, gotoHarness, test } from "./desktopTest";

test("a touch fling keeps the reader's place after the finger lifts", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one narrow layout per engine",
  );
  await page.setViewportSize({ width: 390, height: 844 });
  await gotoHarness(page, "mobile-performance");
  await page.getByTestId("session-session-large").click();
  await page.getByTestId("transcript-panel").getByText("stream-start").last().waitFor();
  const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
  await expect(viewport).not.toHaveAttribute("data-landing", "");

  const result = await viewport.evaluate(async (scroller) => {
    const touch = (type: string, y: number) =>
      scroller.dispatchEvent(
        Object.assign(new Event(type), { touches: [{ clientY: y }] }),
      );
    touch("touchstart", 200);
    touch("touchmove", 240);
    touch("touchend", 240);
    const positions: { expected: number; actual: number }[] = [];
    // Native inertia keeps producing scrolls after the last touch event.
    for (let step = 0; step < 12; step += 1) {
      const expected = scroller.scrollTop - 24;
      scroller.scrollTop = expected;
      scroller.dispatchEvent(new Event("scroll"));
      await new Promise((resolve) => setTimeout(resolve, 60));
      positions.push({ expected, actual: scroller.scrollTop });
    }
    const viewTop = scroller.getBoundingClientRect().top;
    const reader = [...scroller.querySelectorAll<HTMLElement>("[data-row-key]")].find(
      (row) => row.getBoundingClientRect().bottom > viewTop + 72,
    )!;
    const beforeRest = reader.getBoundingClientRect().top - viewTop;
    await new Promise((resolve) => setTimeout(resolve, 500));
    return {
      positions,
      beforeRest,
      afterRest: reader.getBoundingClientRect().top - viewTop,
    };
  });

  for (const { expected, actual } of result.positions)
    expect(actual - expected, JSON.stringify(result)).toBeLessThanOrEqual(1);
  expect(Math.abs(result.afterRest - result.beforeRest)).toBeLessThanOrEqual(1);
  await expect(viewport).toHaveAttribute("data-following", "false");
});
