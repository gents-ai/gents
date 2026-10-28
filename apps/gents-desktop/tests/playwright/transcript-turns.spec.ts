import { composer, expect, gotoHarness, sendButton, test } from "./desktopTest";

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

  /* a jump is written once the reader's scroll has rested, so wait for a turn
     other than `from` to reach the reading line rather than sampling once */
  async function landed(page: import("@playwright/test").Page, from?: string) {
    let at: Awaited<ReturnType<typeof current>> = null;
    await expect
      .poll(async () => {
        const turn = await current(page);
        at = turn && turn.key !== from && Math.abs(turn.offset - 56) <= 1 ? turn : null;
        return at !== null;
      })
      .toBe(true);
    return at!;
  }

  test("brings the previous turn to the reading line, then the next", async ({
    page,
  }) => {
    const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
    await expect(viewport).toHaveAttribute("data-following", "true");
    const start = await current(page);
    await page.getByTestId("transcript-panel").hover();
    await page.keyboard.press("Alt+Meta+ArrowUp");
    const up = await landed(page, start?.key);
    expect(up.following).toBe("false");

    await page.keyboard.press("Alt+Meta+ArrowUp");
    await landed(page, up.key);

    await page.keyboard.press("Alt+Meta+ArrowDown");
    await expect
      .poll(async () => {
        const turn = await current(page);
        return turn?.key === up.key && Math.abs(turn.offset - 56) <= 1;
      })
      .toBe(true);
  });
});

/* With the preference on, a sent turn settles near the top of the view and
   the reply fills in beneath it; the view follows again once it has. */
