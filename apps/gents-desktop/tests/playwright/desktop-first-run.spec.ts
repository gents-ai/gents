import {
  composer,
  expect,
  gotoHarness,
  openConfig,
  sendButton,
  test,
} from "./desktopTest";

test.describe("first-run install", () => {
  test("creates a local agent, adds inference, and starts a conversation", async ({
    page,
  }) => {
    test.setTimeout(60_000);
    await gotoHarness(page, "empty-fleet");
    await expect(page.getByTestId("setup-screen")).toBeVisible();
    await expect(page.getByRole("heading", { name: "Let’s get set up" })).toBeVisible();

    await expect(page.getByRole("textbox", { name: "Agent name" })).toHaveValue(
      "Forge",
    );
    await expect(page.getByRole("combobox", { name: "Tool ceiling" })).toHaveValue(
      "readwrite",
    );
    await expect(
      page.getByRole("textbox", { name: "Tool root", exact: true }),
    ).toHaveValue("/tmp/gents-bombadil/workspace");
    await page.getByTestId("setup-next").click();
    await expect(
      page.getByRole("heading", { name: "Choose an inference provider" }),
    ).toBeVisible({
      timeout: 15_000,
    });
    await expect(
      page.getByRole("radiogroup", { name: "Inference provider" }),
    ).toBeVisible();
    for (const provider of ["OpenAI", "Anthropic", "Grok", "Local", "OpenRouter"]) {
      await expect(
        page.getByRole("radio", { name: new RegExp(`^${provider}`) }),
      ).toBeVisible();
    }
    await page.getByTestId("setup-provider-local").click();
    await page.getByTestId("setup-next").click();
    await expect(page.getByRole("heading", { name: "Connect Local" })).toBeVisible();
    await page.getByTestId("setup-next").click();
    await expect(page.getByRole("heading", { name: "Choose a model" })).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.getByTestId("setup-next")).toBeDisabled();
    await page.getByRole("option", { name: "GLM-5.3-Flash-NVFP4" }).click();
    await expect(page.getByTestId("setup-next")).toBeEnabled();
    await page.getByTestId("setup-next").click();
    await expect(page.getByRole("heading", { name: "Review inference" })).toBeVisible();
    await expect(page.getByText(/temperature 1 and top-p 0.95/)).toBeVisible();
    await page.getByRole("button", { name: "Customize" }).click();
    await expect(page.getByLabel("Temperature")).toHaveValue("1");
    await expect(page.getByLabel("Top-p")).toHaveValue("0.95");
    await page.getByTestId("setup-next").click();

    await expect(page.getByRole("heading", { name: "You’re in" })).toHaveCount(0);

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

    await openConfig(page);
    await expect(page.getByText("Local server")).toBeVisible();
    await page.getByRole("button", { name: "Change access…" }).click();
    await page
      .getByRole("combobox", { name: "Tool ceiling" })
      .selectOption("meta-only");
    await page.getByRole("button", { name: "Review complete — restart" }).click();
    await expect(page.getByText("meta-only")).toBeVisible();
    await expect(page.getByText("No host path").first()).toBeVisible();
  });

  test("validates custom roots inline and can select metatools without another step", async ({
    page,
  }) => {
    await gotoHarness(page, "empty-fleet");
    await page
      .getByRole("textbox", { name: "Tool root", exact: true })
      .fill("/missing folder");
    await page.getByRole("textbox", { name: "Tool root", exact: true }).press("Tab");
    await expect(page.getByText(/Cannot access \/missing folder/)).toBeVisible();
    await expect(page.getByTestId("setup-next")).toBeDisabled();
    await page
      .getByRole("combobox", { name: "Tool ceiling" })
      .selectOption("meta-only");
    await expect(
      page.getByRole("textbox", { name: "Tool root", exact: true }),
    ).toBeDisabled();
    await page.getByTestId("setup-next").click();
    await expect(
      page.getByRole("heading", { name: "Choose an inference provider" }),
    ).toBeVisible({ timeout: 15000 });
  });

  test("starts a read-only custom root and retains local choices across remote selection", async ({
    page,
  }) => {
    await gotoHarness(page, "empty-fleet");
    await page.getByRole("textbox", { name: "Agent name" }).fill("Read-only helper");
    await page.getByRole("combobox", { name: "Tool ceiling" }).selectOption("readonly");
    await page
      .getByRole("textbox", { name: "Tool root", exact: true })
      .fill("/tmp/my workspace");
    await page.getByRole("textbox", { name: "Tool root", exact: true }).press("Tab");
    await expect(page.getByTestId("setup-next")).toBeEnabled();
    await page.getByRole("radio", { name: /Remote connect/ }).click();
    await expect(
      page.getByRole("textbox", { name: "Tool root", exact: true }),
    ).toHaveCount(0);
    await page.getByRole("radio", { name: /Local agent/ }).click();
    await expect(
      page.getByRole("textbox", { name: "Tool root", exact: true }),
    ).toHaveValue("/tmp/my workspace");
    await expect(page.getByRole("combobox", { name: "Tool ceiling" })).toHaveValue(
      "readonly",
    );
    await page.getByTestId("setup-next").click();
    await expect(
      page.getByRole("heading", { name: "Choose an inference provider" }),
    ).toBeVisible({ timeout: 15000 });
  });
});
