import {
  expect,
  gotoHarness,
  test,
  type HarnessScenario,
  type Page,
} from "./desktopTest";

async function expandedAt(
  page: Page,
  scenario: HarnessScenario,
  state: Record<string, unknown>,
) {
  await page.setViewportSize({ width: 1440, height: 900 });
  await gotoHarness(page, scenario);
  await page.evaluate(
    (s) =>
      localStorage.setItem(
        "gents-preferences",
        JSON.stringify({ state: { nav: "expanded", ...s }, version: 1 }),
      ),
    state,
  );
  await page.reload();
  await expect(page.locator("[data-ground=window]")).toBeVisible();
}

/* every run of text in the expanded sidebar that breaks onto a second line:
   text in one block-level box must share one line */
const wrapped = (page: Page) =>
  page.locator("[data-ground=window]").evaluate((panel) => {
    const lines = new Map<Element, Set<number>>();
    const walker = document.createTreeWalker(panel, NodeFilter.SHOW_TEXT);
    for (let n = walker.nextNode(); n; n = walker.nextNode()) {
      if (!n.textContent?.trim()) continue;
      let box = n.parentElement;
      while (box && getComputedStyle(box).display.startsWith("inline"))
        box = box.parentElement;
      if (!box) continue;
      const range = document.createRange();
      range.selectNodeContents(n);
      const tops = lines.get(box) ?? new Set<number>();
      for (const rect of range.getClientRects())
        if (rect.width > 0) tops.add(Math.round(rect.bottom / 4));
      lines.set(box, tops);
    }
    return [...lines]
      .filter(([, tops]) => tops.size > 1)
      .map(([box]) => (box as HTMLElement).innerText.replace(/\s+/g, " "));
  });

const squeeze = (page: Page) =>
  page
    .locator("[data-ground=window]")
    .evaluate((panel) => void ((panel as HTMLElement).style.width = "150px"));

const header = (page: Page) =>
  page.getByTestId("working-node").evaluate((el) => el.getBoundingClientRect().height);

test.describe("the sidebar at its narrowest", () => {
  /* it sets its own 1440×900 window, so the laptop and narrow projects
     would only repeat the desktop Chromium run */
  test.beforeEach(({}, info) => {
    test.skip(
      info.project.name === "chromium-laptop" ||
        info.project.name === "chromium-narrow",
      "sets its own desktop window",
    );
  });
  test("keeps the node's name and every row on one line", async ({ page }) => {
    await expandedAt(page, "mailbox-many", { navWidth: 304 });
    const wide = await header(page);
    await expandedAt(page, "mailbox-many", { navWidth: 220 });
    expect(await header(page)).toBe(wide);
    expect(await wrapped(page)).toEqual([]);
    /* longer names and counts than these take the room a narrower column
       would: rows still truncate rather than wrap, and the switcher stays */
    await squeeze(page);
    expect(await header(page)).toBe(wide);
    expect(await wrapped(page)).toEqual([]);
    await expect(
      page.locator("[data-ground=window]").getByRole("button", { name: "Switch node" }),
    ).toBeInViewport();
  });

  /* one fixture has the mailbox's items, the other more than one node */
  for (const scenario of ["mailbox-many", "active-nodes"] as const)
    test(`keeps the customized sections on one line too (${scenario})`, async ({
      page,
    }) => {
      await expandedAt(page, scenario, {
        navWidth: 220,
        flags: { "customize-nav": "on" },
        navLayout: {
          order: [],
          shown: {
            "mailbox-all": true,
            "mailbox-decide": true,
            "mailbox-look": true,
            "node-list": true,
            pinned: true,
          },
        },
      });
      expect(await wrapped(page)).toEqual([]);
      await squeeze(page);
      expect(await wrapped(page)).toEqual([]);
    });
});