test.describe("a sent turn kept near the top", () => {
  test("holds the turn at the reading line while the reply fills the view, then follows", async ({
    page,
  }, testInfo) => {
    test.skip(
      !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
      "one layout per engine",
    );
    await page.clock.install();
    await page.setViewportSize({ width: 1280, height: 900 });
    await gotoHarness(page, "mobile-performance");
    /* the preference, as the store keeps it, then the harness again with it */
    await page.evaluate(() =>
      window.localStorage.setItem(
        "gents-preferences",
        JSON.stringify({ state: { sentTurn: "anchor" }, version: 1 }),
      ),
    );
    await gotoHarness(page, "mobile-performance");
    await page.locator('[data-testid="session-session-large"]').click();
    const panel = page.getByTestId("transcript-panel");
    await panel.getByText("stream-start", { exact: false }).last().waitFor();
    const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
    await expect(viewport).toHaveAttribute("data-following", "true");
    await page.waitForTimeout(400);

    /* the last turn ends and the person sends: the app draws its own copy
       of the message first, and the view goes to that */
    await page.evaluate(() =>
      window.__GENTS_MOBILE_PERFORMANCE__!.endReply({
        live: "drop",
        saved: true,
        completed: true,
      }),
    );
    await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.holdSends());
    await composer(page).fill("again");
    await page.clock.pauseAt(await page.evaluate(() => Date.now() + 60_000));
    /* the view goes to the turn as the app draws its copy of the message,
       or once the session makes it the latest request: the turn is sampled
       inside the page every frame from before the send, since driver round
       trips can outlast the glide and its rows are replaced on the way */
    await page.evaluate(() => {
      const scroller = document.querySelector<HTMLElement>(
        '[data-slot="scroll-area-viewport"][data-following]',
      )!;
      const offsets: number[] = [];
      const deadline = performance.now() + 3000;
      const done = new Promise<number[]>((resolve) => {
        const sample = () => {
          const turn = Array.from(scroller.querySelectorAll<HTMLElement>("[data-turn]"))
            .filter((row) => row.textContent?.includes("again"))
            .at(-1);
          const at = turn
            ? Math.round(
                turn.getBoundingClientRect().top - scroller.getBoundingClientRect().top,
              )
            : null;
          if (at !== null) offsets.push(at);
          if (
            (at !== null && scroller.dataset.following === "false" && at <= 89) ||
            performance.now() >= deadline
          )
            resolve(offsets);
          else requestAnimationFrame(sample);
        };
        requestAnimationFrame(sample);
      });
      Object.assign(window, { __glide: done });
    });
    await sendButton(page).dispatchEvent("click");
    await page.clock.runFor(32);
    const turn = panel.locator("[data-turn]").last();
    await expect(turn).toContainText("again");
    await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.userTurn("pending"));
    await page.clock.runFor(500);
    const offsets = await page.evaluate(
      () => (window as unknown as { __glide: Promise<number[]> }).__glide,
    );
    await page.clock.resume();
    const seen = { from: offsets[0]!, offsets };
    await expect(viewport).toHaveAttribute("data-following", "false");
    const offset = async () =>
      Math.round((await turn.boundingBox())!.y - (await viewport.boundingBox())!.y);
    await expect.poll(offset).toBeLessThanOrEqual(89);
    expect(await offset()).toBeGreaterThanOrEqual(87);
    expect(
      seen.offsets.filter((y) => y > 89 && y < seen.from - 1),
      JSON.stringify(seen),
    ).not.toHaveLength(0);
    /* the bridge shows the message as its saved row, and the reply begins:
       the hold and its room stay with the turn through every row that
       stands for it */
    for (const step of ["saved"] as const) {
      await page.evaluate(
        (step) => window.__GENTS_MOBILE_PERFORMANCE__!.userTurn(step),
        step,
      );
      await page.waitForTimeout(300);
      await expect(viewport).toHaveAttribute("data-following", "false");
      expect(Math.abs((await offset()) - 88)).toBeLessThanOrEqual(1);
    }
    await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.beginReply());
    await page.waitForTimeout(200);
    expect(Math.abs((await offset()) - 88)).toBeLessThanOrEqual(1);

    /* the reply streams in under the turn; the turn does not move */
    for (let n = 0; n < 6; n += 1) {
      await page.evaluate(
        (n) =>
          window.__GENTS_MOBILE_PERFORMANCE__!.streamText(
            `\n\nParagraph ${n} of the reply, long enough to wrap onto a second line in the column and then some more.`,
          ),
        n,
      );
      await page.waitForTimeout(120);
      if ((await viewport.getAttribute("data-following")) === "false")
        expect(Math.abs((await offset()) - 88)).toBeLessThanOrEqual(1);
    }
    /* the turn takes a tool step while room is left: still held */
    await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.liveTool("running"));
    await page.waitForTimeout(200);
    await expect(viewport).toHaveAttribute("data-following", "false");
    expect(Math.abs((await offset()) - 88)).toBeLessThanOrEqual(1);

    /* enough reply to fill the view: a working turn is followed from the
       layout where it crosses the foot (a plain answer would stay held;
       the unit tests cover that) */
    for (let n = 6; n < 30; n += 1) {
      await page.evaluate(
        (n) =>
          window.__GENTS_MOBILE_PERFORMANCE__!.streamText(
            `\n\nParagraph ${n} of the reply, long enough to wrap onto a second line in the column and then some more.`,
          ),
        n,
      );
      await page.waitForTimeout(60);
      if ((await viewport.getAttribute("data-following")) === "true") break;
    }
    await expect(viewport).toHaveAttribute("data-following", "true", {
      timeout: 10_000,
    });
  });

  test("a turn sent from far up passes the rows between without drawing them", async ({
    page,
  }, testInfo) => {
    test.skip(
      !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
      "one layout per engine",
    );
    test.slow();
    await page.setViewportSize({ width: 1280, height: 900 });
    await gotoHarness(page, "mobile-performance");
    await page.evaluate(() =>
      window.localStorage.setItem(
        "gents-preferences",
        JSON.stringify({ state: { sentTurn: "anchor" }, version: 1 }),
      ),
    );
    await gotoHarness(page, "mobile-performance");
    await page.locator('[data-testid="session-session-large"]').click();
    const panel = page.getByTestId("transcript-panel");
    await panel.getByText("stream-start", { exact: false }).last().waitFor();
    await page.evaluate(() =>
      window.__GENTS_MOBILE_PERFORMANCE__!.endReply({
        live: "drop",
        saved: true,
        completed: true,
      }),
    );
    /* far up the session, with the history between held */
    await panel.hover();
    const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
    const minimumDistance = 15_000;
    const distanceFromBottom = () =>
      viewport.evaluate((s) => s.scrollHeight - s.clientHeight - s.scrollTop);
    /* Wheel dispatch returns before scrolling settles. Start observation only
       after the required reading distance has been reached. */
    await expect
      .poll(
        async () => {
          const distance = await distanceFromBottom();
          if (distance <= minimumDistance) await page.mouse.wheel(0, -1500);
          return distance;
        },
        { timeout: 10_000, intervals: [150] },
      )
      .toBeGreaterThan(minimumDistance);
    await page.waitForTimeout(600);
    expect(await distanceFromBottom()).toBeGreaterThan(minimumDistance);

    /* every row drawn from the send until the turn is in place */
    await page.evaluate(() => {
      const list = document.querySelector<HTMLElement>("[data-transcript-list]")!;
      const keys = new Set<string>();
      new MutationObserver((records) => {
        for (const record of records)
          for (const node of record.addedNodes)
            if (node instanceof HTMLElement && node.dataset.rowKey)
              keys.add(node.dataset.rowKey);
      }).observe(list, { childList: true });
      Object.assign(window, { __drawn: keys });
    });
    await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.holdSends());
    await composer(page).fill("from far up");
    await sendButton(page).click();
    const turn = panel.locator("[data-turn]").last();
    await expect(turn).toContainText("from far up");
    await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.userTurn("pending"));
    const offset = async () =>
      Math.round((await turn.boundingBox())!.y - (await viewport.boundingBox())!.y);
    await expect.poll(offset).toBeLessThanOrEqual(89);
    expect(await offset()).toBeGreaterThanOrEqual(87);
    const drawn = await page.evaluate(
      () => (window as unknown as { __drawn: Set<string> }).__drawn.size,
    );
    /* a view or two of rows at the foot, never the history passed */
    expect(drawn).toBeLessThan(40);
  });
});
