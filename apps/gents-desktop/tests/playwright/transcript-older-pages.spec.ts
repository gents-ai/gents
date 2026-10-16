import { expect, gotoHarness, test } from "./desktopTest";

test("scrolling up a long session does not wait at the top for each page", async ({
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

  await panel.evaluate((node) => {
    const scroller = node.closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
    const native = Object.getOwnPropertyDescriptor(Element.prototype, "scrollTop")!;
    const writes: { by: number; sinceMoved: number; from: number }[] = [];
    let movedAt = -Infinity;
    let reading = false;
    scroller.addEventListener(
      "wheel",
      () => {
        reading = true;
        movedAt = performance.now();
      },
      { capture: true, passive: true },
    );
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
  await expect
    .poll(() =>
      panel.evaluate(
        (node) =>
          getComputedStyle(
            node.closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!
              .firstElementChild!,
          ).marginTop,
      ),
    )
    .toBe("0px");

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
  const pagesPutBack = writes.filter((w) => Math.abs(w.by) > 450);
  expect(rows).toBeGreaterThan(40);
  expect(pagesPutBack.length).toBeGreaterThan(0);
  expect(pagesPutBack.filter((w) => w.sinceMoved < 1000 && w.from > 0)).toEqual([]);
  expect(hidden).toBe("0px");
});

test("a page landing mid-wheel keeps the wheel's whole travel", async ({
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
    window.__GENTS_MOBILE_PERFORMANCE__!.setOlderPageDelay(200),
  );

  /* the wheel, the view's own moves, and every position write on one clock; a
     write remembers how long after the reader's last wheel or scroll it came */
  await panel.evaluate((node) => {
    const scroller = node.closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
    const native = Object.getOwnPropertyDescriptor(Element.prototype, "scrollTop")!;
    const writes: { by: number; sinceMoved: number; from: number }[] = [];
    let movedAt = -Infinity;
    let reading = false;
    scroller.addEventListener(
      "wheel",
      () => {
        reading = true;
        movedAt = performance.now();
      },
      { capture: true, passive: true },
    );
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

  /* the row under the reading line, and that same row by its key: the landing
     and the put-back are visually neutral, so the row moves only as far as
     the wheel carries the view */
  const rowAtLine = () =>
    panel.evaluate((node) => {
      const scroller = node.closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
      const top = scroller.getBoundingClientRect().top;
      const row = Array.from(
        scroller.querySelectorAll<HTMLElement>("[data-timeline-key]"),
      ).find((r) => r.getBoundingClientRect().bottom > top + 200)!;
      return {
        key: row.dataset.timelineKey!,
        offset: Math.round(row.getBoundingClientRect().top - top),
      };
    });
  const rowOffset = (key: string) =>
    panel.evaluate((node, key) => {
      const scroller = node.closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
      const top = scroller.getBoundingClientRect().top;
      const row = scroller.querySelector<HTMLElement>(`[data-timeline-key="${key}"]`)!;
      return Math.round(row.getBoundingClientRect().top - top);
    }, key);
  const rowCount = () =>
    panel.evaluate(
      (node) =>
        node
          .closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!
          .querySelectorAll("[data-timeline-key]").length,
    );
  const scrollTop = () =>
    panel.evaluate(
      (node) =>
        node.closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!.scrollTop,
    );
  const stillTop = async () => {
    let still = 0;
    let last = await scrollTop();
    const deadline = Date.now() + 5000;
    while (still < 500 && Date.now() < deadline) {
      await page.waitForTimeout(50);
      const next = await scrollTop();
      still = next === last ? still + 50 : 0;
      last = next;
    }
    return last;
  };

  await page.locator('[data-slot="scroll-area-viewport"][data-following]').hover();
  const rowsBefore = await rowCount();
  const reader = await rowAtLine();
  await page.mouse.wheel(0, -1200);
  await expect
    .poll(() =>
      panel.evaluate(
        () =>
          (
            window as unknown as {
              __writes: { by: number; sinceMoved: number; from: number }[];
            }
          ).__writes.filter((w) => w.from > 0 && Math.abs(w.by) > 450).length,
      ),
    )
    .toBeGreaterThan(0);
  await stillTop();
  const writes = await panel.evaluate(
    () =>
      (
        window as unknown as {
          __writes: { by: number; sinceMoved: number; from: number }[];
        }
      ).__writes,
  );
  expect(await rowCount(), JSON.stringify(writes)).toBeGreaterThan(rowsBefore);
  const pagesPutBack = writes.filter((w) => w.from > 0 && Math.abs(w.by) > 450);
  expect(pagesPutBack.length, JSON.stringify(writes)).toBeGreaterThan(0);
  expect(
    pagesPutBack.filter((w) => w.sinceMoved < 1000),
    JSON.stringify(writes),
  ).toEqual([]);
  const travel = (await rowOffset(reader.key)) - reader.offset;
  console.log(`wheel travel ${travel} px, writes ${JSON.stringify(writes)}`);
  expect(travel).toBeGreaterThan(1140);
  expect(travel).toBeLessThan(1260);
});
