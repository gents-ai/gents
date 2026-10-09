import { expect, gotoHarness, test } from "./desktopTest";

/* Scrolling up a long session at a steady pace, older pages arrive before
   the reader reaches the top: the view does not sit at the top waiting
   for each page while there are older ones to load. */
test("scrolling up a long session does not wait at the top for each page", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  /* scrolls up for 12 s; WebKit on Linux animates each wheel's scroll, so
     this outlasts the default limit there */
  test.slow();
  await page.setViewportSize({ width: 1500, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start").last().waitFor();
  /* a page read takes as long as a bridge read of the session does */
  await page.evaluate(() =>
    window.__GENTS_MOBILE_PERFORMANCE__!.setOlderPageDelay(400),
  );

  await page.evaluate(() => {
    const scroller = document
      .querySelector('[data-testid="transcript-panel"]')!
      .closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
    const frames: { top: number; rows: number; t: number }[] = [];
    const tick = () => {
      frames.push({
        top: scroller.scrollTop,
        rows: scroller.querySelectorAll("[data-timeline-key]").length,
        t: performance.now(),
      });
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
    Object.assign(window, { __olderFrames: frames });
  });

  await panel.hover();
  /* about 2,400 px a second for 12 s, a reader moving up through the session */
  for (let i = 0; i < 240; i += 1) {
    await page.mouse.wheel(0, -120);
    await page.waitForTimeout(50);
  }

  const frames = await page.evaluate(
    () =>
      (
        window as unknown as {
          __olderFrames: { top: number; rows: number; t: number }[];
        }
      ).__olderFrames,
  );
  const allRows = Math.max(...frames.map((f) => f.rows));
  /* time spent at the top while more rows were still to come */
  let waited = 0;
  for (let i = 1; i < frames.length; i += 1)
    if (frames[i].top <= 2 && frames[i].rows < allRows)
      waited += frames[i].t - frames[i - 1].t;
  console.log(
    `rows ${frames[0].rows} -> ${allRows}, waited at the top ${Math.round(waited)} ms`,
  );
  expect(allRows).toBeGreaterThanOrEqual(frames[0].rows + 160);
  expect(waited).toBeLessThan(150);
});

/* A page now lands while the reader is still views below the top; the row
   they are reading stays where it is. */
test("a page landing views above the reader leaves their row in place", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  await page.setViewportSize({ width: 1500, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start").last().waitFor();
  await page.evaluate(() =>
    window.__GENTS_MOBILE_PERFORMANCE__!.setOlderPageDelay(400),
  );

  const before = await panel.evaluate((node) => {
    const scroller = node.closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
    /* as a reader does: their own wheel, then the scroll it makes, ending
       two views below the top */
    scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: -1, bubbles: true }));
    scroller.scrollTop = 2 * scroller.clientHeight;
    const view = scroller.getBoundingClientRect();
    const row = document
      .elementFromPoint(view.left + view.width / 2, view.top + view.height / 2)!
      .closest<HTMLElement>("[data-timeline-key]")!;
    return {
      key: row.dataset.timelineKey!,
      top: row.getBoundingClientRect().top,
      rows: scroller.querySelectorAll("[data-timeline-key]").length,
    };
  });
  const row = page.locator(`[data-timeline-key="${before.key}"]`);
  await expect
    .poll(() =>
      panel.evaluate(
        (node) =>
          node
            .closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!
            .querySelectorAll("[data-timeline-key]").length,
      ),
    )
    .toBeGreaterThan(before.rows);
  await page.waitForTimeout(200);
  const after = await row.evaluate((el) => el.getBoundingClientRect().top);
  expect(Math.abs(after - before.top)).toBeLessThan(1);
});

/* WebKit drops a write to the position made while its own scrolling is under
   way, and stops drawing the transcript until a later write takes. A page
   that lands while the reader scrolls is kept out of sight above the
   content instead; the position is written only once the view rests or
   reaches the top. */
test("a page landing while the reader scrolls is not put back with a write until the view rests", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  test.slow();
  await page.setViewportSize({ width: 1500, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start").last().waitFor();
  await page.evaluate(() =>
    window.__GENTS_MOBILE_PERFORMANCE__!.setOlderPageDelay(400),
  );

  /* every write to the position once the reader starts scrolling (opening
     the session pins it to the foot): how far it moved the view, how long
     after the view last moved, and where it was */
  await panel.evaluate((node) => {
    const scroller = node.closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
    const native = Object.getOwnPropertyDescriptor(Element.prototype, "scrollTop")!;
    const writes: { by: number; sinceMoved: number; from: number }[] = [];
    let movedAt = -Infinity;
    let reading = false;
    scroller.addEventListener("wheel", () => (reading = true), {
      capture: true,
      passive: true,
    });
    scroller.addEventListener("scroll", () => (movedAt = performance.now()), {
      capture: true,
      passive: true,
    });
    Object.defineProperty(scroller, "scrollTop", {
      configurable: true,
      get: () => native.get!.call(scroller),
      set: (value: number) => {
        const from = native.get!.call(scroller) as number;
        if (reading)
          writes.push({
            by: value - from,
            sinceMoved: performance.now() - movedAt,
            from,
          });
        native.set!.call(scroller, value);
      },
    });
    Object.assign(window, { __writes: writes });
  });

  await panel.hover();
  for (let i = 0; i < 60; i += 1) {
    await page.mouse.wheel(0, -120);
    await page.waitForTimeout(40);
  }
  await page.waitForTimeout(600);

  const { writes, rows, hidden } = await panel.evaluate((node) => {
    const scroller = node.closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
    return {
      writes: (
        window as unknown as {
          __writes: { by: number; sinceMoved: number; from: number }[];
        }
      ).__writes,
      rows: scroller.querySelectorAll("[data-timeline-key]").length,
      hidden: getComputedStyle(scroller.firstElementChild!).marginTop,
    };
  });
  /* a write that puts a page back moves the view by the page's height */
  const pagesPutBack = writes.filter((w) => Math.abs(w.by) > 450);
  expect(rows).toBeGreaterThan(40);
  expect(pagesPutBack.length).toBeGreaterThan(0);
  expect(pagesPutBack.filter((w) => w.sinceMoved < 150 && w.from > 0)).toEqual([]);
  /* rested: nothing is left out of sight */
  expect(hidden).toBe("0px");
});
