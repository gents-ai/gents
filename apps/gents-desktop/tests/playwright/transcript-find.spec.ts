import { expect, gotoHarness, test, type Page } from "./desktopTest";

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

  await expect.poll(async () => (await current(page))?.inView).toBe(true);
  const seen = new Set([(await current(page))!.key]);
  for (let i = 0; i < 3; i += 1) {
    await field.press("Enter");
    await expect.poll(async () => (await current(page))?.inView).toBe(true);
    seen.add((await current(page))!.key);
  }
  /* back the other way, past the first match to the last */
  const at = Number((await count.textContent())!.split(" of ")[0]);
  for (let i = 0; i < at; i += 1) await field.press("Shift+Enter");
  await expect(count).toHaveText(new RegExp(`^${total} of ${total}$`));
  await expect.poll(async () => (await current(page))?.inView).toBe(true);
  seen.add((await current(page))!.key);
  expect(seen.size).toBeGreaterThan(2);

  await field.press("Escape");
  await expect(page.getByTestId("find-bar")).toHaveCount(0);
  expect(await page.evaluate(() => CSS.highlights.has("find"))).toBe(false);
});
