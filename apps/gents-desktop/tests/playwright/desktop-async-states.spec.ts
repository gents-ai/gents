import {
  expect,
  gotoHarness,
  openChat,
  openConfig,
  openConfigTab,
  PEER_ID,
  saveConfig,
  test,
} from "./desktopTest";

// Exercises the three harness scenarios that were defined but never driven:
// `loading`, `save-error`, `backend-health-error`. Assertions go deeper than the
// existing single-banner sad-path check — convergence, recovery, and failure
// isolation — so a regression in async/error handling can't ship unseen.
test.describe("desktop async states", () => {
  test("loading scenario converges from a busy fleet-empty to the dashboard", async ({
    page,
  }) => {
    await gotoHarness(page, "loading");
    await expect(page.getByTestId("fleet-dashboard")).toBeVisible();
    await expect(page.getByTestId(`fleet-row-${PEER_ID}`)).toBeVisible();
    await expect(page.getByTestId("error-banner")).toHaveCount(0);
  });

  test("save-error surfaces the banner, suppresses the Saved chip, and recovers", async ({
    page,
  }) => {
    await gotoHarness(page, "save-error");
    await openConfig(page);
    await openConfigTab(page, "behavior");

    await page
      .getByTestId("behavior-system-prompt")
      .fill("This behavior save is rejected by the harness.");
    await page.getByTestId("behavior-save").click();

    await expect(page.getByTestId("error-banner")).toContainText(
      "Harness rejected behavior save",
    );
    await expect(
      page.locator(".config-editor").getByText("Saved", { exact: true }),
    ).toHaveCount(0);
    const saveButton = page.getByTestId("behavior-save");
    await expect(saveButton).toBeEnabled();
    await expect(saveButton).toHaveText("Save Behavior");

    await page.getByTestId("config-tab-backends").click();
    await expect(page.getByTestId("confirm-dialog")).toBeVisible();
    await expect(page.getByTestId("behavior-system-prompt")).toHaveValue(
      "This behavior save is rejected by the harness.",
    );
    await page.getByTestId("confirm-dialog-confirm").click();
    await expect(page.getByTestId("config-tab-backends")).toHaveClass(/selected/);
    await page.getByTestId("backend-name").fill("OpenAI Harness Recovered");
    await saveConfig(page, "backend-save");
    await expect(page.getByTestId("error-banner")).toHaveCount(0);
  });

  test("backend-health-error is isolated to the Backends tab; other ops tabs still work", async ({
    page,
  }) => {
    await gotoHarness(page, "backend-health-error");
    await openChat(page);

    await page.getByRole("button", { name: /open operations drawer/i }).click();
    await expect(page.getByRole("complementary", { name: "Operations" })).toBeVisible();

    await page.getByRole("tab", { name: /Backends/ }).click();
    await expect(page.getByRole("heading", { name: "Backend health" })).toBeVisible();
    await expect(page.locator(".backend-health__error")).toHaveText(
      "Failed to load backend health: Harness backend health bridge unavailable.",
    );

    await page.getByRole("tab", { name: /MCP health/ }).click();
    await expect(
      page.getByRole("heading", { name: "MCP services / health" }),
    ).toBeVisible();
    await expect(page.getByText("mcp-observability")).toBeVisible();

    await page.getByRole("tab", { name: /Lineage/ }).click();
    await expect(page.getByRole("tree", { name: "Subagent lineage" })).toBeVisible();

    await expect(page.getByTestId("error-banner")).toHaveCount(0);
  });
});
