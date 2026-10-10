import {
  expect,
  gotoHarness,
  openConfig,
  openConfigSection,
  test,
  type Page,
} from "./desktopTest";

type Locator = ReturnType<Page["locator"]>;

/* the scrolling body of an editor sheet */
const bodyOf = (sheet: Locator) => sheet.locator(".overflow-y-auto").first();

/* the Open button on the row labelled `label` */
function openFor(scope: Locator, label: string) {
  return scope
    .getByText(label, { exact: true })
    .locator("xpath=ancestor::*[.//button[@aria-label='Open']][1]")
    .getByRole("button", { name: "Open" })
    .first();
}

/* an agent's profile in one sheet, and that profile's backend in a second */
async function stackTwoSheets(page: Page) {
  await page.setViewportSize({ width: 1280, height: 520 });
  await gotoHarness(page);
  await openConfig(page);
  await openConfigSection(page, /^Agents\b/);
  await page
    .getByRole("link", { name: /^Ops\b/ })
    .first()
    .click();
  await openFor(page.locator("main"), "Inference profile").click();
  /* the sheet below is hidden from the accessibility tree while one stacks
     above it, so the sheets are found by their slot */
  const sheets = page.locator("[data-slot=sheet-content]");
  await expect(sheets).toHaveCount(1);
  const below = sheets.first();
  await openFor(below, "Backend").click();
  await expect(sheets).toHaveCount(2);
  const top = sheets.last();
  return { below, top };
}

const scrollTop = (body: Locator) => body.evaluate((el) => el.scrollTop);

test.describe("stacked editor sheets", () => {
  test("a wheel over the top sheet scrolls it", async ({ page }) => {
    const { top } = await stackTwoSheets(page);
    const body = bodyOf(top);
    const box = (await body.boundingBox())!;
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.wheel(0, 300);
    await expect.poll(() => scrollTop(body)).toBeGreaterThan(0);
  });

  test("a wheel over the backdrop scrolls the top sheet, not the one below", async ({
    page,
  }) => {
    const { below, top } = await stackTwoSheets(page);
    const belowBefore = await scrollTop(bodyOf(below));
    await page.mouse.move(20, 260);
    await page.mouse.wheel(0, 300);
    await expect.poll(() => scrollTop(bodyOf(top))).toBeGreaterThan(0);
    expect(await scrollTop(bodyOf(below))).toBe(belowBefore);
  });

  test("the sheet below steps aside so the stack shows", async ({ page }) => {
    const { below, top } = await stackTwoSheets(page);
    await expect
      .poll(async () => {
        const [b, t] = [(await below.boundingBox())!, (await top.boundingBox())!];
        return Math.round(t.x - b.x);
      })
      .toBe(24);
  });
});
