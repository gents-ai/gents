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
  await page.getByRole("link", { name: "Amy configuration" }).click();

  await expect(
    page.getByRole("status").filter({ hasText: "Read-only here" }),
  ).toBeVisible();
  await expect(page.getByText("Display name is required")).toBeHidden();
  await expect(page.getByRole("button", { name: "Save" })).toBeHidden();

  await page
    .getByRole("link", { name: /^Tasks/ })
    .first()
    .click();
  /* in the page, but not offered */
  const create = page.locator("[data-readonly-hide]").filter({ hasText: /New/ });
  await expect(create).toHaveCount(1);
  await expect(create).toBeHidden();
});
