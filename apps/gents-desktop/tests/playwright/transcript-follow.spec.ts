import type { Locator, Page } from "@playwright/test";
import { composer, expect, gotoHarness, sendButton, test } from "./desktopTest";

/* Scrolled up, the reader's row stays where it is on screen whatever
   changes around it. */
/* WebKit on Linux animates a wheel scroll for hundreds of ms, so a
   position read at a fixed delay can land mid-flight; read it once the
   view holds still instead. */
async function restingScrollTop(page: Page, viewport: Locator): Promise<number> {
  let still = 0;
  let last = await viewport.evaluate((scroller) => scroller.scrollTop);
  const deadline = Date.now() + 3000;
  while (still < 150 && Date.now() < deadline) {
    await page.waitForTimeout(50);
    const next = await viewport.evaluate((scroller) => scroller.scrollTop);
    still = next === last ? still + 50 : 0;
    last = next;
  }
  return last;
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
    /* the reader's own wheel takes them off the foot */
    await page.getByTestId("transcript-panel").hover();
    await page.mouse.wheel(0, -1200);
    await page.waitForTimeout(300);
    const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
    await expect(viewport).toHaveAttribute("data-following", "false");

    const moved = await viewport.evaluate(async (scroller) => {
      const top = scroller.getBoundingClientRect().top;
      const rows = Array.from(
        scroller.querySelectorAll<HTMLElement>("[data-timeline-key]"),
      );
      const reader = rows.find(
        (row) => row.getBoundingClientRect().bottom > top + 200,
      )!;
      /* a row wholly above the view, out of the reader's sight */
      const above = rows
        .filter((row) => row.getBoundingClientRect().bottom < top)
        .at(-1)!;
      const before = reader.getBoundingClientRect().top;
      /* something above the reader grows, as a step's output would */
      above.style.paddingTop = "300px";
      await new Promise((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(resolve)),
      );
      return Math.round(reader.getBoundingClientRect().top - before);
    });
    expect(moved).toBe(0);
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
      /* a held arrow key repeats while its scroll runs */
      window.dispatchEvent(
        new KeyboardEvent("keydown", { key: "ArrowUp", repeat: true }),
      );
      /* the view has moved; the content changes before the move's event */
      scroller.scrollTop = moved;
      const note = document.createElement("span");
      scroller.querySelector("[data-timeline-key]")!.append(note);
      for (let i = 0; i < 3; i += 1) await new Promise((r) => requestAnimationFrame(r));
      return { moved, now: scroller.scrollTop };
    });
    expect(Math.abs(result.now - result.moved)).toBeLessThan(1);
  });

  test("keeps their place while the reply below them streams", async ({ page }) => {
    await page.getByTestId("transcript-panel").hover();
    await page.mouse.wheel(0, -1200);
    const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
    const before = await restingScrollTop(page, viewport);
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
    expect(await restingScrollTop(page, viewport)).toBe(before);
  });
});

/* Following at the foot, the view stays there when the message the person
   sent is replaced by its saved copy. */
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

/* Following at the foot through a whole turn, nothing at the foot changes
   height: the run's line keeps its place whether it is thinking, writing,
   blank while a step runs, reviewing, or done. So the page only grows, and
   the view only ever moves down it. */
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

/* A message the person sends is drawn once from the moment it is sent: the
   app's own copy, then the bridge's pending turn, then the saved message.
   The copy and the pending turn name the request by its id and are one row
   throughout; the saved message names it by its document id, so it is a
   row of its own, and the copy never comes back beside it. */
test("a sent message is drawn once from send to saved", async ({ page }, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start").last().waitFor();
  /* the session's last turn ends, so the composer can send */
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
  for (const [step, arg] of [
    ["userTurn", "saved"],
    /* a later read of the session, which once brought the app's copy back */
    ["streamUpdate"],
  ] as [string, unknown][]) {
    await harness(step, arg);
    await page.waitForTimeout(300);
    await expect(sent).toHaveCount(1);
  }
});
