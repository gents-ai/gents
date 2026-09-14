import {
  composer,
  expect,
  gotoHarness,
  openConfig,
  sendButton,
  test,
} from "./desktopTest";

test.describe("first-run install", () => {
  test("configures Claude thinking and model-specific token defaults", async ({
    page,
  }) => {
    await gotoHarness(page, "empty-fleet");
    await page.getByTestId("setup-next").click();
    await page.getByTestId("setup-provider-anthropic").click();
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    await page.getByRole("button", { name: "Connect and find models" }).click();
    await page.getByRole("option", { name: "claude-sonnet-5", exact: true }).click();
    await expect(
      page.getByRole("combobox", { name: "Reasoning effort" }),
    ).toContainText("high");
    await page.getByRole("combobox", { name: "Reasoning effort" }).click();
    await page.getByRole("option", { name: "xhigh", exact: true }).click();
    await page.getByRole("button", { name: "Advanced settings" }).click();
    await expect(page.getByLabel("Context window", { exact: true })).toHaveValue(
      "1000000",
    );
    await expect(page.getByLabel("Max output tokens", { exact: true })).toHaveValue(
      "64000",
    );
    await page.getByTestId("setup-save-inference").click();
    await expect(page.getByTestId("session-screen")).toBeVisible();
  });

  test("adds a backend through the same provider flow after onboarding", async ({
    page,
  }) => {
    await gotoHarness(page, "empty-fleet");
    await page.getByTestId("setup-next").click();
    await page.getByTestId("setup-provider-local").click();
    await page.getByRole("button", { name: "Connect and find models" }).click();
    await page.getByRole("option", { name: "GLM-5.3-Flash-NVFP4" }).click();
    await page.getByTestId("setup-save-inference").click();
    await expect(page.getByTestId("session-screen")).toBeVisible();
    await openConfig(page);
    // Leave the hover-expanded app rail before using the configuration sidebar.
    await page.mouse.move(page.viewportSize()!.width - 30, 100);
    const section = page.getByRole("combobox", { name: "Section", exact: true });
    if (await section.isVisible()) {
      await section.click();
      await page.getByRole("option", { name: /^Backends\b/ }).click();
    } else {
      await page.getByRole("link", { name: /^Backends\b/ }).click();
    }
    await page.getByRole("button", { name: "New backend" }).click();
    await expect(
      page.getByRole("heading", { name: "Add an inference backend" }),
    ).toBeVisible();
    const panel = page.getByTestId("inference-setup-panel");
    const bounds = await panel.boundingBox();
    expect(bounds).not.toBeNull();
    expect(bounds!.x).toBeGreaterThanOrEqual(0);
    expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(page.viewportSize()!.width);
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    await page.getByRole("button", { name: "Connect and find models" }).click();
    await page.getByRole("option", { name: "gpt-5.5", exact: true }).click();
    await page.getByRole("button", { name: "Save backend", exact: true }).click();
    await expect(page.getByRole("button", { name: "New backend" })).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "Add an inference backend" }),
    ).toHaveCount(0);
  });

  test("keeps selection and shows an actionable error when the operator save fails", async ({
    page,
  }) => {
    await page.goto(
      "/tests/ui-harness/harness.html?scenario=empty-fleet&configApplyFailure=once",
    );
    await page.getByTestId("setup-next").click();
    await page.getByTestId("setup-provider-local").click();
    await page.getByRole("button", { name: "Connect and find models" }).click();
    await page.getByRole("option", { name: "GLM-5.3-Flash-NVFP4" }).click();
    await page.getByTestId("setup-save-inference").click();
    await expect(page.getByRole("alert")).toContainText(
      "The agent could not save inference",
    );
    await expect(page.getByTestId("session-screen")).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Change model" })).toBeVisible();
    await page.getByTestId("setup-save-inference").click();
    await expect(page.getByTestId("session-screen")).toBeVisible({ timeout: 10000 });
  });

  test("binds the signed-in Codex model and exposes reasoning before customization", async ({
    page,
  }) => {
    await gotoHarness(page, "empty-fleet");
    await page.getByTestId("setup-next").click();
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    await expect(page.getByText("Account connected", { exact: true })).toBeVisible();
    await page.getByRole("button", { name: "Connect and find models" }).click();
    await page.getByRole("option", { name: "gpt-5.5", exact: true }).click();
    await expect(page.getByRole("listbox", { name: "Advertised models" })).toHaveCount(
      0,
    );
    await expect(
      page.getByRole("textbox", { name: "Search advertised models" }),
    ).toHaveCount(0);
    await page.getByRole("combobox", { name: "Reasoning effort" }).click();
    await page.getByRole("option", { name: "high", exact: true }).click();
    await expect(page.getByText("Provider managed", { exact: true })).toBeVisible();
    await page.getByRole("button", { name: "Change model", exact: true }).click();
    await expect(
      page.getByRole("listbox", { name: "Advertised models" }),
    ).toBeVisible();
    await page.getByRole("option", { name: "gpt-5.5", exact: true }).click();
    await page.getByTestId("setup-save-inference").click();
    await expect(page.getByTestId("session-screen")).toBeVisible({ timeout: 10000 });
    await composer(page).fill("Codex onboarding assurance");
    await sendButton(page).click();
    await expect(page.getByText(/Bombadil harness response/)).toBeVisible();
  });

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
    await expect(
      page.getByRole("textbox", { name: "Endpoint", exact: true }),
    ).toBeVisible();
    await page.getByRole("button", { name: "Connect and find models" }).click();
    await expect(page.getByRole("heading", { name: "Choose a model" })).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.getByTestId("setup-save-inference")).toHaveCount(0);
    await page.getByRole("option", { name: "GLM-5.3-Flash-NVFP4" }).click();
    await expect(page.getByRole("heading", { name: "Model defaults" })).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "Choose an inference provider" }),
    ).toBeVisible();
    await expect(page.getByText(/Gents recommends/)).toHaveCount(0);
    await expect(page.getByTestId("inference-custom-controls")).toHaveCount(0);
    await expect(page.getByLabel("Temperature")).toHaveValue("1");
    await expect(page.getByLabel("Top-p")).toHaveValue("0.95");
    await page.getByTestId("setup-save-inference").click();

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
  test("expands remote connection on the first page and retains its address", async ({
    page,
  }) => {
    await gotoHarness(page, "empty-fleet");
    await page.getByRole("radio", { name: /Remote connect/ }).click();
    await expect(page.getByRole("heading", { name: "Let’s get set up" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Request access" })).toBeDisabled();
    await page
      .getByRole("textbox", { name: "Server address" })
      .fill("https://agent.example.net:8787");
    await expect(page.getByRole("button", { name: "Request access" })).toBeEnabled();
    await page.getByRole("radio", { name: /Local agent/ }).click();
    await page.getByRole("radio", { name: /Remote connect/ }).click();
    await expect(page.getByRole("textbox", { name: "Server address" })).toHaveValue(
      "https://agent.example.net:8787",
    );
    await expect(
      page.getByRole("heading", { name: "Connect to a server" }),
    ).toHaveCount(0);
  });

  test("switches provider cards without carrying the previous model or defaults", async ({
    page,
  }) => {
    await gotoHarness(page, "empty-fleet");
    await page.getByTestId("setup-next").click();
    await page.getByTestId("setup-provider-local").click();
    await page.getByRole("button", { name: "Connect and find models" }).click();
    await page.getByRole("option", { name: "GLM-5.3-Flash-NVFP4" }).click();
    await expect(page.getByRole("heading", { name: "Model defaults" })).toBeVisible();
    await page.getByTestId("setup-provider-openrouter").click();
    await expect(page.getByRole("heading", { name: "Model defaults" })).toHaveCount(0);
    await expect(page.getByLabel("API key", { exact: true })).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Connect and find models" }),
    ).toBeDisabled();
    await page.getByTestId("setup-provider-local").click();
    await expect(
      page.getByRole("textbox", { name: "Endpoint", exact: true }),
    ).toBeVisible();
    await expect(page.getByRole("heading", { name: "Choose a model" })).toHaveCount(0);
    await expect(page.getByTestId("setup-save-inference")).toHaveCount(0);
  });
});
