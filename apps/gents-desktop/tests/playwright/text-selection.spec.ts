import type { Locator } from "@playwright/test";
import { expect, gotoHarness, test } from "./desktopTest";

const selection = (locator: Locator) =>
  locator.evaluate((el) => {
    const style = getComputedStyle(el);
    return style.userSelect || style.getPropertyValue("-webkit-user-select");
  });

/* Text is selectable where people copy it, and nowhere else, as in a
   native app. */
test("selects text in messages, code and fields, and not in the app's controls", async ({
  page,
}) => {
  await gotoHarness(page, "coding");
  await page
    .getByTestId("sessions-screen")
    .getByRole("link", { name: /introduction-and-greetings/ })
    .click();
  const message = page.locator('[data-slot="assistant-message"]').first();
  await expect(message).toBeVisible();

  expect(await selection(page.locator("body"))).toBe("none");
  expect(await selection(page.getByRole("button").first())).toBe("none");

  expect(await selection(message)).toBe("text");
  /* a button inside selectable content is still a control */
  const inMessage = page.locator('[data-slot="assistant-message"] button').first();
  if (await inMessage.count()) expect(await selection(inMessage)).toBe("none");
  expect(await selection(page.getByRole("textbox").first())).toBe("text");
  const code = page.locator('[data-slot="assistant-message"] code').first();
  if (await code.count()) expect(await selection(code)).toBe("text");
});
