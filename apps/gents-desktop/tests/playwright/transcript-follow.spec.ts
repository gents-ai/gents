import type { Locator, Page } from "@playwright/test";
import { composer, expect, gotoHarness, sendButton, test } from "./desktopTest";

async function restingScrollTop(page: Page, viewport: Locator): Promise<number> {
  const STILL_MS = 500;
  let still = 0;
  let last = await viewport.evaluate((scroller) => scroller.scrollTop);
  const deadline = Date.now() + 5000;
  while (still < STILL_MS && Date.now() < deadline) {
    await page.waitForTimeout(50);
    const next = await viewport.evaluate((scroller) => scroller.scrollTop);
    still = next === last ? still + 50 : 0;
    last = next;
  }
  if (still < STILL_MS)
    throw new Error(`the view never came to rest (last at ${last})`);
  return last;
}

function readerRow(viewport: Locator, key?: string) {
  return viewport.evaluate((scroller, key) => {
    const top = scroller.getBoundingClientRect().top;
    const rows = Array.from(
      scroller.querySelectorAll<HTMLElement>("[data-timeline-key]"),
    );
    const row = key
      ? rows.find((row) => row.dataset.timelineKey === key)
      : rows.find((row) => row.getBoundingClientRect().bottom > top + 200)!;
    if (!row) return { key: key!, offset: Number.POSITIVE_INFINITY };
    return {
      key: row.dataset.timelineKey!,
      offset: Math.round(row.getBoundingClientRect().top - top),
    };
  }, key);
}

/* Following stops at the wheel event itself, before the browser has begun
   the scroll, so the move is read from the row under the reader. Under load
   WebKitGTK can lose a wheel to a write made before its animation starts
   (#2389); the reader wheels again. */
async function wheelUp(page: Page, viewport: Locator) {
  await expect(viewport).toHaveAttribute("data-following", "true");
  await restingScrollTop(page, viewport);
  const at = await readerRow(viewport);
  await page.getByTestId("transcript-panel").hover();
  await expect(async () => {
    await page.mouse.wheel(0, -1200);
    await expect
      .poll(async () => (await readerRow(viewport, at.key)).offset, { timeout: 2000 })
      .toBeGreaterThan(at.offset);
  }).toPass({ timeout: 10_000 });
  await expect(viewport).toHaveAttribute("data-following", "false");
  await restingScrollTop(page, viewport);
}

