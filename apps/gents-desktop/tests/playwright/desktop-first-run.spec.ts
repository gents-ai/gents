import { composer, expect, gotoHarness, sendButton, test } from "./desktopTest";

test.describe("first-run install", () => {
  test("creates a local agent, adds inference, and starts a conversation", async ({
    page,
  }) => {
    test.setTimeout(60_000);
    await gotoHarness(page, "empty-fleet");
    await expect(page.getByTestId("setup-screen")).toBeVisible();
    await expect(page.getByRole("heading", { name: "Let’s get set up" })).toBeVisible();

    await page.getByTestId("setup-next").click();
    await expect(
      page.getByRole("heading", { name: "Configure your agent" }),
    ).toBeVisible();
    await expect(page.getByRole("textbox", { name: "Agent name" })).toHaveValue(
      "Forge",
    );

    await page.getByTestId("setup-next").click();
    await expect(
      page.getByRole("heading", { name: "Configure inference" }),
    ).toBeVisible({
      timeout: 15_000,
    });
    await expect(
      page.getByRole("radiogroup", { name: "Inference provider" }),
    ).toBeVisible();
    await page.getByTestId("setup-provider-local").click();
    await expect(page.getByText(/Found a server at/)).toBeVisible({ timeout: 10_000 });
    await page.getByTestId("setup-next").click();

    await expect(page.getByRole("heading", { name: "You’re in" })).toBeVisible();
    await page.getByTestId("setup-next").click();

    await expect(page.getByTestId("session-screen")).toBeVisible({ timeout: 10_000 });
    await expect(
      page.getByRole("heading", { name: /Start a new chat with Forge/ }),
    ).toBeVisible();

    await composer(page).fill("hello from first-run e2e");
    await expect(sendButton(page)).toBeEnabled();
    await sendButton(page).click();
    await expect(page.getByText(/Bombadil harness response/)).toBeVisible({
      timeout: 10_000,
    });
  });
});
