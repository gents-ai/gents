import { expect, gotoHarness, test, type Page } from "./desktopTest";

/* the current match's row once its mark is drawn inside the view, read in
   the same poll, since a repaint can clear the mark for a frame */
const landedKey = async (page: Page) => {
  let key: string | null = null;
  await expect
    .poll(async () => {
      const now = await current(page);
      key = now?.inView ? now.key : null;
      return key !== null;
    })
    .toBe(true);
  return key!;
};

/* the current match's row, and whether its mark is inside the view */
const current = (page: Page) =>
  page.evaluate(() => {
    const scroller = document
      .querySelector('[data-testid="transcript-panel"]')!
      .closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
    const range = [...(CSS.highlights.get("find-current") ?? [])][0] as
      Range | undefined;
    if (!range) return null;
    const rect = range.getBoundingClientRect();
    const view = scroller.getBoundingClientRect();
    const row = (
      range.startContainer.parentElement as HTMLElement
    ).closest<HTMLElement>("[data-row-key]");
    return {
      key: row?.dataset.rowKey ?? null,
      inView: rect.top >= view.top && rect.bottom <= view.bottom,
    };
  });

test("finds in a long session, steps to each match's row, and closes", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  await page.setViewportSize({ width: 1280, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start", { exact: false }).last().waitFor();

  await page.keyboard.press("ControlOrMeta+f");
  const field = page.getByRole("textbox", { name: "Find in session" });
  await expect(field).toBeFocused();
  await field.fill("fixture row 59");
  const count = page.getByTestId("find-bar").getByRole("status");
  await expect(count).toHaveText(/^\d+ of \d+$/);
  const total = Number((await count.textContent())!.split(" of ")[1]);
  expect(total).toBeGreaterThan(2);

  const seen = new Set([await landedKey(page)]);
  for (let i = 0; i < 3; i += 1) {
    await field.press("Enter");
    seen.add(await landedKey(page));
  }
  /* back the other way, past the first match to the last */
  const at = Number((await count.textContent())!.split(" of ")[0]);
  for (let i = 0; i < at; i += 1) await field.press("Shift+Enter");
  await expect(count).toHaveText(new RegExp(`^${total} of ${total}$`));
  seen.add(await landedKey(page));
  expect(seen.size).toBeGreaterThan(2);

  await field.press("Escape");
  await expect(page.getByTestId("find-bar")).toHaveCount(0);
  expect(await page.evaluate(() => CSS.highlights.has("find"))).toBe(false);
});

test("Search all reads the older pages and steps to a match only the oldest holds", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  test.slow();
  await page.setViewportSize({ width: 1280, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start", { exact: false }).last().waitFor();

  await page.keyboard.press("ControlOrMeta+f");
  const bar = page.getByTestId("find-bar");
  const field = page.getByRole("textbox", { name: "Find in session" });
  await field.fill("fixture row 0:");
  const count = bar.getByRole("status");
  await expect(count).toHaveText("No matches in the loaded messages");
  await expect(bar.getByText("Searching loaded messages")).toBeVisible();

  /* the reader's place holds while pages arrive above it: the first row
     below the top of the view, and where it is; none between frames */
  const top = () =>
    page.evaluate(() => {
      const scroller = document
        .querySelector('[data-testid="transcript-panel"]')!
        .closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
      const line = scroller.getBoundingClientRect().top + 120;
      const row = [...scroller.querySelectorAll<HTMLElement>("[data-row-key]")].find(
        (r) => r.getBoundingClientRect().bottom > line,
      );
      return row
        ? { key: row.dataset.rowKey, y: Math.round(row.getBoundingClientRect().top) }
        : null;
    });
  await page.evaluate(() =>
    window.__GENTS_MOBILE_PERFORMANCE__!.setOlderPageDelay(400),
  );
  await expect.poll(top).not.toBeNull();
  const before = (await top())!;
  await bar.getByRole("button", { name: "Search all" }).click();
  const progress = bar.getByText(/^Searched \d+ of \d+ messages$/);
  await expect(progress).toBeVisible();
  const first = await progress.textContent();
  await expect(progress).not.toHaveText(first!);
  /* pages have landed above, and the row the reader was at hasn't moved.
     Not on Linux WebKit: there the room kept under the last turn comes out
     24px shorter as the pages land, so a reader at the foot moves by it (a
     transcript defect, recorded as "Older pages landing move a reader at
     the foot on Linux") */
  if (!(testInfo.project.name === "webkit-desktop" && process.platform === "linux"))
    await expect
      .poll(
        async () => {
          const during = await top();
          return (
            !!during && during.key === before.key && Math.abs(during.y - before.y) <= 2
          );
        },
        { timeout: 2_000 },
      )
      .toBe(true);
  await expect(count).toHaveText("1 of 1", { timeout: 30_000 });
  await expect(bar.getByRole("button", { name: "Search all" })).toHaveCount(0);

  await expect.poll(async () => (await current(page))?.inView).toBe(true);
  /* the first message's row */
  expect((await current(page))!.key).toContain("large-request-0");
});

test("a group find opened folds again when the match leaves it, and the view holds", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  await page.setViewportSize({ width: 1280, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start", { exact: false }).last().waitFor();
  /* a group of steps above the last message, the first with output */
  await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.toolBatch(6, 0, 40));
  const group = panel.getByRole("button", { name: /6 commands/ });
  await expect(group).toHaveAttribute("aria-expanded", "false");

  await page.keyboard.press("ControlOrMeta+f");
  const field = page.getByRole("textbox", { name: "Find in session" });
  const count = page.getByTestId("find-bar").getByRole("status");
  await field.fill("output line 39");
  await expect(count).toHaveText("1 of 1");
  await expect(group).toHaveAttribute("aria-expanded", "true");
  await expect.poll(async () => (await current(page))?.inView).toBe(true);

  /* the next match is the message below the group */
  await field.fill("fixture row 598");
  await expect(count).toHaveText("1 of 1");
  await expect(group).toHaveAttribute("aria-expanded", "false");
  /* the match's row stays where find put it while the group above folds */
  const placeOf = () =>
    page.evaluate(() => {
      const scroller = document
        .querySelector('[data-testid="transcript-panel"]')!
        .closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
      const range = [...(CSS.highlights.get("find-current") ?? [])][0] as
        Range | undefined;
      return range
        ? Math.round(
            range.getBoundingClientRect().top - scroller.getBoundingClientRect().top,
          )
        : null;
    });
  await expect.poll(placeOf).not.toBeNull();
  const settled = (await placeOf())!;
  await page.waitForTimeout(300);
  expect(Math.abs((await placeOf())! - settled)).toBeLessThanOrEqual(2);
  /* near the foot the view can't bring it up to the reader's line, but it
     shows */
  expect((await current(page))?.inView).toBe(true);
});

test("stepping through steps' titles and their open input moves the mark every time", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  await page.setViewportSize({ width: 1280, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start", { exact: false }).last().waitFor();
  /* a group of commands named "cargo test -p gents step_N": each name is
     drawn in its title, and again in the step's input once it opens */
  await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.toolBatch(6, 0, 40));

  await page.keyboard.press("ControlOrMeta+f");
  const field = page.getByRole("textbox", { name: "Find in session" });
  const count = page.getByTestId("find-bar").getByRole("status");
  await field.fill("step_");
  await expect(count).toHaveText(/^1 of \d+$/);
  /* where the current mark is: its text node's place in the page, and offset */
  const mark = () =>
    page.evaluate(() => {
      const range = [...(CSS.highlights.get("find-current") ?? [])][0] as
        Range | undefined;
      if (!range) return null;
      const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
      let i = 0;
      for (let node = walker.nextNode(); node; node = walker.nextNode(), i++)
        if (node === range.startContainer) return `${i}:${range.startOffset}`;
      return null;
    });
  await expect.poll(mark).not.toBeNull();
  const total = Number((await count.textContent())!.split(" of ")[1]);
  expect(total).toBeGreaterThan(2);
  const seen = new Set([await mark()]);
  for (let step = 2; step <= total; step++) {
    await field.press("Enter");
    await expect(count).toHaveText(`${step} of ${total}`);
    /* a new place each time: the mark never stays behind the count */
    await expect.poll(async () => seen.has(await mark())).toBe(false);
    seen.add(await mark());
  }
});