test.describe("a reader scrolled up in a transcript", () => {
  test.beforeEach(async ({ page }, testInfo) => {
    test.skip(
      !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
      "one layout per engine",
    );
    await gotoHarness(page, "mobile-performance");
    await page.locator('[data-testid="session-session-large"]').click();
    await page
      .getByTestId("transcript-panel")
      .getByText("stream-start")
      .last()
      .waitFor();
  });

  test("keeps their row still when a row above it grows", async ({ page }) => {
    const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
    await wheelUp(page, viewport);

    const moved = await viewport.evaluate(async (scroller) => {
      const top = scroller.getBoundingClientRect().top;
      const rows = Array.from(
        scroller.querySelectorAll<HTMLElement>("[data-timeline-key]"),
      );
      const reader = rows.find(
        (row) => row.getBoundingClientRect().bottom > top + 200,
      )!;
      const above = rows
        .filter((row) => row.getBoundingClientRect().bottom < top)
        .at(-1)!;
      const before = reader.getBoundingClientRect().top;
      above.style.paddingTop = "300px";
      await new Promise((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(resolve)),
      );
      return Math.round(reader.getBoundingClientRect().top - before);
    });
    expect(Math.abs(moved)).toBe(0);
  });

  test("holds a growing row during wheel motion without writing the position", async ({
    page,
  }, testInfo) => {
    const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
    await wheelUp(page, viewport);
    const result = await viewport.evaluate(async (scroller) => {
      const top = scroller.getBoundingClientRect().top;
      const rows = Array.from(
        scroller.querySelectorAll<HTMLElement>("[data-timeline-key]"),
      );
      const reader = rows.find(
        (row) => row.getBoundingClientRect().bottom > top + 200,
      )!;
      const above = rows
        .filter((row) => row.getBoundingClientRect().bottom < top)
        .at(-1)!;
      const before = reader.getBoundingClientRect().top;
      const position = Object.getOwnPropertyDescriptor(Element.prototype, "scrollTop")!;
      let writes = 0;
      /* how far the page's own writes moved the view */
      let written = 0;
      Object.defineProperty(scroller, "scrollTop", {
        configurable: true,
        get: () => position.get!.call(scroller),
        set: (value) => {
          writes++;
          const from = position.get!.call(scroller) as number;
          position.set!.call(scroller, value);
          written += (position.get!.call(scroller) as number) - from;
        },
      });
      try {
        const startTop = position.get!.call(scroller) as number;
        scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: -20 }));
        /* the wheel is still moving while the row is checked: slow frames
           would otherwise let the view come to rest, where a write is due */
        const moving = setInterval(
          () => scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: 0 })),
          30,
        );
        above.style.paddingTop = "120px";
        await new Promise((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(resolve)),
        );
        clearInterval(moving);
        /* WebKit scrolls for a wheel event dispatched by a script; the
           reader's row goes as far as their wheel takes it and no further */
        const travel = () =>
          startTop - ((position.get!.call(scroller) as number) - written);
        const during = {
          writes,
          moved: reader.getBoundingClientRect().top - before - travel(),
        };
        await new Promise((resolve) => setTimeout(resolve, 1200));
        return {
          during,
          after: reader.getBoundingClientRect().top - before - travel(),
        };
      } finally {
        delete (scroller as Partial<HTMLElement>).scrollTop;
      }
    });
    /* WebKit drops a write made while the wheel moves, so there the row is
       held without one; other engines take the write */
    if (testInfo.project.name.startsWith("webkit"))
      expect(result.during.writes).toBe(0);
    expect(Math.abs(result.during.moved)).toBeLessThan(1);
    expect(Math.abs(result.after)).toBeLessThan(1);
  });

  /* A held arrow key or a trackpad's momentum moves the view a little each
     frame, and the scroll event saying so comes after; a row swapping or a
     reply growing in between must not pull the reader back by the move they
     just made (WebKit stops a keyboard scroll at any such write). */
  test("does not undo the reader's own move when the content changes before its scroll event", async ({
    page,
  }) => {
    await page.getByTestId("transcript-panel").hover();
    await page.mouse.wheel(0, -1200);
    const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
    await expect(viewport).toHaveAttribute("data-following", "false");
    await page.waitForTimeout(800);
    const result = await viewport.evaluate(async (scroller) => {
      const moved = scroller.scrollTop - 10;
      window.dispatchEvent(
        new KeyboardEvent("keydown", { key: "ArrowUp", repeat: true }),
      );
      scroller.scrollTop = moved;
      const note = document.createElement("span");
      scroller.querySelector("[data-timeline-key]")!.append(note);
      for (let i = 0; i < 3; i += 1) await new Promise((r) => requestAnimationFrame(r));
      return { moved, now: scroller.scrollTop };
    });
    expect(Math.abs(result.now - result.moved)).toBeLessThan(1);
  });

  test("keeps their place while the reply below them streams", async ({ page }) => {
    const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
    await wheelUp(page, viewport);
    const before = await readerRow(viewport);
    for (let n = 0; n < 8; n += 1) {
      await page.evaluate(
        (n) =>
          window.__GENTS_MOBILE_PERFORMANCE__!.streamText(
            `\\n\\nMore of the reply, paragraph ${n}, long enough to wrap onto a second line in the column.`,
          ),
        n,
      );
      await page.waitForTimeout(100);
    }
    await expect(viewport).toHaveAttribute("data-following", "false");
    await restingScrollTop(page, viewport);
    const after = await readerRow(viewport, before.key);
    expect(after.key).toBe(before.key);
    expect(Math.abs(after.offset - before.offset)).toBeLessThanOrEqual(1);
  });

  /* Chromium can apply a wheel's scroll on its compositor before dispatching
     the wheel, leaving a scroll with no reader intent behind it (#2388); the
     wheel listener is non-passive, which forces the dispatch first. The probe
     watches every position write from the wheel on: a write to the foot is the
     failure, because the pin cancels the reader's scroll in flight. */
  test("is never written back to the foot while the reply streams under their wheel", async ({
    page,
  }, testInfo) => {
    test.skip(
      testInfo.project.name !== "chromium-desktop",
      "scroll-before-wheel order is Chromium's; WebKit dispatches the wheel first",
    );
    const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
    await expect(viewport).toHaveAttribute("data-following", "true");
    await restingScrollTop(page, viewport);
    await viewport.evaluate((scroller) => {
      const writes: { from: number; to: number; foot: number; written: number }[] = [];
      const real = Object.getOwnPropertyDescriptor(Element.prototype, "scrollTop")!;
      let written = real.get!.call(scroller) as number;
      Object.defineProperty(scroller, "scrollTop", {
        configurable: true,
        get: () => real.get!.call(scroller) as number,
        set: (value: number) => {
          writes.push({
            from: real.get!.call(scroller) as number,
            to: value,
            foot: scroller.scrollHeight - scroller.clientHeight,
            written,
          });
          real.set!.call(scroller, value);
          written = real.get!.call(scroller) as number;
        },
      });
      Object.assign(window, { __pinWrites: writes });
    });
    await page.evaluate(() => {
      let chunk = 0;
      const timer = window.setInterval(() => {
        window.__GENTS_MOBILE_PERFORMANCE__!.streamText(
          `\\n\\nMore of the reply, paragraph ${chunk}, long enough to wrap onto a second line in the column.`,
        );
        chunk += 1;
        if (chunk === 24) window.clearInterval(timer);
      }, 60);
    });
    /* the scroller, not the panel: hovering the panel is a move with no reader
       input behind it that following rightly writes back to the foot */
    await viewport.hover();
    for (let guard = 0; guard < 12; guard++) {
      const clear = await viewport.evaluate(
        (scroller) =>
          scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight,
      );
      if (clear > 600) break;
      await page.mouse.wheel(0, -1200);
      await page.waitForTimeout(400);
    }
    await expect(viewport).toHaveAttribute("data-following", "false");
    const top = await restingScrollTop(page, viewport);
    const writes = await page.evaluate(
      () =>
        (
          window as unknown as {
            __pinWrites: { from: number; to: number; foot: number; written: number }[];
          }
        ).__pinWrites,
    );
    /* a pin writes the foot over a reader who has moved the view up from
       where the module last put it, however little the platform's wheel
       carried them */
    const pins = writes.filter(
      (write) =>
        write.from < write.written - 1 &&
        write.to > write.from + 1 &&
        Math.round(write.to) === Math.round(write.foot),
    );
    expect(pins).toEqual([]);
    const aboveFoot = await viewport.evaluate(
      (scroller, top) => scroller.scrollHeight - top - scroller.clientHeight,
      top,
    );
    expect(aboveFoot).toBeGreaterThan(400);
  });
});

