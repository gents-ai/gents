import { expect, gotoHarness, test } from "./desktopTest";

/* an enrolled node whose settings the app hasn't read shows them without
   anything that would change them */
test("a read-only node's pages offer nothing that would change its settings", async ({
  page,
}) => {
  await gotoHarness(page, "active-nodes");
  await page.getByRole("link", { name: "Nodes", exact: true }).first().click();
  await page.getByRole("link", { name: "Amy sessions", exact: true }).click();
  /* switching may ask first, when a session would be left */
  const review = page.getByRole("alertdialog");
  if (await review.isVisible())
    await review.getByRole("button", { name: "Switch node" }).click();
  await page.getByLabel("Active node", { exact: true }).click();

  await expect(
    page.getByRole("status").filter({ hasText: "Read-only here" }),
  ).toBeVisible();
  await expect(page.getByText("Display name is required")).toBeHidden();
  await expect(page.getByRole("button", { name: "Save" })).toBeHidden();

  /* a narrow window picks sections from a select */
  const section = page.getByRole("combobox", { name: "Section" });
  if (await section.isVisible()) {
    await section.click();
    await page.getByRole("option", { name: /^Tasks/ }).click();
  } else {
    /* off the hover nav, which stays open while the pointer is inside it */
    await page.mouse.move(page.viewportSize()!.width - 30, 400);
    await page
      .getByRole("link", { name: /^Tasks/ })
      .first()
      .click();
  }
  /* in the page, but not offered */
  const create = page.locator("[data-readonly-hide]").filter({ hasText: /New/ });
  await expect(create).toHaveCount(1);
  await expect(create).toBeHidden();
});
