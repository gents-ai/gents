import { expect, gotoHarness, openConfig, test } from "../playwright/desktopTest";

/* at the display's own pixels; the default scales the shot down to CSS pixels,
   which hides the blur this guards against */
const SHARP = { animations: "disabled", scale: "device" } as const;

test("a node's portrait stays sharp when it zooms on a 2× display", async ({
  page,
}) => {
  await gotoHarness(page);
  await openConfig(page);
  const roll = page.getByRole("button", { name: "Roll a new avatar" });
  const portrait = roll.locator("img");
  await expect(roll).toBeEnabled();
  await expect(portrait).toHaveJSProperty("complete", true);
  await expect(roll).toHaveScreenshot("avatar-rest.png", SHARP);

  await roll.hover();
  await expect(roll).toHaveScreenshot("avatar-zoomed.png", SHARP);
  const { natural, drawn } = await portrait.evaluate((img: HTMLImageElement) => ({
    natural: img.naturalWidth,
    drawn: img.getBoundingClientRect().width * devicePixelRatio,
  }));
  expect(drawn).toBe(224);
  expect(natural).toBeGreaterThanOrEqual(drawn);
});
