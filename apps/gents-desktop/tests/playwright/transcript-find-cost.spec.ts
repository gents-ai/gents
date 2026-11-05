import { expect, gotoHarness, test } from "./desktopTest";

/* what one step to the next match costs the page: the work the key does
   at once (render, the list placing the row, opening what holds it), and
   the time until the frame after it, when the marks are drawn */
test("stepping between matches costs about a frame", async ({ page }, testInfo) => {
  /* a budget in milliseconds fails on a busy or slower machine, as CI's
     shared runners are; npm run perf:find runs it on its own */
  test.skip(!process.env.FIND_PERF, "timings run with npm run perf:find");
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
  await page.keyboard.press("ControlOrMeta+f");
  const field = page.getByRole("textbox", { name: "Find in session" });
  await field.fill("fixture row");
  await expect(page.getByTestId("find-bar").getByRole("status")).toHaveText(
    /^\d+ of \d+/,
  );

  const times = await page.evaluate(async () => {
    const input = document.querySelector<HTMLInputElement>(
      '[data-testid="find-bar"] input',
    )!;
    const frame = () => new Promise((done) => requestAnimationFrame(() => done(null)));
    const work: number[] = [];
    const toFrame: number[] = [];
    for (let i = 0; i < 40; i += 1) {
      await frame();
      const start = performance.now();
      input.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }),
      );
      /* React renders, commits and runs its effects before the next task */
      await new Promise((done) => setTimeout(done, 0));
      work.push(performance.now() - start);
      await frame();
      toFrame.push(performance.now() - start);
    }
    const at = (list: number[], p: number) =>
      [...list].sort((a, b) => a - b)[Math.floor((list.length - 1) * p)]!;
    return {
      work: { median: at(work, 0.5), p95: at(work, 0.95) },
      toFrame: { median: at(toFrame, 0.5), p95: at(toFrame, 0.95) },
    };
  });
  console.info(
    `${testInfo.project.name} find step: work median ${times.work.median.toFixed(1)}ms p95 ${times.work.p95.toFixed(1)}ms; ` +
      `to the next frame (marks drawn) median ${times.toFrame.median.toFixed(1)}ms p95 ${times.toFrame.p95.toFixed(1)}ms`,
  );
  expect(times.work.p95).toBeLessThan(50);
});