test("stays at the foot when a sent message is saved", async ({ page }, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  await page.getByTestId("transcript-panel").getByText("stream-start").last().waitFor();
  await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.userTurn("pending"));
  await expect(page.getByTestId("transcript-panel").getByText("again")).toBeVisible();
  await page.waitForTimeout(300);
  const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
  await viewport.evaluate((scroller) => {
    const gaps: number[] = [];
    const tick = () => {
      gaps.push(
        Math.round(scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight),
      );
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
    Object.assign(window, { __footGaps: gaps });
  });
  await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.userTurn("saved"));
  await page.waitForTimeout(600);
  const gaps = await page.evaluate(
    () => (window as unknown as { __footGaps: number[] }).__footGaps,
  );
  expect([...new Set(gaps)].filter((gap) => gap > 1)).toEqual([]);
});

test("only moves down the page through a whole turn", async ({ page }, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  await page.getByTestId("transcript-panel").getByText("stream-start").last().waitFor();
  await page.waitForTimeout(300);
  await page.evaluate(() => {
    const scroller = document
      .querySelector('[data-testid="transcript-panel"]')!
      .closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
    const frames: { top: number; height: number }[] = [];
    const tick = () => {
      frames.push({ top: scroller.scrollTop, height: scroller.scrollHeight });
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
    Object.assign(window, { __turnFrames: frames });
  });
  const harness = (step: string, arg?: unknown) =>
    page.evaluate(
      ([step, arg]) =>
        (
          window.__GENTS_MOBILE_PERFORMANCE__ as unknown as Record<
            string,
            (a?: unknown) => void
          >
        )[step as string]!(arg),
      [step, arg] as const,
    );
  const phases: [string, unknown][] = [
    ["userTurn", "pending"],
    ["userTurn", "saved"],
    ["streamText", " and the reply begins to say something about it."],
    ["liveTool", "running"],
    ["liveTool", "done"],
    ["streamText", " Then it carries on writing after the step."],
    ["endReply", { live: "drop", saved: true, completed: true }],
  ];
  for (const [step, arg] of phases) {
    await harness(step, arg);
    await page.waitForTimeout(500);
  }
  const frames = await page.evaluate(
    () =>
      (window as unknown as { __turnFrames: { top: number; height: number }[] })
        .__turnFrames,
  );
  const shrinks = frames
    .slice(1)
    .map((frame, i) => Math.round(frames[i]!.height - frame.height))
    .filter((by) => by > 0);
  const stepsBack = frames
    .slice(1)
    .map((frame, i) => Math.round(frames[i]!.top - frame.top))
    .filter((by) => by > 0);
  expect({ shrinks, stepsBack }).toEqual({ shrinks: [], stepsBack: [] });
});

test("a sent message is drawn once from send to saved", async ({ page }, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start").last().waitFor();
  await page.evaluate(() =>
    window.__GENTS_MOBILE_PERFORMANCE__!.endReply({
      live: "drop",
      saved: true,
      completed: true,
    }),
  );
  await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.holdSends());
  const sent = panel.getByText("again", { exact: true });
  const row = () => sent.locator("xpath=ancestor::*[@data-timeline-key][1]");

  await composer(page).fill("again");
  await sendButton(page).click();
  await expect(sent).toHaveCount(1);
  await row().evaluate((el) => (el.dataset.probe = "sent"));

  const harness = (step: string, arg?: unknown) =>
    page.evaluate(
      ([step, arg]) =>
        (
          window.__GENTS_MOBILE_PERFORMANCE__ as unknown as Record<
            string,
            (a?: unknown) => void
          >
        )[step as string]!(arg),
      [step, arg] as const,
    );
  await harness("userTurn", "pending");
  await page.waitForTimeout(300);
  await expect(sent).toHaveCount(1);
  await expect(row()).toHaveAttribute("data-probe", "sent");
  for (const [step, arg] of [["userTurn", "saved"], ["streamUpdate"]] as [
    string,
    unknown,
  ][]) {
    await harness(step, arg);
    await page.waitForTimeout(300);
    await expect(sent).toHaveCount(1);
  }
});

/* Typing in the composer is not a move of the transcript: when what is
   below shrinks and the browser moves the view up to the new foot, the
   transcript keeps following the reply. */
test("keeps following while the person types and the foot moves", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  await page.getByTestId("transcript-panel").getByText("stream-start").last().waitFor();
  const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
  await expect(viewport).toHaveAttribute("data-following", "true");
  await composer(page).pressSequentially("a draft", { delay: 5 });
  /* the browser's own move to a nearer foot, as when the rows below shrink */
  await viewport.evaluate((scroller) => {
    scroller.scrollTop = scroller.scrollHeight - scroller.clientHeight - 10;
  });
  await page.waitForTimeout(200);
  await expect(viewport).toHaveAttribute("data-following", "true");
});

/* A command takes the view from wherever it is: going back to the foot
   just after a wheel, before the browser reports where the wheel left the
   view, still goes to the foot and follows. */
test("goes back to the foot when asked just after a wheel", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start").last().waitFor();
  const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
  await panel.hover();
  /* history is read in above the reader as they wheel, so the view leaves
     the foot only once enough of it is there */
  const back = page.getByRole("button", { name: "Back to bottom" });
  for (let i = 0; i < 10 && !(await back.isVisible()); i += 1) {
    await page.mouse.wheel(0, -1500);
    await page.waitForTimeout(300);
  }
  await expect(viewport).toHaveAttribute("data-following", "false");
  await restingScrollTop(page, viewport);
  await back.waitFor();
  await viewport.evaluate((scroller) => {
    scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: -100, bubbles: true }));
    scroller.scrollTop -= 100;
    document
      .querySelector<HTMLButtonElement>('button[aria-label="Back to bottom"]')!
      .click();
  });
  await expect(viewport).toHaveAttribute("data-following", "true");
});

test("the transcript stops at its ends instead of bouncing past them", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  await page.getByTestId("transcript-panel").getByText("stream-start").last().waitFor();
  const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
  expect(
    await viewport.evaluate(
      (scroller) => getComputedStyle(scroller).overscrollBehaviorY,
    ),
  ).toBe("none");
});

/* The composer is pinned within the transcript's content. A row drawn
   taller than the list has placed it yet must not stretch the scroll range
   past that content, or the composer rides up with the content's end and
   rows show below it. */
test("keeps the composer at the foot while a row outgrows its place", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  await page.getByTestId("transcript-panel").getByText("stream-start").last().waitFor();
  const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
  await expect(viewport).toHaveAttribute("data-following", "true");
  const gap = await viewport.evaluate((scroller) => {
    const rows = scroller.querySelectorAll<HTMLElement>("[data-row-key]");
    rows[rows.length - 1]!.style.paddingBottom = "400px";
    /* before the list hears of it, the view goes as far down as it can */
    scroller.scrollTop = scroller.scrollHeight;
    const composer = scroller
      .querySelector('[aria-label="Message"]')!
      .closest<HTMLElement>(".sticky")!;
    return Math.round(
      scroller.getBoundingClientRect().bottom - composer.getBoundingClientRect().bottom,
    );
  });
  expect(gap).toBeLessThanOrEqual(1);
});
